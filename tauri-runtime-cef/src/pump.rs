use cef::external_message_pump::{ExternalMessagePump, ExternalMessagePumpHost};
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
