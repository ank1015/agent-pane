//! Stateful, tool-agnostic runtime for Codex-style JavaScript code mode.
//!
//! Every [`InProcessCodeModeSession`] owns live JavaScript cells and values
//! shared through `store`/`load`. Effects are exposed only through a
//! harness-supplied [`CodeModeSessionDelegate`]; this crate has no dependency
//! on any concrete Codex tool package or execution environment.
//!
//! The cell/runtime implementation is adapted from OpenAI Codex's
//! `codex-code-mode-runtime` at revision
//! `536f86e5cc9ec1ff38457d099bf320b9d08eeeba`, under Apache-2.0.

mod cell_actor;
mod runtime;
mod service;
mod session_runtime;
mod types;
mod v8_init;

pub(crate) type TaskFailureHandler = std::sync::Arc<dyn Fn(String) + Send + Sync>;

pub use service::InProcessCodeModeSession;
pub use types::*;
pub use v8_init::{V8JitMode, initialize_v8};
