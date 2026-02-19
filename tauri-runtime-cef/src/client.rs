use crate::browser_slot::BrowserSlot;
use cef::*;
use std::sync::Arc;

pub type BrowserEventHandler = Arc<dyn Fn(BrowserEvent) + Send + Sync + 'static>;

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

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(RuntimeLifeSpanHandler::new(self.state.clone()))
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(RuntimeLoadHandler::new(self.state.clone()))
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
