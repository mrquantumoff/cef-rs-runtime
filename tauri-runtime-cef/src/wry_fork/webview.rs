// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT
//
// CEF replacement for WRY's platform-specific Webview type.
//
// In the original tauri-runtime-wry this was:
// - Linux: `webkit2gtk::WebView`
// - macOS: a struct with raw pointers to NSView/NSWindow
// - Windows: a struct with ICoreWebView2Controller
//
// For CEF, we use `cef::Browser` on all platforms.

use cef::Browser;

/// The underlying webview handle exposed to user code via `WebviewMessage::WithWebview`.
///
/// This is `cef::Browser` — the cross-platform CEF browser object.
pub type Webview = Browser;
