//! Self-contained QuickJS async-module guest. Only the framed capability bridge
//! is installed; the process runs under the existing fail-closed OS sandbox.
use rquickjs::{Context, Function, Module, Runtime};
use std::io::{self, BufRead, Read, Write};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

fn exit_guest() {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{{\"kind\":\"done\"}}").and_then(|_| out.flush());
    // Only the disposable guest installs this function. A JavaScript catch
    // cannot intercept exit and the credential-bearing host stays alive.
    std::process::exit(0);
}

fn read_reply() -> rquickjs::Result<String> {
    let mut reply = String::new();
    io::stdin()
        .lock()
        .take(256 * 1024 + 1)
        .read_line(&mut reply)
        .map_err(|_| rquickjs::Error::Unknown)?;
    if reply.is_empty() || reply.len() > 256 * 1024 {
        return Err(rquickjs::Error::Unknown);
    }
    Ok(reply)
}

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin()
        .lock()
        .take(2 * 1024 * 1024 + 1)
        .read_line(&mut input)?;
    if input.len() > 2 * 1024 * 1024 {
        return Err("Bootstrap limit".into());
    }
    let input: serde_json::Value = serde_json::from_str(&input)?;
    let runtime = Runtime::new()?;
    runtime.set_memory_limit(64 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let context = Context::full(&runtime)?;
    let replies = Rc::new(RefCell::new(VecDeque::new()));
    context.with(|ctx| -> Result<(), Box<dyn std::error::Error>> {
        ctx.globals()
            .set("__liveExit", Function::new(ctx.clone(), exit_guest)?)?;
        let queued = replies.clone();
        ctx.globals().set(
            "__liveSync",
            Function::new(
                ctx.clone(),
                move |text: String| -> rquickjs::Result<String> {
                    if text.len() > 256 * 1024 {
                        return Err(rquickjs::Error::Unknown);
                    }
                    {
                        let mut out = io::stdout().lock();
                        writeln!(out, "{text}")
                            .and_then(|_| out.flush())
                            .map_err(|_| rquickjs::Error::Unknown)?;
                    }
                    loop {
                        let reply = read_reply()?;
                        let frame: serde_json::Value =
                            serde_json::from_str(&reply).map_err(|_| rquickjs::Error::Unknown)?;
                        if frame["sync"].as_bool() == Some(true) {
                            return Ok(reply);
                        }
                        queued.borrow_mut().push_back(reply);
                        if queued.borrow().len() > 1024 {
                            return Err(rquickjs::Error::Unknown);
                        }
                    }
                },
            )?,
        )?;
        ctx.globals().set(
            "__liveEmit",
            Function::new(ctx.clone(), |text: String| -> rquickjs::Result<()> {
                if text.len() > 256 * 1024 {
                    return Err(rquickjs::Error::Unknown);
                }
                let mut out = io::stdout().lock();
                writeln!(out, "{text}")
                    .and_then(|_| out.flush())
                    .map_err(|_| rquickjs::Error::Unknown)
            })?,
        )?;
        ctx.globals().set("__liveInput", input.to_string())?;
        let receiver: Function = ctx.eval(include_str!("live_guest.js"))?;
        let source = input["source"].as_str().ok_or("Missing source")?;
        let module = Module::declare(ctx.clone(), "cell.js", source);
        let promise = match module.and_then(|m| m.eval().map(|(_, promise)| promise)) {
            Ok(promise) => promise,
            Err(_) => {
                let error = ctx.catch();
                receiver.call::<_, ()>(("failure", error))?;
                return Ok(());
            }
        };
        // Attach completion before pumping jobs. Receiver is retained only by Rust.
        receiver.call::<_, ()>(("start", promise))?;
        loop {
            while ctx.execute_pending_job() {}
            let done: bool = receiver.call(("done", ()))?;
            if done {
                break;
            }
            let reply = match replies.borrow_mut().pop_front() {
                Some(reply) => reply,
                None => read_reply()?,
            };
            receiver.call::<_, ()>(("reply", reply))?;
        }
        Ok(())
    })
}
