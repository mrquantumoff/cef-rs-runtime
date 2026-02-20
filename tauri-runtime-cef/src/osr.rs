/// Off-Screen Rendering (OSR) support for Wayland.
///
/// CEF cannot embed child windows on Wayland. Instead, CEF renders to an
/// in-memory BGRA buffer via the `RenderHandler` callback (`on_paint`). We
/// then blit that buffer onto the tao `Window` using `softbuffer` and forward
/// input events from tao back to the CEF browser host.
#[cfg(feature = "wayland-osr")]
pub mod wayland {
    use cef::rc::Rc;
    use cef::{
        wrap_render_handler, ImplRenderHandler, PaintElementType, Rect as CefRect, RenderHandler,
        WrapRenderHandler,
    };
    use softbuffer::{Context, Surface};
    use std::num::NonZeroU32;
    use std::sync::{Arc, Mutex};
    use tao::window::Window;

    // -------------------------------------------------------------------------
    // OsrState — shared between OsrRenderHandler and the event loop
    // -------------------------------------------------------------------------

    /// State shared between the CEF render callback and the tao event loop.
    #[derive(Debug)]
    pub struct OsrState {
        /// Current logical size (width × height in physical pixels).
        pub size: (i32, i32),
        /// XRGB pixel buffer (softbuffer format: 0x00RRGGBB stored as u32 LE).
        pub pixels: Vec<u32>,
        /// True when `pixels` contains a freshly painted frame not yet presented.
        pub dirty: bool,
    }

    impl OsrState {
        pub fn new(width: i32, height: i32) -> Self {
            let n = (width.max(1) * height.max(1)) as usize;
            Self {
                size: (width, height),
                pixels: vec![0u32; n],
                dirty: false,
            }
        }

        /// Resize the pixel buffer to accommodate `(width, height)`.
        pub fn resize(&mut self, width: i32, height: i32) {
            self.size = (width.max(1), height.max(1));
            let n = (self.size.0 * self.size.1) as usize;
            self.pixels.resize(n, 0u32);
            self.dirty = false;
        }
    }

    // -------------------------------------------------------------------------
    // OsrSurface — wraps the softbuffer surface for blitting
    // -------------------------------------------------------------------------

    /// Manages a `softbuffer::Surface` bound to a tao `Window`.
    pub struct OsrSurface {
        surface: Surface<Arc<Window>, Arc<Window>>,
    }

    impl OsrSurface {
        /// Create an `OsrSurface` from a tao `Window`.
        pub fn new(window: Arc<Window>) -> Option<Self> {
            let context = Context::new(window.clone()).ok()?;
            let surface = Surface::new(&context, window).ok()?;
            Some(Self { surface })
        }

        /// Blit an XRGB pixel buffer onto the window.
        ///
        /// `pixels` must have exactly `width * height` elements in the
        /// softbuffer format (0x00RRGGBB as a native-endian `u32`).
        pub fn present(&mut self, pixels: &[u32], width: i32, height: i32) {
            let w = width.max(1) as u32;
            let h = height.max(1) as u32;

            if self
                .surface
                .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
                .is_err()
            {
                return;
            }

            let Ok(mut buf) = self.surface.buffer_mut() else {
                return;
            };

            let expected = (w * h) as usize;
            let src_len = pixels.len().min(expected);
            buf[..src_len].copy_from_slice(&pixels[..src_len]);
            // Zero-fill any trailing pixels (shouldn't happen with a correct buffer).
            for px in buf[src_len..].iter_mut() {
                *px = 0;
            }

            let _ = buf.present();
        }
    }

    // -------------------------------------------------------------------------
    // OsrRenderHandler — implements ImplRenderHandler for OSR
    // -------------------------------------------------------------------------

    wrap_render_handler! {
        pub struct OsrRenderHandler {
            pub state: Arc<Mutex<OsrState>>,
            pub redraw: Arc<dyn Fn() + Send + Sync>,
        }

        impl RenderHandler {
            fn view_rect(&self, _browser: Option<&mut cef::Browser>, rect: Option<&mut CefRect>) {
                let Ok(state) = self.state.lock() else { return; };
                if let Some(rect) = rect {
                    rect.x = 0;
                    rect.y = 0;
                    rect.width = state.size.0;
                    rect.height = state.size.1;
                }
            }

            fn on_paint(
                &self,
                _browser: Option<&mut cef::Browser>,
                type_: PaintElementType,
                _dirty_rects: Option<&[CefRect]>,
                buffer: *const u8,
                width: i32,
                height: i32,
            ) {
                // Only handle the view (not popup overlays).
                if type_ != PaintElementType::VIEW {
                    return;
                }

                if buffer.is_null() || width <= 0 || height <= 0 {
                    return;
                }

                let n = (width * height) as usize;
                // SAFETY: CEF guarantees that `buffer` points to `width * height * 4`
                // bytes of valid BGRA data for the duration of this callback.
                let bgra: &[u8] = unsafe { std::slice::from_raw_parts(buffer, n * 4) };

                let Ok(mut state) = self.state.lock() else { return; };

                if state.size != (width, height) {
                    state.resize(width, height);
                }

                // Convert BGRA (CEF) → XRGB (softbuffer: 0x00RRGGBB).
                let pixels = &mut state.pixels;
                for (i, chunk) in bgra.chunks_exact(4).enumerate() {
                    let b = chunk[0] as u32;
                    let g = chunk[1] as u32;
                    let r = chunk[2] as u32;
                    // alpha (chunk[3]) is discarded — softbuffer uses opaque XRGB.
                    pixels[i] = (r << 16) | (g << 8) | b;
                }
                state.dirty = true;
                drop(state);

                // Request a redraw from the event loop.
                (self.redraw)();
            }
        }
    }

    impl OsrRenderHandler {
        /// Build a new `OsrRenderHandler` and return the render handler along
        /// with its shared state.
        pub fn build(
            initial_width: i32,
            initial_height: i32,
            redraw: Arc<dyn Fn() + Send + Sync>,
        ) -> (RenderHandler, Arc<Mutex<OsrState>>) {
            let state = Arc::new(Mutex::new(OsrState::new(initial_width, initial_height)));
            let handler = Self::new(state.clone(), redraw);
            (handler, state)
        }
    }
}
