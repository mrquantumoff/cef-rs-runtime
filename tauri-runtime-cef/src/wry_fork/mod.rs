// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! CEF-backed Tauri [`Runtime`].
//!
//! Forked from `tauri-runtime-wry` with WRY/WebKit/WebView2 replaced by CEF.

#![allow(unexpected_cfgs)]

use self::monitor::MonitorExt;
use http::Request;
use raw_window_handle::{DisplayHandle, HasDisplayHandle, HasWindowHandle};

#[cfg(feature = "new-window-opener-optional")]
use tauri_runtime::webview::{NewWindowFeatures, NewWindowResponse};
use tauri_runtime::{
    dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize, Position, Size},
    monitor::Monitor,
    webview::{
        DetachedWebview, DownloadEvent, InitializationScript, PageLoadEvent, PendingWebview,
    },
    window::{
        CursorIcon, DetachedWindow, DetachedWindowWebview, DragDropEvent, PendingWindow, RawWindow,
        WebviewEvent, WindowBuilder, WindowBuilderBase, WindowEvent, WindowId,
        WindowSizeConstraints,
    },
    Cookie, DeviceEventFilter, Error, EventLoopProxy, ExitRequestedEventAction, Icon,
    ProgressBarState, ProgressBarStatus, Result, RunEvent, Runtime, RuntimeHandle, RuntimeInitArgs,
    UserAttentionType, UserEvent, WebviewDispatch, WebviewEventId, WindowDispatch, WindowEventId,
};

#[cfg(target_os = "macos")]
use tao::platform::macos::{EventLoopWindowTargetExtMacOS, WindowBuilderExtMacOS};
#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
use tao::platform::unix::{WindowBuilderExtUnix, WindowExtUnix};
#[cfg(windows)]
use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};

use tao::{
    dpi::{
        LogicalPosition as TaoLogicalPosition, LogicalSize as TaoLogicalSize,
        PhysicalPosition as TaoPhysicalPosition, PhysicalSize as TaoPhysicalSize,
        Position as TaoPosition, Size as TaoSize,
    },
    event::{Event, StartCause, WindowEvent as TaoWindowEvent},
    event_loop::{
        ControlFlow, DeviceEventFilter as TaoDeviceEventFilter, EventLoop, EventLoopBuilder,
        EventLoopProxy as TaoEventLoopProxy, EventLoopWindowTarget,
    },
    monitor::MonitorHandle,
    window::{
        CursorIcon as TaoCursorIcon, Fullscreen, Icon as TaoWindowIcon,
        ProgressBarState as TaoProgressBarState, ProgressState as TaoProgressState,
        Theme as TaoTheme, UserAttentionType as TaoUserAttentionType,
    },
};
#[cfg(desktop)]
use tauri_utils::config::PreventOverflowConfig;
#[cfg(target_os = "macos")]
use tauri_utils::TitleBarStyle;
use tauri_utils::{
    config::{Color, WindowConfig},
    Theme,
};
use url::Url;

// CEF imports
use crate::bootstrap::renderer_init_scripts_available;
use crate::browser_slot::BrowserSlot;
#[cfg(feature = "new-window-opener-optional")]
use crate::client::PopupRequestFeatures;
use crate::client::{BrowserEvent, ResourceRequestPayload, RuntimeClientBuilder};
#[cfg(feature = "tao-runtime")]
use crate::tao_window::{cef_null_window_handle, cef_window_handle_is_null, HostWindowInfo};
use cef::rc::Rc;
use cef::{
    browser_host_create_browser_sync, cookie_manager_get_global_manager, dictionary_value_create,
    process_message_create, request_context_create_context, value_create, BrowserSettings,
    CefString, CookieVisitor, ImplBrowser, ImplBrowserHost, ImplCookieManager, ImplCookieVisitor,
    ImplDictionaryValue, ImplFrame, ImplListValue, ImplPreferenceManager, ImplProcessMessage,
    ImplRequestContext, ImplValue, ProcessId, Rect as CefRect, RequestContext,
    RequestContextSettings, WindowInfo, WrapCookieVisitor,
};

#[cfg(all(
    feature = "tao-runtime",
    any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
use x11_dl::xlib;

pub use tao;
pub use tao::window::{Window, WindowBuilder as TaoWindowBuilder, WindowId as TaoWindowId};

#[cfg(target_os = "macos")]
pub use tao::platform::macos::{
    ActivationPolicy as TaoActivationPolicy, EventLoopExtMacOS, WindowExtMacOS,
};
#[cfg(target_os = "macos")]
use tauri_runtime::ActivationPolicy;

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    ops::Deref,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{channel, Sender},
        Arc, Mutex, Weak,
    },
    thread::{current as current_thread, ThreadId},
    time::{Duration, Instant},
};

pub type WebviewId = u32;
#[cfg(not(debug_assertions))]
mod dialog;
mod monitor;
#[cfg(any(
    windows,
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
mod undecorated_resizing;
mod util;
mod webview;
mod window;

pub use webview::Webview;
use window::WindowExt as _;

#[cfg(feature = "wayland-osr")]
use crate::osr::wayland::{OsrRenderHandler, OsrSurface, OsrState};

/// CEF browser context — replaces WRY's `WebContext`.
///
/// CEF uses a global context initialized via `cef::initialize()`, so this is
/// much simpler than WRY's per-data-directory context model.
#[derive(Debug)]
pub struct WebContext {
    pub referenced_by_webviews: HashSet<String>,
    pub registered_custom_protocols: HashSet<String>,
}

pub type WebContextStore = Arc<Mutex<HashMap<Option<PathBuf>, WebContext>>>;
// window
pub type WindowEventHandler = Box<dyn Fn(&WindowEvent) + Send>;
pub type WindowEventListeners = Arc<Mutex<HashMap<WindowEventId, WindowEventHandler>>>;
pub type WebviewEventHandler = Box<dyn Fn(&WebviewEvent) + Send>;
pub type WebviewEventListeners = Arc<Mutex<HashMap<WebviewEventId, WebviewEventHandler>>>;

#[derive(Debug, Clone, Default)]
pub struct WindowIdStore(Arc<Mutex<HashMap<TaoWindowId, WindowId>>>);

impl WindowIdStore {
    pub fn insert(&self, w: TaoWindowId, id: WindowId) {
        self.0.lock().unwrap().insert(w, id);
    }

    pub fn get(&self, w: &TaoWindowId) -> Option<WindowId> {
        self.0.lock().unwrap().get(w).copied()
    }
}

macro_rules! getter {
    ($self: ident, $rx: expr, $message: expr) => {{
        send_user_message(&$self.context, $message)?;
        $rx.recv().map_err(|_| Error::FailedToReceiveMessage)
    }};
}

macro_rules! window_getter {
    ($self: ident, $message: expr) => {{
        let (tx, rx) = channel();
        getter!($self, rx, Message::Window($self.window_id, $message(tx)))
    }};
}

macro_rules! event_loop_window_getter {
    ($self: ident, $message: expr) => {{
        let (tx, rx) = channel();
        getter!($self, rx, Message::EventLoopWindowTarget($message(tx)))
    }};
}

macro_rules! webview_getter {
    ($self: ident, $message: expr) => {{
        let (tx, rx) = channel();
        getter!(
            $self,
            rx,
            Message::Webview(
                *$self.window_id.lock().unwrap(),
                $self.webview_id,
                $message(tx)
            )
        )
    }};
}

pub(crate) fn send_user_message<T: UserEvent>(
    context: &Context<T>,
    message: Message<T>,
) -> Result<()> {
    if current_thread().id() == context.main_thread_id {
        handle_user_message(
            &context.main_thread.window_target,
            message,
            UserMessageContext {
                window_id_map: context.window_id_map.clone(),
                windows: context.main_thread.windows.clone(),
            },
        );
        Ok(())
    } else {
        context
            .proxy
            .send_event(message)
            .map_err(|_| Error::FailedToSendMessage)
    }
}

#[derive(Clone)]
pub struct Context<T: UserEvent> {
    pub window_id_map: WindowIdStore,
    main_thread_id: ThreadId,
    pub proxy: TaoEventLoopProxy<Message<T>>,
    main_thread: DispatcherMainThreadContext<T>,
    plugins: Arc<Mutex<Vec<Box<dyn Plugin<T> + Send>>>>,
    next_window_id: Arc<AtomicU32>,
    next_webview_id: Arc<AtomicU32>,
    next_window_event_id: Arc<AtomicU32>,
    next_webview_event_id: Arc<AtomicU32>,
    webview_runtime_installed: bool,
}

impl<T: UserEvent> Context<T> {
    pub fn run_threaded<R, F>(&self, f: F) -> R
    where
        F: FnOnce(Option<&DispatcherMainThreadContext<T>>) -> R,
    {
        f(if current_thread().id() == self.main_thread_id {
            Some(&self.main_thread)
        } else {
            None
        })
    }

    fn next_window_id(&self) -> WindowId {
        self.next_window_id.fetch_add(1, Ordering::Relaxed).into()
    }

    fn next_webview_id(&self) -> WebviewId {
        self.next_webview_id.fetch_add(1, Ordering::Relaxed)
    }

    fn next_window_event_id(&self) -> u32 {
        self.next_window_event_id.fetch_add(1, Ordering::Relaxed)
    }

    fn next_webview_event_id(&self) -> u32 {
        self.next_webview_event_id.fetch_add(1, Ordering::Relaxed)
    }
}

impl<T: UserEvent> Context<T> {
    fn create_window<F: Fn(RawWindow) + Send + 'static>(
        &self,
        pending: PendingWindow<T, Wry<T>>,
        after_window_creation: Option<F>,
    ) -> Result<DetachedWindow<T, Wry<T>>> {
        let label = pending.label.clone();
        let context = self.clone();
        let window_id = self.next_window_id();
        let (webview_id, use_https_scheme) = pending
            .webview
            .as_ref()
            .map(|w| {
                (
                    Some(context.next_webview_id()),
                    w.webview_attributes.use_https_scheme,
                )
            })
            .unwrap_or((None, false));

        send_user_message(
            self,
            Message::CreateWindow(
                window_id,
                Box::new(move |event_loop| {
                    create_window(
                        window_id,
                        webview_id.unwrap_or_default(),
                        event_loop,
                        &context,
                        pending,
                        after_window_creation,
                    )
                }),
            ),
        )?;

        let dispatcher = WryWindowDispatcher {
            window_id,
            context: self.clone(),
        };

        let detached_webview = webview_id.map(|id| {
            let webview = DetachedWebview {
                label: label.clone(),
                dispatcher: WryWebviewDispatcher {
                    window_id: Arc::new(Mutex::new(window_id)),
                    webview_id: id,
                    context: self.clone(),
                },
            };
            DetachedWindowWebview {
                webview,
                use_https_scheme,
            }
        });

        Ok(DetachedWindow {
            id: window_id,
            label,
            dispatcher,
            webview: detached_webview,
        })
    }

    fn create_webview(
        &self,
        window_id: WindowId,
        pending: PendingWebview<T, Wry<T>>,
    ) -> Result<DetachedWebview<T, Wry<T>>> {
        let label = pending.label.clone();
        let context = self.clone();

        let webview_id = self.next_webview_id();

        let window_id_wrapper = Arc::new(Mutex::new(window_id));
        let window_id_wrapper_ = window_id_wrapper.clone();

        send_user_message(
            self,
            Message::CreateWebview(
                window_id,
                Box::new(move |window, options| {
                    create_webview(
                        WebviewKind::WindowChild,
                        window,
                        window_id_wrapper_,
                        webview_id,
                        &context,
                        pending,
                        options.focused_webview,
                    )
                }),
            ),
        )?;

        let dispatcher = WryWebviewDispatcher {
            window_id: window_id_wrapper,
            webview_id,
            context: self.clone(),
        };

        Ok(DetachedWebview { label, dispatcher })
    }
}

#[cfg(feature = "tracing")]
#[derive(Debug, Clone, Default)]
pub struct ActiveTraceSpanStore(Rc<RefCell<Vec<ActiveTracingSpan>>>);

#[cfg(feature = "tracing")]
impl ActiveTraceSpanStore {
    pub fn remove_window_draw(&self) {
        self.0
            .borrow_mut()
            .retain(|t| !matches!(t, ActiveTracingSpan::WindowDraw { id: _, span: _ }));
    }
}

#[cfg(feature = "tracing")]
#[derive(Debug)]
pub enum ActiveTracingSpan {
    WindowDraw {
        id: TaoWindowId,
        span: tracing::span::EnteredSpan,
    },
}

#[derive(Debug)]
pub struct WindowsStore(pub RefCell<BTreeMap<WindowId, WindowWrapper>>);

// SAFETY: we ensure this type is only used on the main thread.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for WindowsStore {}

// SAFETY: we ensure this type is only used on the main thread.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Sync for WindowsStore {}

#[derive(Debug, Clone)]
pub struct DispatcherMainThreadContext<T: UserEvent> {
    pub window_target: EventLoopWindowTarget<Message<T>>,
    pub web_context: WebContextStore,
    // changing this to an Rc will cause frequent app crashes.
    pub windows: Arc<WindowsStore>,
    #[cfg(feature = "tracing")]
    pub active_tracing_spans: ActiveTraceSpanStore,
}

// SAFETY: we ensure this type is only used on the main thread.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: UserEvent> Send for DispatcherMainThreadContext<T> {}

// SAFETY: we ensure this type is only used on the main thread.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: UserEvent> Sync for DispatcherMainThreadContext<T> {}

impl<T: UserEvent> fmt::Debug for Context<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("main_thread_id", &self.main_thread_id)
            .field("proxy", &self.proxy)
            .field("main_thread", &self.main_thread)
            .finish()
    }
}

pub struct DeviceEventFilterWrapper(pub TaoDeviceEventFilter);

impl From<DeviceEventFilter> for DeviceEventFilterWrapper {
    fn from(item: DeviceEventFilter) -> Self {
        match item {
            DeviceEventFilter::Always => Self(TaoDeviceEventFilter::Always),
            DeviceEventFilter::Never => Self(TaoDeviceEventFilter::Never),
            DeviceEventFilter::Unfocused => Self(TaoDeviceEventFilter::Unfocused),
        }
    }
}

/// Webview rectangle — replaces `wry::Rect`.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub position: tauri_runtime::dpi::Position,
    pub size: tauri_runtime::dpi::Size,
}

pub struct RectWrapper(pub Rect);
impl From<tauri_runtime::dpi::Rect> for RectWrapper {
    fn from(value: tauri_runtime::dpi::Rect) -> Self {
        RectWrapper(Rect {
            position: value.position,
            size: value.size,
        })
    }
}

/// Wrapper around a [`tao::window::Icon`] that can be created from an [`Icon`].
pub struct TaoIcon(pub TaoWindowIcon);

impl TryFrom<Icon<'_>> for TaoIcon {
    type Error = Error;
    fn try_from(icon: Icon<'_>) -> std::result::Result<Self, Self::Error> {
        TaoWindowIcon::from_rgba(icon.rgba.to_vec(), icon.width, icon.height)
            .map(Self)
            .map_err(|e| Error::InvalidIcon(Box::new(e)))
    }
}

pub struct WindowEventWrapper(pub Option<WindowEvent>);

impl WindowEventWrapper {
    fn map_from_tao(
        event: &TaoWindowEvent<'_>,
        #[allow(unused_variables)] window: &WindowWrapper,
    ) -> Self {
        let event = match event {
            TaoWindowEvent::Resized(size) => {
                WindowEvent::Resized(PhysicalSizeWrapper(*size).into())
            }
            TaoWindowEvent::Moved(position) => {
                WindowEvent::Moved(PhysicalPositionWrapper(*position).into())
            }
            TaoWindowEvent::Destroyed => WindowEvent::Destroyed,
            TaoWindowEvent::ScaleFactorChanged {
                scale_factor,
                new_inner_size,
            } => WindowEvent::ScaleFactorChanged {
                scale_factor: *scale_factor,
                new_inner_size: PhysicalSizeWrapper(**new_inner_size).into(),
            },
            TaoWindowEvent::Focused(focused) => {
                #[cfg(not(windows))]
                return Self(Some(WindowEvent::Focused(*focused)));
                // on multiwebview mode, if there's no focused webview, it means we're receiving a direct window focus change
                // (without receiving a webview focus, such as when clicking the taskbar app icon or using Alt + Tab)
                // in this case we must send the focus change event here
                #[cfg(windows)]
                if window.has_children.load(Ordering::Relaxed) {
                    const FOCUSED_WEBVIEW_MARKER: &str = "__tauriWindow?";
                    let mut focused_webview = window.focused_webview.lock().unwrap();
                    // when we focus a webview and the window was previously focused, we get a blur event here
                    // so on blur we should only send events if the current focus is owned by the window
                    if !*focused
                        && focused_webview
                            .as_deref()
                            .is_some_and(|w| w != FOCUSED_WEBVIEW_MARKER)
                    {
                        return Self(None);
                    }

                    // reset focused_webview on blur, or set to a dummy value on focus
                    // (to prevent double focus event when we click a webview after focusing a window)
                    *focused_webview = (*focused).then(|| FOCUSED_WEBVIEW_MARKER.to_string());

                    return Self(Some(WindowEvent::Focused(*focused)));
                } else {
                    // when not on multiwebview mode, we handle focus change events on the webview (add_GotFocus and add_LostFocus)
                    return Self(None);
                }
            }
            TaoWindowEvent::ThemeChanged(theme) => WindowEvent::ThemeChanged(map_theme(theme)),
            _ => return Self(None),
        };
        Self(Some(event))
    }

    fn parse(window: &WindowWrapper, event: &TaoWindowEvent<'_>) -> Self {
        match event {
            // resized event from tao doesn't include a reliable size on macOS
            // because wry replaces the NSView
            TaoWindowEvent::Resized(_) => {
                if let Some(w) = &window.inner {
                    let size = inner_size(
                        w,
                        &window.webviews,
                        window.has_children.load(Ordering::Relaxed),
                    );
                    Self(Some(WindowEvent::Resized(PhysicalSizeWrapper(size).into())))
                } else {
                    Self(None)
                }
            }
            e => Self::map_from_tao(e, window),
        }
    }
}

pub fn map_theme(theme: &TaoTheme) -> Theme {
    match theme {
        TaoTheme::Light => Theme::Light,
        TaoTheme::Dark => Theme::Dark,
        _ => Theme::Light,
    }
}

#[cfg(target_os = "macos")]
fn tao_activation_policy(activation_policy: ActivationPolicy) -> TaoActivationPolicy {
    match activation_policy {
        ActivationPolicy::Regular => TaoActivationPolicy::Regular,
        ActivationPolicy::Accessory => TaoActivationPolicy::Accessory,
        ActivationPolicy::Prohibited => TaoActivationPolicy::Prohibited,
        _ => TaoActivationPolicy::Regular,
    }
}

pub struct MonitorHandleWrapper(pub MonitorHandle);

impl From<MonitorHandleWrapper> for Monitor {
    fn from(monitor: MonitorHandleWrapper) -> Monitor {
        Self {
            name: monitor.0.name(),
            position: PhysicalPositionWrapper(monitor.0.position()).into(),
            size: PhysicalSizeWrapper(monitor.0.size()).into(),
            work_area: monitor.0.work_area(),
            scale_factor: monitor.0.scale_factor(),
        }
    }
}

pub struct PhysicalPositionWrapper<T>(pub TaoPhysicalPosition<T>);

impl<T> From<PhysicalPositionWrapper<T>> for PhysicalPosition<T> {
    fn from(position: PhysicalPositionWrapper<T>) -> Self {
        Self {
            x: position.0.x,
            y: position.0.y,
        }
    }
}

impl<T> From<PhysicalPosition<T>> for PhysicalPositionWrapper<T> {
    fn from(position: PhysicalPosition<T>) -> Self {
        Self(TaoPhysicalPosition {
            x: position.x,
            y: position.y,
        })
    }
}

struct LogicalPositionWrapper<T>(TaoLogicalPosition<T>);

impl<T> From<LogicalPosition<T>> for LogicalPositionWrapper<T> {
    fn from(position: LogicalPosition<T>) -> Self {
        Self(TaoLogicalPosition {
            x: position.x,
            y: position.y,
        })
    }
}

pub struct PhysicalSizeWrapper<T>(pub TaoPhysicalSize<T>);

impl<T> From<PhysicalSizeWrapper<T>> for PhysicalSize<T> {
    fn from(size: PhysicalSizeWrapper<T>) -> Self {
        Self {
            width: size.0.width,
            height: size.0.height,
        }
    }
}

impl<T> From<PhysicalSize<T>> for PhysicalSizeWrapper<T> {
    fn from(size: PhysicalSize<T>) -> Self {
        Self(TaoPhysicalSize {
            width: size.width,
            height: size.height,
        })
    }
}

struct LogicalSizeWrapper<T>(TaoLogicalSize<T>);

impl<T> From<LogicalSize<T>> for LogicalSizeWrapper<T> {
    fn from(size: LogicalSize<T>) -> Self {
        Self(TaoLogicalSize {
            width: size.width,
            height: size.height,
        })
    }
}

pub struct SizeWrapper(pub TaoSize);

impl From<Size> for SizeWrapper {
    fn from(size: Size) -> Self {
        match size {
            Size::Logical(s) => Self(TaoSize::Logical(LogicalSizeWrapper::from(s).0)),
            Size::Physical(s) => Self(TaoSize::Physical(PhysicalSizeWrapper::from(s).0)),
        }
    }
}

pub struct PositionWrapper(pub TaoPosition);

