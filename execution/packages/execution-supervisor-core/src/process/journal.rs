use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use execution_core::{
    BinaryData, ExecutionErrorCode, ExecutionResult, ExecutionState, ProcessEvent,
    ProcessEventKind, ProcessOutputStream, ReadExecutionResult, TimestampMs,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Mutex, Notify},
};

use crate::error::{error, io_error};

pub(super) struct EventJournal {
    path: PathBuf,
    inner: Mutex<JournalState>,
    changed: Notify,
}

struct JournalState {
    next_sequence: u64,
    entries: Vec<JournalEntry>,
    file_length: u64,
    state: ExecutionState,
    started_at: Option<TimestampMs>,
    exit_code: Option<i32>,
    finished_at: Option<Instant>,
}

#[derive(Clone, Copy)]
struct JournalEntry {
    first_sequence: u64,
    last_sequence: u64,
    offset: u64,
    length: u64,
    output_bytes: u64,
}

#[derive(Clone, Copy)]
struct SelectedEntry {
    journal: JournalEntry,
    output_offset: usize,
    output_length: usize,
    returned_sequence: u64,
}

impl EventJournal {
    pub async fn create(path: PathBuf) -> ExecutionResult<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| io_error(parent, source))?;
        }
        tokio::fs::File::create(&path)
            .await
            .map_err(|source| io_error(&path, source))?;
        Ok(Self {
            path,
            inner: Mutex::new(JournalState {
                next_sequence: 1,
                entries: Vec::new(),
                file_length: 0,
                state: ExecutionState::Starting,
                started_at: None,
                exit_code: None,
                finished_at: None,
            }),
            changed: Notify::new(),
        })
    }

    pub async fn mark_started(&self) -> ExecutionResult<TimestampMs> {
        let timestamp = TimestampMs::now();
        let mut state = self.inner.lock().await;
        state.state = ExecutionState::Running;
        state.started_at = Some(timestamp);
        self.append_locked(&mut state, timestamp, ProcessEventKind::Started)
            .await?;
        drop(state);
        self.changed.notify_waiters();
        Ok(timestamp)
    }

    pub async fn append_output(
        &self,
        stream: ProcessOutputStream,
        bytes: Vec<u8>,
    ) -> ExecutionResult<()> {
        let mut state = self.inner.lock().await;
        if is_terminal(state.state) {
            return Ok(());
        }
        self.append_locked(
            &mut state,
            TimestampMs::now(),
            ProcessEventKind::Output {
                stream,
                data: BinaryData::new(bytes),
            },
        )
        .await?;
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn finish_exited(
        &self,
        exit_code: i32,
        final_state: ExecutionState,
    ) -> ExecutionResult<()> {
        let mut state = self.inner.lock().await;
        if is_terminal(state.state) {
            return Ok(());
        }
        state.state = final_state;
        state.exit_code = Some(exit_code);
        state.finished_at = Some(Instant::now());
        self.append_locked(
            &mut state,
            TimestampMs::now(),
            ProcessEventKind::Exited { exit_code },
        )
        .await?;
        self.append_locked(&mut state, TimestampMs::now(), ProcessEventKind::Closed)
            .await?;
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn finish_failed(&self, message: String) -> ExecutionResult<()> {
        let mut state = self.inner.lock().await;
        if is_terminal(state.state) {
            return Ok(());
        }
        state.state = ExecutionState::Failed;
        state.finished_at = Some(Instant::now());
        self.append_locked(
            &mut state,
            TimestampMs::now(),
            ProcessEventKind::Failed { message },
        )
        .await?;
        self.append_locked(&mut state, TimestampMs::now(), ProcessEventKind::Closed)
            .await?;
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn handle(&self) -> (ExecutionState, Option<TimestampMs>) {
        let state = self.inner.lock().await;
        (state.state, state.started_at)
    }

    pub async fn is_running(&self) -> bool {
        !is_terminal(self.inner.lock().await.state)
    }

    pub async fn finished_for(&self) -> Option<Duration> {
        self.inner
            .lock()
            .await
            .finished_at
            .map(|finished| finished.elapsed())
    }

    pub async fn wait_until_finished(&self, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.is_running().await {
                return true;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return !self.is_running().await;
            }
        }
    }

    pub async fn read(
        &self,
        after_sequence: u64,
        max_bytes: u64,
        wait: Option<Duration>,
    ) -> ExecutionResult<ReadExecutionResult> {
        let notified = self.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let result = self.read_now(after_sequence, max_bytes).await?;
        if !result.events.is_empty() || is_terminal(result.state) || wait.is_none() {
            return Ok(result);
        }
        if let Some(wait) = wait {
            let _ = tokio::time::timeout(wait, notified).await;
        }
        self.read_now(after_sequence, max_bytes).await
    }

    async fn read_now(
        &self,
        after_sequence: u64,
        max_bytes: u64,
    ) -> ExecutionResult<ReadExecutionResult> {
        let state = self.inner.lock().await;
        let last_sequence = state.next_sequence.saturating_sub(1);
        if after_sequence > last_sequence {
            return Err(error(
                ExecutionErrorCode::InvalidRequest,
                format!(
                    "process cursor {after_sequence} is ahead of latest sequence {last_sequence}"
                ),
            ));
        }

        let mut selected = Vec::new();
        let mut selected_bytes = 0_u64;
        for entry in state
            .entries
            .iter()
            .filter(|entry| entry.last_sequence > after_sequence)
        {
            if entry.output_bytes == 0 {
                selected.push(SelectedEntry {
                    journal: *entry,
                    output_offset: 0,
                    output_length: 0,
                    returned_sequence: entry.last_sequence,
                });
                continue;
            }

            let already_read = if after_sequence < entry.first_sequence {
                0
            } else {
                after_sequence
                    .saturating_sub(entry.first_sequence)
                    .saturating_add(1)
                    .min(entry.output_bytes)
            };
            let available = entry.output_bytes.saturating_sub(already_read);
            let remaining = max_bytes.saturating_sub(selected_bytes);
            if remaining == 0 {
                break;
            }
            let selected_length = available.min(remaining);
            let returned_sequence = entry
                .first_sequence
                .saturating_add(already_read)
                .saturating_add(selected_length)
                .saturating_sub(1);
            selected.push(SelectedEntry {
                journal: *entry,
                output_offset: usize::try_from(already_read).map_err(|_| {
                    error(
                        ExecutionErrorCode::ResourceExhausted,
                        "journal output offset is too large to read",
                    )
                })?,
                output_length: usize::try_from(selected_length).map_err(|_| {
                    error(
                        ExecutionErrorCode::ResourceExhausted,
                        "journal output selection is too large to read",
                    )
                })?,
                returned_sequence,
            });
            selected_bytes = selected_bytes.saturating_add(selected_length);
            if selected_length < available {
                break;
            }
        }

        let mut events = Vec::with_capacity(selected.len());
        if !selected.is_empty() {
            let mut file = tokio::fs::File::open(&self.path)
                .await
                .map_err(|source| io_error(&self.path, source))?;
            for selected_entry in &selected {
                let entry = selected_entry.journal;
                file.seek(std::io::SeekFrom::Start(entry.offset))
                    .await
                    .map_err(|source| io_error(&self.path, source))?;
                let length = usize::try_from(entry.length).map_err(|_| {
                    error(
                        ExecutionErrorCode::ResourceExhausted,
                        "journal record is too large to read",
                    )
                })?;
                let mut encoded = vec![0_u8; length];
                file.read_exact(&mut encoded)
                    .await
                    .map_err(|source| io_error(&self.path, source))?;
                let mut event: ProcessEvent =
                    serde_json::from_slice(&encoded).map_err(|source| {
                        error(
                            ExecutionErrorCode::Internal,
                            format!("process journal is corrupt: {source}"),
                        )
                    })?;
                if let ProcessEventKind::Output { data, .. } = &mut event.event {
                    let end = selected_entry
                        .output_offset
                        .saturating_add(selected_entry.output_length);
                    *data = BinaryData::new(
                        data.as_slice()[selected_entry.output_offset..end].to_vec(),
                    );
                }
                event.sequence = selected_entry.returned_sequence;
                events.push(event);
            }
        }
        let next_sequence = events
            .last()
            .map_or(after_sequence, |event: &ProcessEvent| event.sequence);
        Ok(ReadExecutionResult {
            events,
            next_sequence,
            state: state.state,
            exit_code: state.exit_code,
        })
    }

    async fn append_locked(
        &self,
        state: &mut JournalState,
        timestamp: TimestampMs,
        event: ProcessEventKind,
    ) -> ExecutionResult<()> {
        let output_bytes = match &event {
            ProcessEventKind::Output { data, .. } => u64::try_from(data.len()).unwrap_or(u64::MAX),
            ProcessEventKind::Started
            | ProcessEventKind::Exited { .. }
            | ProcessEventKind::Failed { .. }
            | ProcessEventKind::Closed => 0,
        };
        let first_sequence = state.next_sequence;
        let sequence_span = output_bytes.max(1);
        let last_sequence = first_sequence.saturating_add(sequence_span.saturating_sub(1));
        let value = ProcessEvent {
            sequence: last_sequence,
            timestamp,
            event,
        };
        let encoded = serde_json::to_vec(&value).map_err(|source| {
            error(
                ExecutionErrorCode::Internal,
                format!("failed to encode process event: {source}"),
            )
        })?;
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .await
            .map_err(|source| io_error(&self.path, source))?;
        file.write_all(&encoded)
            .await
            .map_err(|source| io_error(&self.path, source))?;
        file.write_all(b"\n")
            .await
            .map_err(|source| io_error(&self.path, source))?;
        file.flush()
            .await
            .map_err(|source| io_error(&self.path, source))?;
        let length = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
        state.entries.push(JournalEntry {
            first_sequence,
            last_sequence,
            offset: state.file_length,
            length,
            output_bytes,
        });
        state.file_length = state.file_length.saturating_add(length).saturating_add(1);
        state.next_sequence = last_sequence.saturating_add(1);
        Ok(())
    }
}

fn is_terminal(state: ExecutionState) -> bool {
    matches!(
        state,
        ExecutionState::Exited
            | ExecutionState::Failed
            | ExecutionState::Cancelled
            | ExecutionState::Lost
    )
}

pub(super) async fn remove_execution_directory(path: &Path) {
    let _ = tokio::fs::remove_dir_all(path).await;
}
