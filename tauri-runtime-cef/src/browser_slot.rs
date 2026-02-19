use cef::{
    self, Browser, CefString, ImplBrowser, ImplBrowserHost, ImplFrame, ImplListValue,
    ImplProcessMessage, ProcessId,
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct BrowserSlot {
    browser: Arc<Mutex<Option<Browser>>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NavigationState {
    pub is_loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

impl BrowserSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, browser: Option<Browser>) {
        if let Ok(mut current_browser) = self.browser.lock() {
            *current_browser = browser;
        }
    }

    pub fn current(&self) -> Option<Browser> {
        self.browser
            .lock()
            .ok()
            .and_then(|current_browser| current_browser.clone())
    }

    pub fn clear(&self) {
        self.set(None);
    }

    pub fn notify_resized(&self) {
        if let Some(current_browser) = self.current() {
            if let Some(host) = current_browser.host() {
                host.notify_move_or_resize_started();
                host.was_resized();
            }
        }
    }

    pub fn close(&self, force: bool) {
        if let Some(current_browser) = self.current() {
            if let Some(host) = current_browser.host() {
                host.close_browser(i32::from(force));
            }
        }
    }

    pub fn navigation_state(&self) -> Option<NavigationState> {
        let current_browser = self.current()?;
        Some(NavigationState {
            is_loading: current_browser.is_loading() != 0,
            can_go_back: current_browser.can_go_back() != 0,
            can_go_forward: current_browser.can_go_forward() != 0,
        })
    }

    pub fn go_back(&self) -> bool {
        let Some(current_browser) = self.current() else {
            return false;
        };
        if current_browser.can_go_back() == 0 {
            return false;
        }
        current_browser.go_back();
        true
    }

    pub fn go_forward(&self) -> bool {
        let Some(current_browser) = self.current() else {
            return false;
        };
        if current_browser.can_go_forward() == 0 {
            return false;
        }
        current_browser.go_forward();
        true
    }

    pub fn reload(&self, ignore_cache: bool) -> bool {
        let Some(current_browser) = self.current() else {
            return false;
        };
        if ignore_cache {
            current_browser.reload_ignore_cache();
        } else {
            current_browser.reload();
        }
        true
    }

    pub fn stop_load(&self) -> bool {
        let Some(current_browser) = self.current() else {
            return false;
        };
        current_browser.stop_load();
        true
    }

    pub fn load_url(&self, url: &str) -> bool {
        let Some(current_browser) = self.current() else {
            return false;
        };
        let Some(main_frame) = current_browser.main_frame() else {
            return false;
        };
        let url = CefString::from(url);
        main_frame.load_url(Some(&url));
        true
    }

    pub fn eval(&self, script: &str) -> bool {
        let Some(current_browser) = self.current() else {
            return false;
        };
        let Some(main_frame) = current_browser.main_frame() else {
            return false;
        };
        let script = CefString::from(script);
        let script_url = CefString::from("tauri-runtime-cef://eval");
        main_frame.execute_java_script(Some(&script), Some(&script_url), 1);
        true
    }

    pub fn send_process_message(
        &self,
        target_process: ProcessId,
        name: &str,
        payload: Option<&str>,
    ) -> bool {
        let Some(current_browser) = self.current() else {
            return false;
        };
        let Some(main_frame) = current_browser.main_frame() else {
            return false;
        };

        let message_name = CefString::from(name);
        let Some(mut message) = cef::process_message_create(Some(&message_name)) else {
            return false;
        };

        if let Some(payload) = payload {
            let Some(arguments) = message.argument_list() else {
                return false;
            };

            if arguments.set_size(1) == 0 {
                return false;
            }

            let payload = CefString::from(payload);
            if arguments.set_string(0, Some(&payload)) == 0 {
                return false;
            }
        }

        main_frame.send_process_message(target_process, Some(&mut message));
        true
    }
}
