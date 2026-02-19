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
        RawWindowHandle::Win32(handle) => handle.hwnd.get() as cef::sys::cef_window_handle_t,
        RawWindowHandle::Xlib(handle) => handle.window as cef::sys::cef_window_handle_t,
        RawWindowHandle::Xcb(handle) => handle.window.get() as cef::sys::cef_window_handle_t,
        RawWindowHandle::Wayland(_) => return Err(HostWindowError::WaylandUnsupported),
        RawWindowHandle::AppKit(handle) => {
            handle.ns_view.as_ptr() as usize as cef::sys::cef_window_handle_t
        }
        _ => return Err(HostWindowError::UnsupportedHandle),
    };

    Ok(handle)
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
        assert_eq!(handle as usize, 42);
    }

    #[test]
    fn rejects_wayland_handle() {
        let raw =
            RawWindowHandle::Wayland(WaylandWindowHandle::new(core::ptr::NonNull::dangling()));
        let err = cef_parent_handle_from_raw(raw).expect_err("wayland should be rejected");
        assert!(matches!(err, HostWindowError::WaylandUnsupported));
    }
}
