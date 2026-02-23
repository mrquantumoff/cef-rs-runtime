use cef::{self};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tao::window::Window;

#[derive(Debug, thiserror::Error)]
pub enum HostWindowError {
    #[error("failed to obtain raw window handle")]
    RawHandleUnavailable,
    #[error("window dimensions exceed i32 limits")]
    SizeOverflow,
    #[error("wayland embedding is not supported by this prototype; run with X11 backend")]
    WaylandUnsupported,
    #[error("unsupported Tao window handle for CEF embedding")]
    UnsupportedHandle,
}

#[derive(Debug, Clone, Copy)]
pub struct HostWindowInfo {
    pub parent_handle: cef::sys::cef_window_handle_t,
    pub width: i32,
    pub height: i32,
}

impl HostWindowInfo {
    pub fn from_tao_window(window: &Window) -> Result<Self, HostWindowError> {
        let size = window.inner_size();
        let width = i32::try_from(size.width).map_err(|_| HostWindowError::SizeOverflow)?;
        let height = i32::try_from(size.height).map_err(|_| HostWindowError::SizeOverflow)?;

        let window_handle = window
            .window_handle()
            .map_err(|_| HostWindowError::RawHandleUnavailable)?;

        let parent_handle = cef_parent_handle_from_raw(window_handle.as_raw())?;

        Ok(Self {
            parent_handle,
            width,
            height,
        })
    }
}

pub fn cef_parent_handle_from_raw(
    raw: RawWindowHandle,
) -> Result<cef::sys::cef_window_handle_t, HostWindowError> {
    let handle = match raw {
        RawWindowHandle::Win32(handle) => cef_window_handle_from_usize(handle.hwnd.get() as usize),
        RawWindowHandle::Xlib(handle) => cef_window_handle_from_usize(handle.window as usize),
        RawWindowHandle::Xcb(handle) => cef_window_handle_from_usize(handle.window.get() as usize),
        RawWindowHandle::Wayland(handle) => {
            cef_window_handle_from_usize(handle.surface.as_ptr() as usize)
        }
        RawWindowHandle::AppKit(handle) => {
            cef_window_handle_from_usize(handle.ns_view.as_ptr() as usize)
        }
        _ => return Err(HostWindowError::UnsupportedHandle),
    };

    Ok(handle)
}

pub fn cef_null_window_handle() -> cef::sys::cef_window_handle_t {
    cef_window_handle_from_usize(0)
}

#[cfg(target_os = "windows")]
pub fn cef_window_handle_is_null(handle: cef::sys::cef_window_handle_t) -> bool {
    handle.0.is_null()
}

#[cfg(not(target_os = "windows"))]
pub fn cef_window_handle_is_null(handle: cef::sys::cef_window_handle_t) -> bool {
    handle == 0
}

#[cfg(target_os = "windows")]
fn cef_window_handle_from_usize(value: usize) -> cef::sys::cef_window_handle_t {
    cef::sys::HWND(value as *mut cef::sys::HWND__)
}

#[cfg(not(target_os = "windows"))]
fn cef_window_handle_from_usize(value: usize) -> cef::sys::cef_window_handle_t {
    value as cef::sys::cef_window_handle_t
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::num::NonZeroU32;
    use raw_window_handle::{WaylandWindowHandle, XcbWindowHandle};

    #[test]
    fn maps_xcb_handle() {
        let raw = RawWindowHandle::Xcb(XcbWindowHandle::new(NonZeroU32::new(42).unwrap()));
        let handle = cef_parent_handle_from_raw(raw).expect("failed to map xcb handle");
        assert_eq!(handle_to_usize(handle), 42);
    }

    #[test]
    fn maps_wayland_handle() {
        let mut wl_handle = WaylandWindowHandle::new(core::ptr::NonNull::dangling());
        wl_handle.surface = core::ptr::NonNull::new(42 as *mut _).unwrap();
        let raw = RawWindowHandle::Wayland(wl_handle);
        let handle = cef_parent_handle_from_raw(raw).expect("failed to map wayland handle");
        assert_eq!(handle_to_usize(handle), 42);
    }

    #[cfg(target_os = "windows")]
    fn handle_to_usize(handle: cef::sys::cef_window_handle_t) -> usize {
        handle.0 as usize
    }

    #[cfg(not(target_os = "windows"))]
    fn handle_to_usize(handle: cef::sys::cef_window_handle_t) -> usize {
        handle as usize
    }
}
