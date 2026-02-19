use cef::external_message_pump::{
    ExternalMessagePump, ExternalMessagePumpHost, ExternalMessagePumpScheduleQueue,
};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct TimerHost {
    deadline: Option<Instant>,
}

impl TimerHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn is_due(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
}

impl ExternalMessagePumpHost for TimerHost {
    fn set_timer(&mut self, delay: Duration) {
        self.deadline = Some(Instant::now() + delay);
    }

    fn kill_timer(&mut self) {
        self.deadline = None;
    }

    fn is_timer_pending(&self) -> bool {
        self.deadline.is_some()
    }
}

#[derive(Debug, Default)]
pub struct RuntimePump {
    pump: ExternalMessagePump,
    host: TimerHost,
}

impl RuntimePump {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn schedule_work(&mut self, delay_ms: i64) {
        self.pump
            .on_schedule_message_pump_work(&mut self.host, delay_ms);
    }

    pub fn process_due_work(&mut self, now: Instant) {
        if self.host.is_due(now) {
            self.pump.on_timer(&mut self.host);
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.host.deadline()
    }
}

#[derive(Debug)]
struct ExternalPumpState {
    queue: Arc<ExternalMessagePumpScheduleQueue>,
    pump: RuntimePump,
    primed: bool,
}

impl ExternalPumpState {
    fn new(queue: Arc<ExternalMessagePumpScheduleQueue>) -> Self {
        Self {
            queue,
            pump: RuntimePump::new(),
            primed: false,
        }
    }

    fn drain_scheduled_work(&mut self) {
        while let Some(delay_ms) = self.queue.pop() {
            self.pump.schedule_work(delay_ms);
        }
    }
}

static EXTERNAL_PUMP_STATE: LazyLock<Mutex<Option<ExternalPumpState>>> =
    LazyLock::new(|| Mutex::new(None));

pub fn install_external_message_pump_queue(queue: Arc<ExternalMessagePumpScheduleQueue>) {
    if let Ok(mut state) = EXTERNAL_PUMP_STATE.lock() {
        *state = Some(ExternalPumpState::new(queue));
    }
}

pub fn clear_external_message_pump_queue() {
    if let Ok(mut state) = EXTERNAL_PUMP_STATE.lock() {
        *state = None;
    }
}

pub fn tick_external_message_pump(now: Instant) -> bool {
    let Ok(mut state) = EXTERNAL_PUMP_STATE.lock() else {
        return false;
    };
    let Some(state) = state.as_mut() else {
        return false;
    };

    state.drain_scheduled_work();

    if !state.primed {
        cef::do_message_loop_work();
        state.primed = true;
    }

    state.pump.process_due_work(now);
    true
}

pub fn next_external_message_pump_deadline() -> Option<Instant> {
    let Ok(mut state) = EXTERNAL_PUMP_STATE.lock() else {
        return None;
    };
    let state = state.as_mut()?;
    state.drain_scheduled_work();
    state.pump.next_deadline()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_positive_delay_sets_deadline() {
        let mut pump = RuntimePump::new();
        pump.schedule_work(10_000);
        assert!(pump.next_deadline().is_some());
    }

    #[test]
    fn due_processing_keeps_timer_for_future_deadline() {
        let mut pump = RuntimePump::new();
        pump.schedule_work(10_000);
        let before_deadline = Instant::now();
        pump.process_due_work(before_deadline);
        assert!(pump.next_deadline().is_some());
    }
}
