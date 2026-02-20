/// Off-Screen Rendering (OSR) support for Wayland.
///
/// CEF cannot embed child windows on Wayland. Instead, CEF renders to an
/// in-memory BGRA buffer via the `RenderHandler` callback (`on_paint`). We
/// then blit that buffer onto the tao `Window` using `softbuffer` and forward
/// input events from tao back to the CEF browser host.
///
/// ## CEF coordinate system for OSR
///
/// * `get_view_rect`  → **logical (DIP) pixels**.  CEF uses this for page
///   layout.  At 2× scale a 1280×720 logical window should report 1280×720
///   here, NOT 2560×1440.
/// * `get_screen_info.device_scale_factor` → the HiDPI multiplier (e.g. 2.0).
///   CEF multiplies `view_rect` by this factor to determine the physical size
///   of the paint buffer it will pass to `on_paint`.
/// * `on_paint` buffer `width`/`height` → **physical pixels** (logical ×
///   scale_factor).  We use these to size our pixel buffer, but do NOT feed
///   them back into `view_rect`.
/// * `send_mouse_*` coordinates → **logical (DIP) pixels**, same space as
///   `view_rect`.
#[cfg(feature = "wayland-osr")]
pub mod wayland {
    use cef::rc::Rc;
    use cef::{
        wrap_render_handler, ImplRenderHandler, PaintElementType, Rect as CefRect, RenderHandler,
        ScreenInfo, WrapRenderHandler,
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
        /// Window size in **logical (DIP) pixels** — reported to CEF via
        /// `get_view_rect` and `get_screen_info.rect`.
        pub logical_size: (i32, i32),
        /// HiDPI scale factor (e.g. 2.0 on a 2× display).
        pub scale_factor: f32,
        /// Physical dimensions of the current pixel buffer
        /// (`logical_size × scale_factor`, as supplied by CEF's `on_paint`).
        pub phys_size: (i32, i32),
        /// XRGB pixel buffer at physical resolution (softbuffer 0x00RRGGBB u32).
        pub pixels: Vec<u32>,
        /// True when `pixels` contains a freshly painted frame not yet presented.
        pub dirty: bool,
        /// Last known cursor position in **logical (DIP) pixels** (used for
        /// click and scroll events sent to CEF).
        pub last_cursor: (i32, i32),
    }

    impl OsrState {
        /// Create initial state from a **logical** size and scale factor.
        pub fn new(logical_w: i32, logical_h: i32, scale_factor: f64) -> Self {
            let sf = scale_factor as f32;
            let phys_w = ((logical_w as f64) * scale_factor).round() as i32;
            let phys_h = ((logical_h as f64) * scale_factor).round() as i32;
            let n = (phys_w.max(1) * phys_h.max(1)) as usize;
            Self {
                logical_size: (logical_w.max(1), logical_h.max(1)),
                scale_factor: sf,
                phys_size: (phys_w.max(1), phys_h.max(1)),
                pixels: vec![0u32; n],
                dirty: false,
                last_cursor: (0, 0),
            }
        }

        /// Update after a window resize.  `logical_w`/`logical_h` are DIP pixels.
        pub fn resize(&mut self, logical_w: i32, logical_h: i32, scale_factor: f64) {
            self.logical_size = (logical_w.max(1), logical_h.max(1));
            self.scale_factor = scale_factor as f32;
            let phys_w = ((logical_w as f64) * scale_factor).round() as i32;
            let phys_h = ((logical_h as f64) * scale_factor).round() as i32;
            self.phys_size = (phys_w.max(1), phys_h.max(1));
            let n = (self.phys_size.0 * self.phys_size.1) as usize;
            self.pixels.resize(n, 0u32);
            self.dirty = false;
        }

        /// Resize the pixel buffer to match a new physical size supplied by
        /// `on_paint`.  Does not change `logical_size` or `scale_factor`.
        pub fn resize_phys(&mut self, phys_w: i32, phys_h: i32) {
            self.phys_size = (phys_w.max(1), phys_h.max(1));
            let n = (self.phys_size.0 * self.phys_size.1) as usize;
            self.pixels.resize(n, 0u32);
        }
    }

    // -------------------------------------------------------------------------
    // OsrSurface — wraps the softbuffer surface for blitting
    // -------------------------------------------------------------------------

    /// Manages a `softbuffer::Surface` bound to a tao `Window`.
    pub struct OsrSurface {
        surface: Surface<Arc<Window>, Arc<Window>>,
        /// Physical dimensions the softbuffer surface was last resized to.
        /// Tracked so we only call `surface.resize()` when the size actually
        /// changes — calling it every frame causes the Wayland compositor to
        /// emit spurious `Resized` window events, creating an infinite loop.
        last_surface_size: (u32, u32),
    }

