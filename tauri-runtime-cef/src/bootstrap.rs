use crate::config::CefRuntimeConfig;
use cef::{args::Args, execute_process, initialize, shutdown, App, CefString, ImplCommandLine};

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
    app: Option<&mut App>,
    config: CefRuntimeConfig,
    options: BootstrapOptions,
) -> Result<BootstrapOutcome, BootstrapError> {
    let args = Args::new();
    let Some(command_line) = args.as_cmd_line() else {
        return Err(BootstrapError::InvalidCommandLine);
    };

    let switch = CefString::from("type");
    let is_browser_process = command_line.has_switch(Some(&switch)) != 1;
    let ret = execute_process(Some(args.as_main_args()), None, options.sandbox_info);

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
    if initialize(
        Some(args.as_main_args()),
        Some(&settings),
        app,
        options.sandbox_info,
    ) != 1
    {
        return Err(BootstrapError::InitializeFailed);
    }

    Ok(BootstrapOutcome::Browser(InitializedCef {
        _args: args,
        active: true,
    }))
}
