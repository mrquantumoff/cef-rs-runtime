// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT
//
// CEF fork: Undecorated window resize handling.
//
// The original WRY version hooks into the webkit2gtk::WebView for
// button-press / touch events. CEF handles its own input, so the GTK
// resize handler is a no-op stub for now.  The Windows side is removed
// entirely since we are Linux-only in this iteration.

#![cfg(any(
    target_os = "linux",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]

/// Stub: CEF handles its own input, so there is no webview widget to
/// attach a GTK resize handler to.  If undecorated resizing is needed
/// it should be driven from the tao window level instead.
#[allow(dead_code)]
pub fn attach_resize_handler(_window: &tao::window::Window) {
    // TODO: implement undecorated resize via tao window events if needed
}
