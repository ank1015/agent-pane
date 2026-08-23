//! Reusable response-level conformance checks for execution backends.
//!
//! Adapter crates can combine these checks with backend fixtures. Checks that
//! require repeated operations—mutation idempotency, stale revisions, process
//! resumption, and cancellation—belong in fixture-driven suites added alongside
//! the first concrete runtime.

use execution_contracts::{
    ListRequest, ListResult, ProcessEvent, ProcessEventKind, ReadRequest, ReadResult,
};
use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("runtime conformance violation at {path}: {message}")]
pub struct ConformanceViolation {
    pub path: &'static str,
    pub message: String,
}

/// Checks bounded text-read invariants promised by `WorkspaceQuery`.
pub fn check_read_result(
    request: &ReadRequest,
    result: &ReadResult,
) -> Result<(), ConformanceViolation> {
    let ReadResult::Text { page } = result else {
        return Ok(());
    };
    let Some(bounds) = &request.page else {
        return Ok(());
    };

    if page.content.len() as u64 > bounds.max_bytes {
        return violation("read.content", "exceeds max_bytes");
    }
    if page.content.lines().count() > bounds.max_lines as usize {
        return violation("read.content", "exceeds max_lines");
    }
    if !page.lines_truncated
        && page
            .content
            .lines()
            .any(|line| line.len() as u64 > bounds.max_line_bytes)
    {
        return violation("read.content", "contains a line exceeding max_line_bytes");
    }
    if page.has_more && page.cursor.is_none() && page.next_line.is_none() {
        return violation(
            "read.pagination",
            "has_more requires a continuation cursor or next line",
        );
    }
    if page.end_line < page.start_line {
        return violation("read.lines", "end_line precedes start_line");
    }
    Ok(())
}

/// Checks bounded directory-listing invariants promised by `WorkspaceQuery`.
pub fn check_list_result(
    request: &ListRequest,
    result: &ListResult,
) -> Result<(), ConformanceViolation> {
    if result.entries.len() > request.limit as usize {
        return violation("list.entries", "contains more entries than requested");
    }
    if result.truncated && result.cursor.is_none() {
        return violation("list.pagination", "truncated result has no cursor");
    }
    Ok(())
}

/// Checks ordering, resumption, identity, and terminal-event invariants.
pub fn check_process_events(
    events: &[ProcessEvent],
    after_sequence: Option<u64>,
) -> Result<(), ConformanceViolation> {
    let Some(first) = events.first() else {
        return Ok(());
    };
    if after_sequence.is_some_and(|sequence| first.sequence <= sequence) {
        return violation(
            "process.sequence",
            "first resumed event does not follow after_sequence",
        );
    }

    let mut previous_sequence = None;
    let mut terminal = false;
    let mut exited = false;
    for event in events {
        if event.execution_id != first.execution_id {
            return violation("process.execution_id", "changed within one event stream");
        }
        if previous_sequence.is_some_and(|sequence| event.sequence <= sequence) {
            return violation("process.sequence", "is not strictly increasing");
        }
        if terminal {
            return violation("process.events", "contains an event after stream closure");
        }
        if exited && !matches!(event.event, ProcessEventKind::Closed) {
            return violation("process.events", "contains a non-closed event after exit");
        }

        match event.event {
            ProcessEventKind::Exited { .. } => exited = true,
            ProcessEventKind::Closed | ProcessEventKind::Failed { .. } => terminal = true,
            ProcessEventKind::Started | ProcessEventKind::Output { .. } => {}
        }
        previous_sequence = Some(event.sequence);
    }
    Ok(())
}

fn violation<T>(path: &'static str, message: impl Into<String>) -> Result<T, ConformanceViolation> {
    Err(ConformanceViolation {
        path,
        message: message.into(),
    })
}
