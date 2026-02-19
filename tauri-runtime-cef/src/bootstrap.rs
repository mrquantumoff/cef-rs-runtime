use crate::config::CefRuntimeConfig;
use crate::pump::{clear_external_message_pump_queue, install_external_message_pump_queue};
use cef::external_message_pump::ExternalMessagePumpScheduleQueue;
use cef::rc::Rc;
use cef::{
    args::Args, execute_process, initialize, shutdown, wrap_app, wrap_browser_process_handler, App,
    BrowserProcessHandler, CefString, ImplApp, ImplBrowserProcessHandler, ImplCommandLine, WrapApp,
    WrapBrowserProcessHandler,
};
use std::sync::Arc;

wrap_browser_process_handler! {
    struct RuntimeBootstrapBrowserProcessHandler {
        queue: Arc<ExternalMessagePumpScheduleQueue>,
    }

    impl BrowserProcessHandler {
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            let _ = self.queue.push(delay_ms);
        }
    }
}

wrap_app! {
    struct RuntimeBootstrapApp {
        queue: Arc<ExternalMessagePumpScheduleQueue>,
    }

    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(RuntimeBootstrapBrowserProcessHandler::new(self.queue.clone()))
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("failed to parse command line arguments")]
    InvalidCommandLine,
    #[error("failed to execute subprocess")]
    SubprocessExecuteFailed,
    #[error("failed to execute browser process")]
    BrowserExecuteFailed,
    #[error("cef initialize returned 0")]
    InitializeFailed,
}

pub enum BootstrapOutcome {
    Browser(InitializedCef),
    Subprocess(i32),
}

pub struct InitializedCef {
    _args: Args,
    active: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct BootstrapOptions {
    pub sandbox_info: *mut u8,
}

impl Default for BootstrapOptions {
    fn default() -> Self {
        Self {
            sandbox_info: std::ptr::null_mut(),
        }
    }
}

impl InitializedCef {
    pub fn shutdown(&mut self) {
        if self.active {
            shutdown();
            self.active = false;
        }
    }
}

impl Drop for InitializedCef {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn bootstrap(
    app: Option<&mut App>,
    config: CefRuntimeConfig,
) -> Result<BootstrapOutcome, BootstrapError> {
    bootstrap_with_options(app, config, BootstrapOptions::default())
}

pub fn bootstrap_with_options(
    mut app: Option<&mut App>,
    config: CefRuntimeConfig,
    options: BootstrapOptions,
) -> Result<BootstrapOutcome, BootstrapError> {
    let mut internal_app = None;
    if app.is_none() && config.external_message_pump {
        let queue = Arc::new(ExternalMessagePumpScheduleQueue::new());
        install_external_message_pump_queue(queue.clone());
        internal_app = Some(RuntimeBootstrapApp::new(queue));
    } else {
        clear_external_message_pump_queue();
    }

    let args = Args::new();
    let Some(command_line) = args.as_cmd_line() else {
        return Err(BootstrapError::InvalidCommandLine);
    };

    let switch = CefString::from("type");
    let is_browser_process = command_line.has_switch(Some(&switch)) != 1;
    let execute_process_app = match app.as_deref_mut() {
        Some(app) => Some(app),
        None => internal_app.as_mut(),
    };
    let ret = execute_process(
        Some(args.as_main_args()),
        execute_process_app,
        options.sandbox_info,
    );

    if !is_browser_process {
        if ret >= 0 {
            return Ok(BootstrapOutcome::Subprocess(ret));
        }
        return Err(BootstrapError::SubprocessExecuteFailed);
    }

    if ret != -1 {
        return Err(BootstrapError::BrowserExecuteFailed);
    }

    let settings = config.into_settings();
    let initialize_app = match app.as_deref_mut() {
        Some(app) => Some(app),
        None => internal_app.as_mut(),
    };
    if initialize(
        Some(args.as_main_args()),
        Some(&settings),
        initialize_app,
        options.sandbox_info,
    ) != 1
    {
        clear_external_message_pump_queue();
        return Err(BootstrapError::InitializeFailed);
    }

    Ok(BootstrapOutcome::Browser(InitializedCef {
        _args: args,
        active: true,
    }))
}
