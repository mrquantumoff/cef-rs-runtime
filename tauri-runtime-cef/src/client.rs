use crate::browser_slot::BrowserSlot;
use cef::string::CefStringList;
use cef::wrapper::message_router::{
    BrowserSideCallback, BrowserSideHandler, BrowserSideRouter, MessageRouterBrowserSide,
    MessageRouterBrowserSideHandlerCallbacks, MessageRouterConfig,
};
use cef::*;
use http::header::CONTENT_TYPE;
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub type BrowserEventHandler = Arc<dyn Fn(BrowserEvent) + Send + Sync + 'static>;
pub type BeforeBrowseHandler = Arc<dyn Fn(&str) -> bool + Send + Sync + 'static>;
pub type OpenUrlFromTabHandler = Arc<dyn Fn(&str) -> bool + Send + Sync + 'static>;
pub type PopupRequestedHandler =
    Arc<dyn Fn(&str, PopupRequestFeatures) -> bool + Send + Sync + 'static>;
pub type ResourceRequestHandlerCallback = Arc<
    dyn Fn(ResourceRequestPayload) -> Option<http::Response<Cow<'static, [u8]>>>
        + Send
        + Sync
        + 'static,
>;
pub type DownloadRequestedHandler =
    Arc<dyn Fn(String, String) -> Option<PathBuf> + Send + Sync + 'static>;
pub type DownloadFinishedHandler =
    Arc<dyn Fn(String, Option<PathBuf>, bool) + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub struct ResourceRequestPayload {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub is_navigation: bool,
    pub is_download: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PopupRequestFeatures {
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
}

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
    JsDialog {
        browser_id: i32,
        origin_url: String,
        dialog_type: u32,
        message: String,
        default_prompt: String,
    },
    BeforeUnloadDialog {
        browser_id: i32,
        message: String,
        is_reload: bool,
    },
    FileDialog {
        browser_id: i32,
        mode: u32,
        title: String,
        default_file_path: String,
        accept_filters: Vec<String>,
        accept_extensions: Vec<String>,
        accept_descriptions: Vec<String>,
    },
    DragEnter {
        browser_id: i32,
        files: Vec<String>,
        mask: i32,
    },
}

#[derive(Clone, Default)]
pub struct RuntimeClientCallbacks {
    pub on_event: Option<BrowserEventHandler>,
    pub on_before_browse: Option<BeforeBrowseHandler>,
    pub on_open_url_from_tab: Option<OpenUrlFromTabHandler>,
    pub on_popup_requested: Option<PopupRequestedHandler>,
    pub on_resource_request: Option<ResourceRequestHandlerCallback>,
    pub on_download_requested: Option<DownloadRequestedHandler>,
    pub on_download_finished: Option<DownloadFinishedHandler>,
    pub drag_drop_handler_enabled: bool,
}

#[derive(Clone)]
struct RuntimeClientState {
    browser_slot: BrowserSlot,
    callbacks: RuntimeClientCallbacks,
    ipc_router: Arc<BrowserSideRouter>,
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

    pub fn on_popup_requested<F>(mut self, on_popup_requested: F) -> Self
    where
        F: Fn(&str, PopupRequestFeatures) -> bool + Send + Sync + 'static,
    {
        self.callbacks.on_popup_requested = Some(Arc::new(on_popup_requested));
        self
    }

