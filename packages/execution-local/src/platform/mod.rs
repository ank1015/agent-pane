#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::{configure_process, default_shell, terminate_process};
#[cfg(windows)]
pub(crate) use windows::{configure_process, default_shell, terminate_process};
