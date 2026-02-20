# Implement Off-Screen Rendering (OSR) for Wayland CEF

CEF cannot embed child windows on Wayland. The only viable approach is **Off-Screen Rendering (OSR)**: CEF renders to an in-memory BGRA buffer, which we blit onto the tao Window using `softbuffer`, while forwarding input events from tao to CEF.

## Proposed Changes

### Dependencies

#### [MODIFY] [Cargo.toml](file:///home/dy/git/cef-rs/tauri-runtime-cef/Cargo.toml)

Add `softbuffer` for CPU framebuffer rendering on Wayland:
```diff
+softbuffer = { version = "0.4", optional = true, default-features = false, features = ["wayland"] }
```
Add it to the `tao-runtime` feature gate.

---

### OSR Render Handler

#### [NEW] [osr.rs](file:///home/dy/git/cef-rs/tauri-runtime-cef/src/osr.rs)

New module implementing Off-Screen Rendering:

1. **`OsrRenderHandler`** — implements `ImplRenderHandler`:
   - `view_rect`: Reports the current window size from a shared `Arc<Mutex<(i32,i32)>>`
   - `on_paint`: Receives BGRA buffer from CEF, converts BGRA→XRGB (softbuffer format), writes to a shared pixel buffer, triggers a window redraw request via tao `EventLoopProxy`

2. **`OsrSurface`** — manages `softbuffer::Surface`:
   - Created from the tao `Window`
   - `present(buffer)` — resizes the softbuffer surface and blits the XRGB pixel buffer

3. **`OsrState`** — shared state between the render handler and event loop:
   - Current size `(width, height)`
   - Pixel buffer `Vec<u32>`
   - Reference to `softbuffer::Surface`
   - Flag indicating buffer is dirty

---

### Client Integration

#### [MODIFY] [client.rs](file:///home/dy/git/cef-rs/tauri-runtime-cef/src/client.rs)

- Add `render_handler()` method to `RuntimeClient` (line 540) that returns the `OsrRenderHandler` when in OSR mode
- Add optional `OsrRenderHandler` field to `RuntimeClientState`
- Add `with_osr_render_handler` builder method to `RuntimeClientBuilder`

---

### Webview Creation (OSR mode)

#### [MODIFY] [mod.rs](file:///home/dy/git/cef-rs/tauri-runtime-cef/src/wry_fork/mod.rs)

At the browser creation site (line ~5758):
- Detect Wayland via `RawWindowHandle::Wayland`
- When Wayland: use `WindowInfo::default().set_as_windowless(0)` instead of `set_as_child`
- Enable `windowless_rendering_enabled = 1` in CEF `Settings` during bootstrap
- Create `OsrSurface` from the tao window
- On `TaoWindowEvent::Resized`: update `OsrState` size and call `host.was_resized()`
- On `RedrawRequested` (triggered by `on_paint`): blit pixel buffer via `OsrSurface::present`

---

### Input Forwarding

#### [MODIFY] [mod.rs](file:///home/dy/git/cef-rs/tauri-runtime-cef/src/wry_fork/mod.rs)

In the event loop (line ~4984 `_ => {}`), add handlers for OSR windows:
- `CursorMoved` → `host.send_mouse_move_event()`
- `MouseInput` → `host.send_mouse_click_event()`
- `MouseWheel` → `host.send_mouse_wheel_event()`
- `KeyboardInput` → `host.send_key_event()`
- `Focused` → `host.set_focus()`
- `CursorLeft` → `host.send_mouse_move_event(mouse_leave=1)`

---

### Bootstrap Configuration

#### [MODIFY] [bootstrap.rs](file:///home/dy/git/cef-rs/tauri-runtime-cef/src/bootstrap.rs)

When on Wayland, set `settings.windowless_rendering_enabled = 1` in the CEF settings passed to `cef_initialize`.

## Verification Plan

1. `cargo build` — verify compilation with softbuffer dependency
2. `cargo run` in Quadrant on Wayland — verify single window with embedded CEF content
3. Test mouse clicks, scrolling, typing in the webview
4. Test window resizing
