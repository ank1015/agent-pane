#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::{default_shell, terminate_process};
#[cfg(windows)]
pub(crate) use windows::{default_shell, terminate_process};
