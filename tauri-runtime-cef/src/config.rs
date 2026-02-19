use cef::{CefString, Color, Settings};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CefRuntimeConfig {
    pub no_sandbox: bool,
    pub external_message_pump: bool,
    pub multi_threaded_message_loop: bool,
    pub root_cache_path: Option<PathBuf>,
    pub cache_path: Option<PathBuf>,
    pub user_agent: Option<String>,
    pub background_color: Option<Color>,
}

impl Default for CefRuntimeConfig {
    fn default() -> Self {
        Self {
            no_sandbox: true,
            external_message_pump: true,
            multi_threaded_message_loop: false,
            root_cache_path: None,
            cache_path: None,
            user_agent: None,
            background_color: None,
        }
    }
}

impl CefRuntimeConfig {
    pub fn into_settings(self) -> Settings {
        let mut settings = Settings {
            no_sandbox: i32::from(self.no_sandbox),
            external_message_pump: i32::from(self.external_message_pump),
            multi_threaded_message_loop: i32::from(self.multi_threaded_message_loop),
            ..Default::default()
        };

        if let Some(cache_path) = self.cache_path {
            settings.cache_path = CefString::from(cache_path.to_string_lossy().as_ref());
        }

        if let Some(root_cache_path) = self.root_cache_path {
            settings.root_cache_path = CefString::from(root_cache_path.to_string_lossy().as_ref());
        }

        if let Some(user_agent) = self.user_agent {
            settings.user_agent = CefString::from(user_agent.as_str());
        }

        if let Some(background_color) = self.background_color {
            settings.background_color = background_color;
        }

        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_enables_external_pump() {
        let settings = CefRuntimeConfig::default().into_settings();
        assert_eq!(settings.external_message_pump, 1);
        assert_eq!(settings.multi_threaded_message_loop, 0);
    }
}