impl From<Position> for PositionWrapper {
    fn from(position: Position) -> Self {
        match position {
            Position::Logical(s) => Self(TaoPosition::Logical(LogicalPositionWrapper::from(s).0)),
            Position::Physical(s) => {
                Self(TaoPosition::Physical(PhysicalPositionWrapper::from(s).0))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct UserAttentionTypeWrapper(pub TaoUserAttentionType);

impl From<UserAttentionType> for UserAttentionTypeWrapper {
    fn from(request_type: UserAttentionType) -> Self {
        let o = match request_type {
            UserAttentionType::Critical => TaoUserAttentionType::Critical,
            UserAttentionType::Informational => TaoUserAttentionType::Informational,
        };
        Self(o)
    }
}

#[derive(Debug)]
pub struct CursorIconWrapper(pub TaoCursorIcon);

impl From<CursorIcon> for CursorIconWrapper {
    fn from(icon: CursorIcon) -> Self {
        use CursorIcon::*;
        let i = match icon {
            Default => TaoCursorIcon::Default,
            Crosshair => TaoCursorIcon::Crosshair,
            Hand => TaoCursorIcon::Hand,
            Arrow => TaoCursorIcon::Arrow,
            Move => TaoCursorIcon::Move,
            Text => TaoCursorIcon::Text,
            Wait => TaoCursorIcon::Wait,
            Help => TaoCursorIcon::Help,
            Progress => TaoCursorIcon::Progress,
            NotAllowed => TaoCursorIcon::NotAllowed,
            ContextMenu => TaoCursorIcon::ContextMenu,
            Cell => TaoCursorIcon::Cell,
            VerticalText => TaoCursorIcon::VerticalText,
            Alias => TaoCursorIcon::Alias,
            Copy => TaoCursorIcon::Copy,
            NoDrop => TaoCursorIcon::NoDrop,
            Grab => TaoCursorIcon::Grab,
            Grabbing => TaoCursorIcon::Grabbing,
            AllScroll => TaoCursorIcon::AllScroll,
            ZoomIn => TaoCursorIcon::ZoomIn,
            ZoomOut => TaoCursorIcon::ZoomOut,
            EResize => TaoCursorIcon::EResize,
            NResize => TaoCursorIcon::NResize,
            NeResize => TaoCursorIcon::NeResize,
            NwResize => TaoCursorIcon::NwResize,
            SResize => TaoCursorIcon::SResize,
            SeResize => TaoCursorIcon::SeResize,
            SwResize => TaoCursorIcon::SwResize,
            WResize => TaoCursorIcon::WResize,
            EwResize => TaoCursorIcon::EwResize,
            NsResize => TaoCursorIcon::NsResize,
            NeswResize => TaoCursorIcon::NeswResize,
            NwseResize => TaoCursorIcon::NwseResize,
            ColResize => TaoCursorIcon::ColResize,
            RowResize => TaoCursorIcon::RowResize,
            _ => TaoCursorIcon::Default,
        };
        Self(i)
    }
}

pub struct ProgressStateWrapper(pub TaoProgressState);

impl From<ProgressBarStatus> for ProgressStateWrapper {
    fn from(status: ProgressBarStatus) -> Self {
        let state = match status {
            ProgressBarStatus::None => TaoProgressState::None,
            ProgressBarStatus::Normal => TaoProgressState::Normal,
            ProgressBarStatus::Indeterminate => TaoProgressState::Indeterminate,
            ProgressBarStatus::Paused => TaoProgressState::Paused,
            ProgressBarStatus::Error => TaoProgressState::Error,
        };
        Self(state)
    }
}

pub struct ProgressBarStateWrapper(pub TaoProgressBarState);

impl From<ProgressBarState> for ProgressBarStateWrapper {
    fn from(progress_state: ProgressBarState) -> Self {
        Self(TaoProgressBarState {
            progress: progress_state.progress,
            state: progress_state
                .status
                .map(|state| ProgressStateWrapper::from(state).0),
            desktop_filename: progress_state.desktop_filename,
        })
    }
}

#[derive(Clone, Default)]
pub struct WindowBuilderWrapper {
    inner: TaoWindowBuilder,
    center: bool,
    prevent_overflow: Option<Size>,
    #[cfg(target_os = "macos")]
    tabbing_identifier: Option<String>,
}

impl std::fmt::Debug for WindowBuilderWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("WindowBuilderWrapper");
        s.field("inner", &self.inner)
            .field("center", &self.center)
            .field("prevent_overflow", &self.prevent_overflow);
        #[cfg(target_os = "macos")]
        {
            s.field("tabbing_identifier", &self.tabbing_identifier);
        }
        s.finish()
    }
}

// SAFETY: this type is `Send` since `menu_items` are read only here
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for WindowBuilderWrapper {}

impl WindowBuilderBase for WindowBuilderWrapper {}
impl WindowBuilder for WindowBuilderWrapper {
    fn new() -> Self {
        #[allow(unused_mut)]
        let mut builder = Self::default().focused(true);

        #[cfg(target_os = "macos")]
        {
            // TODO: find a proper way to prevent webview being pushed out of the window.
            // Workround for issue: https://github.com/tauri-apps/tauri/issues/10225
            // The window requires `NSFullSizeContentViewWindowMask` flag to prevent devtools
            // pushing the content view out of the window.
            // By setting the default style to `TitleBarStyle::Visible` should fix the issue for most of the users.
            builder = builder.title_bar_style(TitleBarStyle::Visible);
        }

        builder = builder.title("Tauri App");

        #[cfg(windows)]
        {
            builder = builder.window_classname("Tauri Window");
        }

        builder
    }

    fn with_config(config: &WindowConfig) -> Self {
        let mut window = WindowBuilderWrapper::new();

        #[cfg(target_os = "macos")]
        {
            window = window
                .hidden_title(config.hidden_title)
                .title_bar_style(config.title_bar_style);
            if let Some(identifier) = &config.tabbing_identifier {
                window = window.tabbing_identifier(identifier);
            }
            if let Some(position) = &config.traffic_light_position {
                window = window.traffic_light_position(tauri_runtime::dpi::LogicalPosition::new(
                    position.x, position.y,
                ));
            }
        }

        #[cfg(any(not(target_os = "macos"), feature = "macos-private-api"))]
        {
            window = window.transparent(config.transparent);
        }
        #[cfg(all(
            target_os = "macos",
            not(feature = "macos-private-api"),
            debug_assertions
        ))]
        if config.transparent {
            eprintln!(
        "The window is set to be transparent but the `macos-private-api` is not enabled.
        This can be enabled via the `tauri.macOSPrivateApi` configuration property <https://v2.tauri.app/reference/config/#macosprivateapi>
      ");
        }

        #[cfg(any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        ))]
        {
            // Mouse move events are disabled on Linux to prevent event-loop saturation.
            // When the wayland-osr feature is active we need cursor moves to track the
            // mouse position for CEF hit-testing, so we leave events enabled in that case.
            #[cfg(not(feature = "wayland-osr"))]
            {
                window.inner = window.inner.with_cursor_moved_event(false);
            }
        }

        #[cfg(desktop)]
        {
            window = window
                .title(config.title.to_string())
                .inner_size(config.width, config.height)
                .focused(config.focus)
                .focusable(config.focusable)
                .visible(config.visible)
                .resizable(config.resizable)
                .fullscreen(config.fullscreen)
                .decorations(config.decorations)
                .maximized(config.maximized)
                .always_on_bottom(config.always_on_bottom)
                .always_on_top(config.always_on_top)
                .visible_on_all_workspaces(config.visible_on_all_workspaces)
                .content_protected(config.content_protected)
                .skip_taskbar(config.skip_taskbar)
                .theme(config.theme)
                .closable(config.closable)
                .maximizable(config.maximizable)
                .minimizable(config.minimizable)
                .shadow(config.shadow);

            let mut constraints = WindowSizeConstraints::default();

            if let Some(min_width) = config.min_width {
                constraints.min_width = Some(tao::dpi::LogicalUnit::new(min_width).into());
            }
            if let Some(min_height) = config.min_height {
                constraints.min_height = Some(tao::dpi::LogicalUnit::new(min_height).into());
            }
            if let Some(max_width) = config.max_width {
                constraints.max_width = Some(tao::dpi::LogicalUnit::new(max_width).into());
            }
            if let Some(max_height) = config.max_height {
                constraints.max_height = Some(tao::dpi::LogicalUnit::new(max_height).into());
            }
            if let Some(color) = config.background_color {
                window = window.background_color(color);
            }
            window = window.inner_size_constraints(constraints);

            if let (Some(x), Some(y)) = (config.x, config.y) {
                window = window.position(x, y);
            }

            if config.center {
                window = window.center();
            }

            if let Some(window_classname) = &config.window_classname {
                window = window.window_classname(window_classname);
            }

            if let Some(prevent_overflow) = &config.prevent_overflow {
                window = match prevent_overflow {
                    PreventOverflowConfig::Enable(true) => window.prevent_overflow(),
                    PreventOverflowConfig::Margin(margin) => window.prevent_overflow_with_margin(
                        TaoPhysicalSize::new(margin.width, margin.height).into(),
                    ),
                    _ => window,
                };
            }
        }

        window
    }

    fn center(mut self) -> Self {
        self.center = true;
        self
    }

    fn position(mut self, x: f64, y: f64) -> Self {
        self.inner = self.inner.with_position(TaoLogicalPosition::new(x, y));
        self
    }

    fn inner_size(mut self, width: f64, height: f64) -> Self {
        self.inner = self
            .inner
            .with_inner_size(TaoLogicalSize::new(width, height));
        self
    }

    fn min_inner_size(mut self, min_width: f64, min_height: f64) -> Self {
        self.inner = self
            .inner
            .with_min_inner_size(TaoLogicalSize::new(min_width, min_height));
        self
    }

    fn max_inner_size(mut self, max_width: f64, max_height: f64) -> Self {
        self.inner = self
            .inner
            .with_max_inner_size(TaoLogicalSize::new(max_width, max_height));
        self
    }

    fn inner_size_constraints(mut self, constraints: WindowSizeConstraints) -> Self {
        self.inner.window.inner_size_constraints = tao::window::WindowSizeConstraints {
            min_width: constraints.min_width,
            min_height: constraints.min_height,
            max_width: constraints.max_width,
            max_height: constraints.max_height,
        };
        self
    }

    /// Prevent the window from overflowing the working area (e.g. monitor size - taskbar size) on creation
    ///
    /// ## Platform-specific
    ///
    /// - **iOS / Android:** Unsupported.
    fn prevent_overflow(mut self) -> Self {
        self.prevent_overflow
            .replace(PhysicalSize::new(0, 0).into());
        self
    }

    /// Prevent the window from overflowing the working area (e.g. monitor size - taskbar size)
    /// on creation with a margin
    ///
    /// ## Platform-specific
    ///
    /// - **iOS / Android:** Unsupported.
    fn prevent_overflow_with_margin(mut self, margin: Size) -> Self {
        self.prevent_overflow.replace(margin);
        self
    }

    fn resizable(mut self, resizable: bool) -> Self {
        self.inner = self.inner.with_resizable(resizable);
        self
    }

    fn maximizable(mut self, maximizable: bool) -> Self {
        self.inner = self.inner.with_maximizable(maximizable);
        self
    }

    fn minimizable(mut self, minimizable: bool) -> Self {
        self.inner = self.inner.with_minimizable(minimizable);
        self
    }

    fn closable(mut self, closable: bool) -> Self {
        self.inner = self.inner.with_closable(closable);
        self
    }

    fn title<S: Into<String>>(mut self, title: S) -> Self {
        self.inner = self.inner.with_title(title.into());
        self
    }

    fn fullscreen(mut self, fullscreen: bool) -> Self {
        self.inner = if fullscreen {
            self.inner
                .with_fullscreen(Some(Fullscreen::Borderless(None)))
        } else {
            self.inner.with_fullscreen(None)
        };
        self
    }

    fn focused(mut self, focused: bool) -> Self {
        self.inner = self.inner.with_focused(focused);
        self
    }

    fn focusable(mut self, focusable: bool) -> Self {
        self.inner = self.inner.with_focusable(focusable);
        self
    }

    fn maximized(mut self, maximized: bool) -> Self {
        self.inner = self.inner.with_maximized(maximized);
        self
    }

    fn visible(mut self, visible: bool) -> Self {
        self.inner = self.inner.with_visible(visible);
        self
    }

    #[cfg(any(not(target_os = "macos"), feature = "macos-private-api"))]
    fn transparent(mut self, transparent: bool) -> Self {
        self.inner = self.inner.with_transparent(transparent);
        self
    }

    fn decorations(mut self, decorations: bool) -> Self {
        self.inner = self.inner.with_decorations(decorations);
        self
    }

    fn always_on_bottom(mut self, always_on_bottom: bool) -> Self {
        self.inner = self.inner.with_always_on_bottom(always_on_bottom);
        self
    }

    fn always_on_top(mut self, always_on_top: bool) -> Self {
        self.inner = self.inner.with_always_on_top(always_on_top);
        self
    }

    fn visible_on_all_workspaces(mut self, visible_on_all_workspaces: bool) -> Self {
        self.inner = self
            .inner
            .with_visible_on_all_workspaces(visible_on_all_workspaces);
        self
    }

    fn content_protected(mut self, protected: bool) -> Self {
        self.inner = self.inner.with_content_protection(protected);
        self
    }

    fn shadow(#[allow(unused_mut)] mut self, _enable: bool) -> Self {
        #[cfg(windows)]
        {
            self.inner = self.inner.with_undecorated_shadow(_enable);
        }
        #[cfg(target_os = "macos")]
        {
            self.inner = self.inner.with_has_shadow(_enable);
        }
        self
    }

    #[cfg(windows)]
    fn owner(mut self, owner: HWND) -> Self {
        self.inner = self.inner.with_owner_window(owner.0 as _);
        self
    }

    #[cfg(windows)]
    fn parent(mut self, parent: HWND) -> Self {
        self.inner = self.inner.with_parent_window(parent.0 as _);
        self
    }

    #[cfg(target_os = "macos")]
    fn parent(mut self, parent: *mut std::ffi::c_void) -> Self {
        self.inner = self.inner.with_parent_window(parent);
        self
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    fn transient_for(mut self, parent: &impl gtk::glib::IsA<gtk::Window>) -> Self {
        self.inner = self.inner.with_transient_for(parent);
        self
    }

    #[cfg(windows)]
    fn drag_and_drop(mut self, enabled: bool) -> Self {
        self.inner = self.inner.with_drag_and_drop(enabled);
        self
    }

    #[cfg(target_os = "macos")]
    fn title_bar_style(mut self, style: TitleBarStyle) -> Self {
        match style {
            TitleBarStyle::Visible => {
                self.inner = self.inner.with_titlebar_transparent(false);
                // Fixes rendering issue when resizing window with devtools open (https://github.com/tauri-apps/tauri/issues/3914)
                self.inner = self.inner.with_fullsize_content_view(true);
            }
            TitleBarStyle::Transparent => {
                self.inner = self.inner.with_titlebar_transparent(true);
                self.inner = self.inner.with_fullsize_content_view(false);
            }
            TitleBarStyle::Overlay => {
                self.inner = self.inner.with_titlebar_transparent(true);
                self.inner = self.inner.with_fullsize_content_view(true);
            }
            unknown => {
                #[cfg(feature = "tracing")]
                tracing::warn!("unknown title bar style applied: {unknown}");

                #[cfg(not(feature = "tracing"))]
                eprintln!("unknown title bar style applied: {unknown}");
            }
        }
        self
    }

    #[cfg(target_os = "macos")]
    fn traffic_light_position<P: Into<Position>>(mut self, position: P) -> Self {
        self.inner = self.inner.with_traffic_light_inset(position.into());
        self
    }

    #[cfg(target_os = "macos")]
    fn hidden_title(mut self, hidden: bool) -> Self {
        self.inner = self.inner.with_title_hidden(hidden);
        self
    }

    #[cfg(target_os = "macos")]
    fn tabbing_identifier(mut self, identifier: &str) -> Self {
        self.inner = self.inner.with_tabbing_identifier(identifier);
        self.tabbing_identifier.replace(identifier.into());
        self
    }

    fn icon(mut self, icon: Icon) -> Result<Self> {
        self.inner = self
            .inner
            .with_window_icon(Some(TaoIcon::try_from(icon)?.0));
        Ok(self)
    }

    fn background_color(mut self, color: Color) -> Self {
        self.inner = self.inner.with_background_color(color.into());
        self
    }

    #[cfg(any(
        windows,
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    fn skip_taskbar(mut self, skip: bool) -> Self {
        self.inner = self.inner.with_skip_taskbar(skip);
        self
    }

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
    fn skip_taskbar(self, _skip: bool) -> Self {
        self
    }

    fn theme(mut self, theme: Option<Theme>) -> Self {
        self.inner = self.inner.with_theme(if let Some(t) = theme {
            match t {
                Theme::Dark => Some(TaoTheme::Dark),
                _ => Some(TaoTheme::Light),
            }
        } else {
            None
        });

        self
    }

    fn has_icon(&self) -> bool {
        self.inner.window.window_icon.is_some()
    }

    fn get_theme(&self) -> Option<Theme> {
        self.inner.window.preferred_theme.map(|theme| match theme {
            TaoTheme::Dark => Theme::Dark,
            _ => Theme::Light,
        })
    }

    #[cfg(windows)]
    fn window_classname<S: Into<String>>(mut self, window_classname: S) -> Self {
        self.inner = self.inner.with_window_classname(window_classname);
        self
    }
    #[cfg(not(windows))]
    fn window_classname<S: Into<String>>(self, _window_classname: S) -> Self {
        self
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub struct GtkWindow(pub gtk::ApplicationWindow);
#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for GtkWindow {}

#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub struct GtkBox(pub gtk::Box);
#[cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for GtkBox {}

pub struct SendRawWindowHandle(pub raw_window_handle::RawWindowHandle);
unsafe impl Send for SendRawWindowHandle {}

pub enum ApplicationMessage {
    #[cfg(target_os = "macos")]
    Show,
    #[cfg(target_os = "macos")]
    Hide,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    FetchDataStoreIdentifiers(Box<dyn FnOnce(Vec<[u8; 16]>) + Send + 'static>),
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    RemoveDataStore([u8; 16], Box<dyn FnOnce(Result<()>) + Send + 'static>),
}

pub enum WindowMessage {
    AddEventListener(WindowEventId, Box<dyn Fn(&WindowEvent) + Send>),
    // Getters
    ScaleFactor(Sender<f64>),
    InnerPosition(Sender<Result<PhysicalPosition<i32>>>),
    OuterPosition(Sender<Result<PhysicalPosition<i32>>>),
    InnerSize(Sender<PhysicalSize<u32>>),
    OuterSize(Sender<PhysicalSize<u32>>),
    IsFullscreen(Sender<bool>),
    IsMinimized(Sender<bool>),
    IsMaximized(Sender<bool>),
    IsFocused(Sender<bool>),
    IsDecorated(Sender<bool>),
    IsResizable(Sender<bool>),
    IsMaximizable(Sender<bool>),
    IsMinimizable(Sender<bool>),
    IsClosable(Sender<bool>),
    IsVisible(Sender<bool>),
    Title(Sender<String>),
    CurrentMonitor(Sender<Option<MonitorHandle>>),
    PrimaryMonitor(Sender<Option<MonitorHandle>>),
    MonitorFromPoint(Sender<Option<MonitorHandle>>, (f64, f64)),
    AvailableMonitors(Sender<Vec<MonitorHandle>>),
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    GtkWindow(Sender<GtkWindow>),
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    GtkBox(Sender<GtkBox>),
    RawWindowHandle(
        Sender<std::result::Result<SendRawWindowHandle, raw_window_handle::HandleError>>,
    ),
    Theme(Sender<Theme>),
    IsEnabled(Sender<bool>),
    IsAlwaysOnTop(Sender<bool>),
    // Setters
    Center,
    RequestUserAttention(Option<UserAttentionTypeWrapper>),
    SetEnabled(bool),
    SetResizable(bool),
    SetMaximizable(bool),
    SetMinimizable(bool),
    SetClosable(bool),
    SetTitle(String),
    Maximize,
    Unmaximize,
    Minimize,
    Unminimize,
    Show,
    Hide,
    Close,
    Destroy,
    SetDecorations(bool),
    SetShadow(bool),
    SetAlwaysOnBottom(bool),
    SetAlwaysOnTop(bool),
    SetVisibleOnAllWorkspaces(bool),
    SetContentProtected(bool),
    SetSize(Size),
    SetMinSize(Option<Size>),
    SetMaxSize(Option<Size>),
    SetSizeConstraints(WindowSizeConstraints),
    SetPosition(Position),
    SetFullscreen(bool),
    #[cfg(target_os = "macos")]
    SetSimpleFullscreen(bool),
    SetFocus,
    SetFocusable(bool),
    SetIcon(TaoWindowIcon),
    SetSkipTaskbar(bool),
    SetCursorGrab(bool),
    SetCursorVisible(bool),
    SetCursorIcon(CursorIcon),
    SetCursorPosition(Position),
    SetIgnoreCursorEvents(bool),
    SetBadgeCount(Option<i64>, Option<String>),
    SetBadgeLabel(Option<String>),
    SetOverlayIcon(Option<TaoIcon>),
    SetProgressBar(ProgressBarState),
    SetTitleBarStyle(tauri_utils::TitleBarStyle),
    SetTrafficLightPosition(Position),
    SetTheme(Option<Theme>),
    SetBackgroundColor(Option<Color>),
    DragWindow,
    ResizeDragWindow(tauri_runtime::ResizeDirection),
    RequestRedraw,
}

#[derive(Debug, Clone)]
pub enum SynthesizedWindowEvent {
    Focused(bool),
    DragDrop(DragDropEvent),
}

impl From<SynthesizedWindowEvent> for WindowEventWrapper {
    fn from(event: SynthesizedWindowEvent) -> Self {
        let event = match event {
            SynthesizedWindowEvent::Focused(focused) => WindowEvent::Focused(focused),
            SynthesizedWindowEvent::DragDrop(event) => WindowEvent::DragDrop(event),
        };
        Self(Some(event))
    }
}

pub enum WebviewMessage {
    AddEventListener(WebviewEventId, Box<dyn Fn(&WebviewEvent) + Send>),
    #[cfg(not(all(feature = "tracing", not(target_os = "android"))))]
    EvaluateScript(String),
    #[cfg(all(feature = "tracing", not(target_os = "android")))]
    EvaluateScript(String, Sender<()>, tracing::Span),
    CookiesForUrl(Url, Sender<Result<Vec<tauri_runtime::Cookie<'static>>>>),
    Cookies(Sender<Result<Vec<tauri_runtime::Cookie<'static>>>>),
    SetCookie(tauri_runtime::Cookie<'static>),
    DeleteCookie(tauri_runtime::Cookie<'static>),
    WebviewEvent(WebviewEvent),
    SynthesizedWindowEvent(SynthesizedWindowEvent),
    Navigate(Url),
    Reload,
    Print,
    Close,
    Show,
    Hide,
    SetPosition(Position),
    SetSize(Size),
    SetBounds(tauri_runtime::dpi::Rect),
    SetFocus,
    Reparent(WindowId, Sender<Result<()>>),
    SetAutoResize(bool),
    SetZoom(f64),
    SetBackgroundColor(Option<Color>),
    ClearAllBrowsingData,
    // Getters
    Url(Sender<Result<String>>),
    Bounds(Sender<Result<tauri_runtime::dpi::Rect>>),
    Position(Sender<Result<PhysicalPosition<i32>>>),
    Size(Sender<Result<PhysicalSize<u32>>>),
    WithWebview(Box<dyn FnOnce(Webview) + Send>),
    // Devtools
    #[cfg(any(debug_assertions, feature = "devtools"))]
    OpenDevTools,
    #[cfg(any(debug_assertions, feature = "devtools"))]
    CloseDevTools,
    #[cfg(any(debug_assertions, feature = "devtools"))]
    IsDevToolsOpen(Sender<bool>),
}

pub enum EventLoopWindowTargetMessage {
    CursorPosition(Sender<Result<PhysicalPosition<f64>>>),
    SetTheme(Option<Theme>),
    SetDeviceEventFilter(DeviceEventFilter),
}

pub type CreateWindowClosure<T> =
    Box<dyn FnOnce(&EventLoopWindowTarget<Message<T>>) -> Result<WindowWrapper> + Send>;

pub type CreateWebviewClosure =
    Box<dyn FnOnce(&Window, CreateWebviewOptions) -> Result<WebviewWrapper> + Send>;

pub struct CreateWebviewOptions {
    pub focused_webview: Arc<Mutex<Option<String>>>,
}

pub enum Message<T: 'static> {
    Task(Box<dyn FnOnce() + Send>),
    #[cfg(target_os = "macos")]
    SetActivationPolicy(ActivationPolicy),
    #[cfg(target_os = "macos")]
    SetDockVisibility(bool),
    RequestExit(i32),
    Application(ApplicationMessage),
    Window(WindowId, WindowMessage),
    Webview(WindowId, WebviewId, WebviewMessage),
    EventLoopWindowTarget(EventLoopWindowTargetMessage),
    CreateWebview(WindowId, CreateWebviewClosure),
    CreateWindow(WindowId, CreateWindowClosure<T>),
    CreateRawWindow(
        WindowId,
        Box<dyn FnOnce() -> (String, TaoWindowBuilder) + Send>,
        Sender<Result<Weak<Window>>>,
    ),
    UserEvent(T),
}

impl<T: UserEvent> Clone for Message<T> {
    fn clone(&self) -> Self {
        match self {
            Self::UserEvent(t) => Self::UserEvent(t.clone()),
            _ => unimplemented!(),
        }
    }
}

/// The Tauri [`WebviewDispatch`] for [`Wry`].
#[derive(Debug, Clone)]
pub struct WryWebviewDispatcher<T: UserEvent> {
    window_id: Arc<Mutex<WindowId>>,
    webview_id: WebviewId,
    context: Context<T>,
}

impl<T: UserEvent> WebviewDispatch<T> for WryWebviewDispatcher<T> {
    type Runtime = Wry<T>;

    fn run_on_main_thread<F: FnOnce() + Send + 'static>(&self, f: F) -> Result<()> {
        send_user_message(&self.context, Message::Task(Box::new(f)))
    }

    fn on_webview_event<F: Fn(&WebviewEvent) + Send + 'static>(&self, f: F) -> WindowEventId {
        let id = self.context.next_webview_event_id();
        let _ = self.context.proxy.send_event(Message::Webview(
            *self.window_id.lock().unwrap(),
            self.webview_id,
            WebviewMessage::AddEventListener(id, Box::new(f)),
        ));
        id
    }

    fn with_webview<F: FnOnce(Box<dyn std::any::Any>) + Send + 'static>(&self, f: F) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::WithWebview(Box::new(move |webview| f(Box::new(webview)))),
            ),
        )
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    fn open_devtools(&self) {
        let _ = send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::OpenDevTools,
            ),
        );
    }

    #[cfg(any(debug_assertions, feature = "devtools"))]
    fn close_devtools(&self) {
        let _ = send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::CloseDevTools,
            ),
        );
    }

