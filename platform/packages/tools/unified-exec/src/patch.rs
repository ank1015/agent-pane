//! Whole-script heredoc recognition adapted from Codex apply-patch/invocation.rs
//! at ac192cd7937b0d73edc6dffe009940ae53782dd4 (Apache-2.0).
use execution_core::{ExecutionErrorCode, ExecutionPath, ExecutionResult};
use serde::{Deserialize, Serialize};
use std::{str::Utf8Error, sync::LazyLock};
use tree_sitter::{LanguageError, Parser, Query, QueryCursor, StreamingIterator};
use tree_sitter_bash::LANGUAGE as BASH;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Invocation {
    pub body: String,
    pub cwd: ExecutionPath,
}

#[derive(Debug)]
enum ExtractHeredocError {
    CommandDidNotStartWithApplyPatch,
    FailedToLoadBashGrammar(LanguageError),
    HeredocNotUtf8(Utf8Error),
    FailedToParsePatchIntoAst,
}

pub(super) fn extract(script: &str) -> ExecutionResult<Option<(String, Option<String>)>> {
    if tool_apply_patch::parse_patch(script).is_ok() {
        return Err(super::error(
            ExecutionErrorCode::InvalidRequest,
            "apply_patch verification failed: patch detected without an explicit apply_patch invocation",
        ));
    }
    match extract_apply_patch_from_bash(script) {
        Ok((body, cwd)) => {
            tool_apply_patch::parse_patch(&body).map_err(|e| {
                super::error(
                    ExecutionErrorCode::InvalidRequest,
                    format!("apply_patch verification failed: {e}"),
                )
            })?;
            Ok(Some((body, cwd)))
        }
        // Codex falls through to shell execution when shell parsing does not match.
        Err(
            ExtractHeredocError::CommandDidNotStartWithApplyPatch
            | ExtractHeredocError::FailedToParsePatchIntoAst,
        ) => Ok(None),
        Err(ExtractHeredocError::FailedToLoadBashGrammar(source)) => Err(super::error(
            ExecutionErrorCode::Internal,
            source.to_string(),
        )),
        Err(ExtractHeredocError::HeredocNotUtf8(source)) => Err(super::error(
            ExecutionErrorCode::Internal,
            source.to_string(),
        )),
    }
}

fn extract_apply_patch_from_bash(
    src: &str,
) -> std::result::Result<(String, Option<String>), ExtractHeredocError> {
    // This function uses a Tree-sitter query to recognize one of two
    // whole-script forms, each expressed as a single top-level statement:
    //
    // 1. apply_patch <<'EOF'\n...\nEOF
    // 2. cd <path> && apply_patch <<'EOF'\n...\nEOF
    //
    // Key ideas when reading the query:
    // - dots (`.`) between named nodes enforces adjacency among named children and
    //   anchor to the start/end of the expression.
    // - we match a single redirected_statement directly under program with leading
    //   and trailing anchors (`.`). This ensures it is the only top-level statement
    //   (so prefixes like `echo ...;` or suffixes like `... && echo done` do not match).
    //
    // Overall, we want to be conservative and only match the intended forms, as other
    // forms are likely to be model errors, or incorrectly interpreted by later code.
    //
    // If you're editing this query, it's helpful to start by creating a debugging binary
    // which will let you see the AST of an arbitrary bash script passed in, and optionally
    // also run an arbitrary query against the AST. This is useful for understanding
    // how tree-sitter parses the script and whether the query syntax is correct. Be sure
    // to test both positive and negative cases.
    static APPLY_PATCH_QUERY: LazyLock<Query> = LazyLock::new(|| {
        let language = BASH.into();
        Query::new(
            &language,
            r#"
            (
              program
                . (redirected_statement
                    body: (command
                            name: (command_name (word) @apply_name) .)
                    (#any-of? @apply_name "apply_patch" "applypatch")
                    redirect: (heredoc_redirect
                                . (heredoc_start)
                                . (heredoc_body) @heredoc
                                . (heredoc_end)
                                .))
                .)

            (
              program
                . (redirected_statement
                    body: (list
                            . (command
                                name: (command_name (word) @cd_name) .
                                argument: [
                                  (word) @cd_path
                                  (string (string_content) @cd_path)
                                  (raw_string) @cd_raw_string
                                ] .)
                            "&&"
                            . (command
                                name: (command_name (word) @apply_name))
                            .)
                    (#eq? @cd_name "cd")
                    (#any-of? @apply_name "apply_patch" "applypatch")
                    redirect: (heredoc_redirect
                                . (heredoc_start)
                                . (heredoc_body) @heredoc
                                . (heredoc_end)
                                .))
                .)
            "#,
        )
        .expect("valid bash query")
    });

    let lang = BASH.into();
    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .map_err(ExtractHeredocError::FailedToLoadBashGrammar)?;
    let tree = parser
        .parse(src, None)
        .ok_or(ExtractHeredocError::FailedToParsePatchIntoAst)?;

    let bytes = src.as_bytes();
    let root = tree.root_node();

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&APPLY_PATCH_QUERY, root, bytes);
    while let Some(m) = matches.next() {
        let mut heredoc_text: Option<String> = None;
        let mut cd_path: Option<String> = None;

        for capture in m.captures.iter() {
            let name = APPLY_PATCH_QUERY.capture_names()[capture.index as usize];
            match name {
                "heredoc" => {
                    let text = capture
                        .node
                        .utf8_text(bytes)
                        .map_err(ExtractHeredocError::HeredocNotUtf8)?
                        .trim_end_matches('\n')
                        .to_string();
                    heredoc_text = Some(text);
                }
                "cd_path" => {
                    let text = capture
                        .node
                        .utf8_text(bytes)
                        .map_err(ExtractHeredocError::HeredocNotUtf8)?
                        .to_string();
                    cd_path = Some(text);
                }
                "cd_raw_string" => {
                    let raw = capture
                        .node
                        .utf8_text(bytes)
                        .map_err(ExtractHeredocError::HeredocNotUtf8)?;
                    let trimmed = raw
                        .strip_prefix('\'')
                        .and_then(|s| s.strip_suffix('\''))
                        .unwrap_or(raw);
                    cd_path = Some(trimmed.to_string());
                }
                _ => {}
            }
        }

        if let Some(heredoc) = heredoc_text {
            return Ok((heredoc, cd_path));
        }
    }

    Err(ExtractHeredocError::CommandDidNotStartWithApplyPatch)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_intercepts_codex_whole_script_forms() {
        let body = "*** Begin Patch\n*** Add File: x\n+hello\n*** End Patch";
        for prefix in [
            "apply_patch",
            "applypatch",
            "cd 'sub dir' && apply_patch",
            "cd \"sub dir\" && apply_patch",
        ] {
            let source = format!("{prefix} <<'EOF'\n{body}\nEOF");
            let (patch, cwd) = extract(&source).unwrap().unwrap();
            assert_eq!(patch, body);
            assert_eq!(
                cwd.as_deref(),
                prefix.starts_with("cd").then_some("sub dir")
            );
        }
        for source in [
            format!("echo before; apply_patch <<'EOF'\n{body}\nEOF"),
            format!("apply_patch <<'EOF'\n{body}\nEOF\necho after"),
            format!("apply_patch <<'EOF' && echo after\n{body}\nEOF"),
            "echo apply_patch".into(),
        ] {
            assert!(extract(&source).unwrap().is_none(), "{source}");
        }
        assert!(extract(body).is_err());
        assert!(extract("apply_patch <<'EOF'\nnot a patch\nEOF").is_err());
    }
}
