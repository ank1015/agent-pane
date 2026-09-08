//! No filesystem, network, environment or module loader is exposed to JavaScript.
use crate::{Extension, Tool};
use rquickjs::{Context, Function, Promise, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read, Write},
    time::{Duration, Instant},
};
pub const FRAME_LIMIT: usize = 256 * 1024;
#[derive(Serialize, Deserialize)]
pub(crate) struct Input {
    pub source: String,
    pub tools: Vec<Tool>,
    pub extensions: Vec<Extension>,
    pub timeout_ms: u64,
}
fn host(message: String) -> String {
    let result = (|| -> io::Result<String> {
        if message.len() > FRAME_LIMIT {
            return Err(io::Error::other("frame limit"));
        }
        let mut out = io::stdout().lock();
        out.write_all(message.as_bytes())?;
        out.write_all(b"\n")?;
        out.flush()?;
        let mut line = String::new();
        io::stdin()
            .lock()
            .take((FRAME_LIMIT + 1) as u64)
            .read_line(&mut line)?;
        if line.len() > FRAME_LIMIT || !line.ends_with('\n') {
            return Err(io::Error::other("invalid frame"));
        }
        Ok(line)
    })();
    result.unwrap_or_else(|_| {
        json!({"error":{"code":"BRIDGE_CLOSED","message":"Code-mode bridge closed"}}).to_string()
    })
}
pub fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut line = String::new();
    io::stdin()
        .lock()
        .take(2 * 1024 * 1024 + 1)
        .read_line(&mut line)?;
    if line.len() > 2 * 1024 * 1024 || !line.ends_with('\n') {
        return Err("Invalid guest input".into());
    }
    let input: Input = serde_json::from_str(&line)?;
    let deadline = Instant::now() + Duration::from_millis(input.timeout_ms);
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(64 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = Context::full(&runtime)?;
    let result = context.with(|ctx| -> rquickjs::Result<String> {
        ctx.globals()
            .set("__codeHost", Function::new(ctx.clone(), host)?)?;
        ctx.globals()
            .set("__codeInput", serde_json::to_string(&input).unwrap())?;
        let runner: Function = ctx.eval(include_str!("guest.js"))?;
        let promise: Promise = runner.call(())?;
        promise.finish::<String>()
    });
    let message = match result {
        Ok(value) if value.len() <= FRAME_LIMIT => serde_json::from_str::<Value>(&value)
            .unwrap_or_else(|_| json!({"kind":"failure","code":"INVALID_RESULT"})),
        Ok(_) => json!({"kind":"failure","code":"RESULT_LIMIT"}),
        Err(_) => {
            json!({"kind":"failure","code":if Instant::now()>=deadline {"CELL_TIMEOUT"} else {"JAVASCRIPT_ERROR"}})
        }
    };
    println!("{message}");
    Ok(())
}
