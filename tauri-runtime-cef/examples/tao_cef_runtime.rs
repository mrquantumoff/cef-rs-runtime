use cef::*;
#[cfg(target_os = "linux")]
use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::sync::{Arc, Mutex};
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use tauri_runtime_cef::{
    browser_slot::BrowserSlot,
    client::{BrowserEvent, RuntimeClientBuilder},
    config::CefRuntimeConfig,
    dispatch::WebviewDispatcher,
    tao::{CefPumpEvent, TaoExternalPumpHandle, TaoPumpDriver},
    tao_window::HostWindowInfo,
};

#[cfg(target_os = "macos")]
type LoadedLibrary = library_loader::LibraryLoader;

#[cfg(not(target_os = "macos"))]
type LoadedLibrary = ();

#[cfg(target_os = "macos")]
fn load_cef() -> LoadedLibrary {
    let loader = library_loader::LibraryLoader::new(&std::env::current_exe().unwrap(), false);
    assert!(loader.load());
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    loader
}

#[cfg(not(target_os = "macos"))]
fn load_cef() -> LoadedLibrary {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    ()
}

wrap_app! {
    struct RuntimeApp {
        handle: Arc<Mutex<TaoExternalPumpHandle>>,
        host_window: HostWindowInfo,
        browser: BrowserSlot,
        client: Arc<Mutex<Option<Client>>>,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else {
                return;
            };

            let process_type = process_type
                .map(CefString::to_string)
                .unwrap_or_default();

            if process_type.is_empty() {
                #[cfg(target_os = "linux")]
                {
                    let is_wayland_session = std::env::var("XDG_SESSION_TYPE")
                        .map(|value| value.eq_ignore_ascii_case("wayland"))
                        .unwrap_or(false);

                    if is_wayland_session {
                        command_line.append_switch_with_value(
                            Some(&CefString::from("ozone-platform-hint")),
                            Some(&CefString::from("wayland")),
                        );
                    }

                    command_line.append_switch(Some(&CefString::from("disable-gpu-process-crash-limit")));
                    command_line.append_switch(Some(&CefString::from("in-process-gpu")));

                    if !is_wayland_session {
                        command_line.append_switch(Some(&CefString::from("disable-gpu")));
                        command_line
                            .append_switch(Some(&CefString::from("disable-gpu-compositing")));
                        command_line.append_switch_with_value(
                            Some(&CefString::from("use-gl")),
                            Some(&CefString::from("swiftshader")),
                        );
                        command_line.append_switch_with_value(
                            Some(&CefString::from("use-angle")),
                            Some(&CefString::from("swiftshader")),
                        );
                    }
                }
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(RuntimeBrowserProcessHandler::new(
                self.handle.clone(),
                self.host_window,
                self.browser.clone(),
                self.client.clone(),
            ))
        }
    }
}

wrap_browser_process_handler! {
    struct RuntimeBrowserProcessHandler {
        handle: Arc<Mutex<TaoExternalPumpHandle>>,
        host_window: HostWindowInfo,
        browser: BrowserSlot,
        client: Arc<Mutex<Option<Client>>>,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            let settings = BrowserSettings::default();
            let url = CefString::from("https://tauri.app");
            let mut client = self.default_client();

            let bounds = Rect {
                x: 0,
                y: 0,
                width: self.host_window.width,
                height: self.host_window.height,
            };

            let window_info = WindowInfo::default().set_as_child(self.host_window.parent_handle, &bounds);

            self.browser.set(browser_host_create_browser_sync(
                Some(&window_info),
                client.as_mut(),
                Some(&url),
                Some(&settings),
                None,
                None,
            ));
        }

        fn default_client(&self) -> Option<Client> {
            self.client.lock().ok().and_then(|client| client.clone())
        }

        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            if let Ok(handle) = self.handle.lock() {
                let _ = handle.schedule_work(delay_ms);
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Removed X11 backend enforcement

    let _library = load_cef();

    let args = args::Args::new();
    let Some(command_line) = args.as_cmd_line() else {
        return Err("failed to parse command line arguments".into());
    };

    let switch = CefString::from("type");
    let is_browser_process = command_line.has_switch(Some(&switch)) != 1;
    let ret = execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());

    if !is_browser_process {
        if ret >= 0 {
            return Ok(());
        }
        return Err("failed to execute subprocess".into());
    }

    if ret != -1 {
        return Err("failed to execute browser process".into());
    }

    let event_loop = EventLoopBuilder::<CefPumpEvent>::with_user_event().build();
    let host_window = WindowBuilder::new()
        .with_title("tauri-runtime-cef prototype")
        .build(&event_loop)?;
    let host_window = HostWindowInfo::from_tao_window(&host_window)?;

    let proxy = event_loop.create_proxy();
    let (mut pump_driver, handle) = TaoPumpDriver::new(proxy);
    let browser = BrowserSlot::new();
    let dispatcher = WebviewDispatcher::new(browser.clone());
    let client = RuntimeClientBuilder::new()
        .with_browser_slot(browser.clone())
        .on_event(|event| {
            if let BrowserEvent::LoadFinished {
                browser_id,
                url,
                http_status_code,
            } = event
            {
                println!("browser {browser_id} load finished: {url} (status {http_status_code})");
            }
        })
        .build();
    let client = Arc::new(Mutex::new(Some(client)));

    let mut app = RuntimeApp::new(
        Arc::new(Mutex::new(handle)),
        host_window,
        browser.clone(),
        client,
    );

    let mut config = CefRuntimeConfig::default();
    let root_cache_path =
        std::env::temp_dir().join(format!("cef-rs-tauri-runtime-cef-{}", std::process::id()));
    let cache_path = root_cache_path.join("cache");
    config.root_cache_path = Some(root_cache_path);
    config.cache_path = Some(cache_path);

    let settings = config.into_settings();
    if initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        return Err("cef initialize returned 0".into());
    }

    event_loop.run(move |event, _, control_flow| {
        *control_flow = pump_driver.control_flow();
        pump_driver.on_event(&event);

        match event {
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => {
                browser.notify_resized();
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                let _ = dispatcher.close_webview(true);
                shutdown();
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
