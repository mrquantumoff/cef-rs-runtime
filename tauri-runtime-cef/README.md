# tauri-runtime-cef

CEF runtime integration for Tao/Tauri apps.

This crate now contains a forked runtime adapter (`wry_fork`) used by
`runtime::CefRuntime`, plus reusable CEF/Tao building blocks.

## What Is Included

- `runtime` (feature `tauri-runtime-adapter`): Tauri runtime types
  (`CefRuntime`, `CefRuntimeHandle`, dispatchers) backed by the local CEF fork.
- `bootstrap`: CEF process/bootstrap helpers around
  `execute_process + initialize + shutdown`.
- `config::CefRuntimeConfig`: CEF settings mapping.
- `tao_window`: raw-window-handle mapping for CEF child embedding.
- `client`, `browser_slot`, `dispatch`, `pump`, `tao`: lower-level integration
  pieces and example-grade Tao loop support.

## Current Status

- Linux/X11 child embedding path is implemented in the runtime fork.
- Runtime compiles and tests pass.
- Background color is applied at browser creation and reinforced on page loads.
- Navigation veto handler (`navigation_handler`) and download handler callbacks are wired.
- Cookies and clear-browsing-data operations are wired to CEF cookie/context APIs.
- Some operations are still transitional (notably popup/new-window semantics,
  custom protocol/resource interception, and some platform-specific APIs).

## Prerequisites

1. Install CEF shared binaries and set environment variables (see workspace
   root `README.md`, section "Install Shared CEF Binaries").
2. On Linux, use X11 backend for embedding. For Wayland sessions, run with:
   - `GDK_BACKEND=x11`
   - `WINIT_UNIX_BACKEND=x11`

## Run The Prototype Example

```sh
cargo run -p tauri-runtime-cef --example tao_cef_runtime
```

## Use In Your Own Tauri App

Add dependency:

```toml
[dependencies]
tauri-runtime-cef = { path = "../cef-rs/tauri-runtime-cef", features = ["tauri-runtime-adapter", "tao-runtime"] }
```

Then bootstrap CEF before launching Tauri, and use `CefRuntime` as your builder
runtime type.

```rust,ignore
use tauri_runtime_cef::{
    bootstrap::{bootstrap, BootstrapOutcome},
    config::CefRuntimeConfig,
    runtime::CefRuntime,
};

type Runtime = CefRuntime<tauri::EventLoopMessage>;

fn main() {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("GDK_BACKEND").is_none() {
            std::env::set_var("GDK_BACKEND", "x11");
        }
        if std::env::var_os("WINIT_UNIX_BACKEND").is_none() {
            std::env::set_var("WINIT_UNIX_BACKEND", "x11");
        }
    }

    let mut config = CefRuntimeConfig::default();
    config.cache_path = Some(std::env::temp_dir().join("my-app-cef-cache"));

    let _cef = match bootstrap(None, config).expect("failed to bootstrap CEF") {
        BootstrapOutcome::Subprocess(code) => std::process::exit(code),
        BootstrapOutcome::Browser(initialized) => initialized,
    };

    tauri::Builder::<Runtime>::new()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

Notes:

- Keep the `InitializedCef` value alive until app exit; dropping it calls
  `cef::shutdown()`.
- For per-webview background color, use normal Tauri webview/background options;
  the CEF runtime maps those into CEF settings and runtime updates.

### Important: keep Tauri crates from a single source

If you see an error like:

`the trait bound tauri_runtime_cef::runtime::CefRuntime<...>: tauri_runtime::Runtime<tauri::EventLoopMessage> is not satisfied`

you likely have mixed Tauri crates from different sources (for example,
`tauri` from a git/path checkout and `tauri-runtime` from crates.io, or vice
versa).

Use one source for all Tauri crates in your app. If you are developing against
your local `~/git/tauri` checkout, add a workspace patch:

```toml
[patch.crates-io]
tauri = { path = "../tauri/crates/tauri" }
tauri-runtime = { path = "../tauri/crates/tauri-runtime" }
tauri-utils = { path = "../tauri/crates/tauri-utils" }
```