    /// Gets the devtools window's current open state.
    #[cfg(any(debug_assertions, feature = "devtools"))]
    fn is_devtools_open(&self) -> Result<bool> {
        webview_getter!(self, WebviewMessage::IsDevToolsOpen)
    }

    // Getters

    fn url(&self) -> Result<String> {
        webview_getter!(self, WebviewMessage::Url)?
    }

    fn bounds(&self) -> Result<tauri_runtime::dpi::Rect> {
        webview_getter!(self, WebviewMessage::Bounds)?
    }

    fn position(&self) -> Result<PhysicalPosition<i32>> {
        webview_getter!(self, WebviewMessage::Position)?
    }

    fn size(&self) -> Result<PhysicalSize<u32>> {
        webview_getter!(self, WebviewMessage::Size)?
    }

    // Setters

    fn navigate(&self, url: Url) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::Navigate(url),
            ),
        )
    }

    fn reload(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::Reload,
            ),
        )
    }

    fn print(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::Print,
            ),
        )
    }

    fn close(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::Close,
            ),
        )
    }

    fn set_bounds(&self, bounds: tauri_runtime::dpi::Rect) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::SetBounds(bounds),
            ),
        )
    }

    fn set_size(&self, size: Size) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::SetSize(size),
            ),
        )
    }

    fn set_position(&self, position: Position) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::SetPosition(position),
            ),
        )
    }

    fn set_focus(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::SetFocus,
            ),
        )
    }

    fn reparent(&self, window_id: WindowId) -> Result<()> {
        let mut current_window_id = self.window_id.lock().unwrap();
        let (tx, rx) = channel();
        send_user_message(
            &self.context,
            Message::Webview(
                *current_window_id,
                self.webview_id,
                WebviewMessage::Reparent(window_id, tx),
            ),
        )?;

        rx.recv().unwrap()?;

        *current_window_id = window_id;
        Ok(())
    }

    fn cookies_for_url(&self, url: Url) -> Result<Vec<Cookie<'static>>> {
        let current_window_id = self.window_id.lock().unwrap();
        let (tx, rx) = channel();
        send_user_message(
            &self.context,
            Message::Webview(
                *current_window_id,
                self.webview_id,
                WebviewMessage::CookiesForUrl(url, tx),
            ),
        )?;

        rx.recv().unwrap()
    }

    fn cookies(&self) -> Result<Vec<Cookie<'static>>> {
        webview_getter!(self, WebviewMessage::Cookies)?
    }

    fn set_cookie(&self, cookie: Cookie<'_>) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::SetCookie(cookie.into_owned()),
            ),
        )?;
        Ok(())
    }

    fn delete_cookie(&self, cookie: Cookie<'_>) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::DeleteCookie(cookie.into_owned()),
            ),
        )?;
        Ok(())
    }

    fn set_auto_resize(&self, auto_resize: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::SetAutoResize(auto_resize),
            ),
        )
    }

    #[cfg(all(feature = "tracing", not(target_os = "android")))]
    fn eval_script<S: Into<String>>(&self, script: S) -> Result<()> {
        // use a channel so the EvaluateScript task uses the current span as parent
        let (tx, rx) = channel();
        getter!(
            self,
            rx,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::EvaluateScript(script.into(), tx, tracing::Span::current()),
            )
        )
    }

    #[cfg(not(all(feature = "tracing", not(target_os = "android"))))]
    fn eval_script<S: Into<String>>(&self, script: S) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::EvaluateScript(script.into()),
            ),
        )
    }

    fn set_zoom(&self, scale_factor: f64) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::SetZoom(scale_factor),
            ),
        )
    }

    fn clear_all_browsing_data(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::ClearAllBrowsingData,
            ),
        )
    }

    fn hide(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::Hide,
            ),
        )
    }

    fn show(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::Show,
            ),
        )
    }

    fn set_background_color(&self, color: Option<Color>) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Webview(
                *self.window_id.lock().unwrap(),
                self.webview_id,
                WebviewMessage::SetBackgroundColor(color),
            ),
        )
    }
}

/// The Tauri [`WindowDispatch`] for [`Wry`].
#[derive(Debug, Clone)]
pub struct WryWindowDispatcher<T: UserEvent> {
    window_id: WindowId,
    context: Context<T>,
}

// SAFETY: this is safe since the `Context` usage is guarded on `send_user_message`.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: UserEvent> Sync for WryWindowDispatcher<T> {}

fn get_raw_window_handle<T: UserEvent>(
    dispatcher: &WryWindowDispatcher<T>,
) -> Result<std::result::Result<SendRawWindowHandle, raw_window_handle::HandleError>> {
    window_getter!(dispatcher, WindowMessage::RawWindowHandle)
}

impl<T: UserEvent> WindowDispatch<T> for WryWindowDispatcher<T> {
    type Runtime = Wry<T>;
    type WindowBuilder = WindowBuilderWrapper;

    fn run_on_main_thread<F: FnOnce() + Send + 'static>(&self, f: F) -> Result<()> {
        send_user_message(&self.context, Message::Task(Box::new(f)))
    }

    fn on_window_event<F: Fn(&WindowEvent) + Send + 'static>(&self, f: F) -> WindowEventId {
        let id = self.context.next_window_event_id();
        let _ = self.context.proxy.send_event(Message::Window(
            self.window_id,
            WindowMessage::AddEventListener(id, Box::new(f)),
        ));
        id
    }

    // Getters

    fn scale_factor(&self) -> Result<f64> {
        window_getter!(self, WindowMessage::ScaleFactor)
    }

    fn inner_position(&self) -> Result<PhysicalPosition<i32>> {
        window_getter!(self, WindowMessage::InnerPosition)?
    }

    fn outer_position(&self) -> Result<PhysicalPosition<i32>> {
        window_getter!(self, WindowMessage::OuterPosition)?
    }

    fn inner_size(&self) -> Result<PhysicalSize<u32>> {
        window_getter!(self, WindowMessage::InnerSize)
    }

    fn outer_size(&self) -> Result<PhysicalSize<u32>> {
        window_getter!(self, WindowMessage::OuterSize)
    }

    fn is_fullscreen(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsFullscreen)
    }

    fn is_minimized(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsMinimized)
    }

    fn is_maximized(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsMaximized)
    }

    fn is_focused(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsFocused)
    }

    /// Gets the window's current decoration state.
    fn is_decorated(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsDecorated)
    }

    /// Gets the window's current resizable state.
    fn is_resizable(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsResizable)
    }

    /// Gets the current native window's maximize button state
    fn is_maximizable(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsMaximizable)
    }

    /// Gets the current native window's minimize button state
    fn is_minimizable(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsMinimizable)
    }

    /// Gets the current native window's close button state
    fn is_closable(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsClosable)
    }

    fn is_visible(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsVisible)
    }

    fn title(&self) -> Result<String> {
        window_getter!(self, WindowMessage::Title)
    }

    fn current_monitor(&self) -> Result<Option<Monitor>> {
        Ok(window_getter!(self, WindowMessage::CurrentMonitor)?
            .map(|m| MonitorHandleWrapper(m).into()))
    }

    fn primary_monitor(&self) -> Result<Option<Monitor>> {
        Ok(window_getter!(self, WindowMessage::PrimaryMonitor)?
            .map(|m| MonitorHandleWrapper(m).into()))
    }

    fn monitor_from_point(&self, x: f64, y: f64) -> Result<Option<Monitor>> {
        let (tx, rx) = channel();

        let _ = send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::MonitorFromPoint(tx, (x, y))),
        );

        Ok(rx
            .recv()
            .map_err(|_| Error::FailedToReceiveMessage)?
            .map(|m| MonitorHandleWrapper(m).into()))
    }

    fn available_monitors(&self) -> Result<Vec<Monitor>> {
        Ok(window_getter!(self, WindowMessage::AvailableMonitors)?
            .into_iter()
            .map(|m| MonitorHandleWrapper(m).into())
            .collect())
    }

    fn theme(&self) -> Result<Theme> {
        window_getter!(self, WindowMessage::Theme)
    }

    fn is_enabled(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsEnabled)
    }

    fn is_always_on_top(&self) -> Result<bool> {
        window_getter!(self, WindowMessage::IsAlwaysOnTop)
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    fn gtk_window(&self) -> Result<gtk::ApplicationWindow> {
        window_getter!(self, WindowMessage::GtkWindow).map(|w| w.0)
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    fn default_vbox(&self) -> Result<gtk::Box> {
        window_getter!(self, WindowMessage::GtkBox).map(|w| w.0)
    }

    fn window_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
    {
        get_raw_window_handle(self)
            .map_err(|_| raw_window_handle::HandleError::Unavailable)
            .and_then(|r| r.map(|h| unsafe { raw_window_handle::WindowHandle::borrow_raw(h.0) }))
    }

    // Setters

    fn center(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::Center),
        )
    }

    fn request_user_attention(&self, request_type: Option<UserAttentionType>) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(
                self.window_id,
                WindowMessage::RequestUserAttention(request_type.map(Into::into)),
            ),
        )
    }

    // Creates a window by dispatching a message to the event loop.
    // Note that this must be called from a separate thread, otherwise the channel will introduce a deadlock.
    fn create_window<F: Fn(RawWindow) + Send + 'static>(
        &mut self,
        pending: PendingWindow<T, Self::Runtime>,
        after_window_creation: Option<F>,
    ) -> Result<DetachedWindow<T, Self::Runtime>> {
        self.context.create_window(pending, after_window_creation)
    }

    // Creates a webview by dispatching a message to the event loop.
    // Note that this must be called from a separate thread, otherwise the channel will introduce a deadlock.
    fn create_webview(
        &mut self,
        pending: PendingWebview<T, Self::Runtime>,
    ) -> Result<DetachedWebview<T, Self::Runtime>> {
        self.context.create_webview(self.window_id, pending)
    }

    fn set_resizable(&self, resizable: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetResizable(resizable)),
        )
    }

    fn set_enabled(&self, enabled: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetEnabled(enabled)),
        )
    }

    fn set_maximizable(&self, maximizable: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetMaximizable(maximizable)),
        )
    }

    fn set_minimizable(&self, minimizable: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetMinimizable(minimizable)),
        )
    }

    fn set_closable(&self, closable: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetClosable(closable)),
        )
    }

    fn set_title<S: Into<String>>(&self, title: S) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetTitle(title.into())),
        )
    }

    fn maximize(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::Maximize),
        )
    }

    fn unmaximize(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::Unmaximize),
        )
    }

    fn minimize(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::Minimize),
        )
    }

    fn unminimize(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::Unminimize),
        )
    }

    fn show(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::Show),
        )
    }

    fn hide(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::Hide),
        )
    }

    fn close(&self) -> Result<()> {
        // NOTE: close cannot use the `send_user_message` function because it accesses the event loop callback
        self.context
            .proxy
            .send_event(Message::Window(self.window_id, WindowMessage::Close))
            .map_err(|_| Error::FailedToSendMessage)
    }

    fn destroy(&self) -> Result<()> {
        // NOTE: destroy cannot use the `send_user_message` function because it accesses the event loop callback
        self.context
            .proxy
            .send_event(Message::Window(self.window_id, WindowMessage::Destroy))
            .map_err(|_| Error::FailedToSendMessage)
    }

    fn set_decorations(&self, decorations: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetDecorations(decorations)),
        )
    }

    fn set_shadow(&self, enable: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetShadow(enable)),
        )
    }

    fn set_always_on_bottom(&self, always_on_bottom: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(
                self.window_id,
                WindowMessage::SetAlwaysOnBottom(always_on_bottom),
            ),
        )
    }

    fn set_always_on_top(&self, always_on_top: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetAlwaysOnTop(always_on_top)),
        )
    }

    fn set_visible_on_all_workspaces(&self, visible_on_all_workspaces: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(
                self.window_id,
                WindowMessage::SetVisibleOnAllWorkspaces(visible_on_all_workspaces),
            ),
        )
    }

    fn set_content_protected(&self, protected: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(
                self.window_id,
                WindowMessage::SetContentProtected(protected),
            ),
        )
    }

    fn set_size(&self, size: Size) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetSize(size)),
        )
    }

    fn set_min_size(&self, size: Option<Size>) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetMinSize(size)),
        )
    }

    fn set_max_size(&self, size: Option<Size>) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetMaxSize(size)),
        )
    }

    fn set_size_constraints(&self, constraints: WindowSizeConstraints) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(
                self.window_id,
                WindowMessage::SetSizeConstraints(constraints),
            ),
        )
    }

    fn set_position(&self, position: Position) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetPosition(position)),
        )
    }

    fn set_fullscreen(&self, fullscreen: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetFullscreen(fullscreen)),
        )
    }

    #[cfg(target_os = "macos")]
    fn set_simple_fullscreen(&self, enable: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetSimpleFullscreen(enable)),
        )
    }

    fn set_focus(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetFocus),
        )
    }

    fn set_focusable(&self, focusable: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetFocusable(focusable)),
        )
    }

    fn set_icon(&self, icon: Icon) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(
                self.window_id,
                WindowMessage::SetIcon(TaoIcon::try_from(icon)?.0),
            ),
        )
    }

    fn set_skip_taskbar(&self, skip: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetSkipTaskbar(skip)),
        )
    }

    fn set_cursor_grab(&self, grab: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetCursorGrab(grab)),
        )
    }

    fn set_cursor_visible(&self, visible: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetCursorVisible(visible)),
        )
    }

    fn set_cursor_icon(&self, icon: CursorIcon) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetCursorIcon(icon)),
        )
    }

    fn set_cursor_position<Pos: Into<Position>>(&self, position: Pos) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(
                self.window_id,
                WindowMessage::SetCursorPosition(position.into()),
            ),
        )
    }

    fn set_ignore_cursor_events(&self, ignore: bool) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetIgnoreCursorEvents(ignore)),
        )
    }

    fn start_dragging(&self) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::DragWindow),
        )
    }

    fn start_resize_dragging(&self, direction: tauri_runtime::ResizeDirection) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::ResizeDragWindow(direction)),
        )
    }

    fn set_badge_count(&self, count: Option<i64>, desktop_filename: Option<String>) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(
                self.window_id,
                WindowMessage::SetBadgeCount(count, desktop_filename),
            ),
        )
    }

    fn set_badge_label(&self, label: Option<String>) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetBadgeLabel(label)),
        )
    }

    fn set_overlay_icon(&self, icon: Option<Icon>) -> Result<()> {
        let icon: Result<Option<TaoIcon>> =
            icon.map_or(Ok(None), |x| Ok(Some(TaoIcon::try_from(x)?)));

        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetOverlayIcon(icon?)),
        )
    }

    fn set_progress_bar(&self, progress_state: ProgressBarState) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(
                self.window_id,
                WindowMessage::SetProgressBar(progress_state),
            ),
        )
    }

    fn set_title_bar_style(&self, style: tauri_utils::TitleBarStyle) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetTitleBarStyle(style)),
        )
    }

    fn set_traffic_light_position(&self, position: Position) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(
                self.window_id,
                WindowMessage::SetTrafficLightPosition(position),
            ),
        )
    }

    fn set_theme(&self, theme: Option<Theme>) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetTheme(theme)),
        )
    }

    fn set_background_color(&self, color: Option<Color>) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Window(self.window_id, WindowMessage::SetBackgroundColor(color)),
        )
    }
}

#[derive(Clone)]
pub struct WebviewWrapper {
    label: String,
    id: WebviewId,
    _client: cef::Client,
    browser_slot: BrowserSlot,
    webview_event_listeners: WebviewEventListeners,
    background_color: Arc<Mutex<(u8, u8, u8, u8)>>,
    rect: Arc<Mutex<Rect>>,
    bounds: Arc<Mutex<Option<WebviewBounds>>>,
    /// OSR pixel-buffer state (Wayland only). The OsrSurface is stored in WindowWrapper.
    #[cfg(feature = "wayland-osr")]
    osr_state: Option<Arc<Mutex<OsrState>>>,
}

fn rgba_to_cef_color((r, g, b, a): (u8, u8, u8, u8)) -> u32 {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

fn zoom_factor_to_cef_level(zoom_factor: f64) -> f64 {
    if !zoom_factor.is_finite() || zoom_factor <= 0.0 {
        return 0.0;
    }

    (zoom_factor.ln() / 1.2_f64.ln()).clamp(-10.0, 10.0)
}

fn apply_background_color(
    slot: &BrowserSlot,
    (r, g, b, a): (u8, u8, u8, u8),
) -> std::result::Result<(), &'static str> {
    let alpha = f64::from(a) / 255.0;
    let script = format!(
    "(() => {{ const c = 'rgba({r}, {g}, {b}, {alpha:.4})'; document.documentElement.style.backgroundColor = c; if (document.body) document.body.style.backgroundColor = c; }})();"
  );

    if slot.eval(&script) {
        Ok(())
    } else {
        Err("browser is not available")
    }
}

fn sanitize_bounds_for_window(bounds: Rect, window: &Window) -> Rect {
    let scale_factor = window.scale_factor();
    let window_size = window.inner_size();

    let mut position = bounds.position.to_physical::<i32>(scale_factor);
    let mut size = bounds.size.to_physical::<u32>(scale_factor);

    if size.width <= 1 || size.height <= 1 {
        position = PhysicalPosition::new(0, 0);
        size = PhysicalSize::new(window_size.width.max(1), window_size.height.max(1));
    }

    if position.x < 0 {
        position.x = 0;
    }
    if position.y < 0 {
        position.y = 0;
    }

    if window_size.width > 0 && (position.x as u32) >= window_size.width {
        position.x = 0;
    }
    if window_size.height > 0 && (position.y as u32) >= window_size.height {
        position.y = 0;
    }

    if window_size.width > 0 {
        let max_width = window_size.width.saturating_sub(position.x as u32).max(1);
        size.width = size.width.min(max_width);
    }
    if window_size.height > 0 {
        let max_height = window_size.height.saturating_sub(position.y as u32).max(1);
        size.height = size.height.min(max_height);
    }

    Rect {
        position: Position::Physical(position),
        size: Size::Physical(size),
    }
}

#[cfg(all(
    feature = "tao-runtime",
    any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
))]
fn move_resize_browser_child(
    window: &Window,
    slot: &BrowserSlot,
    bounds: Rect,
) -> std::result::Result<(), &'static str> {
    let Some(browser) = slot.current() else {
        return Err("browser is not available");
    };
    let Some(host) = browser.host() else {
        return Err("browser host is not available");
    };

    let child_handle = host.window_handle();
    if child_handle == 0 {
        return Err("browser child window handle is not available");
    }

    let bounds = sanitize_bounds_for_window(bounds, window);
    let position = bounds.position.to_physical::<i32>(window.scale_factor());
    let size = bounds.size.to_physical::<u32>(window.scale_factor());
    let width = size.width.max(1);
    let height = size.height.max(1);

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(handle) = window.window_handle() {
        if let RawWindowHandle::Wayland(_) = handle.as_raw() {
            // On Wayland, CEF's Ozone backend manages its own wl_subsurface.
            // We can't use Xlib calls, but we notify CEF so it updates internally.
            if let Some(browser) = slot.current() {
                if let Some(host) = browser.host() {
                    host.notify_move_or_resize_started();
                }
            }
            return Ok(());
        }
    }

    let xlib = xlib::Xlib::open().map_err(|_| "failed to open Xlib")?;
    let display = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
    if display.is_null() {
        return Err("failed to open X11 display");
    }

    unsafe {
        (xlib.XMoveResizeWindow)(
            display,
            child_handle as xlib::Window,
            position.x,
            position.y,
            width,
            height,
        );
        (xlib.XMapRaised)(display, child_handle as xlib::Window);
        (xlib.XFlush)(display);
        (xlib.XCloseDisplay)(display);
    }

    Ok(())
}

#[cfg(not(all(
    feature = "tao-runtime",
    any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )
)))]
fn move_resize_browser_child(
    _window: &Window,
    _slot: &BrowserSlot,
    _bounds: Rect,
) -> std::result::Result<(), &'static str> {
    Ok(())
}

fn get_cookie_manager(slot: &BrowserSlot) -> Option<cef::CookieManager> {
    if let Some(browser) = slot.current() {
        if let Some(host) = browser.host() {
            if let Some(context) = host.request_context() {
                if let Some(manager) = context.cookie_manager(None) {
                    return Some(manager);
                }
            }
        }
    }

    cookie_manager_get_global_manager(None)
}

fn cookie_url_from(domain: Option<&str>, fallback: Option<&str>) -> Option<String> {
    if let Some(domain) = domain {
        let host = domain.trim_start_matches('.');
        if !host.is_empty() {
            return Some(format!("https://{host}/"));
        }
    }

    fallback.and_then(|raw| Url::parse(raw).ok().map(|url| url.to_string()))
}

