use crate::error;
use execution_core::{ExecutionErrorCode as Code, ExecutionResult, OperationContext};

pub(crate) fn replace(
    context: &OperationContext,
    source: &str,
    old: &str,
    new: &str,
    all: bool,
    max_bytes: u64,
) -> ExecutionResult<(String, u64)> {
    context.checkpoint()?;
    let first = source.find(old).ok_or_else(|| error(Code::InvalidRequest, "old_string was not found; it must match exactly, including whitespace and line endings"))?;
    if !all {
        // Reject ambiguous overlapping matches too, while advancing only at a
        // UTF-8 boundary. replace_all deliberately uses non-overlapping matches.
        let next = first
            + source[first..]
                .chars()
                .next()
                .expect("nonempty match")
                .len_utf8();
        if source[next..].contains(old) {
            return Err(error(
                Code::InvalidRequest,
                "old_string has multiple matches; provide more context or set replace_all to true",
            ));
        }
    }
    let mut count = 0u64;
    for _ in source.match_indices(old) {
        context.checkpoint()?;
        count += 1;
        if !all {
            break;
        }
    }
    let removed = count
        .checked_mul(old.len() as u64)
        .ok_or_else(|| error(Code::ResourceExhausted, "edit size overflow"))?;
    let size = (source.len() as u64)
        .checked_sub(removed)
        .and_then(|n| {
            count
                .checked_mul(new.len() as u64)
                .and_then(|added| n.checked_add(added))
        })
        .ok_or_else(|| error(Code::ResourceExhausted, "edit size overflow"))?;
    if size > max_bytes {
        return Err(error(
            Code::ResourceExhausted,
            "edited file exceeds the configured or host write size limit",
        ));
    }
    let capacity = usize::try_from(size).map_err(|_| {
        error(
            Code::ResourceExhausted,
            "edited file is too large for this worker",
        )
    })?;
    let mut output = String::new();
    output.try_reserve_exact(capacity).map_err(|_| {
        error(
            Code::ResourceExhausted,
            "insufficient memory for the edited file",
        )
    })?;
    let mut cursor = 0;
    for (index, _) in source.match_indices(old) {
        context.checkpoint()?;
        output.push_str(&source[cursor..index]);
        output.push_str(new);
        cursor = index + old.len();
        if !all {
            break;
        }
    }
    output.push_str(&source[cursor..]);
    context.checkpoint()?;
    Ok((output, count))
}
