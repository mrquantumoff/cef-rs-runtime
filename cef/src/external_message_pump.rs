use std::time::Duration;
use std::{cmp, sync::Mutex};

/// Special timer delay placeholder value used by CEF examples.
///
/// Hosts should treat this value as "schedule a bounded idle tick".
pub const TIMER_DELAY_PLACEHOLDER_MS: i64 = i32::MAX as i64;

/// Maximum delay between message pump ticks when CEF is idle.
///
/// This matches CEF's reference example behavior (roughly 30 FPS).
pub const MAX_TIMER_DELAY_MS: i64 = 1000 / 30;

/// Platform timer operations required by [`ExternalMessagePump`].
pub trait ExternalMessagePumpHost {
    fn set_timer(&mut self, delay: Duration);
    fn kill_timer(&mut self);
    fn is_timer_pending(&self) -> bool;
}

/// Thread-safe coalescer for
/// `BrowserProcessHandler::on_schedule_message_pump_work` callbacks.
///
/// This helps host runtimes that need to forward CEF scheduling requests from
/// arbitrary threads to the main thread with a single wake-up signal.
#[derive(Debug, Default)]
pub struct ExternalMessagePumpScheduleQueue {
    next_delay_ms: Mutex<Option<i64>>,
}

impl ExternalMessagePumpScheduleQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a scheduling request from any thread.
    ///
    /// Returns `true` when the request should wake the main thread.
    pub fn push(&self, delay_ms: i64) -> bool {
        let normalized = normalize_delay_ms(delay_ms);
        let Ok(mut next_delay_ms) = self.next_delay_ms.lock() else {
            return false;
        };

        match *next_delay_ms {
            Some(current) => {
                let earliest = cmp::min(current, normalized);
                let should_wake = earliest != current;
                *next_delay_ms = Some(earliest);
                should_wake
            }
            None => {
                *next_delay_ms = Some(normalized);
                true
            }
        }
    }

    /// Drain the earliest queued delay for main-thread processing.
    pub fn pop(&self) -> Option<i64> {
        let Ok(mut next_delay_ms) = self.next_delay_ms.lock() else {
            return None;
        };
        next_delay_ms.take()
    }
}

/// Main-thread helper for integrating CEF's external message pump into host
/// event loops (for example Tao/Tauri).
///
/// This helper intentionally does not perform cross-thread dispatching.
/// If [`BrowserProcessHandler::on_schedule_message_pump_work`] is invoked on a
/// background thread, forward that callback to your UI/main thread first and
/// call [`ExternalMessagePump::on_schedule_message_pump_work`] there.
#[derive(Debug, Default)]
pub struct ExternalMessagePump {
    is_active: bool,
    reentrancy_detected: bool,
}

impl ExternalMessagePump {
    pub fn new() -> Self {
        Self::default()
    }

    /// Entry point for forwarded
    /// [`BrowserProcessHandler::on_schedule_message_pump_work`] callbacks.
    pub fn on_schedule_message_pump_work<H: ExternalMessagePumpHost>(
        &mut self,
        host: &mut H,
        delay_ms: i64,
    ) {
        if delay_ms == TIMER_DELAY_PLACEHOLDER_MS && host.is_timer_pending() {
            return;
        }

        host.kill_timer();

        let delay_ms = if delay_ms <= 0 {
            self.do_work(host);
            0
        } else {
            normalize_delay_ms(delay_ms)
        };

        if delay_ms > 0 {
            host.set_timer(Duration::from_millis(delay_ms as u64));
        }
    }

    /// Call this when your host timer fires.
    pub fn on_timer<H: ExternalMessagePumpHost>(&mut self, host: &mut H) {
        host.kill_timer();
        self.do_work(host);
    }

    /// Process one CEF message loop iteration.
    pub fn do_message_loop_work<H: ExternalMessagePumpHost>(&mut self, host: &mut H) {
        self.do_work_with(host, crate::do_message_loop_work);
    }

    fn do_work<H: ExternalMessagePumpHost>(&mut self, host: &mut H) {
        self.do_message_loop_work(host);
    }

    fn do_work_with<H, F>(&mut self, host: &mut H, mut do_message_loop_work: F)
    where
        H: ExternalMessagePumpHost,
        F: FnMut(),
    {
        let was_reentrant = self.perform_message_loop_work_with(&mut do_message_loop_work);
        if was_reentrant {
            self.on_schedule_message_pump_work(host, 0);
        } else if !host.is_timer_pending() {
            self.on_schedule_message_pump_work(host, TIMER_DELAY_PLACEHOLDER_MS);
        }
    }

    fn perform_message_loop_work_with<F>(&mut self, do_message_loop_work: &mut F) -> bool
    where
        F: FnMut(),
    {
        if self.is_active {
            self.reentrancy_detected = true;
            return false;
        }

        self.reentrancy_detected = false;
        self.is_active = true;
        do_message_loop_work();
        self.is_active = false;
        self.reentrancy_detected
    }
}

fn normalize_delay_ms(delay_ms: i64) -> i64 {
    if delay_ms <= 0 {
        0
    } else {
        delay_ms.min(MAX_TIMER_DELAY_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockHost {
        timer_pending: bool,
        set_timer_calls: usize,
        kill_timer_calls: usize,
        last_delay: Option<Duration>,
    }

    impl ExternalMessagePumpHost for MockHost {
        fn set_timer(&mut self, delay: Duration) {
            self.timer_pending = true;
            self.set_timer_calls += 1;
            self.last_delay = Some(delay);
        }

        fn kill_timer(&mut self) {
            self.timer_pending = false;
            self.kill_timer_calls += 1;
        }

        fn is_timer_pending(&self) -> bool {
            self.timer_pending
        }
    }

    #[test]
    fn bounds_large_delays() {
        let mut pump = ExternalMessagePump::new();
        let mut host = MockHost::default();

        pump.on_schedule_message_pump_work(&mut host, 5_000);

        assert_eq!(host.set_timer_calls, 1);
        assert_eq!(
            host.last_delay,
            Some(Duration::from_millis(MAX_TIMER_DELAY_MS as u64))
        );
    }

    #[test]
    fn placeholder_delay_is_ignored_when_timer_exists() {
        let mut pump = ExternalMessagePump::new();
        let mut host = MockHost {
            timer_pending: true,
            ..Default::default()
        };

        pump.on_schedule_message_pump_work(&mut host, TIMER_DELAY_PLACEHOLDER_MS);

        assert_eq!(host.kill_timer_calls, 0);
        assert_eq!(host.set_timer_calls, 0);
    }

    #[test]
    fn queue_coalesces_to_earliest_delay() {
        let queue = ExternalMessagePumpScheduleQueue::new();
        assert!(queue.push(40));
        assert!(!queue.push(100));
        assert!(queue.push(5));
        assert_eq!(queue.pop(), Some(5));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn queue_normalizes_delay_bounds() {
        let queue = ExternalMessagePumpScheduleQueue::new();
        assert!(queue.push(-1));
        assert_eq!(queue.pop(), Some(0));

        assert!(queue.push(10_000));
        assert_eq!(queue.pop(), Some(MAX_TIMER_DELAY_MS));
    }
}