fn tauri_cookie_to_cef(cookie: &Cookie<'_>) -> cef::Cookie {
    let mut cef_cookie = cef::Cookie {
        name: CefString::from(cookie.name()),
        value: CefString::from(cookie.value()),
        secure: i32::from(cookie.secure().unwrap_or(false)),
        httponly: i32::from(cookie.http_only().unwrap_or(false)),
        ..Default::default()
    };

    if let Some(domain) = cookie.domain() {
        cef_cookie.domain = CefString::from(domain);
    }

    if let Some(path) = cookie.path() {
        cef_cookie.path = CefString::from(path);
    } else {
        cef_cookie.path = CefString::from("/");
    }

    cef_cookie
}

fn cef_cookie_to_tauri(cookie: &cef::Cookie) -> Cookie<'static> {
    let name = cookie.name.to_string();
    let value = cookie.value.to_string();
    let mut output = Cookie::new(name, value);

    let domain = cookie.domain.to_string();
    if !domain.is_empty() {
        output.set_domain(domain);
    }

    let path = cookie.path.to_string();
    if !path.is_empty() {
        output.set_path(path);
    }

    output.set_secure(cookie.secure != 0);
    output.set_http_only(cookie.httponly != 0);
    output.into_owned()
}

cef::wrap_cookie_visitor! {
  struct CollectCookiesVisitor {
    items: Arc<Mutex<Vec<Cookie<'static>>>>,
    done_tx: Arc<Mutex<Option<Sender<()>>>>,
  }

  impl CookieVisitor {
    fn visit(
      &self,
      cookie: Option<&cef::Cookie>,
      count: ::std::os::raw::c_int,
      total: ::std::os::raw::c_int,
      _delete_cookie: Option<&mut ::std::os::raw::c_int>,
    ) -> ::std::os::raw::c_int {
      if let Some(cookie) = cookie {
        if let Ok(mut items) = self.items.lock() {
          items.push(cef_cookie_to_tauri(cookie));
        }
      }

      if total <= 0 || count + 1 >= total {
        if let Ok(mut done_tx) = self.done_tx.lock() {
          if let Some(done_tx) = done_tx.take() {
            let _ = done_tx.send(());
          }
        }
      }

      1
    }
  }
}

fn collect_cookies(
    slot: &BrowserSlot,
    url: Option<&str>,
) -> std::result::Result<Vec<Cookie<'static>>, &'static str> {
    let Some(manager) = get_cookie_manager(slot) else {
        return Err("cookie manager is not available");
    };

    let items = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, done_rx) = channel();
    let done_tx = Arc::new(Mutex::new(Some(done_tx)));
    let mut visitor = CollectCookiesVisitor::new(items.clone(), done_tx);

    let accepted = if let Some(url) = url {
        let url = CefString::from(url);
        manager.visit_url_cookies(Some(&url), 1, Some(&mut visitor))
    } else {
        manager.visit_all_cookies(Some(&mut visitor))
    };

    if accepted == 0 {
        return Err("failed to start cookie query");
    }

    let _ = done_rx.recv_timeout(Duration::from_millis(60));

    items
        .lock()
        .map(|items| items.clone())
        .map_err(|_| "failed to collect cookies")
}

fn resource_request_to_http_request(payload: &ResourceRequestPayload) -> Option<Request<Vec<u8>>> {
    let uri = http::Uri::try_from(payload.url.as_str()).ok().or_else(|| {
        Url::parse(&payload.url).ok().and_then(|url| {
            let mut fallback = format!("http://localhost{}", url.path());
            if let Some(query) = url.query() {
                fallback.push('?');
                fallback.push_str(query);
            }
            http::Uri::try_from(fallback).ok()
        })
    })?;

    let mut request = Request::builder()
        .method(payload.method.as_str())
        .uri(uri)
        .body(payload.body.clone())
        .ok()?;

    for (name, value) in &payload.headers {
        if let (Ok(name), Ok(value)) = (
            http::header::HeaderName::from_bytes(name.as_bytes()),
            http::HeaderValue::from_str(value),
        ) {
            request.headers_mut().insert(name, value);
        }
    }

    Some(request)
}

fn apply_cors_headers_for_custom_protocol(
    request: &ResourceRequestPayload,
    _protocol_name: &str,
    response: &mut http::Response<Cow<'static, [u8]>>,
) {
    let headers = response.headers_mut();
    if !headers.contains_key(http::header::ACCESS_CONTROL_ALLOW_ORIGIN) {
        let origin = request
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("origin"))
            .map(|(_, value)| value.as_str())
            .unwrap_or("*");

        let allow_origin = http::HeaderValue::from_str(origin)
            .unwrap_or_else(|_| http::HeaderValue::from_static("*"));
        headers.insert(http::header::ACCESS_CONTROL_ALLOW_ORIGIN, allow_origin);
    }

    if request.method.eq_ignore_ascii_case("OPTIONS")
        && !headers.contains_key(http::header::ACCESS_CONTROL_ALLOW_HEADERS)
    {
        headers.insert(
            http::header::ACCESS_CONTROL_ALLOW_HEADERS,
            http::HeaderValue::from_static("*"),
        );
    }

    // Always expose all response headers to JavaScript for custom protocol responses.
    // CEF's Chromium engine applies standard CORS header filtering even for custom schemes,
    // so without this wildcard, custom headers like `Tauri-Response` would not be visible
    // in the JavaScript fetch Response.headers API.
    headers.insert(
        http::header::ACCESS_CONTROL_EXPOSE_HEADERS,
        http::HeaderValue::from_static("*"),
    );
}

fn protocol_name_from_url(url: &Url) -> Option<String> {
    let scheme = url.scheme();
    if !matches!(scheme, "http" | "https") {
        return Some(scheme.to_string());
    }

    let host = url.host_str()?;
    let protocol = host.strip_suffix(".localhost")?;
    if protocol.is_empty() {
        return None;
    }

    Some(protocol.to_string())
}

fn normalize_cef_initial_url(url: &str, use_https_scheme: bool) -> String {
    let Ok(parsed_url) = Url::parse(url) else {
        return url.to_string();
    };

    if parsed_url.scheme() != "tauri" || parsed_url.host_str() != Some("localhost") {
        return url.to_string();
    }

    let scheme = if use_https_scheme { "https" } else { "http" };
    let mut normalized = format!("{scheme}://tauri.localhost{}", parsed_url.path());
    if let Some(query) = parsed_url.query() {
        normalized.push('?');
        normalized.push_str(query);
    }
    if let Some(fragment) = parsed_url.fragment() {
        normalized.push('#');
        normalized.push_str(fragment);
    }

    normalized
}

fn set_string_preference(context: &RequestContext, name: &str, value: &str) -> bool {
    let name_string = name.to_string();
    let name = CefString::from(name);
    if context.can_set_preference(Some(&name)) == 0 {
        log::warn!("CEF cannot set preference '{}'", name_string);
        return false;
    }

    let mut pref_value = match value_create() {
        Some(value) => value,
        None => {
            log::warn!(
                "CEF failed to allocate preference value for '{}'",
                name_string
            );
            return false;
        }
    };

    if pref_value.set_string(Some(&CefString::from(value))) == 0 {
        log::warn!(
            "CEF failed to assign preference value for '{}'",
            name_string
        );
        return false;
    }

    let mut error = CefString::default();
    if context.set_preference(Some(&name), Some(&mut pref_value), Some(&mut error)) == 0 {
        let error = error.to_string();
        if error.is_empty() {
            log::warn!("CEF rejected preference '{}'", name_string);
        } else {
            log::warn!("CEF rejected preference '{}': {error}", name_string);
        }
        return false;
    }

    true
}

fn apply_proxy_preference(context: &RequestContext, proxy_url: &Url) {
    let Some(host) = proxy_url.host_str() else {
        log::warn!("ignoring proxy_url without host: {proxy_url}");
        return;
    };

    let proxy_server = proxy_url
        .port_or_known_default()
        .map(|port| format!("{}://{host}:{port}", proxy_url.scheme()))
        .unwrap_or_else(|| format!("{}://{host}", proxy_url.scheme()));

    let mut proxy_dict = match dictionary_value_create() {
        Some(dict) => dict,
        None => {
            log::warn!("CEF failed to allocate proxy preference dictionary");
            return;
        }
    };

    let mode_key = CefString::from("mode");
    let mode_value = CefString::from("fixed_servers");
    let _ = proxy_dict.set_string(Some(&mode_key), Some(&mode_value));

    let server_key = CefString::from("server");
    let server_value = CefString::from(proxy_server.as_str());
    let _ = proxy_dict.set_string(Some(&server_key), Some(&server_value));

    let bypass_key = CefString::from("bypass_list");
    let bypass_value = CefString::from("<-loopback>");
    let _ = proxy_dict.set_string(Some(&bypass_key), Some(&bypass_value));

    let mut proxy_value = match value_create() {
        Some(value) => value,
        None => {
            log::warn!("CEF failed to allocate proxy preference value");
            return;
        }
    };

    if proxy_value.set_dictionary(Some(&mut proxy_dict)) == 0 {
        log::warn!("CEF failed to encode proxy preference value");
        return;
    }

    let pref_name = CefString::from("proxy");
    if context.can_set_preference(Some(&pref_name)) == 0 {
        log::warn!("CEF cannot set preference 'proxy'");
        return;
    }

    let mut error = CefString::default();
    if context.set_preference(Some(&pref_name), Some(&mut proxy_value), Some(&mut error)) == 0 {
        let error = error.to_string();
        if error.is_empty() {
            log::warn!("CEF rejected proxy preference");
        } else {
            log::warn!("CEF rejected proxy preference: {error}");
        }
    }
}

fn create_request_context_for_webview(
    attributes: &tauri_runtime::webview::WebviewAttributes,
) -> Option<RequestContext> {
    let should_create_context =
        attributes.incognito || attributes.user_agent.is_some() || attributes.proxy_url.is_some();
    let settings = RequestContextSettings::default();

    if !attributes.incognito {
        // We do not create a separate RequestContext just for data_directory.
        // Doing so conflicts with the global CEF cache path and causes "Cannot create profile" errors in the Chrome runtime.
        // The global CEF cache_path should be configured during `bootstrap()` to point to the app data directory.
    }

    if !should_create_context {
        return None;
    }

    let context = request_context_create_context(Some(&settings), None);
    let Some(context) = context else {
        log::warn!("failed to create custom CEF request context");
        return None;
    };

    if let Some(user_agent) = attributes.user_agent.as_deref() {
        let _ = set_string_preference(&context, "general.useragent.override", user_agent);
    }

    if let Some(proxy_url) = attributes.proxy_url.as_ref() {
        apply_proxy_preference(&context, proxy_url);
    }

    Some(context)
}

fn send_init_scripts_to_renderer(browser: &cef::Browser, scripts: &[InitializationScript]) {
    let Some(main_frame) = browser.main_frame() else {
        return;
    };

    let clear_name = CefString::from(INIT_SCRIPT_CLEAR_MESSAGE);
    if let Some(mut message) = process_message_create(Some(&clear_name)) {
        main_frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
    }

    for script in scripts {
        let message_name = CefString::from(INIT_SCRIPT_ADD_MESSAGE);
        let Some(mut message) = process_message_create(Some(&message_name)) else {
            continue;
        };
        let Some(arguments) = message.argument_list() else {
            continue;
        };
        if arguments.set_size(2) == 0 {
            continue;
        }

        let script_code = CefString::from(script.script.as_str());
        if arguments.set_string(0, Some(&script_code)) == 0 {
            continue;
        }
        let _ = arguments.set_bool(1, i32::from(script.for_main_frame_only));

        main_frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
    }
}

#[cfg(feature = "new-window-opener-optional")]
fn popup_features_to_new_window_features(features: PopupRequestFeatures) -> NewWindowFeatures {
    let position = features
        .position
        .map(|(x, y)| LogicalPosition::new(f64::from(x), f64::from(y)));
    let size = features
        .size
        .map(|(width, height)| LogicalSize::new(f64::from(width), f64::from(height)));

    NewWindowFeatures::new(size, position, None)
}

#[cfg(feature = "new-window-opener-optional")]
fn route_new_window_to_existing_window<T: UserEvent>(
    context: &Context<T>,
    target_window_id: WindowId,
    url: &Url,
) -> bool {
    let windows = &context.main_thread.windows.0;
    let deadline = Instant::now() + Duration::from_secs(2);

    let target_webview = loop {
        if let Some(webview) = windows.try_borrow().ok().and_then(|windows| {
            windows
                .get(&target_window_id)
                .and_then(|window| window.webviews.first().cloned())
        }) {
            break Some(webview);
        }

        if Instant::now() >= deadline {
            break None;
        }

        std::thread::sleep(Duration::from_millis(25));
    };

    let Some(webview) = target_webview else {
        log::warn!(
            "new_window_handler returned Create for window without webview: {:?}",
            target_window_id
        );
        return false;
    };

    if let Err(e) = webview.load_url(url.as_str()) {
        log::error!(
            "failed to route popup to target window {:?}: {}",
            target_window_id,
            e
        );
    }

    false
}

impl WebviewWrapper {
    fn evaluate_script(&self, script: &str) -> std::result::Result<(), &'static str> {
        if self.browser_slot.eval(script) {
            Ok(())
        } else {
            Err("browser is not available")
        }
    }

    fn load_url(&self, url: &str) -> std::result::Result<(), &'static str> {
        if self.browser_slot.load_url(url) {
            Ok(())
        } else {
            Err("browser is not available")
        }
    }

    fn reload(&self) -> std::result::Result<(), &'static str> {
        if self.browser_slot.reload(false) {
            Ok(())
        } else {
            Err("browser is not available")
        }
    }

    fn set_visible(&self, visible: bool) -> std::result::Result<(), &'static str> {
        if let Some(browser) = self.browser_slot.current() {
            if let Some(host) = browser.host() {
                host.was_hidden(i32::from(!visible));
                return Ok(());
            }
        }

        Err("browser is not available")
    }

    fn print(&self) -> std::result::Result<(), &'static str> {
        if let Some(browser) = self.browser_slot.current() {
            if let Some(host) = browser.host() {
                host.print();
                return Ok(());
            }
        }

        Err("browser is not available")
    }

    fn set_bounds(&self, bounds: Rect, window: &Window) -> std::result::Result<(), &'static str> {
        let bounds = sanitize_bounds_for_window(bounds, window);

        if let Ok(mut rect) = self.rect.lock() {
            *rect = bounds;
        }

        if let Err(error) = move_resize_browser_child(window, &self.browser_slot, bounds) {
            log::debug!("failed to move/resize native browser child: {error}");
        }
        self.browser_slot.notify_resized();
        Ok(())
    }

    fn bounds(&self) -> std::result::Result<Rect, &'static str> {
        if let Ok(rect) = self.rect.lock() {
            return Ok(*rect);
        }

        Err("failed to lock webview bounds")
    }

    fn zoom(&self, scale_factor: f64) -> std::result::Result<(), &'static str> {
        if let Some(browser) = self.browser_slot.current() {
            if let Some(host) = browser.host() {
                host.set_zoom_level(zoom_factor_to_cef_level(scale_factor));
                return Ok(());
            }
        }

        Err("browser is not available")
    }

    fn set_background_color(
        &self,
        rgba: (u8, u8, u8, u8),
    ) -> std::result::Result<(), &'static str> {
        if let Ok(mut color) = self.background_color.lock() {
            *color = rgba;
        }

        apply_background_color(&self.browser_slot, rgba)
    }

    fn clear_all_browsing_data(&self) -> std::result::Result<(), &'static str> {
        let mut cleared = false;

        if let Some(browser) = self.browser_slot.current() {
            if let Some(host) = browser.host() {
                if let Some(context) = host.request_context() {
                    context.clear_http_cache(None);
                    cleared = true;

                    if let Some(manager) = context.cookie_manager(None) {
                        let _ = manager.delete_cookies(None, None, None);
                        let _ = manager.flush_store(None);
                    }
                }
            }
        }

        if !cleared {
            if let Some(manager) = cookie_manager_get_global_manager(None) {
                let _ = manager.delete_cookies(None, None, None);
                let _ = manager.flush_store(None);
                cleared = true;
            }
        }

        if cleared {
            Ok(())
        } else {
            Err("browser context is not available")
        }
    }

    fn url(&self) -> std::result::Result<String, &'static str> {
        if let Some(browser) = self.browser_slot.current() {
            if let Some(frame) = browser.main_frame() {
                return Ok(CefString::from(&frame.url()).to_string());
            }
        }

        Err("browser is not available")
    }

    fn cookies(&self) -> std::result::Result<Vec<Cookie<'static>>, &'static str> {
        collect_cookies(&self.browser_slot, None)
    }

    fn set_cookie(&self, cookie: &Cookie<'_>) -> std::result::Result<(), &'static str> {
        let Some(manager) = get_cookie_manager(&self.browser_slot) else {
            return Err("cookie manager is not available");
        };

        let current_url = self.url().ok();
        let Some(url) = cookie_url_from(cookie.domain(), current_url.as_deref()) else {
            return Err("unable to determine cookie URL");
        };

        let url = CefString::from(url.as_str());
        let mut cef_cookie = tauri_cookie_to_cef(cookie);

        if manager.set_cookie(Some(&url), Some(&mut cef_cookie), None) == 0 {
            return Err("failed to set cookie");
        }

        Ok(())
    }

    fn delete_cookie(&self, cookie: &Cookie<'_>) -> std::result::Result<(), &'static str> {
        let Some(manager) = get_cookie_manager(&self.browser_slot) else {
            return Err("cookie manager is not available");
        };

        let current_url = self.url().ok();
        let Some(url) = cookie_url_from(cookie.domain(), current_url.as_deref()) else {
            return Err("unable to determine cookie URL");
        };

        let url = CefString::from(url.as_str());
        let name = CefString::from(cookie.name());
        let deleted = manager.delete_cookies(Some(&url), Some(&name), None);

        if deleted < 0 {
            return Err("failed to delete cookie");
        }

        Ok(())
    }

    fn cookies_for_url(
        &self,
        url: &str,
    ) -> std::result::Result<Vec<Cookie<'static>>, &'static str> {
        collect_cookies(&self.browser_slot, Some(url))
    }

    fn focus(&self) -> std::result::Result<(), &'static str> {
        if let Some(browser) = self.browser_slot.current() {
            if let Some(host) = browser.host() {
                host.set_focus(1);
                return Ok(());
            }
        }

        Err("browser is not available")
    }

    fn webview(&self) -> Webview {
        self.browser_slot
            .current()
            .expect("webview browser is not available")
    }

    fn open_devtools(&self) {
        if let Some(browser) = self.browser_slot.current() {
            if let Some(host) = browser.host() {
                host.show_dev_tools(None, None, Some(&BrowserSettings::default()), None);
            }
        }
    }

    fn close_devtools(&self) {
        if let Some(browser) = self.browser_slot.current() {
            if let Some(host) = browser.host() {
                host.close_dev_tools();
            }
        }
    }

    fn is_devtools_open(&self) -> bool {
        if let Some(browser) = self.browser_slot.current() {
            if let Some(host) = browser.host() {
                return host.has_dev_tools() != 0;
            }
        }

        false
    }
}

pub struct WindowWrapper {
    label: String,
    inner: Option<Arc<Window>>,
    // whether this window has child webviews
    // or it's just a container for a single webview
    has_children: AtomicBool,
    webviews: Vec<WebviewWrapper>,
    window_event_listeners: WindowEventListeners,
    #[cfg(windows)]
    background_color: Option<tao::window::RGBA>,
    #[cfg(windows)]
    is_window_transparent: bool,
    #[cfg(windows)]
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    focused_webview: Arc<Mutex<Option<String>>>,
    /// OSR softbuffer surface (Wayland only).
    #[cfg(feature = "wayland-osr")]
    osr_surface: Option<OsrSurface>,
}

impl WindowWrapper {
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl fmt::Debug for WindowWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowWrapper")
            .field("label", &self.label)
            .field("inner", &self.inner)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct EventProxy<T: UserEvent>(TaoEventLoopProxy<Message<T>>);

#[cfg(target_os = "ios")]
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: UserEvent> Sync for EventProxy<T> {}

impl<T: UserEvent> EventLoopProxy<T> for EventProxy<T> {
    fn send_event(&self, event: T) -> Result<()> {
        self.0
            .send_event(Message::UserEvent(event))
            .map_err(|_| Error::EventLoopClosed)
    }
}

pub trait PluginBuilder<T: UserEvent> {
    type Plugin: Plugin<T>;
    fn build(self, context: Context<T>) -> Self::Plugin;
}

pub trait Plugin<T: UserEvent> {
    fn on_event(
        &mut self,
        event: &Event<Message<T>>,
        event_loop: &EventLoopWindowTarget<Message<T>>,
        proxy: &TaoEventLoopProxy<Message<T>>,
        control_flow: &mut ControlFlow,
        context: EventLoopIterationContext<'_, T>,
        web_context: &WebContextStore,
    ) -> bool;
}

/// A Tauri [`Runtime`] wrapper around wry.
pub struct Wry<T: UserEvent> {
    context: Context<T>,
    event_loop: EventLoop<Message<T>>,
}

impl<T: UserEvent> fmt::Debug for Wry<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wry")
            .field("main_thread_id", &self.context.main_thread_id)
            .field("event_loop", &self.event_loop)
            .field("windows", &self.context.main_thread.windows)
            .field("web_context", &self.context.main_thread.web_context)
            .finish()
    }
}

/// A handle to the Wry runtime.
#[derive(Debug, Clone)]
pub struct WryHandle<T: UserEvent> {
    context: Context<T>,
}

// SAFETY: this is safe since the `Context` usage is guarded on `send_user_message`.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl<T: UserEvent> Sync for WryHandle<T> {}

impl<T: UserEvent> WryHandle<T> {
    /// Creates a new tao window using a callback, and returns its window id.
    pub fn create_tao_window<F: FnOnce() -> (String, TaoWindowBuilder) + Send + 'static>(
        &self,
        f: F,
    ) -> Result<Weak<Window>> {
        let id = self.context.next_window_id();
        let (tx, rx) = channel();
        send_user_message(&self.context, Message::CreateRawWindow(id, Box::new(f), tx))?;
        rx.recv().unwrap()
    }

    /// Gets the [`WebviewId'] associated with the given [`WindowId`].
    pub fn window_id(&self, window_id: TaoWindowId) -> WindowId {
        *self
            .context
            .window_id_map
            .0
            .lock()
            .unwrap()
            .get(&window_id)
            .unwrap()
    }

    /// Send a message to the event loop.
    pub fn send_event(&self, message: Message<T>) -> Result<()> {
        self.context
            .proxy
            .send_event(message)
            .map_err(|_| Error::FailedToSendMessage)?;
        Ok(())
    }

    pub fn plugin<P: PluginBuilder<T> + 'static>(&mut self, plugin: P)
    where
        <P as PluginBuilder<T>>::Plugin: Send,
    {
        self.context
            .plugins
            .lock()
            .unwrap()
            .push(Box::new(plugin.build(self.context.clone())));
    }
}

