use crate::browser_slot::NavigationState;
use crate::dispatch::{WebviewCommand, WebviewDispatcher};
use cef::ProcessId;

/// Minimal runtime-facing webview dispatch trait.
///
/// This trait intentionally mirrors the command-oriented shape we need for a
/// future `tauri-runtime` adapter while remaining independent from Tauri's
/// unstable internal traits.
pub trait TauriLikeWebviewDispatch {
    fn dispatch(&self, command: WebviewCommand) -> bool;
    fn navigation_state(&self) -> Option<NavigationState>;

    fn navigate(&self, url: String) -> bool {
        self.dispatch(WebviewCommand::LoadUrl(url))
    }

    fn eval_script(&self, script: String) -> bool {
        self.dispatch(WebviewCommand::Eval(script))
    }

    fn go_back(&self) -> bool {
        self.dispatch(WebviewCommand::GoBack)
    }

    fn go_forward(&self) -> bool {
        self.dispatch(WebviewCommand::GoForward)
    }

    fn reload(&self) -> bool {
        self.dispatch(WebviewCommand::Reload)
    }

    fn reload_ignore_cache(&self) -> bool {
        self.dispatch(WebviewCommand::ReloadIgnoreCache)
    }

    fn stop_load(&self) -> bool {
        self.dispatch(WebviewCommand::StopLoad)
    }

    fn send_process_message(
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

    fn close_webview(&self, force: bool) -> bool {
        self.dispatch(WebviewCommand::Close { force })
    }
}

impl TauriLikeWebviewDispatch for WebviewDispatcher {
    fn dispatch(&self, command: WebviewCommand) -> bool {
        Self::dispatch(self, command)
    }

    fn navigation_state(&self) -> Option<NavigationState> {
        Self::navigation_state(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trait_defaults_forward_to_dispatch() {
        let dispatcher = WebviewDispatcher::new(crate::browser_slot::BrowserSlot::new());
        assert!(!dispatcher.navigate("https://tauri.app".to_string()));
        assert!(!dispatcher.eval_script("1 + 1".to_string()));
        assert!(!dispatcher.reload());
        assert!(!dispatcher.reload_ignore_cache());
        assert!(!dispatcher.stop_load());
    }
}
