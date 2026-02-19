use crate::browser_slot::BrowserSlot;
use cef::*;
use std::{path::PathBuf, sync::Arc};

pub type BrowserEventHandler = Arc<dyn Fn(BrowserEvent) + Send + Sync + 'static>;
pub type BeforeBrowseHandler = Arc<dyn Fn(&str) -> bool + Send + Sync + 'static>;
pub type OpenUrlFromTabHandler = Arc<dyn Fn(&str) -> bool + Send + Sync + 'static>;
pub type DownloadRequestedHandler =
    Arc<dyn Fn(String, String) -> Option<PathBuf> + Send + Sync + 'static>;
pub type DownloadFinishedHandler =
    Arc<dyn Fn(String, Option<PathBuf>, bool) + Send + Sync + 'static>;

#[derive(Debug)]
pub enum BrowserEvent {
    Created {
        browser_id: i32,
    },
    BeforeClose {
        browser_id: i32,
    },
    TitleChanged {
        browser_id: i32,
        title: String,
    },
    LoadingStateChanged {
        browser_id: i32,
        is_loading: bool,
        can_go_back: bool,
        can_go_forward: bool,
    },
    LoadStarted {
        browser_id: i32,
        url: String,
    },
    LoadFinished {
        browser_id: i32,
        url: String,
        http_status_code: i32,
    },
    ProcessMessage {
        browser_id: i32,
        source_process: ProcessId,
        name: String,
        arguments: Vec<String>,
    },
}

#[derive(Clone, Default)]
pub struct RuntimeClientCallbacks {
    pub on_event: Option<BrowserEventHandler>,
    pub on_before_browse: Option<BeforeBrowseHandler>,
    pub on_open_url_from_tab: Option<OpenUrlFromTabHandler>,
    pub on_download_requested: Option<DownloadRequestedHandler>,
    pub on_download_finished: Option<DownloadFinishedHandler>,
}

#[derive(Clone)]
struct RuntimeClientState {
    browser_slot: BrowserSlot,
    callbacks: RuntimeClientCallbacks,
}

impl RuntimeClientState {
    fn emit(&self, event: BrowserEvent) {
        if let Some(on_event) = &self.callbacks.on_event {
            on_event(event);
        }
    }
}

pub struct RuntimeClientBuilder {
    browser_slot: BrowserSlot,
    callbacks: RuntimeClientCallbacks,
}

impl Default for RuntimeClientBuilder {
    fn default() -> Self {
        Self {
            browser_slot: BrowserSlot::new(),
            callbacks: RuntimeClientCallbacks::default(),
        }
    }
}

impl RuntimeClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_browser_slot(mut self, browser_slot: BrowserSlot) -> Self {
        self.browser_slot = browser_slot;
        self
    }

    pub fn on_event<F>(mut self, on_event: F) -> Self
    where
        F: Fn(BrowserEvent) + Send + Sync + 'static,
    {
        self.callbacks.on_event = Some(Arc::new(on_event));
        self
    }

    pub fn on_before_browse<F>(mut self, on_before_browse: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        self.callbacks.on_before_browse = Some(Arc::new(on_before_browse));
        self
    }

    pub fn on_open_url_from_tab<F>(mut self, on_open_url_from_tab: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        self.callbacks.on_open_url_from_tab = Some(Arc::new(on_open_url_from_tab));
        self
    }

    pub fn on_download_requested<F>(mut self, on_download_requested: F) -> Self
    where
        F: Fn(String, String) -> Option<PathBuf> + Send + Sync + 'static,
    {
        self.callbacks.on_download_requested = Some(Arc::new(on_download_requested));
        self
    }

    pub fn on_download_finished<F>(mut self, on_download_finished: F) -> Self
    where
        F: Fn(String, Option<PathBuf>, bool) + Send + Sync + 'static,
    {
        self.callbacks.on_download_finished = Some(Arc::new(on_download_finished));
        self
    }

    pub fn build(self) -> Client {
        RuntimeClient::new(Arc::new(RuntimeClientState {
            browser_slot: self.browser_slot,
            callbacks: self.callbacks,
        }))
    }
}

fn to_string(value: cef::CefStringUserfree) -> String {
    CefString::from(&value).to_string()
}

fn browser_id(browser: Option<&mut cef::Browser>) -> i32 {
    browser.map_or(0, |browser| browser.identifier())
}

fn frame_url(frame: Option<&mut Frame>) -> String {
    let Some(frame) = frame else {
        return String::new();
    };
    to_string(frame.url())
}

fn process_message_arguments(message: Option<&mut ProcessMessage>) -> Vec<String> {
    let Some(message) = message else {
        return Vec::new();
    };
    let Some(arguments) = message.argument_list() else {
        return Vec::new();
    };
    (0..arguments.size())
        .map(|index| to_string(arguments.string(index)))
        .collect()
}

wrap_client! {
    pub struct RuntimeClient {
        state: Arc<RuntimeClientState>,
    }

    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(RuntimeDisplayHandler::new(self.state.clone()))
        }

        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(RuntimeDownloadHandler::new(self.state.clone()))
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(RuntimeLifeSpanHandler::new(self.state.clone()))
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(RuntimeLoadHandler::new(self.state.clone()))
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(RuntimeRequestHandler::new(self.state.clone()))
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut cef::Browser>,
            message_frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            let browser_id = browser_id(browser);
            let name = message
                .as_ref()
                .map_or_else(String::new, |message| to_string(message.name()));
            let frame_url = frame_url(message_frame);
            let mut arguments = process_message_arguments(message);
            if !frame_url.is_empty() {
                arguments.insert(0, frame_url);
            }

            self.state.emit(BrowserEvent::ProcessMessage {
                browser_id,
                source_process,
                name,
                arguments,
            });

            0
        }
    }
}

