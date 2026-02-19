<!-- @format -->

s# cef

Use the [Chromium Embedded Framework](https://github.com/chromiumembedded/cef) in Rust.

## External Message Pump Helper

The crate provides `cef::external_message_pump` to simplify integration with
host event loops (for example Tao/Tauri when using
`Settings::external_message_pump = 1`).

Core pieces:

- `ExternalMessagePump`: main-thread state machine that drives
  `cef::do_message_loop_work()`.
- `ExternalMessagePumpHost`: trait your platform timer implementation provides.
- `ExternalMessagePumpScheduleQueue`: thread-safe queue for coalescing
  `on_schedule_message_pump_work` callbacks before forwarding to the main thread.
