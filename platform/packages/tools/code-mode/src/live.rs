//! General, resumable code cells. No Platform, Sites, or Codex dependency.
mod output;
mod process;
pub mod protocol;

use crate::{Dispatcher, Extension, Registry};
use protocol::{ExecInput, Report, WaitInput};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::{
    sync::{Notify, mpsc, watch},
    task::JoinHandle,
};
use uuid::Uuid;

pub use protocol::{ContentItem, Notification, Status};

pub(crate) struct CellState {
    pub status: Status,
    pub output: Vec<ContentItem>,
    pub error: Option<String>,
    pub bytes: usize,
    pub yielded: bool,
}

struct Cell {
    state: Arc<Mutex<CellState>>,
    changed: Arc<Notify>,
    resume: Arc<Notify>,
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Drop for Cell {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
        self.task.abort();
    }
}

/// A host-owned session. Cells have independent globals and share only explicit
/// store/load data. Drop cancels all cells, nested futures, and guest processes.
pub struct Session {
    executable: PathBuf,
    registry: Registry,
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    extensions: Vec<Extension>,
    store: Arc<Mutex<BTreeMap<String, Value>>>,
    cells: BTreeMap<String, Cell>,
    notifications: mpsc::Sender<Notification>,
}

impl Session {
    pub fn new(
        executable: impl Into<PathBuf>,
        registry: Registry,
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        extensions: Vec<Extension>,
    ) -> Result<(Self, mpsc::Receiver<Notification>), String> {
        let mut names = std::collections::HashSet::new();
        for tool in registry.tools() {
            let name = protocol::normalize_name(&tool.name);
            if matches!(name.as_str(), "exec" | "wait") || !names.insert(name) {
                return Err("Reserved or duplicate normalized code-mode tool name".into());
            }
        }
        let (notifications, receiver) = mpsc::channel(64);
        Ok((
            Self {
                executable: executable.into(),
                registry,
                dispatcher,
                extensions,
                store: Default::default(),
                cells: BTreeMap::new(),
                notifications,
            },
            receiver,
        ))
    }

    /// The caller must persist admission before calling this method. Never
    /// replay an admitted source after losing the host process.
    pub async fn exec(&mut self, call_id: String, input: ExecInput) -> Result<Report, String> {
        if self.cells.len() >= 32 {
            return Err("Too many live code cells; collect or terminate a cell".into());
        }
        let id = Uuid::now_v7().to_string();
        let state = Arc::new(Mutex::new(CellState {
            status: Status::Running,
            output: vec![],
            error: None,
            bytes: 0,
            yielded: false,
        }));
        let changed = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        let (cancel, cancelled) = watch::channel(false);
        let process = process::Process {
            executable: self.executable.clone(),
            registry: self.registry.clone(),
            dispatcher: self.dispatcher.clone(),
            extensions: self.extensions.clone(),
            store: self.store.clone(),
            state: state.clone(),
            changed: changed.clone(),
            resume: resume.clone(),
            notifications: self.notifications.clone(),
            id: id.clone(),
            call_id,
        };
        let source = input.source;
        let task = tokio::spawn(async move {
            process.run(source, cancelled).await;
        });
        self.cells.insert(
            id.clone(),
            Cell {
                state,
                changed,
                resume,
                cancel,
                task,
            },
        );
        self.observe(
            &id,
            input.yield_time_ms.unwrap_or(10_000),
            input.max_output_tokens,
        )
        .await
    }

    pub async fn wait(&mut self, input: WaitInput) -> Result<Report, String> {
        if input.terminate
            && let Some(cell) = self.cells.get_mut(&input.cell_id)
        {
            let _ = cell.cancel.send(true);
            // Cancellation is observed by the process supervisor even during CPU loops.
            (&mut cell.task)
                .await
                .map_err(|_| "Code cell task failed".to_owned())?;
        }
        self.observe(&input.cell_id, input.yield_time_ms, input.max_tokens)
            .await
    }

    async fn observe(
        &mut self,
        id: &str,
        milliseconds: u64,
        max_tokens: Option<usize>,
    ) -> Result<Report, String> {
        let Some(cell) = self.cells.get(id) else {
            return Ok(Report::missing(id, max_tokens));
        };
        let started = std::time::Instant::now();
        let wait = async {
            loop {
                let notified = cell.changed.notified();
                {
                    let state = cell.state.lock().unwrap();
                    if state.status != Status::Running || state.yielded {
                        break;
                    }
                }
                notified.await;
            }
        };
        let _ = tokio::time::timeout(std::time::Duration::from_millis(milliseconds), wait).await;
        let report = {
            let mut state = cell.state.lock().unwrap();
            if state.yielded {
                cell.resume.notify_one();
            }
            state.yielded = false;
            state.bytes = 0;
            Report {
                cell_id: id.into(),
                status: state.status,
                content: std::mem::take(&mut state.output),
                error: state.error.clone(),
                wall_time: started.elapsed().as_secs_f64(),
                max_tokens,
            }
        };
        if report.status != Status::Running {
            self.cells.remove(id);
        }
        Ok(report)
    }
}
