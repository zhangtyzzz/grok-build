//! Isolated JavaScript tool composition. No session, workspace, or credential access.
//! The parent owns authorization, admission, transcripts, and cancellation.
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use rquickjs::{Context, FromJs, Function, Promise, Runtime, Value};
use serde::{Deserialize, Serialize};

pub const WORKER_ARG: &str = "--internal-code-mode-worker";
pub const ENV_ENABLED: &str = "GROK_CODE_MODE";
pub const MAX_CODE_BYTES: usize = 64 * 1024;
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Largest final value the worker hands to the host; the host inlines at most
/// `MAX_OUTPUT_BYTES` for the model and writes larger values to a file.
pub const MAX_RESULT_BYTES: usize = MAX_FRAME_BYTES - 2 * MAX_OUTPUT_BYTES;
pub const MAX_CALLS: usize = 64;
pub const MAX_IN_FLIGHT: usize = 8;
pub const WALL_TIMEOUT: Duration = Duration::from_secs(120);
const ACTIVE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Init {
    pub code: String,
    /// Exact names and schemas from the host's immutable per-run inventory.
    pub catalog: Vec<serde_json::Value>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerMessage {
    Call {
        id: u64,
        name: String,
        arguments: serde_json::Value,
    },
    Finished {
        ok: bool,
        value: serde_json::Value,
        error: Option<String>,
        logs: Vec<String>,
        warnings: Vec<String>,
    },
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub id: u64,
    pub ok: bool,
    /// Serialized JSON data, never executable source.
    pub payload: String,
}

pub fn write_frame(mut writer: impl Write, value: &impl Serialize) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::other("Code Mode frame exceeds limit"));
    }
    let len = u32::try_from(bytes.len()).map_err(std::io::Error::other)?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}
pub fn read_frame<T: serde::de::DeserializeOwned>(mut reader: impl Read) -> std::io::Result<T> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let len = u32::from_le_bytes(size) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::other("Code Mode frame exceeds limit"));
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Called by the executable before normal CLI, credentials, or telemetry startup.
pub fn run_worker() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let init: Init = read_frame(&mut reader)?;
    execute(init, &mut reader, stdout)
}

