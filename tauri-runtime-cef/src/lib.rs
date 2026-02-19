#![doc = include_str!("../README.md")]
#![allow(unexpected_cfgs)]

pub mod bootstrap;
pub mod browser_slot;
pub mod client;
pub mod config;
pub mod dispatch;
pub mod dispatch_trait;
pub mod pump;

#[cfg(feature = "tauri-runtime-adapter")]
pub mod wry_fork;

#[cfg(feature = "tauri-runtime-adapter")]
pub mod runtime;

#[cfg(feature = "tao-runtime")]
pub mod tao;

#[cfg(feature = "tao-runtime")]
pub mod tao_window;