    impl OsrSurface {
        /// Create an `OsrSurface` from a tao `Window`.
        pub fn new(window: Arc<Window>) -> Option<Self> {
            let context = Context::new(window.clone()).ok()?;
            let surface = Surface::new(&context, window).ok()?;
            Some(Self {
                surface,
                last_surface_size: (0, 0),
            })
        }

        /// Blit an XRGB pixel buffer onto the window.
        ///
        /// `pixels` must have exactly `phys_w * phys_h` elements in the
        /// softbuffer format (0x00RRGGBB as a native-endian `u32`).
        /// `phys_w` and `phys_h` are **physical pixels**.
        pub fn present(&mut self, pixels: &[u32], phys_w: i32, phys_h: i32) {
            let w = phys_w.max(1) as u32;
            let h = phys_h.max(1) as u32;

            // Only call surface.resize() when the dimensions actually change.
            // On Wayland, resizing the softbuffer surface every frame causes the
            // compositor to fire a Resized window event each time, creating an
            // infinite resize loop.
            if (w, h) != self.last_surface_size {
                if self
                    .surface
                    .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
                    .is_err()
                {
                    return;
                }
                self.last_surface_size = (w, h);
            }

            let Ok(mut buf) = self.surface.buffer_mut() else {
                return;
            };

            let expected = (w * h) as usize;
            let src_len = pixels.len().min(expected);
            buf[..src_len].copy_from_slice(&pixels[..src_len]);
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
            /// Report the view size in **logical (DIP) pixels**.
            fn view_rect(&self, _browser: Option<&mut cef::Browser>, rect: Option<&mut CefRect>) {
                let Ok(state) = self.state.lock() else { return; };
                if let Some(rect) = rect {
                    rect.x = 0;
                    rect.y = 0;
                    rect.width = state.logical_size.0;
                    rect.height = state.logical_size.1;
                }
            }

            /// Report HiDPI scale factor so CEF knows the physical paint buffer
            /// will be `logical_size × device_scale_factor` pixels.
            fn screen_info(
                &self,
                _browser: Option<&mut cef::Browser>,
                screen_info: Option<&mut ScreenInfo>,
            ) -> ::std::os::raw::c_int {
                let Ok(state) = self.state.lock() else { return 0; };
                if let Some(info) = screen_info {
                    info.device_scale_factor = state.scale_factor;
                    info.depth = 32;
                    info.depth_per_component = 8;
                    info.is_monochrome = 0;
                    // rect is in DIP pixels, matching view_rect.
                    info.rect = CefRect {
                        x: 0,
                        y: 0,
                        width: state.logical_size.0,
                        height: state.logical_size.1,
                    };
                    info.available_rect = info.rect.clone();
                    return 1;
                }
                0
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
                // SAFETY: CEF guarantees `buffer` points to `width * height * 4`
                // bytes of valid BGRA data for the duration of this callback.
                // width/height here are **physical pixels**.
                let bgra: &[u8] = unsafe { std::slice::from_raw_parts(buffer, n * 4) };

                let Ok(mut state) = self.state.lock() else { return; };

                // Resize physical pixel buffer if CEF gave us a different size.
                // Do NOT update logical_size here — CEF's paint buffer dimensions
                // are physical (logical × scale_factor).
                if state.phys_size != (width, height) {
                    state.resize_phys(width, height);
                }

                // Convert BGRA (CEF) → XRGB (softbuffer: 0x00RRGGBB).
                let pixels = &mut state.pixels;
                for (i, chunk) in bgra.chunks_exact(4).enumerate() {
                    let b = chunk[0] as u32;
                    let g = chunk[1] as u32;
                    let r = chunk[2] as u32;
                    // alpha (chunk[3]) discarded — softbuffer is opaque XRGB.
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
        /// Build a new `OsrRenderHandler` and return the handler along with its
        /// shared state.
        ///
        /// `initial_logical_w`/`initial_logical_h` must be in **logical (DIP)
        /// pixels** (i.e. `window.inner_size().to_logical(scale_factor)`).
        pub fn build(
            initial_logical_w: i32,
            initial_logical_h: i32,
            scale_factor: f64,
            redraw: Arc<dyn Fn() + Send + Sync>,
        ) -> (RenderHandler, Arc<Mutex<OsrState>>) {
            let state = Arc::new(Mutex::new(OsrState::new(
                initial_logical_w,
                initial_logical_h,
                scale_factor,
            )));
            let handler = Self::new(state.clone(), redraw);
            (handler, state)
        }
    }
}
