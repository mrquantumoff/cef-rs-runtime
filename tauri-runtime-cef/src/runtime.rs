//! Tauri runtime adapter.
//!
//! This module provides the concrete runtime type that a Tauri app can plug
//! into `tauri::Builder::<R>`.

pub use crate::wry_fork::{
    Wry as CefRuntime, WryHandle as CefRuntimeHandle, WryWebviewDispatcher as CefWebviewDispatcher,
    WryWindowDispatcher as CefWindowDispatcher,
};
pub use tauri_runtime::UserEvent;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    enum TestEvent {}

    #[test]
    fn runtime_alias_implements_tauri_runtime_trait() {
        fn assert_runtime<R: tauri_runtime::Runtime<TestEvent>>() {}
        assert_runtime::<CefRuntime<TestEvent>>();
    }
}