impl<T: UserEvent> RuntimeHandle<T> for WryHandle<T> {
    type Runtime = Wry<T>;

    fn create_proxy(&self) -> EventProxy<T> {
        EventProxy(self.context.proxy.clone())
    }

    #[cfg(target_os = "macos")]
    fn set_activation_policy(&self, activation_policy: ActivationPolicy) -> Result<()> {
        send_user_message(
            &self.context,
            Message::SetActivationPolicy(activation_policy),
        )
    }

    #[cfg(target_os = "macos")]
    fn set_dock_visibility(&self, visible: bool) -> Result<()> {
        send_user_message(&self.context, Message::SetDockVisibility(visible))
    }

    fn request_exit(&self, code: i32) -> Result<()> {
        // NOTE: request_exit cannot use the `send_user_message` function because it accesses the event loop callback
        self.context
            .proxy
            .send_event(Message::RequestExit(code))
            .map_err(|_| Error::FailedToSendMessage)
    }

    // Creates a window by dispatching a message to the event loop.
    // Note that this must be called from a separate thread, otherwise the channel will introduce a deadlock.
    fn create_window<F: Fn(RawWindow) + Send + 'static>(
        &self,
        pending: PendingWindow<T, Self::Runtime>,
        after_window_creation: Option<F>,
    ) -> Result<DetachedWindow<T, Self::Runtime>> {
        self.context.create_window(pending, after_window_creation)
    }

    // Creates a webview by dispatching a message to the event loop.
    // Note that this must be called from a separate thread, otherwise the channel will introduce a deadlock.
    fn create_webview(
        &self,
        window_id: WindowId,
        pending: PendingWebview<T, Self::Runtime>,
    ) -> Result<DetachedWebview<T, Self::Runtime>> {
        self.context.create_webview(window_id, pending)
    }

    fn run_on_main_thread<F: FnOnce() + Send + 'static>(&self, f: F) -> Result<()> {
        send_user_message(&self.context, Message::Task(Box::new(f)))
    }

    fn display_handle(
        &self,
    ) -> std::result::Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
        self.context.main_thread.window_target.display_handle()
    }

    fn primary_monitor(&self) -> Option<Monitor> {
        self.context
            .main_thread
            .window_target
            .primary_monitor()
            .map(|m| MonitorHandleWrapper(m).into())
    }

    fn monitor_from_point(&self, x: f64, y: f64) -> Option<Monitor> {
        self.context
            .main_thread
            .window_target
            .monitor_from_point(x, y)
            .map(|m| MonitorHandleWrapper(m).into())
    }

    fn available_monitors(&self) -> Vec<Monitor> {
        self.context
            .main_thread
            .window_target
            .available_monitors()
            .map(|m| MonitorHandleWrapper(m).into())
            .collect()
    }

    fn cursor_position(&self) -> Result<PhysicalPosition<f64>> {
        event_loop_window_getter!(self, EventLoopWindowTargetMessage::CursorPosition)?
            .map(PhysicalPositionWrapper)
            .map(Into::into)
            .map_err(|_| Error::FailedToGetCursorPosition)
    }

    fn set_theme(&self, theme: Option<Theme>) {
        let _ = send_user_message(
            &self.context,
            Message::EventLoopWindowTarget(EventLoopWindowTargetMessage::SetTheme(theme)),
        );
    }

    #[cfg(target_os = "macos")]
    fn show(&self) -> tauri_runtime::Result<()> {
        send_user_message(
            &self.context,
            Message::Application(ApplicationMessage::Show),
        )
    }

    #[cfg(target_os = "macos")]
    fn hide(&self) -> tauri_runtime::Result<()> {
        send_user_message(
            &self.context,
            Message::Application(ApplicationMessage::Hide),
        )
    }

    fn set_device_event_filter(&self, filter: DeviceEventFilter) {
        let _ = send_user_message(
            &self.context,
            Message::EventLoopWindowTarget(EventLoopWindowTargetMessage::SetDeviceEventFilter(
                filter,
            )),
        );
    }

    #[cfg(target_os = "android")]
    fn find_class<'a>(
        &self,
        env: &mut jni::JNIEnv<'a>,
        activity: &jni::objects::JObject<'_>,
        name: impl Into<String>,
    ) -> std::result::Result<jni::objects::JClass<'a>, jni::errors::Error> {
        find_class(env, activity, name.into())
    }

    #[cfg(target_os = "android")]
    fn run_on_android_context<F>(&self, f: F)
    where
        F: FnOnce(&mut jni::JNIEnv, &jni::objects::JObject, &jni::objects::JObject)
            + Send
            + 'static,
    {
        dispatch(f)
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn fetch_data_store_identifiers<F: FnOnce(Vec<[u8; 16]>) + Send + 'static>(
        &self,
        cb: F,
    ) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Application(ApplicationMessage::FetchDataStoreIdentifiers(Box::new(cb))),
        )
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn remove_data_store<F: FnOnce(Result<()>) + Send + 'static>(
        &self,
        uuid: [u8; 16],
        cb: F,
    ) -> Result<()> {
        send_user_message(
            &self.context,
            Message::Application(ApplicationMessage::RemoveDataStore(uuid, Box::new(cb))),
        )
    }
}

impl<T: UserEvent> Wry<T> {
    fn init_with_builder(
        mut event_loop_builder: EventLoopBuilder<Message<T>>,
        #[allow(unused_variables)] args: RuntimeInitArgs,
    ) -> Result<Self> {
        #[cfg(windows)]
        if let Some(hook) = args.msg_hook {
            use tao::platform::windows::EventLoopBuilderExtWindows;
            event_loop_builder.with_msg_hook(hook);
        }

        #[cfg(any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        ))]
        if let Some(app_id) = args.app_id {
            use tao::platform::unix::EventLoopBuilderExtUnix;
            event_loop_builder.with_app_id(app_id);
        }
        Self::init(event_loop_builder.build())
    }

    fn init(event_loop: EventLoop<Message<T>>) -> Result<Self> {
        let main_thread_id = current_thread().id();
        let web_context = WebContextStore::default();

        let windows = Arc::new(WindowsStore(RefCell::new(BTreeMap::default())));
        let window_id_map = WindowIdStore::default();

        let context = Context {
            window_id_map,
            main_thread_id,
            proxy: event_loop.create_proxy(),
            main_thread: DispatcherMainThreadContext {
                window_target: event_loop.deref().clone(),
                web_context,
                windows,
                #[cfg(feature = "tracing")]
                active_tracing_spans: Default::default(),
            },
            plugins: Default::default(),
            next_window_id: Default::default(),
            next_webview_id: Default::default(),
            next_window_event_id: Default::default(),
            next_webview_event_id: Default::default(),
            webview_runtime_installed: true,
        };

        Ok(Self {
            context,
            event_loop,
        })
    }
}

impl<T: UserEvent> Runtime<T> for Wry<T> {
    type WindowDispatcher = WryWindowDispatcher<T>;
    type WebviewDispatcher = WryWebviewDispatcher<T>;
    type Handle = WryHandle<T>;

    type EventLoopProxy = EventProxy<T>;

    fn new(args: RuntimeInitArgs) -> Result<Self> {
        Self::init_with_builder(EventLoopBuilder::<Message<T>>::with_user_event(), args)
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    fn new_any_thread(args: RuntimeInitArgs) -> Result<Self> {
        use tao::platform::unix::EventLoopBuilderExtUnix;
        let mut event_loop_builder = EventLoopBuilder::<Message<T>>::with_user_event();
        event_loop_builder.with_any_thread(true);
        Self::init_with_builder(event_loop_builder, args)
    }

    #[cfg(windows)]
    fn new_any_thread(args: RuntimeInitArgs) -> Result<Self> {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        let mut event_loop_builder = EventLoopBuilder::<Message<T>>::with_user_event();
        event_loop_builder.with_any_thread(true);
        Self::init_with_builder(event_loop_builder, args)
    }

    fn create_proxy(&self) -> EventProxy<T> {
        EventProxy(self.event_loop.create_proxy())
    }

    fn handle(&self) -> Self::Handle {
        WryHandle {
            context: self.context.clone(),
        }
    }

    fn create_window<F: Fn(RawWindow) + Send + 'static>(
        &self,
        pending: PendingWindow<T, Self>,
        after_window_creation: Option<F>,
    ) -> Result<DetachedWindow<T, Self>> {
        let label = pending.label.clone();
        let window_id = self.context.next_window_id();
        let (webview_id, use_https_scheme) = pending
            .webview
            .as_ref()
            .map(|w| {
                (
                    Some(self.context.next_webview_id()),
                    w.webview_attributes.use_https_scheme,
                )
            })
            .unwrap_or((None, false));

        let window = create_window(
            window_id,
            webview_id.unwrap_or_default(),
            &self.event_loop,
            &self.context,
            pending,
            after_window_creation,
        )?;

        let dispatcher = WryWindowDispatcher {
            window_id,
            context: self.context.clone(),
        };

        self.context
            .main_thread
            .windows
            .0
            .borrow_mut()
            .insert(window_id, window);

        let detached_webview = webview_id.map(|id| {
            let webview = DetachedWebview {
                label: label.clone(),
                dispatcher: WryWebviewDispatcher {
                    window_id: Arc::new(Mutex::new(window_id)),
                    webview_id: id,
                    context: self.context.clone(),
                },
            };
            DetachedWindowWebview {
                webview,
                use_https_scheme,
            }
        });

        Ok(DetachedWindow {
            id: window_id,
            label,
            dispatcher,
            webview: detached_webview,
        })
    }

    fn create_webview(
        &self,
        window_id: WindowId,
        pending: PendingWebview<T, Self>,
    ) -> Result<DetachedWebview<T, Self>> {
        let label = pending.label.clone();

        let window = self
            .context
            .main_thread
            .windows
            .0
            .borrow()
            .get(&window_id)
            .map(|w| (w.inner.clone(), w.focused_webview.clone()));
        if let Some((Some(window), focused_webview)) = window {
            let window_id_wrapper = Arc::new(Mutex::new(window_id));

            let webview_id = self.context.next_webview_id();

            let webview = create_webview(
                WebviewKind::WindowChild,
                &window,
                window_id_wrapper.clone(),
                webview_id,
                &self.context,
                pending,
                focused_webview,
            )?;

            #[allow(unknown_lints, clippy::manual_inspect)]
            self.context
                .main_thread
                .windows
                .0
                .borrow_mut()
                .get_mut(&window_id)
                .map(|w| {
                    w.webviews.push(webview);
                    w.has_children.store(true, Ordering::Relaxed);
                    w
                });

            let dispatcher = WryWebviewDispatcher {
                window_id: window_id_wrapper,
                webview_id,
                context: self.context.clone(),
            };

            Ok(DetachedWebview { label, dispatcher })
        } else {
            Err(Error::WindowNotFound)
        }
    }

    fn primary_monitor(&self) -> Option<Monitor> {
        self.context
            .main_thread
            .window_target
            .primary_monitor()
            .map(|m| MonitorHandleWrapper(m).into())
    }

    fn monitor_from_point(&self, x: f64, y: f64) -> Option<Monitor> {
        self.context
            .main_thread
            .window_target
            .monitor_from_point(x, y)
            .map(|m| MonitorHandleWrapper(m).into())
    }

    fn available_monitors(&self) -> Vec<Monitor> {
        self.context
            .main_thread
            .window_target
            .available_monitors()
            .map(|m| MonitorHandleWrapper(m).into())
            .collect()
    }

    fn cursor_position(&self) -> Result<PhysicalPosition<f64>> {
        self.context
            .main_thread
            .window_target
            .cursor_position()
            .map(PhysicalPositionWrapper)
            .map(Into::into)
            .map_err(|_| Error::FailedToGetCursorPosition)
    }

    fn set_theme(&self, theme: Option<Theme>) {
        self.event_loop.set_theme(to_tao_theme(theme));
    }

    #[cfg(target_os = "macos")]
    fn set_activation_policy(&mut self, activation_policy: ActivationPolicy) {
        self.event_loop
            .set_activation_policy(tao_activation_policy(activation_policy));
    }

    #[cfg(target_os = "macos")]
    fn set_dock_visibility(&mut self, visible: bool) {
        self.event_loop.set_dock_visibility(visible);
    }

    #[cfg(target_os = "macos")]
    fn show(&self) {
        self.event_loop.show_application();
    }

    #[cfg(target_os = "macos")]
    fn hide(&self) {
        self.event_loop.hide_application();
    }

    fn set_device_event_filter(&mut self, filter: DeviceEventFilter) {
        self.event_loop
            .set_device_event_filter(DeviceEventFilterWrapper::from(filter).0);
    }

    fn run_iteration<F: FnMut(RunEvent<T>) + 'static>(&mut self, mut callback: F) {
        use tao::platform::run_return::EventLoopExtRunReturn;
        let windows = self.context.main_thread.windows.clone();
        let window_id_map = self.context.window_id_map.clone();
        let web_context = &self.context.main_thread.web_context;
        let plugins = self.context.plugins.clone();

        #[cfg(feature = "tracing")]
        let active_tracing_spans = self.context.main_thread.active_tracing_spans.clone();

        let proxy = self.event_loop.create_proxy();

        self.event_loop
            .run_return(|event, event_loop, control_flow| {
                *control_flow = ControlFlow::Wait;
                if let Event::MainEventsCleared = &event {
                    *control_flow = ControlFlow::Exit;
                }

                for p in plugins.lock().unwrap().iter_mut() {
                    let prevent_default = p.on_event(
                        &event,
                        event_loop,
                        &proxy,
                        control_flow,
                        EventLoopIterationContext {
                            callback: &mut callback,
                            window_id_map: window_id_map.clone(),
                            windows: windows.clone(),
                            #[cfg(feature = "tracing")]
                            active_tracing_spans: active_tracing_spans.clone(),
                        },
                        web_context,
                    );
                    if prevent_default {
                        return;
                    }
                }

                handle_event_loop(
                    event,
                    event_loop,
                    control_flow,
                    EventLoopIterationContext {
                        callback: &mut callback,
                        windows: windows.clone(),
                        window_id_map: window_id_map.clone(),
                        #[cfg(feature = "tracing")]
                        active_tracing_spans: active_tracing_spans.clone(),
                    },
                );
            });
    }

    fn run<F: FnMut(RunEvent<T>) + 'static>(self, callback: F) {
        let event_handler = make_event_handler(&self, callback);

        self.event_loop.run(event_handler)
    }

    #[cfg(not(target_os = "ios"))]
    fn run_return<F: FnMut(RunEvent<T>) + 'static>(mut self, callback: F) -> i32 {
        use tao::platform::run_return::EventLoopExtRunReturn;

        let event_handler = make_event_handler(&self, callback);

        self.event_loop.run_return(event_handler)
    }

    #[cfg(target_os = "ios")]
    fn run_return<F: FnMut(RunEvent<T>) + 'static>(self, callback: F) -> i32 {
        self.run(callback);
        0
    }
}

fn make_event_handler<T, F>(
    runtime: &Wry<T>,
    mut callback: F,
) -> impl FnMut(Event<'_, Message<T>>, &EventLoopWindowTarget<Message<T>>, &mut ControlFlow)
where
    T: UserEvent,
    F: FnMut(RunEvent<T>) + 'static,
{
    let windows = runtime.context.main_thread.windows.clone();
    let window_id_map = runtime.context.window_id_map.clone();
    let web_context = runtime.context.main_thread.web_context.clone();
    let plugins = runtime.context.plugins.clone();

    #[cfg(feature = "tracing")]
    let active_tracing_spans = runtime.context.main_thread.active_tracing_spans.clone();
    let proxy = runtime.event_loop.create_proxy();

    move |event, event_loop, control_flow| {
        for p in plugins.lock().unwrap().iter_mut() {
            let prevent_default = p.on_event(
                &event,
                event_loop,
                &proxy,
                control_flow,
                EventLoopIterationContext {
                    callback: &mut callback,
                    window_id_map: window_id_map.clone(),
                    windows: windows.clone(),
                    #[cfg(feature = "tracing")]
                    active_tracing_spans: active_tracing_spans.clone(),
                },
                &web_context,
            );
            if prevent_default {
                return;
            }
        }
        handle_event_loop(
            event,
            event_loop,
            control_flow,
            EventLoopIterationContext {
                callback: &mut callback,
                window_id_map: window_id_map.clone(),
                windows: windows.clone(),
                #[cfg(feature = "tracing")]
                active_tracing_spans: active_tracing_spans.clone(),
            },
        );
    }
}

pub struct EventLoopIterationContext<'a, T: UserEvent> {
    pub callback: &'a mut (dyn FnMut(RunEvent<T>) + 'static),
    pub window_id_map: WindowIdStore,
    pub windows: Arc<WindowsStore>,
    #[cfg(feature = "tracing")]
    pub active_tracing_spans: ActiveTraceSpanStore,
}

struct UserMessageContext {
    windows: Arc<WindowsStore>,
    window_id_map: WindowIdStore,
}

