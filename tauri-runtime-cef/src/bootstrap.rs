use crate::config::CefRuntimeConfig;
use crate::pump::{clear_external_message_pump_queue, install_external_message_pump_queue};
use cef::external_message_pump::ExternalMessagePumpScheduleQueue;
use cef::rc::Rc;
use cef::wrapper::message_router::{
    MessageRouterConfig, MessageRouterRendererSide, MessageRouterRendererSideHandlerCallbacks,
    RendererSideRouter,
};
use cef::{
    args::Args, execute_process, initialize, shutdown, wrap_app, wrap_browser_process_handler,
    wrap_render_process_handler, App, Browser, BrowserProcessHandler, CefString, Frame, ImplApp,
    ImplBrowser, ImplBrowserProcessHandler, ImplCommandLine, ImplFrame, ImplListValue,
    ImplProcessMessage, ImplRenderProcessHandler, ProcessId, ProcessMessage, RenderProcessHandler,
    V8Context, WrapApp, WrapBrowserProcessHandler, WrapRenderProcessHandler,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

const INIT_SCRIPT_ADD_MESSAGE: &str = "__TAURI_ADD_INIT_SCRIPT__";
const INIT_SCRIPT_CLEAR_MESSAGE: &str = "__TAURI_CLEAR_INIT_SCRIPTS__";
#[allow(dead_code)]
static RENDERER_INIT_SCRIPTS_AVAILABLE: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
pub(crate) fn renderer_init_scripts_available() -> bool {
    RENDERER_INIT_SCRIPTS_AVAILABLE.load(Ordering::Relaxed)
}

#[derive(Clone)]
struct RendererInitScript {
    script: String,
    for_main_frame_only: bool,
}

wrap_render_process_handler! {
    struct RuntimeBootstrapRenderProcessHandler {
        router: Arc<RendererSideRouter>,
        init_scripts: Arc<Mutex<HashMap<i32, Vec<RendererInitScript>>>>,
    }

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let browser_clone = browser.as_ref().map(|browser| (*(*browser)).clone());
            let frame_clone = frame.as_ref().map(|frame| (*(*frame)).clone());
            let context_clone = context.as_ref().map(|context| (*(*context)).clone());

            self.router
                .on_context_created(browser_clone, frame_clone.clone(), context_clone);

            let browser_id = browser.as_ref().map(|browser| browser.identifier());
            let Some(browser_id) = browser_id else {
                return;
            };
            let Some(frame) = frame_clone else {
                return;
            };

            let scripts = self
                .init_scripts
                .lock()
                .ok()
                .and_then(|map| map.get(&browser_id).cloned());
            let Some(scripts) = scripts else {
                return;
            };

            let is_main_frame = frame.is_main() != 0;
            let script_url = CefString::from("tauri-runtime-cef://document-start");
            for script in scripts {
                if script.for_main_frame_only && !is_main_frame {
                    continue;
                }

                let code = CefString::from(script.script.as_str());
                frame.execute_java_script(Some(&code), Some(&script_url), 1);
            }
        }

        fn on_context_released(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            self.router
                .on_context_released(browser.cloned(), frame.cloned(), context.cloned());
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            let browser_id = browser.as_ref().map(|browser| browser.identifier());
            let message_name = message
                .as_ref()
                .map(|message| CefString::from(&message.name()).to_string())
                .unwrap_or_default();

            if source_process == ProcessId::BROWSER {
                if message_name == INIT_SCRIPT_CLEAR_MESSAGE {
                    if let Some(browser_id) = browser_id {
                        if let Ok(mut scripts) = self.init_scripts.lock() {
                            scripts.remove(&browser_id);
                        }
                    }
                    return 1;
                }

                if message_name == INIT_SCRIPT_ADD_MESSAGE {
                    if let (Some(browser_id), Some(message)) = (browser_id, message.as_ref()) {
                        if let Some(arguments) = message.argument_list() {
                            let script = CefString::from(&arguments.string(0)).to_string();
                            let for_main_frame_only = arguments.bool(1) != 0;
                            if !script.is_empty() {
                                if let Ok(mut scripts) = self.init_scripts.lock() {
                                    let entry = scripts.entry(browser_id).or_default();
                                    entry.push(RendererInitScript {
                                        script,
                                        for_main_frame_only,
                                    });
                                }
                            }
                        }
                    }
                    return 1;
                }
            }

            i32::from(self.router.on_process_message_received(
                browser.cloned(),
                frame.cloned(),
                Some(source_process),
                message.cloned(),
            ))
        }

        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            let browser_id = browser.as_ref().map(|browser| browser.identifier());
            if let Some(browser_id) = browser_id {
                if let Ok(mut scripts) = self.init_scripts.lock() {
                    scripts.remove(&browser_id);
                }
            }
        }
    }
}

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
        router: Arc<RendererSideRouter>,
        init_scripts: Arc<Mutex<HashMap<i32, Vec<RendererInitScript>>>>,
    }

    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(RuntimeBootstrapBrowserProcessHandler::new(self.queue.clone()))
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(RuntimeBootstrapRenderProcessHandler::new(
                self.router.clone(),
                self.init_scripts.clone(),
            ))
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
    RENDERER_INIT_SCRIPTS_AVAILABLE.store(false, Ordering::Relaxed);

    let mut internal_app = None;
    if app.is_none() {
        let queue = Arc::new(ExternalMessagePumpScheduleQueue::new());
        let router = RendererSideRouter::new(MessageRouterConfig::default());
        let init_scripts = Arc::new(Mutex::new(HashMap::new()));
        if config.external_message_pump {
            install_external_message_pump_queue(queue.clone());
        } else {
            clear_external_message_pump_queue();
        }
        internal_app = Some(RuntimeBootstrapApp::new(queue, router, init_scripts));
        RENDERER_INIT_SCRIPTS_AVAILABLE.store(true, Ordering::Relaxed);
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
        RENDERER_INIT_SCRIPTS_AVAILABLE.store(false, Ordering::Relaxed);
        return Err(BootstrapError::InitializeFailed);
    }

    Ok(BootstrapOutcome::Browser(InitializedCef {
        _args: args,
        active: true,
    }))
}
