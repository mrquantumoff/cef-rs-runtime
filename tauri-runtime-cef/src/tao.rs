use crate::pump::RuntimePump;
use cef::external_message_pump::ExternalMessagePumpScheduleQueue;
use std::sync::Arc;
use std::time::Instant;
use tao::event::Event;
use tao::event_loop::ControlFlow;
use tao::event_loop::EventLoopProxy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CefPumpEvent {
    ScheduleWork,
}

#[derive(Debug, thiserror::Error)]
#[error("tao event loop is closed")]
pub struct EventLoopClosed;

#[derive(Clone)]
pub struct TaoExternalPumpHandle {
    queue: Arc<ExternalMessagePumpScheduleQueue>,
    proxy: EventLoopProxy<CefPumpEvent>,
}

impl TaoExternalPumpHandle {
    pub fn schedule_work(&self, delay_ms: i64) -> Result<(), EventLoopClosed> {
        if self.queue.push(delay_ms) {
            self.proxy
                .send_event(CefPumpEvent::ScheduleWork)
                .map_err(|_| EventLoopClosed)?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct TaoExternalPump {
    queue: Arc<ExternalMessagePumpScheduleQueue>,
    pump: RuntimePump,
}

#[derive(Debug)]
pub struct TaoPumpDriver {
    pump: TaoExternalPump,
}

impl TaoPumpDriver {
    pub fn new(proxy: EventLoopProxy<CefPumpEvent>) -> (Self, TaoExternalPumpHandle) {
        let (pump, handle) = TaoExternalPump::new(proxy);
        (Self { pump }, handle)
    }

    pub fn control_flow(&self) -> ControlFlow {
        match self.pump.next_deadline() {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        }
    }

    pub fn on_event(&mut self, event: &Event<CefPumpEvent>) {
        match event {
            Event::UserEvent(event) => self.pump.on_event(*event),
            Event::MainEventsCleared => self.pump.on_event_loop_tick(),
            _ => {}
        }
    }
}

impl TaoExternalPump {
    pub fn new(proxy: EventLoopProxy<CefPumpEvent>) -> (Self, TaoExternalPumpHandle) {
        let queue = Arc::new(ExternalMessagePumpScheduleQueue::new());
        let handle = TaoExternalPumpHandle {
            queue: queue.clone(),
            proxy,
        };
        (
            Self {
                queue,
                pump: RuntimePump::new(),
            },
            handle,
        )
    }

    pub fn on_event(&mut self, event: CefPumpEvent) {
        if matches!(event, CefPumpEvent::ScheduleWork) {
            while let Some(delay_ms) = self.queue.pop() {
                self.pump.schedule_work(delay_ms);
            }
        }
    }

    pub fn on_event_loop_tick(&mut self) {
        self.pump.process_due_work(Instant::now());
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pump.next_deadline()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tao::event_loop::EventLoopBuilder;

    #[cfg(target_os = "linux")]
    use tao::platform::unix::EventLoopBuilderExtUnix;
    #[cfg(target_os = "windows")]
    use tao::platform::windows::EventLoopBuilderExtWindows;

    #[test]
    fn driver_switches_to_wait_until_after_schedule() {
        let mut builder = EventLoopBuilder::<CefPumpEvent>::with_user_event();
        #[cfg(target_os = "linux")]
        builder.with_any_thread(true);
        #[cfg(target_os = "windows")]
        builder.with_any_thread(true);
        let event_loop = builder.build();
        let proxy = event_loop.create_proxy();
        let (mut driver, handle) = TaoPumpDriver::new(proxy);

        handle.schedule_work(10).expect("failed to schedule work");
        driver.on_event(&Event::UserEvent(CefPumpEvent::ScheduleWork));

        assert!(matches!(driver.control_flow(), ControlFlow::WaitUntil(_)));
    }
}