fn handle_user_message<T: UserEvent>(
    event_loop: &EventLoopWindowTarget<Message<T>>,
    message: Message<T>,
    context: UserMessageContext,
) {
    let UserMessageContext {
        window_id_map,
        windows,
    } = context;
    match message {
        Message::Task(task) => task(),
        #[cfg(target_os = "macos")]
        Message::SetActivationPolicy(activation_policy) => {
            event_loop.set_activation_policy_at_runtime(tao_activation_policy(activation_policy))
        }
        #[cfg(target_os = "macos")]
        Message::SetDockVisibility(visible) => event_loop.set_dock_visibility(visible),
        Message::RequestExit(_code) => panic!("cannot handle RequestExit on the main thread"),
        Message::Application(application_message) => match application_message {
            #[cfg(target_os = "macos")]
            ApplicationMessage::Show => {
                event_loop.show_application();
            }
            #[cfg(target_os = "macos")]
            ApplicationMessage::Hide => {
                event_loop.hide_application();
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            ApplicationMessage::FetchDataStoreIdentifiers(cb) => {
                if let Err(e) = WebView::fetch_data_store_identifiers(cb) {
                    // this shouldn't ever happen because we're running on the main thread
                    // but let's be safe and warn here
                    log::error!("failed to fetch data store identifiers: {e}");
                }
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            ApplicationMessage::RemoveDataStore(uuid, cb) => {
                WebView::remove_data_store(&uuid, move |res| {
                    cb(res.map_err(|_| Error::FailedToRemoveDataStore))
                })
            }
        },
        Message::Window(id, window_message) => {
            let w = windows.0.borrow().get(&id).map(|w| {
                (
                    w.inner.clone(),
                    w.webviews.clone(),
                    w.has_children.load(Ordering::Relaxed),
                    w.window_event_listeners.clone(),
                )
            });
            if let Some((Some(window), webviews, has_children, window_event_listeners)) = w {
                match window_message {
                    WindowMessage::AddEventListener(id, listener) => {
                        window_event_listeners.lock().unwrap().insert(id, listener);
                    }

                    // Getters
                    WindowMessage::ScaleFactor(tx) => tx.send(window.scale_factor()).unwrap(),
                    WindowMessage::InnerPosition(tx) => tx
                        .send(
                            window
                                .inner_position()
                                .map(|p| PhysicalPositionWrapper(p).into())
                                .map_err(|_| Error::FailedToSendMessage),
                        )
                        .unwrap(),
                    WindowMessage::OuterPosition(tx) => tx
                        .send(
                            window
                                .outer_position()
                                .map(|p| PhysicalPositionWrapper(p).into())
                                .map_err(|_| Error::FailedToSendMessage),
                        )
                        .unwrap(),
                    WindowMessage::InnerSize(tx) => tx
                        .send(
                            PhysicalSizeWrapper(inner_size(&window, &webviews, has_children))
                                .into(),
                        )
                        .unwrap(),
                    WindowMessage::OuterSize(tx) => tx
                        .send(PhysicalSizeWrapper(window.outer_size()).into())
                        .unwrap(),
                    WindowMessage::IsFullscreen(tx) => {
                        tx.send(window.fullscreen().is_some()).unwrap()
                    }
                    WindowMessage::IsMinimized(tx) => tx.send(window.is_minimized()).unwrap(),
                    WindowMessage::IsMaximized(tx) => tx.send(window.is_maximized()).unwrap(),
                    WindowMessage::IsFocused(tx) => tx.send(window.is_focused()).unwrap(),
                    WindowMessage::IsDecorated(tx) => tx.send(window.is_decorated()).unwrap(),
                    WindowMessage::IsResizable(tx) => tx.send(window.is_resizable()).unwrap(),
                    WindowMessage::IsMaximizable(tx) => tx.send(window.is_maximizable()).unwrap(),
                    WindowMessage::IsMinimizable(tx) => tx.send(window.is_minimizable()).unwrap(),
                    WindowMessage::IsClosable(tx) => tx.send(window.is_closable()).unwrap(),
                    WindowMessage::IsVisible(tx) => tx.send(window.is_visible()).unwrap(),
                    WindowMessage::Title(tx) => tx.send(window.title()).unwrap(),
                    WindowMessage::CurrentMonitor(tx) => tx.send(window.current_monitor()).unwrap(),
                    WindowMessage::PrimaryMonitor(tx) => tx.send(window.primary_monitor()).unwrap(),
                    WindowMessage::MonitorFromPoint(tx, (x, y)) => {
                        tx.send(window.monitor_from_point(x, y)).unwrap()
                    }
                    WindowMessage::AvailableMonitors(tx) => {
                        tx.send(window.available_monitors().collect()).unwrap()
                    }
                    #[cfg(any(
                        target_os = "linux",
                        target_os = "dragonfly",
                        target_os = "freebsd",
                        target_os = "netbsd",
                        target_os = "openbsd"
                    ))]
                    WindowMessage::GtkWindow(tx) => {
                        tx.send(GtkWindow(window.gtk_window().clone())).unwrap()
                    }
                    #[cfg(any(
                        target_os = "linux",
                        target_os = "dragonfly",
                        target_os = "freebsd",
                        target_os = "netbsd",
                        target_os = "openbsd"
                    ))]
                    WindowMessage::GtkBox(tx) => tx
                        .send(GtkBox(window.default_vbox().unwrap().clone()))
                        .unwrap(),
                    WindowMessage::RawWindowHandle(tx) => tx
                        .send(
                            window
                                .window_handle()
                                .map(|h| SendRawWindowHandle(h.as_raw())),
                        )
                        .unwrap(),
                    WindowMessage::Theme(tx) => {
                        tx.send(map_theme(&window.theme())).unwrap();
                    }
                    WindowMessage::IsEnabled(tx) => tx.send(window.is_enabled()).unwrap(),
                    WindowMessage::IsAlwaysOnTop(tx) => tx.send(window.is_always_on_top()).unwrap(),
                    // Setters
                    WindowMessage::Center => window.center(),
                    WindowMessage::RequestUserAttention(request_type) => {
                        window.request_user_attention(request_type.map(|r| r.0));
                    }
                    WindowMessage::SetResizable(resizable) => {
                        window.set_resizable(resizable);
                        #[cfg(windows)]
                        if !resizable {
                            undecorated_resizing::detach_resize_handler(window.hwnd());
                        } else if !window.is_decorated() {
                            undecorated_resizing::attach_resize_handler(
                                window.hwnd(),
                                window.has_undecorated_shadow(),
                            );
                        }
                    }
                    WindowMessage::SetMaximizable(maximizable) => {
                        window.set_maximizable(maximizable)
                    }
                    WindowMessage::SetMinimizable(minimizable) => {
                        window.set_minimizable(minimizable)
                    }
                    WindowMessage::SetClosable(closable) => window.set_closable(closable),
                    WindowMessage::SetTitle(title) => window.set_title(&title),
                    WindowMessage::Maximize => window.set_maximized(true),
                    WindowMessage::Unmaximize => window.set_maximized(false),
                    WindowMessage::Minimize => window.set_minimized(true),
                    WindowMessage::Unminimize => window.set_minimized(false),
                    WindowMessage::SetEnabled(enabled) => window.set_enabled(enabled),
                    WindowMessage::Show => window.set_visible(true),
                    WindowMessage::Hide => window.set_visible(false),
                    WindowMessage::Close => {
                        panic!("cannot handle `WindowMessage::Close` on the main thread")
                    }
                    WindowMessage::Destroy => {
                        panic!("cannot handle `WindowMessage::Destroy` on the main thread")
                    }
                    WindowMessage::SetDecorations(decorations) => {
                        window.set_decorations(decorations);
                        #[cfg(windows)]
                        if decorations {
                            undecorated_resizing::detach_resize_handler(window.hwnd());
                        } else if window.is_resizable() {
                            undecorated_resizing::attach_resize_handler(
                                window.hwnd(),
                                window.has_undecorated_shadow(),
                            );
                        }
                    }
                    WindowMessage::SetShadow(_enable) => {
                        #[cfg(windows)]
                        {
                            window.set_undecorated_shadow(_enable);
                            undecorated_resizing::update_drag_hwnd_rgn_for_undecorated(
                                window.hwnd(),
                                _enable,
                            );
                        }
                        #[cfg(target_os = "macos")]
                        window.set_has_shadow(_enable);
                    }
                    WindowMessage::SetAlwaysOnBottom(always_on_bottom) => {
                        window.set_always_on_bottom(always_on_bottom)
                    }
                    WindowMessage::SetAlwaysOnTop(always_on_top) => {
                        window.set_always_on_top(always_on_top)
                    }
                    WindowMessage::SetVisibleOnAllWorkspaces(visible_on_all_workspaces) => {
                        window.set_visible_on_all_workspaces(visible_on_all_workspaces)
                    }
                    WindowMessage::SetContentProtected(protected) => {
                        window.set_content_protection(protected)
                    }
                    WindowMessage::SetSize(size) => {
                        window.set_inner_size(SizeWrapper::from(size).0);
                    }
                    WindowMessage::SetMinSize(size) => {
                        window.set_min_inner_size(size.map(|s| SizeWrapper::from(s).0));
                    }
                    WindowMessage::SetMaxSize(size) => {
                        window.set_max_inner_size(size.map(|s| SizeWrapper::from(s).0));
                    }
                    WindowMessage::SetSizeConstraints(constraints) => {
                        window.set_inner_size_constraints(tao::window::WindowSizeConstraints {
                            min_width: constraints.min_width,
                            min_height: constraints.min_height,
                            max_width: constraints.max_width,
                            max_height: constraints.max_height,
                        });
                    }
                    WindowMessage::SetPosition(position) => {
                        window.set_outer_position(PositionWrapper::from(position).0)
                    }
                    WindowMessage::SetFullscreen(fullscreen) => {
                        if fullscreen {
                            window.set_fullscreen(Some(Fullscreen::Borderless(None)))
                        } else {
                            window.set_fullscreen(None)
                        }
                    }

                    #[cfg(target_os = "macos")]
                    WindowMessage::SetSimpleFullscreen(enable) => {
                        window.set_simple_fullscreen(enable);
                    }

                    WindowMessage::SetFocus => {
                        window.set_focus();
                    }
                    WindowMessage::SetFocusable(focusable) => {
                        window.set_focusable(focusable);
                    }
                    WindowMessage::SetIcon(icon) => {
                        window.set_window_icon(Some(icon));
                    }
                    #[allow(unused_variables)]
                    WindowMessage::SetSkipTaskbar(skip) => {
                        #[cfg(any(
                            windows,
                            target_os = "linux",
                            target_os = "dragonfly",
                            target_os = "freebsd",
                            target_os = "netbsd",
                            target_os = "openbsd"
                        ))]
                        let _ = window.set_skip_taskbar(skip);
                    }
                    WindowMessage::SetCursorGrab(grab) => {
                        let _ = window.set_cursor_grab(grab);
                    }
                    WindowMessage::SetCursorVisible(visible) => {
                        window.set_cursor_visible(visible);
                    }
                    WindowMessage::SetCursorIcon(icon) => {
                        window.set_cursor_icon(CursorIconWrapper::from(icon).0);
                    }
                    WindowMessage::SetCursorPosition(position) => {
                        let _ = window.set_cursor_position(PositionWrapper::from(position).0);
                    }
                    WindowMessage::SetIgnoreCursorEvents(ignore) => {
                        let _ = window.set_ignore_cursor_events(ignore);
                    }
                    WindowMessage::DragWindow => {
                        let _ = window.drag_window();
                    }
                    WindowMessage::ResizeDragWindow(direction) => {
                        let _ = window.drag_resize_window(match direction {
                            tauri_runtime::ResizeDirection::East => {
                                tao::window::ResizeDirection::East
                            }
                            tauri_runtime::ResizeDirection::North => {
                                tao::window::ResizeDirection::North
                            }
                            tauri_runtime::ResizeDirection::NorthEast => {
                                tao::window::ResizeDirection::NorthEast
                            }
                            tauri_runtime::ResizeDirection::NorthWest => {
                                tao::window::ResizeDirection::NorthWest
                            }
                            tauri_runtime::ResizeDirection::South => {
                                tao::window::ResizeDirection::South
                            }
                            tauri_runtime::ResizeDirection::SouthEast => {
                                tao::window::ResizeDirection::SouthEast
                            }
                            tauri_runtime::ResizeDirection::SouthWest => {
                                tao::window::ResizeDirection::SouthWest
                            }
                            tauri_runtime::ResizeDirection::West => {
                                tao::window::ResizeDirection::West
                            }
                        });
                    }
                    WindowMessage::RequestRedraw => {
                        window.request_redraw();
                    }
                    WindowMessage::SetBadgeCount(_count, _desktop_filename) => {
                        #[cfg(target_os = "ios")]
                        window.set_badge_count(
                            _count.map_or(0, |x| x.clamp(i32::MIN as i64, i32::MAX as i64) as i32),
                        );

                        #[cfg(target_os = "macos")]
                        window.set_badge_label(_count.map(|x| x.to_string()));

                        #[cfg(any(
                            target_os = "linux",
                            target_os = "dragonfly",
                            target_os = "freebsd",
                            target_os = "netbsd",
                            target_os = "openbsd"
                        ))]
                        window.set_badge_count(_count, _desktop_filename);
                    }
                    WindowMessage::SetBadgeLabel(_label) => {
                        #[cfg(target_os = "macos")]
                        window.set_badge_label(_label);
                    }
                    WindowMessage::SetOverlayIcon(_icon) => {
                        #[cfg(windows)]
                        window.set_overlay_icon(_icon.map(|x| x.0).as_ref());
                    }
                    WindowMessage::SetProgressBar(progress_state) => {
                        window.set_progress_bar(ProgressBarStateWrapper::from(progress_state).0);
                    }
                    WindowMessage::SetTitleBarStyle(_style) => {
                        #[cfg(target_os = "macos")]
                        match _style {
                            TitleBarStyle::Visible => {
                                window.set_titlebar_transparent(false);
                                window.set_fullsize_content_view(true);
                            }
                            TitleBarStyle::Transparent => {
                                window.set_titlebar_transparent(true);
                                window.set_fullsize_content_view(false);
                            }
                            TitleBarStyle::Overlay => {
                                window.set_titlebar_transparent(true);
                                window.set_fullsize_content_view(true);
                            }
                            unknown => {
                                #[cfg(feature = "tracing")]
                                tracing::warn!("unknown title bar style applied: {unknown}");

                                #[cfg(not(feature = "tracing"))]
                                eprintln!("unknown title bar style applied: {unknown}");
                            }
                        };
                    }
                    WindowMessage::SetTrafficLightPosition(_position) => {
                        #[cfg(target_os = "macos")]
                        window.set_traffic_light_inset(_position);
                    }
                    WindowMessage::SetTheme(theme) => {
                        window.set_theme(to_tao_theme(theme));
                    }
                    WindowMessage::SetBackgroundColor(color) => {
                        window.set_background_color(color.map(Into::into))
                    }
                }
            }
        }
        Message::Webview(window_id, webview_id, webview_message) => {
            #[cfg(any(
                target_os = "macos",
                windows,
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ))]
            if let WebviewMessage::Reparent(new_parent_window_id, tx) = webview_message {
                let webview_handle = windows.0.borrow_mut().get_mut(&window_id).and_then(|w| {
                    w.webviews
                        .iter()
                        .position(|w| w.id == webview_id)
                        .map(|webview_index| w.webviews.remove(webview_index))
                });

                if let Some(webview) = webview_handle {
                    if let Some(new_parent_window_webviews) = windows
                        .0
                        .borrow_mut()
                        .get_mut(&new_parent_window_id)
                        .map(|w| &mut w.webviews)
                    {
                        new_parent_window_webviews.push(webview);
                        tx.send(Ok(())).unwrap();
                    } else {
                        tx.send(Err(Error::FailedToSendMessage)).unwrap();
                    }
                } else {
                    tx.send(Err(Error::FailedToSendMessage)).unwrap();
                }

                return;
            }

            let webview_handle = windows.0.borrow().get(&window_id).map(|w| {
                (
                    w.inner.clone(),
                    w.webviews.iter().find(|w| w.id == webview_id).cloned(),
                )
            });
            if let Some((Some(window), Some(webview))) = webview_handle {
                match webview_message {
                    WebviewMessage::WebviewEvent(_) => { /* already handled */ }
                    WebviewMessage::SynthesizedWindowEvent(_) => { /* already handled */ }
                    WebviewMessage::Reparent(_window_id, _tx) => { /* already handled */ }
                    WebviewMessage::AddEventListener(id, listener) => {
                        webview
                            .webview_event_listeners
                            .lock()
                            .unwrap()
                            .insert(id, listener);
                    }

                    #[cfg(all(feature = "tracing", not(target_os = "android")))]
                    WebviewMessage::EvaluateScript(script, tx, span) => {
                        let _span = span.entered();
                        if let Err(e) = webview.evaluate_script(&script) {
                            log::error!("{e}");
                        }
                        tx.send(()).unwrap();
                    }
                    #[cfg(not(all(feature = "tracing", not(target_os = "android"))))]
                    WebviewMessage::EvaluateScript(script) => {
                        if let Err(e) = webview.evaluate_script(&script) {
                            log::error!("{e}");
                        }
                    }
                    WebviewMessage::Navigate(url) => {
                        if let Err(e) = webview.load_url(url.as_str()) {
                            log::error!("failed to navigate to url {}: {}", url, e);
                        }
                    }
                    WebviewMessage::Reload => {
                        if let Err(e) = webview.reload() {
                            log::error!("failed to reload: {e}");
                        }
                    }
                    WebviewMessage::Show => {
                        if let Err(e) = webview.set_visible(true) {
                            log::error!("failed to change webview visibility: {e}");
                        }
                    }
                    WebviewMessage::Hide => {
                        if let Err(e) = webview.set_visible(false) {
                            log::error!("failed to change webview visibility: {e}");
                        }
                    }
                    WebviewMessage::Print => {
                        let _ = webview.print();
                    }
                    WebviewMessage::Close => {
                        #[allow(unknown_lints, clippy::manual_inspect)]
                        windows.0.borrow_mut().get_mut(&window_id).map(|window| {
                            if let Some(i) = window.webviews.iter().position(|w| w.id == webview.id)
                            {
                                window.webviews.remove(i);
                            }
                            window
                        });
                    }
                    WebviewMessage::SetBounds(bounds) => {
                        let bounds: RectWrapper = bounds.into();
                        let bounds = bounds.0;

                        if let Some(b) = &mut *webview.bounds.lock().unwrap() {
                            let scale_factor = window.scale_factor();
                            let size = bounds.size.to_logical::<f32>(scale_factor);
                            let position = bounds.position.to_logical::<f32>(scale_factor);
                            let window_size = window.inner_size().to_logical::<f32>(scale_factor);
                            b.width_rate = size.width / window_size.width;
                            b.height_rate = size.height / window_size.height;
                            b.x_rate = position.x / window_size.width;
                            b.y_rate = position.y / window_size.height;
                        }

                        if let Err(e) = webview.set_bounds(bounds, window.as_ref()) {
                            log::error!("failed to set webview size: {e}");
                        }
                    }
                    WebviewMessage::SetSize(size) => match webview.bounds() {
                        Ok(mut bounds) => {
                            bounds.size = size;

                            let scale_factor = window.scale_factor();
                            let size = size.to_logical::<f32>(scale_factor);

                            if let Some(b) = &mut *webview.bounds.lock().unwrap() {
                                let window_size =
                                    window.inner_size().to_logical::<f32>(scale_factor);
                                b.width_rate = size.width / window_size.width;
                                b.height_rate = size.height / window_size.height;
                            }

                            if let Err(e) = webview.set_bounds(bounds, window.as_ref()) {
                                log::error!("failed to set webview size: {e}");
                            }
                        }
                        Err(e) => {
                            log::error!("failed to get webview bounds: {e}");
                        }
                    },
                    WebviewMessage::SetPosition(position) => match webview.bounds() {
                        Ok(mut bounds) => {
                            bounds.position = position;

                            let scale_factor = window.scale_factor();
                            let position = position.to_logical::<f32>(scale_factor);

                            if let Some(b) = &mut *webview.bounds.lock().unwrap() {
                                let window_size =
                                    window.inner_size().to_logical::<f32>(scale_factor);
                                b.x_rate = position.x / window_size.width;
                                b.y_rate = position.y / window_size.height;
                            }

                            if let Err(e) = webview.set_bounds(bounds, window.as_ref()) {
                                log::error!("failed to set webview position: {e}");
                            }
                        }
                        Err(e) => {
                            log::error!("failed to get webview bounds: {e}");
                        }
                    },
                    WebviewMessage::SetZoom(scale_factor) => {
                        if let Err(e) = webview.zoom(scale_factor) {
                            log::error!("failed to set webview zoom: {e}");
                        }
                    }
                    WebviewMessage::SetBackgroundColor(color) => {
                        if let Err(e) = webview.set_background_color(
                            color.map(Into::into).unwrap_or((255, 255, 255, 255)),
                        ) {
                            log::error!("failed to set webview background color: {e}");
                        }
                    }
                    WebviewMessage::ClearAllBrowsingData => {
                        if let Err(e) = webview.clear_all_browsing_data() {
                            log::error!("failed to clear webview browsing data: {e}");
                        }
                    }
                    // Getters
                    WebviewMessage::Url(tx) => {
                        tx.send(
                            webview
                                .url()
                                .map(|u| u.parse().expect("invalid webview URL"))
                                .map_err(|_| Error::FailedToSendMessage),
                        )
                        .unwrap();
                    }

                    WebviewMessage::Cookies(tx) => {
                        tx.send(webview.cookies().map_err(|_| Error::FailedToSendMessage))
                            .unwrap();
                    }

                    WebviewMessage::SetCookie(cookie) => {
                        if let Err(e) = webview.set_cookie(&cookie) {
                            log::error!("failed to set webview cookie: {e}");
                        }
                    }

                    WebviewMessage::DeleteCookie(cookie) => {
                        if let Err(e) = webview.delete_cookie(&cookie) {
                            log::error!("failed to delete webview cookie: {e}");
                        }
                    }

                    WebviewMessage::CookiesForUrl(url, tx) => {
                        let webview_cookies = webview
                            .cookies_for_url(url.as_str())
                            .map_err(|_| Error::FailedToSendMessage);
                        tx.send(webview_cookies).unwrap();
                    }

                    WebviewMessage::Bounds(tx) => {
                        tx.send(
                            webview
                                .bounds()
                                .map(|bounds| tauri_runtime::dpi::Rect {
                                    size: bounds.size,
                                    position: bounds.position,
                                })
                                .map_err(|_| Error::FailedToSendMessage),
                        )
                        .unwrap();
                    }
                    WebviewMessage::Position(tx) => {
                        tx.send(
                            webview
                                .bounds()
                                .map(|bounds| bounds.position.to_physical(window.scale_factor()))
                                .map_err(|_| Error::FailedToSendMessage),
                        )
                        .unwrap();
                    }
                    WebviewMessage::Size(tx) => {
                        tx.send(
                            webview
                                .bounds()
                                .map(|bounds| bounds.size.to_physical(window.scale_factor()))
                                .map_err(|_| Error::FailedToSendMessage),
                        )
                        .unwrap();
                    }
                    WebviewMessage::SetFocus => {
                        if let Err(e) = webview.focus() {
                            log::error!("failed to focus webview: {e}");
                        }
                    }
                    WebviewMessage::SetAutoResize(auto_resize) => match webview.bounds() {
                        Ok(bounds) => {
                            let scale_factor = window.scale_factor();
                            let window_size = window.inner_size().to_logical::<f32>(scale_factor);
                            *webview.bounds.lock().unwrap() = if auto_resize {
                                let size = bounds.size.to_logical::<f32>(scale_factor);
                                let position = bounds.position.to_logical::<f32>(scale_factor);
                                Some(WebviewBounds {
                                    x_rate: position.x / window_size.width,
                                    y_rate: position.y / window_size.height,
                                    width_rate: size.width / window_size.width,
                                    height_rate: size.height / window_size.height,
                                })
                            } else {
                                None
                            };
                        }
                        Err(e) => {
                            log::error!("failed to get webview bounds: {e}");
                        }
                    },
                    WebviewMessage::WithWebview(f) => {
                        f(webview.webview());
                    }
                    #[cfg(any(debug_assertions, feature = "devtools"))]
                    WebviewMessage::OpenDevTools => {
                        webview.open_devtools();
                    }
                    #[cfg(any(debug_assertions, feature = "devtools"))]
                    WebviewMessage::CloseDevTools => {
                        webview.close_devtools();
                    }
                    #[cfg(any(debug_assertions, feature = "devtools"))]
                    WebviewMessage::IsDevToolsOpen(tx) => {
                        tx.send(webview.is_devtools_open()).unwrap();
                    }
                }
            }
        }
        Message::CreateWebview(window_id, handler) => {
            let window = windows
                .0
                .borrow()
                .get(&window_id)
                .map(|w| (w.inner.clone(), w.focused_webview.clone()));
            if let Some((Some(window), focused_webview)) = window {
                match handler(&window, CreateWebviewOptions { focused_webview }) {
                    Ok(webview) => {
                        #[allow(unknown_lints, clippy::manual_inspect)]
                        windows.0.borrow_mut().get_mut(&window_id).map(|w| {
                            w.webviews.push(webview);
                            w.has_children.store(true, Ordering::Relaxed);
                            w
                        });
                    }
                    Err(e) => {
                        log::error!("{e}");
                    }
                }
            }
        }
        Message::CreateWindow(window_id, handler) => match handler(event_loop) {
            // wait for borrow_mut to be available - on Windows we might poll for the window to be inserted
            Ok(webview) => loop {
                if let Ok(mut windows) = windows.0.try_borrow_mut() {
                    windows.insert(window_id, webview);
                    break;
                }
            },
            Err(e) => {
                log::error!("{e}");
            }
        },
        Message::CreateRawWindow(window_id, handler, sender) => {
            let (label, builder) = handler();

            #[cfg(windows)]
            let background_color = builder.window.background_color;
            #[cfg(windows)]
            let is_window_transparent = builder.window.transparent;

            if let Ok(window) = builder.build(event_loop) {
                window_id_map.insert(window.id(), window_id);

                let window = Arc::new(window);

                #[cfg(windows)]
                let surface = if is_window_transparent {
                    if let Ok(context) = softbuffer::Context::new(window.clone()) {
                        if let Ok(mut surface) = softbuffer::Surface::new(&context, window.clone())
                        {
                            window.draw_surface(&mut surface, background_color);
                            Some(surface)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                windows.0.borrow_mut().insert(
                    window_id,
                    WindowWrapper {
                        label,
                        has_children: AtomicBool::new(false),
                        inner: Some(window.clone()),
                        window_event_listeners: Default::default(),
                        webviews: Vec::new(),
                        #[cfg(windows)]
                        background_color,
                        #[cfg(windows)]
                        is_window_transparent,
                        #[cfg(windows)]
                        surface,
                        focused_webview: Default::default(),
                        #[cfg(feature = "wayland-osr")]
                        osr_surface: None,
                    },
                );
                sender.send(Ok(Arc::downgrade(&window))).unwrap();
            } else {
                sender.send(Err(Error::CreateWindow)).unwrap();
            }
        }

        Message::UserEvent(_) => (),
        Message::EventLoopWindowTarget(message) => match message {
            EventLoopWindowTargetMessage::CursorPosition(sender) => {
                let pos = event_loop
                    .cursor_position()
                    .map_err(|_| Error::FailedToSendMessage);
                sender.send(pos).unwrap();
            }
            EventLoopWindowTargetMessage::SetTheme(theme) => {
                event_loop.set_theme(to_tao_theme(theme));
            }
            EventLoopWindowTargetMessage::SetDeviceEventFilter(filter) => {
                event_loop.set_device_event_filter(DeviceEventFilterWrapper::from(filter).0);
            }
        },
    }
}

fn handle_event_loop<T: UserEvent>(
    event: Event<'_, Message<T>>,
    event_loop: &EventLoopWindowTarget<Message<T>>,
    control_flow: &mut ControlFlow,
    context: EventLoopIterationContext<'_, T>,
) {
    let EventLoopIterationContext {
        callback,
        window_id_map,
        windows,
        #[cfg(feature = "tracing")]
        active_tracing_spans,
    } = context;
    if *control_flow != ControlFlow::Exit {
        *control_flow = crate::pump::next_external_message_pump_deadline()
            .map(ControlFlow::WaitUntil)
            .unwrap_or(ControlFlow::Wait);
    }

    match event {
        Event::NewEvents(StartCause::Init) => {
            callback(RunEvent::Ready);
        }

        Event::NewEvents(StartCause::Poll) => {
            callback(RunEvent::Resumed);
        }

        Event::MainEventsCleared => {
            if !crate::pump::tick_external_message_pump(Instant::now()) {
                cef::do_message_loop_work();
            }
            callback(RunEvent::MainEventsCleared);

            if *control_flow != ControlFlow::Exit {
                *control_flow = crate::pump::next_external_message_pump_deadline()
                    .map(ControlFlow::WaitUntil)
                    .unwrap_or(ControlFlow::Wait);
            }
        }

        Event::LoopDestroyed => {
            callback(RunEvent::Exit);
        }

        #[cfg(windows)]
        Event::RedrawRequested(id) => {
            if let Some(window_id) = window_id_map.get(&id) {
                let mut windows_ref = windows.0.borrow_mut();
                if let Some(window) = windows_ref.get_mut(&window_id) {
                    if window.is_window_transparent {
                        let background_color = window.background_color;
                        if let Some(surface) = &mut window.surface {
                            if let Some(window) = &window.inner {
                                window.draw_surface(surface, background_color);
                            }
                        }
                    }
                }
            }
        }

        #[cfg(feature = "wayland-osr")]
        Event::RedrawRequested(id) => {
            if let Some(window_id) = window_id_map.get(&id) {
                let mut windows_ref = windows.0.borrow_mut();
                if let Some(window) = windows_ref.get_mut(&window_id) {
                    if let Some(osr_surface) = &mut window.osr_surface {
                        // Find any OSR webview's pixel buffer and blit it.
                        for webview in &window.webviews {
                            if let Some(state_arc) = &webview.osr_state {
                                if let Ok(mut state) = state_arc.lock() {
                                    if state.dirty {
                                        let (w, h) = state.phys_size;
                                        osr_surface.present(&state.pixels, w, h);
                                        state.dirty = false;
                                    }
                                }
                                break; // one OSR webview per window
                            }
                        }
                    }
                }
            }
        }

        #[cfg(feature = "tracing")]
        Event::RedrawEventsCleared => {
            active_tracing_spans.remove_window_draw();
        }

        Event::UserEvent(Message::Webview(
            window_id,
            webview_id,
            WebviewMessage::WebviewEvent(event),
        )) => {
            let windows_ref = windows.0.borrow();
            if let Some(window) = windows_ref.get(&window_id) {
                if let Some(webview) = window.webviews.iter().find(|w| w.id == webview_id) {
                    let label = webview.label.clone();
                    let webview_event_listeners = webview.webview_event_listeners.clone();

                    drop(windows_ref);

                    callback(RunEvent::WebviewEvent {
                        label,
                        event: event.clone(),
                    });
                    let listeners = webview_event_listeners.lock().unwrap();
                    let handlers = listeners.values();
                    for handler in handlers {
                        handler(&event);
                    }
                }
            }
        }

        Event::UserEvent(Message::Webview(
            window_id,
            _webview_id,
            WebviewMessage::SynthesizedWindowEvent(event),
        )) => {
            if let Some(event) = WindowEventWrapper::from(event).0 {
                let windows_ref = windows.0.borrow();
                let window = windows_ref.get(&window_id);
                if let Some(window) = window {
                    let label = window.label.clone();
                    let window_event_listeners = window.window_event_listeners.clone();

                    drop(windows_ref);

                    callback(RunEvent::WindowEvent {
                        label,
                        event: event.clone(),
                    });

                    let listeners = window_event_listeners.lock().unwrap();
                    let handlers = listeners.values();
                    for handler in handlers {
                        handler(&event);
                    }
                }
            }
        }

        Event::WindowEvent {
            event, window_id, ..
        } => {
            if let Some(window_id) = window_id_map.get(&window_id) {
                {
                    let windows_ref = windows.0.borrow();
                    if let Some(window) = windows_ref.get(&window_id) {
                        if let Some(event) = WindowEventWrapper::parse(window, &event).0 {
                            let label = window.label.clone();
                            let window_event_listeners = window.window_event_listeners.clone();

                            drop(windows_ref);

                            callback(RunEvent::WindowEvent {
                                label,
                                event: event.clone(),
                            });
                            let listeners = window_event_listeners.lock().unwrap();
                            let handlers = listeners.values();
                            for handler in handlers {
                                handler(&event);
                            }
                        }
                    }
                }

                match event {
                    #[cfg(windows)]
                    TaoWindowEvent::ThemeChanged(theme) => {
                        if let Some(window) = windows.0.borrow().get(&window_id) {
                            for webview in &window.webviews {
                                let theme = match theme {
                                    TaoTheme::Dark => wry::Theme::Dark,
                                    TaoTheme::Light => wry::Theme::Light,
                                    _ => wry::Theme::Light,
                                };
                                if let Err(e) = webview.set_theme(theme) {
                                    log::error!("failed to set theme: {e}");
                                }
                            }
                        }
                    }
                    TaoWindowEvent::CloseRequested => {
                        on_close_requested(callback, window_id, windows);
                    }
                    TaoWindowEvent::Destroyed => {
                        let removed = windows.0.borrow_mut().remove(&window_id).is_some();
                        if removed {
                            let is_empty = windows.0.borrow().is_empty();
                            if is_empty {
                                let (tx, rx) = channel();
                                callback(RunEvent::ExitRequested { code: None, tx });

                                let recv = rx.try_recv();
                                let should_prevent =
                                    matches!(recv, Ok(ExitRequestedEventAction::Prevent));

                                if !should_prevent {
                                    *control_flow = ControlFlow::Exit;
                                }
                            }
                        }
                    }
                    TaoWindowEvent::Resized(size) => {
                        if let Some((Some(window), webviews)) = windows
                            .0
                            .borrow()
                            .get(&window_id)
                            .map(|w| (w.inner.clone(), w.webviews.clone()))
                        {
                            let size = size.to_logical::<f32>(window.scale_factor());
                            for webview in webviews {
                                #[cfg(feature = "wayland-osr")]
                                if let Some(state_arc) = &webview.osr_state {
                                    // Update the OSR state size and notify CEF only when the
                                    // logical size actually changed.  Calling was_resized() on
                                    // every Resized event creates a feedback loop: was_resized()
                                    // → on_paint → present() → wl_surface.commit() → Resized.
                                    let sf = window.scale_factor();
                                    let phys = window.inner_size();
                                    let log: tao::dpi::LogicalSize<f64> = phys.to_logical(sf);
                                    let new_w = log.width.round() as i32;
                                    let new_h = log.height.round() as i32;
                                    let size_changed = state_arc
                                        .lock()
                                        .map(|s| s.logical_size != (new_w, new_h))
                                        .unwrap_or(false);
                                    if size_changed {
                                        if let Ok(mut state) = state_arc.lock() {
                                            state.resize(new_w, new_h, sf);
                                        }
                                        if let Some(browser) = webview.browser_slot.current() {
                                            if let Some(host) = browser.host() {
                                                host.was_resized();
                                            }
                                        }
                                    }
                                    continue;
                                }

                                if let Some(b) = &*webview.bounds.lock().unwrap() {
                                    if let Err(e) = webview.set_bounds(
                                        Rect {
                                            position: LogicalPosition::new(
                                                size.width * b.x_rate,
                                                size.height * b.y_rate,
                                            )
                                            .into(),
                                            size: LogicalSize::new(
                                                size.width * b.width_rate,
                                                size.height * b.height_rate,
                                            )
                                            .into(),
                                        },
                                        window.as_ref(),
                                    ) {
                                        log::error!("failed to autoresize webview: {e}");
                                    }
                                }
                            }
                        }
                    }
                    #[cfg(feature = "wayland-osr")]
                    ref ev => {
                        // Forward input events to the CEF browser host for OSR windows.
                        use tao::event::{ElementState, MouseButton, MouseScrollDelta};
                        let windows_ref = windows.0.borrow();
                        if let Some(window) = windows_ref.get(&window_id) {
                            // Collect browsers from OSR webviews.
                            let osr_browsers: Vec<_> = window
                                .webviews
                                .iter()
                                .filter(|wv| wv.osr_state.is_some())
                                .filter_map(|wv| wv.browser_slot.current())
                                .collect();

                            // Update last cursor position before dispatching to browsers.
                            // tao emits CursorMoved as physical pixels on Linux
                            // (LogicalPosition * scale_factor). CEF's SendMouseMoveEvent on
                            // Linux expects device (physical) pixel coordinates relative to
                            // the view — it applies device_scale_factor internally when
                            // hit-testing. So we store and forward the raw physical coords.
                            if let TaoWindowEvent::CursorMoved { position, .. } = ev {
                                let px = position.x.round() as i32;
                                let py = position.y.round() as i32;
                                for webview in &window.webviews {
                                    if let Some(state_arc) = &webview.osr_state {
                                        if let Ok(mut state) = state_arc.lock() {
                                            state.last_cursor = (px, py);
                                        }
                                    }
                                }
                            }

                            for browser in &osr_browsers {
                                let Some(host) = browser.host() else { continue };

                                // Read last cursor position for events that need it.
                                let cursor_pos = window
                                    .webviews
                                    .iter()
                                    .find_map(|wv| wv.osr_state.as_ref())
                                    .and_then(|s| s.lock().ok())
                                    .map(|s| s.last_cursor)
                                    .unwrap_or((0, 0));

                                match ev {
                                    TaoWindowEvent::CursorMoved { .. } => {
                                        // CEF mouse coords are logical (DIP) pixels.
                                        // cursor_pos was already converted from physical above.
                                        let me = cef::MouseEvent {
                                            x: cursor_pos.0,
                                            y: cursor_pos.1,
                                            modifiers: 0,
                                        };
                                        host.send_mouse_move_event(Some(&me), 0);
                                    }
                                    TaoWindowEvent::CursorLeft { .. } => {
                                        let me = cef::MouseEvent {
                                            x: cursor_pos.0,
                                            y: cursor_pos.1,
                                            modifiers: 0,
                                        };
                                        host.send_mouse_move_event(Some(&me), 1);
                                    }
                                    TaoWindowEvent::MouseInput { state, button, .. } => {
                                        let btn = match button {
                                            MouseButton::Left => cef::MouseButtonType::LEFT,
                                            MouseButton::Right => cef::MouseButtonType::RIGHT,
                                            MouseButton::Middle => cef::MouseButtonType::MIDDLE,
                                            _ => cef::MouseButtonType::LEFT,
                                        };
                                        let me = cef::MouseEvent {
                                            x: cursor_pos.0,
                                            y: cursor_pos.1,
                                            modifiers: 0,
                                        };
                                        // mouse_up=1 means button released, mouse_up=0 means pressed.
                                        let (mouse_up, click_count) = match state {
                                            ElementState::Pressed => (0, 1),
                                            ElementState::Released => (1, 1),
                                            _ => (0, 1),
                                        };
                                        host.send_mouse_click_event(
                                            Some(&me),
                                            btn,
                                            mouse_up,
                                            click_count,
                                        );
                                    }
                                    TaoWindowEvent::MouseWheel { delta, .. } => {
                                        let (dx, dy) = match delta {
                                            MouseScrollDelta::LineDelta(x, y) => {
                                                (*x as i32 * 120, *y as i32 * 120)
                                            }
                                            MouseScrollDelta::PixelDelta(pos) => {
                                                (pos.x as i32, pos.y as i32)
                                            }
                                            _ => (0, 0),
                                        };
                                        let me = cef::MouseEvent {
                                            x: cursor_pos.0,
                                            y: cursor_pos.1,
                                            modifiers: 0,
                                        };
                                        host.send_mouse_wheel_event(Some(&me), dx, dy);
                                    }
                                    TaoWindowEvent::KeyboardInput { event: key_ev, .. } => {
                                        use tao::event::ElementState;
                                        use tao::keyboard::Key;
                                        let is_press =
                                            key_ev.state == ElementState::Pressed;
                                        // Map tao logical key to a windows_key_code best-effort.
                                        let windows_key_code =
                                            tao_key_to_windows_vk(&key_ev.logical_key);
                                        let cef_type = if is_press {
                                            cef::KeyEventType::RAWKEYDOWN
                                        } else {
                                            cef::KeyEventType::KEYUP
                                        };
                                        let ke = cef::KeyEvent {
                                            type_: cef_type,
                                            windows_key_code,
                                            native_key_code: 0,
                                            modifiers: 0,
                                            is_system_key: 0,
                                            character: 0,
                                            unmodified_character: 0,
                                            focus_on_editable_field: 0,
                                            ..Default::default()
                                        };
                                        host.send_key_event(Some(&ke));

                                        // Also send a CHAR event for printable keys on press.
                                        if is_press {
                                            if let Key::Character(ch) = &key_ev.logical_key {
                                                for c in ch.chars() {
                                                    let char_ke = cef::KeyEvent {
                                                        type_: cef::KeyEventType::CHAR,
                                                        windows_key_code: c as i32,
                                                        character: c as u16,
                                                        unmodified_character: c as u16,
                                                        modifiers: 0,
                                                        native_key_code: 0,
                                                        is_system_key: 0,
                                                        focus_on_editable_field: 0,
                                                        ..Default::default()
                                                    };
                                                    host.send_key_event(Some(&char_ke));
                                                }
                                            }
                                        }
                                    }
                                    TaoWindowEvent::Focused(focused) => {
                                        host.set_focus(i32::from(*focused));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    #[cfg(not(feature = "wayland-osr"))]
                    _ => {}
                }
            }
        }
        Event::UserEvent(message) => match message {
            Message::RequestExit(code) => {
                let (tx, rx) = channel();
                callback(RunEvent::ExitRequested {
                    code: Some(code),
                    tx,
                });

                let recv = rx.try_recv();
                let should_prevent = matches!(recv, Ok(ExitRequestedEventAction::Prevent));

                if !should_prevent {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Message::Window(id, WindowMessage::Close) => {
                on_close_requested(callback, id, windows);
            }
            Message::Window(id, WindowMessage::Destroy) => {
                on_window_close(id, windows);
            }
            Message::UserEvent(t) => callback(RunEvent::UserEvent(t)),
            message => {
                handle_user_message(
                    event_loop,
                    message,
                    UserMessageContext {
                        window_id_map,
                        windows,
                    },
                );
            }
        },
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        Event::Opened { urls } => {
            callback(RunEvent::Opened { urls });
        }
        #[cfg(target_os = "macos")]
        Event::Reopen {
            has_visible_windows,
            ..
        } => callback(RunEvent::Reopen {
            has_visible_windows,
        }),
        _ => (),
    }
}

fn on_close_requested<'a, T: UserEvent>(
    callback: &'a mut (dyn FnMut(RunEvent<T>) + 'static),
    window_id: WindowId,
    windows: Arc<WindowsStore>,
) {
    let (tx, rx) = channel();
    let windows_ref = windows.0.borrow();
    if let Some(w) = windows_ref.get(&window_id) {
        let label = w.label.clone();
        let window_event_listeners = w.window_event_listeners.clone();

        drop(windows_ref);

        let listeners = window_event_listeners.lock().unwrap();
        let handlers = listeners.values();
        for handler in handlers {
            handler(&WindowEvent::CloseRequested {
                signal_tx: tx.clone(),
            });
        }
        callback(RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { signal_tx: tx },
        });
        if let Ok(true) = rx.try_recv() {
        } else {
            on_window_close(window_id, windows);
        }
    }
}

fn on_window_close(window_id: WindowId, windows: Arc<WindowsStore>) {
    if let Some(window_wrapper) = windows.0.borrow_mut().get_mut(&window_id) {
        window_wrapper.inner = None;
        #[cfg(windows)]
        window_wrapper.surface.take();
    }
}

fn create_window<T: UserEvent, F: Fn(RawWindow) + Send + 'static>(
    window_id: WindowId,
    webview_id: u32,
    event_loop: &EventLoopWindowTarget<Message<T>>,
    context: &Context<T>,
    pending: PendingWindow<T, Wry<T>>,
    after_window_creation: Option<F>,
) -> Result<WindowWrapper> {
    #[allow(unused_mut)]
    let PendingWindow {
        mut window_builder,
        label,
        webview,
    } = pending;

    #[cfg(feature = "tracing")]
    let _webview_create_span = tracing::debug_span!("wry::webview::create").entered();
    #[cfg(feature = "tracing")]
    let window_draw_span = tracing::debug_span!("wry::window::draw").entered();
    #[cfg(feature = "tracing")]
    let window_create_span =
        tracing::debug_span!(parent: &window_draw_span, "wry::window::create").entered();

    let window_event_listeners = WindowEventListeners::default();

    #[cfg(windows)]
    let background_color = window_builder.inner.window.background_color;
    #[cfg(windows)]
    let is_window_transparent = window_builder.inner.window.transparent;

    #[cfg(target_os = "macos")]
    {
        if window_builder.tabbing_identifier.is_none()
            || window_builder.inner.window.transparent
            || !window_builder.inner.window.decorations
        {
            window_builder.inner = window_builder.inner.with_automatic_window_tabbing(false);
        }
    }

    #[cfg(desktop)]
    if window_builder.prevent_overflow.is_some() || window_builder.center {
        let monitor = if let Some(window_position) = &window_builder.inner.window.position {
            event_loop.available_monitors().find(|m| {
                let monitor_pos = m.position();
                let monitor_size = m.size();

                // type annotations required for 32bit targets.
                let window_position = window_position.to_physical::<i32>(m.scale_factor());

                monitor_pos.x <= window_position.x
                    && window_position.x < monitor_pos.x + monitor_size.width as i32
                    && monitor_pos.y <= window_position.y
                    && window_position.y < monitor_pos.y + monitor_size.height as i32
            })
        } else {
            event_loop.primary_monitor()
        };
        if let Some(monitor) = monitor {
            let scale_factor = monitor.scale_factor();
            let desired_size = window_builder
                .inner
                .window
                .inner_size
                .unwrap_or_else(|| TaoPhysicalSize::new(800, 600).into());
            let mut inner_size = window_builder
                .inner
                .window
                .inner_size_constraints
                .clamp(desired_size, scale_factor)
                .to_physical::<u32>(scale_factor);
            let mut window_size = inner_size;
            #[allow(unused_mut)]
            // Left and right window shadow counts as part of the window on Windows
            // We need to include it when calculating positions, but not size
            let mut shadow_width = 0;
            #[cfg(windows)]
            if window_builder.inner.window.decorations {
                use windows::Win32::UI::WindowsAndMessaging::{
                    AdjustWindowRect, WS_OVERLAPPEDWINDOW,
                };
                let mut rect = windows::Win32::Foundation::RECT::default();
                let result = unsafe { AdjustWindowRect(&mut rect, WS_OVERLAPPEDWINDOW, false) };
                if result.is_ok() {
                    shadow_width = (rect.right - rect.left) as u32;
                    // rect.bottom is made out of shadow, and we don't care about it
                    window_size.height += -rect.top as u32;
                }
            }

            if let Some(margin) = window_builder.prevent_overflow {
                let work_area = monitor.work_area();
                let margin = margin.to_physical::<u32>(scale_factor);
                let constraint = PhysicalSize::new(
                    work_area.size.width - margin.width,
                    work_area.size.height - margin.height,
                );
                if window_size.width > constraint.width || window_size.height > constraint.height {
                    if window_size.width > constraint.width {
                        inner_size.width = inner_size
                            .width
                            .saturating_sub(window_size.width - constraint.width);
                        window_size.width = constraint.width;
                    }
                    if window_size.height > constraint.height {
                        inner_size.height = inner_size
                            .height
                            .saturating_sub(window_size.height - constraint.height);
                        window_size.height = constraint.height;
                    }
                    window_builder.inner.window.inner_size = Some(inner_size.into());
                }
            }

            if window_builder.center {
                window_size.width += shadow_width;
                let position = window::calculate_window_center_position(window_size, monitor);
                let logical_position = position.to_logical::<f64>(scale_factor);
                window_builder = window_builder.position(logical_position.x, logical_position.y);
            }
        }
    };

    let window = window_builder
        .inner
        .build(event_loop)
        .map_err(|_| Error::CreateWindow)?;

    #[cfg(feature = "tracing")]
    {
        drop(window_create_span);

        context
            .main_thread
            .active_tracing_spans
            .0
            .borrow_mut()
            .push(ActiveTracingSpan::WindowDraw {
                id: window.id(),
                span: window_draw_span,
            });
    }

    context.window_id_map.insert(window.id(), window_id);

    if let Some(handler) = after_window_creation {
        let raw = RawWindow {
            #[cfg(windows)]
            hwnd: window.hwnd(),
            #[cfg(any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ))]
            gtk_window: window.gtk_window(),
            #[cfg(any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ))]
            default_vbox: window.default_vbox(),
            _marker: &std::marker::PhantomData,
        };
        handler(raw);
    }

    let mut webviews = Vec::new();

    let focused_webview = Arc::new(Mutex::new(None));

    if let Some(webview) = webview {
        webviews.push(create_webview(
            #[cfg(feature = "unstable")]
            WebviewKind::WindowChild,
            #[cfg(not(feature = "unstable"))]
            WebviewKind::WindowContent,
            &window,
            Arc::new(Mutex::new(window_id)),
            webview_id,
            context,
            webview,
            focused_webview.clone(),
        )?);
    }

    let window = Arc::new(window);

    #[cfg(windows)]
    let surface = if is_window_transparent {
        if let Ok(context) = softbuffer::Context::new(window.clone()) {
            if let Ok(mut surface) = softbuffer::Surface::new(&context, window.clone()) {
                window.draw_surface(&mut surface, background_color);
                Some(surface)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // If any webview is OSR, create the softbuffer surface now that window is Arc.
    #[cfg(feature = "wayland-osr")]
    let osr_surface = {
        let needs_osr = webviews.iter().any(|w| w.osr_state.is_some());
        if needs_osr {
            OsrSurface::new(window.clone())
        } else {
            None
        }
    };

    Ok(WindowWrapper {
        label,
        has_children: AtomicBool::new(false),
        inner: Some(window),
        webviews,
        window_event_listeners,
        #[cfg(windows)]
        background_color,
        #[cfg(windows)]
        is_window_transparent,
        #[cfg(windows)]
        surface,
        focused_webview,
        #[cfg(feature = "wayland-osr")]
        osr_surface,
    })
}

/// the kind of the webview
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum WebviewKind {
    // webview is the entire window content
    WindowContent,
    // webview is a child of the window, which can contain other webviews too
    WindowChild,
}

const INIT_SCRIPT_ADD_MESSAGE: &str = "__TAURI_ADD_INIT_SCRIPT__";
const INIT_SCRIPT_CLEAR_MESSAGE: &str = "__TAURI_CLEAR_INIT_SCRIPTS__";

const CEF_IPC_FALLBACK_SHIM: &str = r#"
(() => {
  if (window.ipc && typeof window.ipc.postMessage === "function") {
    return;
  }

  const postMessage = (message) => {
    if (typeof window.cefQuery !== "function") {
      console.error("tauri-runtime-cef: window.cefQuery is not available for IPC fallback");
      return;
    }

    let request = "";
    if (typeof message === "string") {
      request = message;
    } else {
      try {
        request = JSON.stringify(message);
      } catch (_error) {
        request = String(message);
      }
    }

    window.cefQuery({
      request,
      onFailure: (_code, error) => {
        if (error) {
          console.error("tauri-runtime-cef: IPC query failed:", error);
        }
      },
    });
  };

  Object.defineProperty(window, "ipc", {
    configurable: true,
    value: Object.freeze({ postMessage }),
  });
})();
"#;


#[derive(Debug, Clone)]
struct WebviewBounds {
    x_rate: f32,
    y_rate: f32,
    width_rate: f32,
    height_rate: f32,
}

fn create_webview<T: UserEvent>(
    kind: WebviewKind,
    window: &Window,
    window_id: Arc<Mutex<WindowId>>,
    id: WebviewId,
    context: &Context<T>,
    pending: PendingWebview<T, Wry<T>>,
    #[allow(unused_variables)] _focused_webview: Arc<Mutex<Option<String>>>,
) -> Result<WebviewWrapper> {
    let PendingWebview {
        label,
        url,
        webview_attributes,
        uri_scheme_protocols,
        ipc_handler,
        navigation_handler,
        new_window_handler,
        document_title_changed_handler,
        web_resource_request_handler,
        on_page_load_handler,
        download_handler,
        ..
    } = pending;

    if !context.webview_runtime_installed {
        log::warn!("webview runtime is not marked as installed; continuing with CEF stub");
    }

    let scale_factor = window.scale_factor();
    let default_size = window.inner_size();
    let initial_url = normalize_cef_initial_url(url.as_str(), webview_attributes.use_https_scheme);
    let mut initialization_scripts = vec![InitializationScript {
        script: CEF_IPC_FALLBACK_SHIM.to_string(),
        for_main_frame_only: true,
    }];
    initialization_scripts.extend(webview_attributes.initialization_scripts.iter().cloned());
    let initialization_scripts = Arc::new(initialization_scripts);
    let use_load_started_init_script_fallback = !renderer_init_scripts_available();
    let javascript_disabled = webview_attributes.javascript_disabled;
    let focused = webview_attributes.focus;
    let drag_drop_handler_enabled = webview_attributes.drag_drop_handler_enabled;
    let open_devtools = webview_attributes.devtools.unwrap_or(false);
    let initial_rect = if let Some(bounds) = webview_attributes.bounds {
        let bounds: RectWrapper = bounds.into();
        bounds.0
    } else {
        Rect {
            position: Position::Physical(PhysicalPosition::new(0, 0)),
            size: Size::Physical(PhysicalSize::new(default_size.width, default_size.height)),
        }
    };

    let title_handler = Arc::new(Mutex::new(document_title_changed_handler));
    let page_load_handler = Arc::new(Mutex::new(on_page_load_handler));
    let ipc_handler = Arc::new(Mutex::new(ipc_handler));
    let navigation_handler = Arc::new(Mutex::new(navigation_handler));
    let web_resource_request_handler = Arc::new(Mutex::new(web_resource_request_handler));
    let uri_scheme_protocols = Arc::new(Mutex::new(uri_scheme_protocols));
    let download_handler = download_handler.clone();
    #[cfg(feature = "new-window-opener-optional")]
    let new_window_handler: Option<
        Arc<dyn Fn(Url, NewWindowFeatures) -> NewWindowResponse + Send + Sync>,
    > = new_window_handler.map(Arc::from);
    #[cfg(not(feature = "new-window-opener-optional"))]
    let has_new_window_handler = new_window_handler.is_some();
    // Remember whether the caller provided an explicit background color.
    // When no explicit color is given we skip the JS background-color injection
    // (apply_background_color) so that the page's own CSS is not overridden.
    let explicit_background_color = webview_attributes.background_color.is_some();
    let default_background = if webview_attributes.transparent {
        (0, 0, 0, 0)
    } else {
        // For OSR on Wayland, default to black rather than white so there is no
        // jarring white flash before the page content renders.  On other paths
        // the conventional white default is preserved.
        #[cfg(feature = "wayland-osr")]
        { (0, 0, 0, 255) }
        #[cfg(not(feature = "wayland-osr"))]
        { (255, 255, 255, 255) }
    };
    let background_color = Arc::new(Mutex::new(
        webview_attributes
            .background_color
            .map(Into::into)
            .unwrap_or(default_background),
    ));

    let browser_slot = BrowserSlot::new();
    let client_builder = RuntimeClientBuilder::new()
        .with_browser_slot(browser_slot.clone())
        .with_drag_drop_handler_enabled(drag_drop_handler_enabled)
        .on_before_browse({
            let navigation_handler = navigation_handler.clone();
            move |url| {
                let Ok(parsed_url) = Url::parse(url) else {
                    return true;
                };

                if let Ok(handler) = navigation_handler.lock() {
                    if let Some(handler) = handler.as_ref() {
                        return handler(&parsed_url);
                    }
                }

                true
            }
        })
        .on_resource_request({
            let web_resource_request_handler = web_resource_request_handler.clone();
            let uri_scheme_protocols = uri_scheme_protocols.clone();
            let protocol_webview_id = label.clone();
            move |request_payload| {
                if let Ok(parsed_url) = Url::parse(&request_payload.url) {
                    if let Ok(protocols) = uri_scheme_protocols.lock() {
                        let protocol_name = protocol_name_from_url(&parsed_url);
                        if let Some(protocol_name) = protocol_name.as_deref() {
                            if protocol_name == "ipc" {
                                log::debug!("handling ipc request: url={} method={} headers={:?}", request_payload.url, request_payload.method, request_payload.headers);
                            }
                            if let Some(protocol_handler) = protocols.get(protocol_name) {
                                if let Some(request) = resource_request_to_http_request(&request_payload) {
                                    let request_for_handler = request.clone();
                                    let (tx, rx) = channel();
                                    protocol_handler(
                                        protocol_webview_id.as_str(),
                                        request,
                                        Box::new(move |response| {
                                            let _ = tx.send(response);
                                        }),
                                    );

                                    match rx.recv_timeout(Duration::from_secs(10)) {
                                        Ok(mut response) => {
                                            if let Ok(handler) = web_resource_request_handler.lock()
                                            {
                                                if let Some(handler) = handler.as_ref() {
                                                    handler(request_for_handler, &mut response);
                                                }
                                            }

                                            apply_cors_headers_for_custom_protocol(
                                                &request_payload,
                                                protocol_name,
                                                &mut response,
                                            );



                                            return Some(response);
                                        }
                                        Err(_) => {
                                            log::warn!(
                                                "timed out waiting for custom protocol response: {}",
                                                parsed_url
                                            );
                                            return Some(
                                                http::Response::builder()
                                                    .status(504)
                                                    .body(Cow::Owned(Vec::new()))
                                                    .expect("valid timeout response"),
                                            );
                                        }
                                    }
                                }
                            } else if protocol_name == "ipc" {
                                log::warn!("ipc protocol handler is not registered in this webview");
                            }
                        }
                    }
                }

                None
            }
        })
        .on_popup_requested({
            #[cfg(feature = "new-window-opener-optional")]
            {
                let new_window_handler = new_window_handler.clone();
                let context = context.clone();
                move |target_url, popup_features| {
                    let Some(new_window_handler) = new_window_handler.as_ref() else {
                        return true;
                    };

                    let Ok(url) = Url::parse(target_url) else {
                        log::warn!("blocking popup with invalid URL: {target_url}");
                        return false;
                    };

                    let response = new_window_handler(
                        url.clone(),
                        popup_features_to_new_window_features(popup_features),
                    );

                    match response {
                        NewWindowResponse::Allow => true,
                        NewWindowResponse::Deny => false,
                        NewWindowResponse::Create { window_id } => {
                            route_new_window_to_existing_window(&context, window_id, &url)
                        }
                    }
                }
            }

            #[cfg(not(feature = "new-window-opener-optional"))]
            {
                move |target_url, _| {
                    if has_new_window_handler {
                        log::warn!(
                            "new_window_handler is set but opener-optional tauri-runtime support is disabled; blocking popup: {target_url}"
                        );
                        false
                    } else {
                        true
                    }
                }
            }
        })
        .on_open_url_from_tab(move |_| true)
        .on_download_requested({
            let download_handler = download_handler.clone();
            move |url, suggested_name| {
                let mut destination = std::env::temp_dir().join(suggested_name);

                if let Some(handler) = &download_handler {
                    let parsed_url = Url::parse(&url).ok()?;
                    if !handler(DownloadEvent::Requested {
                        url: parsed_url,
                        destination: &mut destination,
                    }) {
                        return None;
                    }
                }

                if destination.is_relative() {
                    if let Ok(cwd) = std::env::current_dir() {
                        destination = cwd.join(destination);
                    }
                }

                Some(destination)
            }
        })
        .on_download_finished({
            let download_handler = download_handler.clone();
            move |url, path, success| {
                let Some(handler) = &download_handler else {
                    return;
                };

                if let Ok(parsed_url) = Url::parse(&url) {
                    let _ = handler(DownloadEvent::Finished {
                        url: parsed_url,
                        path,
                        success,
                    });
                }
            }
        })
        .on_event({
            let context = context.clone();
            let window_id = window_id.clone();
            let label = label.clone();
            let title_handler = title_handler.clone();
            let page_load_handler = page_load_handler.clone();
            let ipc_handler = ipc_handler.clone();
            let initialization_scripts = initialization_scripts.clone();
            let background_color = background_color.clone();
            let browser_slot = browser_slot.clone();
            let kind = kind;
            move |event| match event {
                BrowserEvent::TitleChanged { title, .. } => {
                    if let Ok(handler) = title_handler.lock() {
                        if let Some(handler) = handler.as_ref() {
                            handler(title);
                        }
                    }
                }
                BrowserEvent::LoadStarted { url, .. } => {
                    log::info!("cef load started: {}", url);
                    if let Ok(url) = Url::parse(&url) {
                        if let Ok(handler) = page_load_handler.lock() {
                            if let Some(handler) = handler.as_ref() {
                                handler(url, PageLoadEvent::Started);
                            }
                        }
                    }

                    // Only inject a JS background color when the app explicitly
                    // requested one.  Without this guard the default background
                    // (black for OSR, white otherwise) would override the page's
                    // own CSS background-color declarations.
                    if explicit_background_color {
                        if let Ok(color) = background_color.lock() {
                            let _ = apply_background_color(&browser_slot, *color);
                        }
                    }

                    if use_load_started_init_script_fallback {
                        for script in initialization_scripts.iter() {
                            let _ = browser_slot.eval(script.script.as_str());
                        }
                    }
                }
                BrowserEvent::LoadFinished { url, .. } => {
                    log::info!("cef load finished: {}", url);
                    if let Ok(url) = Url::parse(&url) {
                        if let Ok(handler) = page_load_handler.lock() {
                            if let Some(handler) = handler.as_ref() {
                                handler(url, PageLoadEvent::Finished);
                            }
                        }
                    }

                    if explicit_background_color {
                        if let Ok(color) = background_color.lock() {
                            let _ = apply_background_color(&browser_slot, *color);
                        }
                    }
                }
                BrowserEvent::ProcessMessage {
                    name, arguments, ..
                } => {
                    let is_ipc_message = matches!(
                        name.as_str(),
                        "tauri-ipc" | "__TAURI_IPC__" | "tauri:ipc" | "ipc"
                    );
                    if !is_ipc_message {
                        return;
                    }

                    let payload = arguments.last().cloned().unwrap_or_default();
                    let request_uri = arguments
                        .first()
                        .filter(|candidate| Url::parse(candidate).is_ok())
                        .cloned()
                        .unwrap_or_else(|| "tauri://localhost".to_string());
                    let request = Request::builder()
                        .method("POST")
                        .uri(request_uri)
                        .header(http::header::CONTENT_TYPE, "application/json")
                        .body(payload);

                    if let Ok(request) = request {
                        if let Ok(handler) = ipc_handler.lock() {
                            if let Some(handler) = handler.as_ref() {
                                handler(
                                    DetachedWebview {
                                        label: label.clone(),
                                        dispatcher: WryWebviewDispatcher {
                                            window_id: window_id.clone(),
                                            webview_id: id,
                                            context: context.clone(),
                                        },
                                    },
                                    request,
                                );
                            }
                        }
                    }
                }
                BrowserEvent::DragEnter { files, .. } => {
                    let event = DragDropEvent::Enter {
                        paths: files.into_iter().map(PathBuf::from).collect(),
                        position: PhysicalPosition::new(0.0, 0.0),
                    };

                    let message = if kind == WebviewKind::WindowContent {
                        WebviewMessage::SynthesizedWindowEvent(SynthesizedWindowEvent::DragDrop(
                            event,
                        ))
                    } else {
                        WebviewMessage::WebviewEvent(WebviewEvent::DragDrop(event))
                    };

                    let _ = context.proxy.send_event(Message::Webview(
                        *window_id.lock().unwrap(),
                        id,
                        message,
                    ));
                }
                BrowserEvent::BeforeClose { .. } => {
                    let _ = context.proxy.send_event(Message::Webview(
                        *window_id.lock().unwrap(),
                        id,
                        WebviewMessage::Close,
                    ));
                }
                _ => {}
            }
        });
    // client_builder is now fully configured (except for optional OSR render handler).

    // Detect Wayland before building the browser so we can configure OSR.
    #[cfg(feature = "wayland-osr")]
    let is_wayland = {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        window
            .window_handle()
            .ok()
            .map(|h| matches!(h.as_raw(), RawWindowHandle::Wayland(_)))
            .unwrap_or(false)
    };
    #[cfg(not(feature = "wayland-osr"))]
    let is_wayland = false;

    // OSR state created before building the client (Wayland only).
    #[cfg(feature = "wayland-osr")]
    let (osr_state, mut client) = {
        if is_wayland {
            let scale_factor = window.scale_factor();
            // view_rect must be in logical (DIP) pixels — divide physical by scale.
            let phys = window.inner_size();
            let log: tao::dpi::LogicalSize<f64> = phys.to_logical(scale_factor);
            let w = log.width.round() as i32;
            let h = log.height.round() as i32;
            let proxy_for_redraw = context.proxy.clone();
            let window_id_for_redraw = window_id.clone();
            let redraw_fn: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let _ = proxy_for_redraw.send_event(Message::Window(
                    *window_id_for_redraw.lock().unwrap(),
                    WindowMessage::RequestRedraw,
                ));
            });

            let (render_handler, state) = OsrRenderHandler::build(w, h, scale_factor, redraw_fn);
            let client = client_builder
                .with_osr_render_handler(render_handler)
                .build();
            (Some(state), client)
        } else {
            (None, client_builder.build())
        }
    };
    #[cfg(not(feature = "wayland-osr"))]
    let (_osr_state, mut client) = (None::<()>, client_builder.build());

    #[cfg(feature = "tao-runtime")]
    {
        let position = initial_rect.position.to_physical::<i32>(scale_factor);
        let size = initial_rect.size.to_physical::<i32>(scale_factor);
        let width = if size.width <= 0 { 800 } else { size.width };
        let height = if size.height <= 0 { 600 } else { size.height };

        let cef_bounds = CefRect {
            x: position.x,
            y: position.y,
            width,
            height,
        };
        let url = CefString::from(initial_url.as_str());
        let mut request_context = create_request_context_for_webview(&webview_attributes);
        let mut settings = BrowserSettings::default();
        if let Ok(color) = background_color.lock() {
            settings.background_color = rgba_to_cef_color(*color);
        }
        if javascript_disabled {
            settings.javascript = cef::State::DISABLED;
        }
        if !webview_attributes.clipboard {
            settings.javascript_access_clipboard = cef::State::DISABLED;
            settings.javascript_dom_paste = cef::State::DISABLED;
        }

        let mut browser = None;
        let mut last_handle_error = None;

        if is_wayland {
            // OSR path: windowless browser, no X11 parent needed.
            let window_info = WindowInfo::default().set_as_windowless(cef_null_window_handle());
            browser = browser_host_create_browser_sync(
                Some(&window_info),
                Some(&mut client),
                Some(&url),
                Some(&settings),
                None,
                request_context.as_mut(),
            );
            if browser.is_none() {
                let message = format!(
                    "CEF OSR browser creation failed ({}x{}).",
                    width, height
                );
                log::error!("{message}");
                return Err(Error::CreateWebview(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    message,
                ))));
            }
        } else {
            for attempt in 0..=30 {
                match HostWindowInfo::from_tao_window(window) {
                    Ok(host_window) => {
                        if cef_window_handle_is_null(host_window.parent_handle) {
                            last_handle_error = Some("got null parent window handle".to_string());
                        } else {
                            let window_info = WindowInfo::default()
                                .set_as_child(host_window.parent_handle, &cef_bounds);
                            browser = browser_host_create_browser_sync(
                                Some(&window_info),
                                Some(&mut client),
                                Some(&url),
                                Some(&settings),
                                None,
                                request_context.as_mut(),
                            );
                            if browser.is_some() {
                                if attempt > 0 {
                                    log::warn!(
                                        "CEF browser creation succeeded after retry {} (parent={:?})",
                                        attempt,
                                        host_window.parent_handle
                                    );
                                }
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        last_handle_error = Some(e.to_string());
                    }
                }

                std::thread::sleep(Duration::from_millis(20));
            }

            if browser.is_none() {
                let message = if let Some(reason) = last_handle_error {
                    format!(
                        "CEF browser creation failed ({reason}; bounds={}x{}+{},{}).",
                        cef_bounds.width, cef_bounds.height, cef_bounds.x, cef_bounds.y
                    )
                } else {
                    format!(
                        "CEF browser creation failed (bounds={}x{}+{},{}).",
                        cef_bounds.width, cef_bounds.height, cef_bounds.x, cef_bounds.y
                    )
                };

                log::error!("{message}");
                return Err(Error::CreateWebview(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    message,
                ))));
            }
        }

        if let Some(browser) = browser.as_ref() {
            send_init_scripts_to_renderer(browser, initialization_scripts.as_ref());
        }

        browser_slot.set(browser);

        if !is_wayland {
            let _ = move_resize_browser_child(window, &browser_slot, initial_rect);
        }

        // For OSR browsers, CEF starts internally unfocused — we must explicitly
        // grant focus.  For windowed browsers the OS handles this automatically.
        #[cfg(feature = "wayland-osr")]
        if is_wayland {
            if let Some(browser) = browser_slot.current() {
                if let Some(host) = browser.host() {
                    if focused {
                        host.set_focus(1);
                    } else {
                        host.set_focus(0);
                    }
                }
            }
        }
        #[cfg(not(feature = "wayland-osr"))]
        if !focused {
            if let Some(browser) = browser_slot.current() {
                if let Some(host) = browser.host() {
                    host.set_focus(0);
                }
            }
        }
        // Keep the non-OSR path working when wayland-osr is enabled.
        #[cfg(feature = "wayland-osr")]
        if !is_wayland && !focused {
            if let Some(browser) = browser_slot.current() {
                if let Some(host) = browser.host() {
                    host.set_focus(0);
                }
            }
        }
        if explicit_background_color {
            if let Ok(color) = background_color.lock() {
                let _ = apply_background_color(&browser_slot, *color);
            }
        }
        if open_devtools {
            if let Some(browser) = browser_slot.current() {
                if let Some(host) = browser.host() {
                    host.show_dev_tools(None, None, Some(&BrowserSettings::default()), None);
                }
            }
        }
    }

    #[cfg(not(feature = "tao-runtime"))]
    {
        let _ = browser_slot.load_url(&url);
    }

    let webview_bounds = if webview_attributes.auto_resize {
        let window_size = window.inner_size().to_logical::<f32>(scale_factor);
        let size = initial_rect.size.to_logical::<f32>(scale_factor);
        let position = initial_rect.position.to_logical::<f32>(scale_factor);
        Some(WebviewBounds {
            x_rate: position.x / window_size.width,
            y_rate: position.y / window_size.height,
            width_rate: size.width / window_size.width,
            height_rate: size.height / window_size.height,
        })
    } else {
        None
    };

    let _ = kind;

    Ok(WebviewWrapper {
        label,
        id,
        _client: client,
        browser_slot,
        webview_event_listeners: Default::default(),
        background_color,
        rect: Arc::new(Mutex::new(initial_rect)),
        bounds: Arc::new(Mutex::new(webview_bounds)),
        #[cfg(feature = "wayland-osr")]
        osr_state,
    })
}

#[cfg(target_os = "macos")]
fn inner_size(
    window: &Window,
    webviews: &[WebviewWrapper],
    has_children: bool,
) -> TaoPhysicalSize<u32> {
    if !has_children && !webviews.is_empty() {
        use wry::WebViewExtMacOS;
        let webview = webviews.first().unwrap();
        let view = unsafe { Retained::cast_unchecked::<objc2_app_kit::NSView>(webview.webview()) };
        let view_frame = view.frame();
        let logical: TaoLogicalSize<f64> = (view_frame.size.width, view_frame.size.height).into();
        return logical.to_physical(window.scale_factor());
    }

    window.inner_size()
}

#[cfg(not(target_os = "macos"))]
#[allow(unused_variables)]
fn inner_size(
    window: &Window,
    webviews: &[WebviewWrapper],
    has_children: bool,
) -> TaoPhysicalSize<u32> {
    window.inner_size()
}

fn to_tao_theme(theme: Option<Theme>) -> Option<TaoTheme> {
    match theme {
        Some(Theme::Light) => Some(TaoTheme::Light),
        Some(Theme::Dark) => Some(TaoTheme::Dark),
        _ => None,
    }
}

/// Map a tao logical key to a Windows virtual-key code for CEF input events.
///
/// This is a best-effort mapping used for OSR on Wayland.
#[cfg(feature = "wayland-osr")]
fn tao_key_to_windows_vk(key: &tao::keyboard::Key<'_>) -> i32 {
    use tao::keyboard::Key;
    match key {
        Key::Character(s) => {
            let c = s.chars().next().unwrap_or('\0');
            // ASCII printable: VK code = ASCII value.
            if c.is_ascii() {
                let upper = c.to_ascii_uppercase();
                return upper as i32;
            }
            0
        }
        Key::Enter => 0x0D,        // VK_RETURN
        Key::Backspace => 0x08,    // VK_BACK
        Key::Tab => 0x09,          // VK_TAB
        Key::Escape => 0x1B,       // VK_ESCAPE
        Key::Space => 0x20,        // VK_SPACE
        Key::ArrowLeft => 0x25,    // VK_LEFT
        Key::ArrowUp => 0x26,      // VK_UP
        Key::ArrowRight => 0x27,   // VK_RIGHT
        Key::ArrowDown => 0x28,    // VK_DOWN
        Key::Home => 0x24,         // VK_HOME
        Key::End => 0x23,          // VK_END
        Key::PageUp => 0x21,       // VK_PRIOR
        Key::PageDown => 0x22,     // VK_NEXT
        Key::Delete => 0x2E,       // VK_DELETE
        Key::Insert => 0x2D,       // VK_INSERT
        Key::F1 => 0x70,           // VK_F1
        Key::F2 => 0x71,
        Key::F3 => 0x72,
        Key::F4 => 0x73,
        Key::F5 => 0x74,
        Key::F6 => 0x75,
        Key::F7 => 0x76,
        Key::F8 => 0x77,
        Key::F9 => 0x78,
        Key::F10 => 0x79,
        Key::F11 => 0x7A,
        Key::F12 => 0x7B,
        Key::Control => 0x11,      // VK_CONTROL
        Key::Alt => 0x12,          // VK_MENU
        Key::Shift => 0x10,        // VK_SHIFT
        Key::Super => 0x5B,        // VK_LWIN
        Key::CapsLock => 0x14,     // VK_CAPITAL
        _ => 0,
    }
}