fn execute(init: Init, mut reader: impl Read, writer: impl Write + 'static) -> std::io::Result<()> {
    if init.code.len() > MAX_CODE_BYTES {
        return Err(std::io::Error::other("Code Mode source exceeds limit"));
    }
    let writer = Rc::new(RefCell::new(writer));
    let rt = Runtime::new().map_err(std::io::Error::other)?;
    // Keep the default QuickJS allocator: rust-alloc/custom allocators disable this quota.
    rt.set_memory_limit(64 * 1024 * 1024);
    rt.set_max_stack_size(512 * 1024);
    let started = Instant::now();
    let waiting = Rc::new(Cell::new(Duration::ZERO));
    let waiting_interrupt = waiting.clone();
    rt.set_interrupt_handler(Some(Box::new(move || {
        started.elapsed().saturating_sub(waiting_interrupt.get()) > ACTIVE_TIMEOUT
    })));
    let unhandled = Rc::new(RefCell::new(HashSet::new()));
    let rejections = unhandled.clone();
    rt.set_host_promise_rejection_tracker(Some(Box::new(move |_, promise, _, handled| {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        promise.hash(&mut hash);
        let id = hash.finish();
        if handled {
            rejections.borrow_mut().remove(&id);
        } else if rejections.borrow().len() < 256 {
            rejections.borrow_mut().insert(id);
        }
    })));
    let logs = Rc::new(RefCell::new(Vec::new()));
    let logs_bytes = Rc::new(Cell::new(0usize));
    let calls = Rc::new(Cell::new(0u64));
    let pending = Rc::new(RefCell::new(HashSet::new()));
    let accepting = Rc::new(Cell::new(true));
    let ctx = Context::full(&rt).map_err(std::io::Error::other)?;
    let result = ctx.with(|ctx| -> rquickjs::Result<serde_json::Value> {
        let tx = writer.clone();
        let seq = calls.clone();
        let admitted = pending.clone();
        let call_admission = accepting.clone();
        ctx.globals().set(
            "__hostCall",
            Function::new(
                ctx.clone(),
                move |ctx: rquickjs::Ctx<'_>,
                      name: String,
                      arguments: String|
                      -> rquickjs::Result<u64> {
                    if !call_admission.get() {
                        return Err(rquickjs::Exception::throw_message(
                            &ctx,
                            "Code Mode program has settled; new calls are closed",
                        ));
                    }
                    let id = seq.get() + 1;
                    if id > MAX_CALLS as u64 || admitted.borrow().len() >= MAX_IN_FLIGHT {
                        return Err(rquickjs::Exception::throw_message(
                            &ctx,
                            "Code Mode tool-call or concurrency limit exceeded",
                        ));
                    }
                    let arguments = serde_json::from_str(&arguments).map_err(|_| {
                        rquickjs::Exception::throw_message(&ctx, "Invalid tool input")
                    })?;
                    write_frame(
                        &mut *tx.borrow_mut(),
                        &WorkerMessage::Call {
                            id,
                            name,
                            arguments,
                        },
                    )
                    .map_err(|_| {
                        rquickjs::Exception::throw_message(
                            &ctx,
                            "Code Mode host disconnected or input exceeded limit",
                        )
                    })?;
                    seq.set(id);
                    admitted.borrow_mut().insert(id);
                    Ok(id)
                },
            )?,
        )?;
        let output = logs.clone();
        let size = logs_bytes.clone();
        ctx.globals().set(
            "__hostLog",
            Function::new(
                ctx.clone(),
                move |ctx: rquickjs::Ctx<'_>, message: String| -> rquickjs::Result<()> {
                    let next = size.get().saturating_add(message.len());
                    if next > MAX_OUTPUT_BYTES || output.borrow().len() >= 256 {
                        return Err(rquickjs::Exception::throw_message(
                            &ctx,
                            "Code Mode log limit exceeded",
                        ));
                    }
                    size.set(next);
                    output.borrow_mut().push(message);
                    Ok(())
                },
            )?,
        )?;
        ctx.globals().set(
            "__inventoryJson",
            serde_json::to_string(&init.catalog).map_err(|_| rquickjs::Error::Unknown)?,
        )?;
        // Bootstrap returns private references, which user code cannot replace through globals.
        let api: rquickjs::Object = ctx.eval(include_str!("bootstrap.js"))?;
        let settle: Function = api.get("settle")?;
        let serialize: Function = api.get("serialize")?;
        let source = format!("(async function() {{ 'use strict';\n{}\n}})()", init.code);
        let program: Promise = ctx.eval(source)?;
        loop {
            // Stop admitting work as soon as the root settles. Draining the entire
            // microtask queue first would let fire-and-forget continuations call
            // tools after an explicit return or fail-fast rejection.
            while program.state() == rquickjs::promise::PromiseState::Pending
                && ctx.execute_pending_job()
            {
                if started.elapsed().saturating_sub(waiting.get()) > ACTIVE_TIMEOUT {
                    return Err(rquickjs::Exception::throw_message(
                        &ctx,
                        "Code Mode computation timeout",
                    ));
                }
            }
            if let Some(result) = program.result::<Value>() {
                accepting.set(false);
                let value = result?;
                let json: String = serialize.call((value,))?;
                if json.len() > MAX_RESULT_BYTES {
                    return Err(rquickjs::Exception::throw_message(
                        &ctx,
                        "Code Mode result exceeds the transfer limit; return a smaller summary",
                    ));
                }
                return serde_json::from_str(&json).map_err(|_| rquickjs::Error::Unknown);
            }
            if pending.borrow().is_empty() {
                return Err(rquickjs::Exception::throw_message(
                    &ctx,
                    "Code Mode program is waiting on a promise with no host work",
                ));
            }
            let before_wait = Instant::now();
            let reply: Reply = read_frame(&mut reader).map_err(|_| {
                rquickjs::Exception::throw_message(&ctx, "Code Mode host disconnected")
            })?;
            waiting.set(waiting.get() + before_wait.elapsed());
            if !pending.borrow_mut().remove(&reply.id) {
                return Err(rquickjs::Exception::throw_message(
                    &ctx,
                    "Invalid Code Mode reply ID",
                ));
            }
            settle.call::<_, ()>((reply.id, reply.ok, reply.payload))?;
        }
    });
    accepting.set(false);
    let mut warnings = Vec::new();
    if rt.is_job_pending() {
        warnings.push("Unawaited JavaScript jobs discarded when the program settled".to_owned());
    }
    if !pending.borrow().is_empty() {
        warnings.push(format!(
            "{} unawaited host calls interrupted; completed effects are not rolled back",
            pending.borrow().len()
        ));
    }
    if !unhandled.borrow().is_empty() {
        warnings.push(format!(
            "{} unhandled promise rejection(s)",
            unhandled.borrow().len()
        ));
    }
    let (ok, value, error) = match result {
        Ok(value) => (true, value, None),
        Err(error) => {
            let message = ctx.with(|ctx| {
                if error.is_exception() {
                    let exception = ctx.catch();
                    rquickjs::Coerced::<String>::from_js(&ctx, exception)
                        .map(|s| s.0)
                        .unwrap_or_else(|_| "JavaScript execution failed".to_owned())
                } else {
                    error.to_string()
                }
            });
            (
                false,
                serde_json::Value::Null,
                Some(truncate_error(message)),
            )
        }
    };
    write_frame(
        &mut *writer.borrow_mut(),
        &WorkerMessage::Finished {
            ok,
            value,
            error,
            logs: std::mem::take(&mut *logs.borrow_mut()),
            warnings,
        },
    )
}

/// Thrown values are user-controlled: bound them like results so the final frame fits.
fn truncate_error(mut message: String) -> String {
    if message.len() > MAX_OUTPUT_BYTES {
        let mut end = MAX_OUTPUT_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push_str("… [error truncated]");
    }
    message
}

#[cfg(test)]
mod tests;