wrap_request_handler! {
    struct RuntimeRequestHandler {
        state: Arc<RuntimeClientState>,
    }

    impl RequestHandler {
        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            let Some(handler) = &self.state.callbacks.on_before_browse else {
                return 0;
            };

            let url = request.map_or_else(String::new, |request| to_string(request.url()));
            i32::from(!handler(&url))
        }

        fn on_open_urlfrom_tab(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            target_url: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: i32,
        ) -> i32 {
            let Some(handler) = &self.state.callbacks.on_open_url_from_tab else {
                return 0;
            };

            let url = target_url.map(CefString::to_string).unwrap_or_default();
            i32::from(!handler(&url))
        }
    }
}

wrap_download_handler! {
    struct RuntimeDownloadHandler {
        state: Arc<RuntimeClientState>,
    }

    impl DownloadHandler {
        fn can_download(
            &self,
            _browser: Option<&mut Browser>,
            _url: Option<&CefString>,
            _request_method: Option<&CefString>,
        ) -> i32 {
            1
        }

        fn on_before_download(
            &self,
            _browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            suggested_name: Option<&CefString>,
            callback: Option<&mut BeforeDownloadCallback>,
        ) -> i32 {
            let Some(callback) = callback else {
                return 0;
            };

            let url = download_item
                .as_ref()
                .map_or_else(String::new, |item| to_string(item.url()));
            let suggested_name = suggested_name
                .map(CefString::to_string)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "download.bin".to_string());

            let destination = if let Some(handler) = &self.state.callbacks.on_download_requested {
                handler(url, suggested_name)
            } else {
                Some(std::env::temp_dir().join(suggested_name))
            };

            let Some(destination) = destination else {
                return 0;
            };

            let destination = CefString::from(destination.to_string_lossy().as_ref());
            callback.cont(Some(&destination), 0);
            1
        }

        fn on_download_updated(
            &self,
            _browser: Option<&mut Browser>,
            download_item: Option<&mut DownloadItem>,
            _callback: Option<&mut DownloadItemCallback>,
        ) {
            let Some(handler) = &self.state.callbacks.on_download_finished else {
                return;
            };

            let Some(download_item) = download_item else {
                return;
            };

            if download_item.is_in_progress() != 0 {
                return;
            }

            let url = to_string(download_item.url());
            let full_path = to_string(download_item.full_path());
            let path = (!full_path.is_empty()).then(|| PathBuf::from(full_path));
            let success = download_item.is_complete() != 0
                && download_item.is_canceled() == 0
                && download_item.is_interrupted() == 0;

            handler(url, path, success);
        }
    }
}

wrap_display_handler! {
    struct RuntimeDisplayHandler {
        state: Arc<RuntimeClientState>,
    }

    impl DisplayHandler {
        fn on_title_change(&self, browser: Option<&mut cef::Browser>, title: Option<&CefString>) {
            self.state.emit(BrowserEvent::TitleChanged {
                browser_id: browser_id(browser),
                title: title.map(CefString::to_string).unwrap_or_default(),
            });
        }
    }
}

wrap_life_span_handler! {
    struct RuntimeLifeSpanHandler {
        state: Arc<RuntimeClientState>,
    }

    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut cef::Browser>) {
            let Some(browser) = browser.cloned() else {
                return;
            };

            let browser_id = browser.identifier();
            self.state.browser_slot.set(Some(browser));
            self.state.emit(BrowserEvent::Created { browser_id });
        }

        fn on_before_close(&self, browser: Option<&mut cef::Browser>) {
            let browser_id = browser_id(browser);

            if let Some(current_browser) = self.state.browser_slot.current() {
                if current_browser.identifier() == browser_id {
                    self.state.browser_slot.clear();
                }
            }

            self.state.emit(BrowserEvent::BeforeClose { browser_id });
        }
    }
}

wrap_load_handler! {
    struct RuntimeLoadHandler {
        state: Arc<RuntimeClientState>,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            browser: Option<&mut cef::Browser>,
            is_loading: i32,
            can_go_back: i32,
            can_go_forward: i32,
        ) {
            self.state.emit(BrowserEvent::LoadingStateChanged {
                browser_id: browser_id(browser),
                is_loading: is_loading != 0,
                can_go_back: can_go_back != 0,
                can_go_forward: can_go_forward != 0,
            });
        }

        fn on_load_start(
            &self,
            browser: Option<&mut cef::Browser>,
            frame: Option<&mut Frame>,
            _transition_type: cef::TransitionType,
        ) {
            if frame.as_ref().is_some_and(|frame| frame.is_main() == 0) {
                return;
            }

            self.state.emit(BrowserEvent::LoadStarted {
                browser_id: browser_id(browser),
                url: frame_url(frame),
            });
        }

        fn on_load_end(
            &self,
            browser: Option<&mut cef::Browser>,
            frame: Option<&mut Frame>,
            http_status_code: i32,
        ) {
            if frame.as_ref().is_some_and(|frame| frame.is_main() == 0) {
                return;
            }

            self.state.emit(BrowserEvent::LoadFinished {
                browser_id: browser_id(browser),
                url: frame_url(frame),
                http_status_code,
            });
        }
    }
}
