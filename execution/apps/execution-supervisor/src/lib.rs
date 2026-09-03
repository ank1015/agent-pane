//! Local Unix-socket endpoint for `execution-supervisor-core`.

#![cfg_attr(not(unix), allow(dead_code))]

#[cfg(not(unix))]
compile_error!("execution-supervisor currently requires a Unix host");

mod client;
mod server;

pub use client::{call, probe, relay_stdio};
pub use server::{ServeConfig, serve};
