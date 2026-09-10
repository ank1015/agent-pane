use serde::{Deserialize, Serialize};

pub const EXEC_GRAMMAR: &str = r#"start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE
PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;

pub const EXEC_DESCRIPTION: &str = "Run JavaScript code to orchestrate/compose tool calls. Accepts raw JavaScript source, not JSON, quoted strings, or Markdown fences. Each cell is a fresh async module with top-level await; use text(value) to emit output and exit() to finish early. No Node, filesystem, network, console, or module imports. Optional first-line // @exec: {\"yield_time_ms\": 10000, \"max_output_tokens\": 1000}. yield_time_ms yields while the cell keeps running (default 10000 ms); max_output_tokens bounds this result (default 10000). Use wait only after a running-cell result. Tools are callable as tools[normalizedName](input); function tools take objects and freeform tools take strings. ALL_TOOLS contains {name, description}. Helpers: text(value), image(dataUrlOrImageContent, detail?), audio(dataUrlOrAudioContent), generatedImage({image_url, output_hint?}), store(key,value), load(key), notify(value), setTimeout(callback,delayMs?), clearTimeout(id), yield_control(), exit(). store/load share serializable values across cells in the host session. notify emits an immediate additional tool output. yield_control yields accumulated output while the cell continues. Once module evaluation finishes, unawaited promises and timers are discarded.";
pub const WAIT_DESCRIPTION: &str = "Waits on a yielded exec cell and returns new output or completion. Use only after exec returns Script running with cell ID .... cell_id identifies the running cell. yield_time_ms defaults to 10000 ms; max_tokens defaults to 10000. terminate: true stops the cell; false or omitted waits for output. Returns only new output since the last yield, or final completion/termination. A running cell may yield again with the same ID. Collecting completion closes the cell.";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecInput {
    pub source: String,
    pub yield_time_ms: Option<u64>,
    pub max_output_tokens: Option<usize>,
}
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pragma {
    yield_time_ms: Option<u64>,
    max_output_tokens: Option<usize>,
}
impl ExecInput {
    pub fn parse(source: &str) -> Result<Self, String> {
        if source.trim().is_empty() {
            return Err("exec expects non-empty raw JavaScript source text".into());
        }
        let (first, rest) = source.split_once('\n').unwrap_or((source, ""));
        let (source, pragma) = if let Some(header) = first.trim_start().strip_prefix("// @exec:") {
            if rest.trim().is_empty() {
                return Err("exec pragma must be followed by JavaScript source".into());
            }
            let pragma: Pragma =
                serde_json::from_str(header).map_err(|e| format!("Invalid exec pragma: {e}"))?;
            (rest, pragma)
        } else {
            (source, Pragma::default())
        };
        if pragma
            .yield_time_ms
            .is_some_and(|v| v > 9_007_199_254_740_991)
            || pragma
                .max_output_tokens
                .is_some_and(|v| v as u128 > 9_007_199_254_740_991)
        {
            return Err("exec pragma fields must be non-negative safe integers".into());
        }
        Ok(Self {
            source: source.into(),
            yield_time_ms: pragma.yield_time_ms,
            max_output_tokens: pragma.max_output_tokens,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitInput {
    pub cell_id: String,
    #[serde(default = "default_yield")]
    pub yield_time_ms: u64,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub terminate: bool,
}
fn default_yield() -> u64 {
    10_000
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Completed,
    Failed,
    Terminated,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    InputAudio {
        audio_url: String,
    },
}
#[derive(Clone, Debug)]
pub struct Notification {
    pub call_id: String,
    pub cell_id: String,
    pub text: String,
}

#[derive(Debug)]
pub struct Report {
    pub cell_id: String,
    pub status: Status,
    pub content: Vec<ContentItem>,
    pub error: Option<String>,
    pub wall_time: f64,
    pub max_tokens: Option<usize>,
}
impl Report {
    pub fn missing(id: &str, max_tokens: Option<usize>) -> Self {
        Self {
            cell_id: id.into(),
            status: Status::Failed,
            content: vec![],
            error: Some(format!("Unknown or already closed cell ID {id}")),
            wall_time: 0.0,
            max_tokens,
        }
    }
    pub fn success(&self) -> bool {
        self.status != Status::Failed
    }
    pub fn render(mut self) -> Vec<ContentItem> {
        if let Some(error) = self.error.take() {
            self.content.push(ContentItem::InputText {
                text: format!("Script error:\n{error}"),
            });
        }
        let mut content = super::output::truncate(self.content, self.max_tokens.unwrap_or(10_000));
        let status = match self.status {
            Status::Running => format!("Script running with cell ID {}", self.cell_id),
            Status::Completed => "Script completed".into(),
            Status::Failed => "Script failed".into(),
            Status::Terminated => "Script terminated".into(),
        };
        content.insert(
            0,
            ContentItem::InputText {
                text: format!(
                    "{status}\nWall time {:.1} seconds\nOutput:\n",
                    self.wall_time
                ),
            },
        );
        content
    }
}

pub fn normalize_name(name: &str) -> String {
    let result: String = name
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if c == '_' || c == '$' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if result.is_empty() {
        "_".into()
    } else {
        result
    }
}