    pub fn on_resource_request<F>(mut self, on_resource_request: F) -> Self
    where
        F: Fn(ResourceRequestPayload) -> Option<http::Response<Cow<'static, [u8]>>>
            + Send
            + Sync
            + 'static,
    {
        self.callbacks.on_resource_request = Some(Arc::new(on_resource_request));
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

    pub fn with_drag_drop_handler_enabled(mut self, enabled: bool) -> Self {
        self.callbacks.drag_drop_handler_enabled = enabled;
        self
    }

    pub fn build(self) -> Client {
        let ipc_router = BrowserSideRouter::new(MessageRouterConfig::default());
        let state = Arc::new(RuntimeClientState {
            browser_slot: self.browser_slot,
            callbacks: self.callbacks,
            ipc_router: ipc_router.clone(),
        });

        let _ = ipc_router.add_handler(
            Arc::new(RuntimeIpcRouterHandler {
                state: state.clone(),
            }),
            true,
        );

        RuntimeClient::new(state)
    }
}

#[derive(Clone)]
struct RuntimeIpcRouterHandler {
    state: Arc<RuntimeClientState>,
}

impl BrowserSideHandler for RuntimeIpcRouterHandler {
    fn on_query_str(
        &self,
        browser: Option<Browser>,
        frame: Option<Frame>,
        _query_id: i64,
        request: &str,
        _persistent: bool,
        callback: Arc<Mutex<dyn BrowserSideCallback>>,
    ) -> bool {
        let browser_id = browser
            .as_ref()
            .map(Browser::identifier)
            .unwrap_or_default();

        if let Some(current_browser) = self.state.browser_slot.current() {
            if browser_id != 0 && current_browser.identifier() != browser_id {
                return false;
            }
        }

        let frame_url = frame
            .as_ref()
            .map(|frame| to_string(frame.url()))
            .filter(|url| !url.is_empty());

        let mut arguments = Vec::with_capacity(2);
        if let Some(frame_url) = frame_url {
            arguments.push(frame_url);
        }
        arguments.push(request.to_string());

        self.state.emit(BrowserEvent::ProcessMessage {
            browser_id,
            source_process: ProcessId::RENDERER,
            name: "__TAURI_IPC__".to_string(),
            arguments,
        });

        if let Ok(callback) = callback.lock() {
            callback.success_str("");
        }

        true
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

fn cef_string(value: Option<&CefString>) -> String {
    value.map(CefString::to_string).unwrap_or_default()
}

fn cef_string_list(value: Option<&mut CefStringList>) -> Vec<String> {
    value
        .map(|list| list.clone().into_iter().collect())
        .unwrap_or_default()
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

fn request_headers(request: &Request) -> Vec<(String, String)> {
    let mut header_map = CefStringMultimap::new();
    request.header_map(Some(&mut header_map));

    let mut headers = Vec::new();
    for (name, values) in header_map {
        for value in values {
            headers.push((name.clone(), value));
        }
    }
    headers
}

fn request_body(request: &Request) -> Vec<u8> {
    let Some(post_data) = request.post_data() else {
        return Vec::new();
    };

    let element_count = post_data.element_count();
    if element_count == 0 {
        return Vec::new();
    }

    let mut elements = vec![None; element_count];
    post_data.elements(Some(&mut elements));

    let mut body = Vec::new();
    for element in elements.into_iter().flatten() {
        if element.get_type() != PostdataelementType::BYTES {
            continue;
        }

        let size = element.bytes_count();
        if size == 0 {
            continue;
        }

        let mut chunk = vec![0; size];
        let read = element.bytes(size, chunk.as_mut_ptr());
        chunk.truncate(read.min(size));
        body.extend_from_slice(&chunk);
    }

    body
}

fn to_resource_request_payload(
    request: Option<&mut Request>,
    is_navigation: bool,
    is_download: bool,
) -> Option<ResourceRequestPayload> {
    let request = request?;

    Some(ResourceRequestPayload {
        url: to_string(request.url()),
        method: to_string(request.method()),
        headers: request_headers(request),
        body: request_body(request),
        is_navigation,
        is_download,
    })
}

fn popup_request_features(features: Option<&PopupFeatures>) -> PopupRequestFeatures {
    let Some(features) = features else {
        return PopupRequestFeatures::default();
    };

    let position = (features.x_set != 0 && features.y_set != 0).then_some((features.x, features.y));
    let size = (features.width_set != 0
        && features.height_set != 0
        && features.width > 0
        && features.height > 0)
        .then_some((features.width, features.height));

    PopupRequestFeatures { position, size }
}

wrap_resource_handler! {
    struct MemoryResourceHandler {
        status: i32,
        status_text: String,
        mime_type: String,
        headers: CefStringMultimap,
        body: Arc<Vec<u8>>,
        offset: Arc<Mutex<usize>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut i32>,
            _callback: Option<&mut Callback>,
        ) -> i32 {
            if let Some(handle_request) = handle_request {
                *handle_request = 1;
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let Some(response) = response else {
                return;
            };

            response.set_status(self.status);
            response.set_status_text(Some(&CefString::from(self.status_text.as_str())));
            response.set_mime_type(Some(&CefString::from(self.mime_type.as_str())));

            let mut headers = self.headers.clone();
            response.set_header_map(Some(&mut headers));

            if let Some(response_length) = response_length {
                *response_length = self.body.len() as i64;
            }
        }

        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: i32,
            bytes_read: Option<&mut i32>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> i32 {
            if bytes_to_read <= 0 {
                return 0;
            }

            let Some(bytes_read) = bytes_read else {
                return 0;
            };

            let Ok(mut offset) = self.offset.lock() else {
                *bytes_read = 0;
                return 0;
            };

            let remaining = self.body.len().saturating_sub(*offset);
            let to_copy = remaining.min(bytes_to_read as usize);
            if to_copy == 0 {
                *bytes_read = 0;
                return 0;
            }

            unsafe {
                std::ptr::copy_nonoverlapping(self.body.as_ptr().add(*offset), data_out, to_copy);
            }
            *offset += to_copy;
            *bytes_read = to_copy as i32;
            1
        }
    }
}

fn to_resource_handler(response: http::Response<Cow<'static, [u8]>>) -> Option<ResourceHandler> {
    let (parts, body) = response.into_parts();

    let mut header_map = CefStringMultimap::new();
    for (name, value) in &parts.headers {
        if let Ok(value) = value.to_str() {
            // Capitalize title: access-control-allow-origin -> Access-Control-Allow-Origin
            let name_str = name.as_str();
            let mut capitalized = String::with_capacity(name_str.len());
            let mut capitalize_next = true;
            for c in name_str.chars() {
                if c == '-' {
                    capitalized.push(c);
                    capitalize_next = true;
                } else if capitalize_next {
                    capitalized.extend(c.to_uppercase());
                    capitalize_next = false;
                } else {
                    capitalized.extend(c.to_lowercase());
                }
            }
            let _ = header_map.append(&capitalized, value);
        }
    }

    let mime_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .filter(|value| !value.is_empty())
        .unwrap_or("application/octet-stream")
        .to_string();

    let status_text = parts.status.canonical_reason().unwrap_or("OK").to_string();
    let status = i32::from(parts.status.as_u16());
    let body = Arc::new(body.into_owned());

    Some(MemoryResourceHandler::new(
        status,
        status_text,
        mime_type,
        header_map,
        body,
        Arc::new(Mutex::new(0)),
    ))
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

        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(RuntimeJsDialogHandler::new(self.state.clone()))
        }

        fn dialog_handler(&self) -> Option<DialogHandler> {
            Some(RuntimeDialogHandler::new(self.state.clone()))
        }

        fn drag_handler(&self) -> Option<DragHandler> {
            Some(RuntimeDragHandler::new(self.state.clone()))
        }

        fn focus_handler(&self) -> Option<FocusHandler> {
            Some(RuntimeFocusHandler::new(self.state.clone()))
        }

        fn keyboard_handler(&self) -> Option<KeyboardHandler> {
            Some(RuntimeKeyboardHandler::new(self.state.clone()))
        }

        fn on_process_message_received(
            &self,
            browser: Option<&mut cef::Browser>,
            message_frame: Option<&mut Frame>,
            source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> i32 {
            let browser_clone = browser.as_ref().map(|browser| (*(*browser)).clone());
            let frame_clone = message_frame.as_ref().map(|frame| (*(*frame)).clone());
            let message_clone = message.as_ref().map(|message| (*(*message)).clone());

            if self.state.ipc_router.on_process_message_received(
                browser_clone,
                frame_clone,
                source_process,
                message_clone,
            ) {
                return 1;
            }

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
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            let browser_clone = browser.as_ref().map(|browser| (*(*browser)).clone());
            let frame_clone = frame.as_ref().map(|frame| (*(*frame)).clone());
            self.state
                .ipc_router
                .on_before_browse(browser_clone, frame_clone);

            let Some(handler) = &self.state.callbacks.on_before_browse else {
                return 0;
            };

            let url = request.map_or_else(String::new, |request| to_string(request.url()));
            i32::from(!handler(&url))
        }

        fn on_render_process_terminated(
            &self,
            browser: Option<&mut Browser>,
            status: TerminationStatus,
            error_code: i32,
            error_string: Option<&CefString>,
        ) {
            self.state
                .ipc_router
                .on_render_process_terminated(browser.cloned());

            log::error!(
                "cef renderer terminated status={:?} code={} message='{}'",
                status,
                error_code,
                cef_string(error_string)
            );
        }

        fn on_open_urlfrom_tab(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            target_url: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: i32,
        ) -> i32 {
            let url = target_url.map(CefString::to_string).unwrap_or_default();

            if let Some(handler) = &self.state.callbacks.on_popup_requested {
                return i32::from(!handler(&url, PopupRequestFeatures::default()));
            }

            let Some(handler) = &self.state.callbacks.on_open_url_from_tab else {
                return 0;
            };

            i32::from(!handler(&url))
        }

        fn resource_request_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            is_navigation: i32,
            is_download: i32,
            _request_initiator: Option<&CefString>,
            _disable_default_handling: Option<&mut i32>,
        ) -> Option<cef::ResourceRequestHandler> {
            self.state.callbacks.on_resource_request.as_ref()?;
            Some(RuntimeResourceRequestHandler::new(
                self.state.clone(),
                is_navigation != 0,
                is_download != 0,
            ))
        }
    }
}

wrap_resource_request_handler! {
    struct RuntimeResourceRequestHandler {
        state: Arc<RuntimeClientState>,
        is_navigation: bool,
        is_download: bool,
    }

    impl ResourceRequestHandler {
        fn on_before_resource_load(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _request: Option<&mut Request>,
            _callback: Option<&mut Callback>,
        ) -> ReturnValue {
            ReturnValue::CONTINUE
        }

        fn resource_handler(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let handler = self.state.callbacks.on_resource_request.as_ref()?;
            let request = to_resource_request_payload(request, self.is_navigation, self.is_download)?;
            let response = handler(request)?;
            to_resource_handler(response)
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

        fn on_console_message(
            &self,
            _browser: Option<&mut cef::Browser>,
            level: cef::LogSeverity,
            message: Option<&CefString>,
            source: Option<&CefString>,
            line: i32,
        ) -> i32 {
            let msg = message.map(CefString::to_string).unwrap_or_default();
            let src = source.map(CefString::to_string).unwrap_or_default();
            match level {
                cef::LogSeverity::ERROR | cef::LogSeverity::FATAL => {
                    log::error!("[CEF CONSOLE] {}:{} - {}", src, line, msg);
                }
                cef::LogSeverity::WARNING => {
                    log::warn!("[CEF CONSOLE] {}:{} - {}", src, line, msg);
                }
                _ => {
                    log::info!("[CEF CONSOLE] {}:{} - {}", src, line, msg);
                }
            }
            0
        }
    }
}

wrap_life_span_handler! {
    struct RuntimeLifeSpanHandler {
        state: Arc<RuntimeClientState>,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: i32,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: i32,
            popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            let url = target_url.map(CefString::to_string).unwrap_or_default();

            if let Some(handler) = &self.state.callbacks.on_popup_requested {
                return i32::from(!handler(&url, popup_request_features(popup_features)));
            }

            let Some(handler) = &self.state.callbacks.on_open_url_from_tab else {
                return 0;
            };

            i32::from(!handler(&url))
        }

        fn on_after_created(&self, browser: Option<&mut cef::Browser>) {
            let Some(browser) = browser.cloned() else {
                return;
            };

            let browser_id = browser.identifier();
            self.state.browser_slot.set(Some(browser));
            self.state.emit(BrowserEvent::Created { browser_id });
        }

        fn on_before_close(&self, browser: Option<&mut cef::Browser>) {
            let browser_clone = browser.as_ref().map(|browser| (*(*browser)).clone());
            self.state.ipc_router.on_before_close(browser_clone);

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

        fn on_load_error(
            &self,
            _browser: Option<&mut cef::Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            if frame.as_ref().is_some_and(|frame| frame.is_main() == 0) {
                return;
            }

            log::error!(
                "cef load error code={:?} text='{}' url='{}'",
                error_code,
                cef_string(error_text),
                cef_string(failed_url)
            );
        }
    }
}

wrap_jsdialog_handler! {
    struct RuntimeJsDialogHandler {
        state: Arc<RuntimeClientState>,
    }

    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            browser: Option<&mut Browser>,
            origin_url: Option<&CefString>,
            dialog_type: JsdialogType,
            message_text: Option<&CefString>,
            default_prompt_text: Option<&CefString>,
            _callback: Option<&mut JsdialogCallback>,
            _suppress_message: Option<&mut i32>,
        ) -> i32 {
            self.state.emit(BrowserEvent::JsDialog {
                browser_id: browser_id(browser),
                origin_url: cef_string(origin_url),
                dialog_type: dialog_type.get_raw(),
                message: cef_string(message_text),
                default_prompt: cef_string(default_prompt_text),
            });

            0
        }

        fn on_before_unload_dialog(
            &self,
            browser: Option<&mut Browser>,
            message_text: Option<&CefString>,
            is_reload: i32,
            _callback: Option<&mut JsdialogCallback>,
        ) -> i32 {
            self.state.emit(BrowserEvent::BeforeUnloadDialog {
                browser_id: browser_id(browser),
                message: cef_string(message_text),
                is_reload: is_reload != 0,
            });

            0
        }
    }
}

wrap_dialog_handler! {
    struct RuntimeDialogHandler {
        state: Arc<RuntimeClientState>,
    }

    impl DialogHandler {
        fn on_file_dialog(
            &self,
            browser: Option<&mut Browser>,
            mode: FileDialogMode,
            title: Option<&CefString>,
            default_file_path: Option<&CefString>,
            accept_filters: Option<&mut CefStringList>,
            accept_extensions: Option<&mut CefStringList>,
            accept_descriptions: Option<&mut CefStringList>,
            _callback: Option<&mut FileDialogCallback>,
        ) -> i32 {
            self.state.emit(BrowserEvent::FileDialog {
                browser_id: browser_id(browser),
                mode: mode.get_raw(),
                title: cef_string(title),
                default_file_path: cef_string(default_file_path),
                accept_filters: cef_string_list(accept_filters),
                accept_extensions: cef_string_list(accept_extensions),
                accept_descriptions: cef_string_list(accept_descriptions),
            });

            0
        }
    }
}

wrap_drag_handler! {
    struct RuntimeDragHandler {
        state: Arc<RuntimeClientState>,
    }

    impl DragHandler {
        fn on_drag_enter(
            &self,
            browser: Option<&mut Browser>,
            drag_data: Option<&mut DragData>,
            mask: DragOperationsMask,
        ) -> i32 {
            if !self.state.callbacks.drag_drop_handler_enabled {
                return 0;
            }

            let browser_id = browser_id(browser);
            let mut files = Vec::new();
            if let Some(drag_data) = drag_data {
                let mut paths = CefStringList::new();
                if drag_data.file_paths(Some(&mut paths)) != 0 {
                    for i in paths {
                        files.push(i);
                    }
                }
            }

            self.state.emit(BrowserEvent::DragEnter {
                browser_id,
                files,
                mask: mask.as_ref().0 as i32,
            });

            0
        }
    }
}

wrap_focus_handler! {
    struct RuntimeFocusHandler {
        state: Arc<RuntimeClientState>,
    }

    impl FocusHandler {
        fn on_take_focus(
            &self,
            _browser: Option<&mut Browser>,
            _next: i32,
        ) {}

        fn on_set_focus(
            &self,
            _browser: Option<&mut Browser>,
            _source: FocusSource,
        ) -> i32 {
            0
        }

        fn on_got_focus(
            &self,
            _browser: Option<&mut Browser>,
        ) {
        }
    }
}

wrap_keyboard_handler! {
    struct RuntimeKeyboardHandler {
        state: Arc<RuntimeClientState>,
    }

    impl KeyboardHandler {
        fn on_pre_key_event(
            &self,
            _browser: Option<&mut Browser>,
            _event: Option<&KeyEvent>,
            _os_event: Option<&mut cef::sys::XEvent>,
            _is_keyboard_shortcut: Option<&mut i32>,
        ) -> i32 {
            0
        }

        fn on_key_event(
            &self,
            _browser: Option<&mut Browser>,
            _event: Option<&KeyEvent>,
            _os_event: Option<&mut cef::sys::XEvent>,
        ) -> i32 {
            0
        }
    }
}
