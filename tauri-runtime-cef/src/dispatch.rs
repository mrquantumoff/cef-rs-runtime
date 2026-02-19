use crate::browser_slot::{BrowserSlot, NavigationState};
use cef::ProcessId;

#[derive(Debug, Clone)]
pub enum WebviewCommand {
    LoadUrl(String),
    Eval(String),
    GoBack,
    GoForward,
    Reload,
    ReloadIgnoreCache,
    StopLoad,
    SendProcessMessage {
        target_process: ProcessId,
        name: String,
        payload: Option<String>,
    },
    Close {
        force: bool,
    },
}

#[derive(Clone)]
pub struct WebviewDispatcher {
    browser_slot: BrowserSlot,
}

impl WebviewDispatcher {
    pub fn new(browser_slot: BrowserSlot) -> Self {
        Self { browser_slot }
    }

    pub fn navigation_state(&self) -> Option<NavigationState> {
        self.browser_slot.navigation_state()
    }

    pub fn navigate(&self, url: String) -> bool {
        self.dispatch(WebviewCommand::LoadUrl(url))
    }

    pub fn eval_script(&self, script: String) -> bool {
        self.dispatch(WebviewCommand::Eval(script))
    }

    pub fn go_back(&self) -> bool {
        self.dispatch(WebviewCommand::GoBack)
    }

    pub fn go_forward(&self) -> bool {
        self.dispatch(WebviewCommand::GoForward)
    }

    pub fn reload(&self) -> bool {
        self.dispatch(WebviewCommand::Reload)
    }

    pub fn reload_ignore_cache(&self) -> bool {
        self.dispatch(WebviewCommand::ReloadIgnoreCache)
    }

    pub fn stop_load(&self) -> bool {
        self.dispatch(WebviewCommand::StopLoad)
    }

    pub fn send_process_message(
        &self,
        target_process: ProcessId,
        name: String,
        payload: Option<String>,
    ) -> bool {
        self.dispatch(WebviewCommand::SendProcessMessage {
            target_process,
            name,
            payload,
        })
    }

    pub fn close_webview(&self, force: bool) -> bool {
        self.dispatch(WebviewCommand::Close { force })
    }

    pub fn dispatch(&self, command: WebviewCommand) -> bool {
        match command {
            WebviewCommand::LoadUrl(url) => self.browser_slot.load_url(&url),
            WebviewCommand::Eval(script) => self.browser_slot.eval(&script),
            WebviewCommand::GoBack => self.browser_slot.go_back(),
            WebviewCommand::GoForward => self.browser_slot.go_forward(),
            WebviewCommand::Reload => self.browser_slot.reload(false),
            WebviewCommand::ReloadIgnoreCache => self.browser_slot.reload(true),
            WebviewCommand::StopLoad => self.browser_slot.stop_load(),
            WebviewCommand::SendProcessMessage {
                target_process,
                name,
                payload,
            } => self
                .browser_slot
                .send_process_message(target_process, &name, payload.as_deref()),
            WebviewCommand::Close { force } => {
                self.browser_slot.close(force);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_browser_returns_false_for_commands() {
        let dispatcher = WebviewDispatcher::new(BrowserSlot::new());
        assert!(!dispatcher.dispatch(WebviewCommand::LoadUrl("https://tauri.app".to_string(),)));
        assert!(!dispatcher.dispatch(WebviewCommand::Eval("1 + 1".to_string())));
        assert!(!dispatcher.dispatch(WebviewCommand::Reload));
    }
}
