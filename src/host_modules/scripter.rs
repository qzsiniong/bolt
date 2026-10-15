//! Built-in `"scripter"` host module.
//!
//! Provides file I/O, HTTP, shell exec, env, and exit — the common building
//! blocks user scripts need. Sync functions are registered directly; `fetch`
//! and `exec` are async (return Promises) and run on the rquickjs scheduler.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use rquickjs::prelude::{Async, Opt, Rest};
use rquickjs::{Ctx, Error, Function, IntoJs, Object, Result, Value};

use crate::console_channel::ConsoleChannel;
use crate::host_module::{HostModule, HostRegistry};
use crate::interaction::{InteractionBridge, PickKind};

const DECLARATION: &str = r#"
  /** Sleep for the given number of milliseconds. */
  export function sleep(ms: number): Promise<void>;
  /** Read a UTF-8 text file synchronously. */
  export function readFile(path: string): string;
  /** Write a UTF-8 text file synchronously. */
  export function writeFile(path: string, content: string): void;
  /** List the entries of a directory. */
  export function listDir(path: string): string[];
  /** HTTP GET request. */
  export function fetch(url: string): Promise<{ status: number; body: string }>;
  /** Execute a shell command via `sh -c`. */
  export function exec(cmd: string): Promise<{ code: number; stdout: string; stderr: string }>;
  /** Get the value of an environment variable. */
  export function env(key: string): string | undefined;
  /** Terminate the running script with the given exit code. */
  export function exit(code: number): void;
  /** Type `text` into the focused window, one character at a time. */
  export function typeStr(text: string, delayMs?: number, label?: string): void;
  /** Wait until the user puts focus on the target window. */
  export function requestFocus(hint?: string): void;
  /** Ask the user a yes/no question; resolves false on "no", throws on abort. */
  export function confirm(message: string): Promise<boolean>;
  /** Let the user pick a file or directory; resolves null when cancelled. */
  export function pickPath(prompt?: string, kind?: "file" | "dir"): Promise<string | null>;
  /** Scan the screens for a QR code; resolves null when none was found. */
  export function scanQr(): Promise<string | null>;
"#;

pub fn register(registry: &mut HostRegistry) {
    registry.register(
        HostModule::new("scripter")
            .declaration(DECLARATION)
            .export("sleep")
            .export("readFile")
            .export("writeFile")
            .export("listDir")
            .export("fetch")
            .export("exec")
            .export("env")
            .export("exit")
            .export("typeStr")
            .export("requestFocus")
            .export("confirm")
            .export("pickPath")
            .export("scanQr")
            .installer(install),
    );
}

/// Install the `__scripter` global object with all native functions.
fn install<'js>(ctx: &Ctx<'js>) -> Result<()> {
    let g = Object::new(ctx.clone())?;

    g.set("sleep", Function::new(ctx.clone(), Async(sleep))?)?;
    g.set("readFile", Function::new(ctx.clone(), read_file)?)?;
    g.set("writeFile", Function::new(ctx.clone(), write_file)?)?;
    g.set("listDir", Function::new(ctx.clone(), list_dir)?)?;
    g.set("env", Function::new(ctx.clone(), env_var)?)?;
    g.set("exit", Function::new(ctx.clone(), exit_fn)?)?;
    g.set("fetch", Function::new(ctx.clone(), Async(fetch_url))?)?;
    g.set("exec", Function::new(ctx.clone(), Async(exec_cmd))?)?;

    ctx.globals().set("__scripter", g)?;
    Ok(())
}

// ── Console global ─────────────────────────────────────────────────────────

/// Install the `console` global object that captures output into the app's
/// Console panel. Called by the runner before script evaluation.
pub fn install_console<'js>(ctx: &Ctx<'js>, channel: &ConsoleChannel) -> Result<()> {
    use crate::console_channel::ConsoleLevel;

    let console = Object::new(ctx.clone())?;
    for (method, level) in [
        ("log", ConsoleLevel::Log),
        ("info", ConsoleLevel::Info),
        ("warn", ConsoleLevel::Warn),
        ("error", ConsoleLevel::Error),
        ("debug", ConsoleLevel::Log),
    ] {
        let ch = channel.clone();
        console.set(
            method,
            Function::new(ctx.clone(), move |args: Rest<Value>| -> Result<()> {
                let parts: Vec<String> = args.0.iter().map(|v| format_value(v)).collect();
                ch.send(level, parts.join(" "));
                Ok(())
            })?,
        )?;
    }
    ctx.globals().set("console", console)?;
    Ok(())
}

// ── Timer globals ──────────────────────────────────────────────────────────

/// Install the standard timer globals on the QuickJS context.
///
/// Timers are host APIs — QuickJS ships none of them, so `setTimeout` is
/// otherwise a `ReferenceError`. Each timer becomes a task on the rquickjs
/// scheduler (driven by the runner's `AsyncRuntime::idle`) that sleeps on
/// tokio's timer, so the engine keeps running other JS while a timer is
/// pending. Ids are plain numbers; `clearTimeout`/`clearInterval` only mark an
/// id as cancelled, because a spawned task cannot be aborted from outside.
pub fn install_timers<'js>(ctx: &Ctx<'js>, console: &ConsoleChannel) -> Result<()> {
    let next_id = Rc::new(Cell::new(1u32));
    // Ids the script has cleared; the spawned task re-checks this right before
    // firing. A cleared id also stops the task from firing again.
    let cancelled = Rc::new(RefCell::new(HashSet::<u32>::new()));
    let globals = ctx.globals();

    let ids = next_id.clone();
    let cancelled_ids = cancelled.clone();
    let ch = console.clone();
    globals.set(
        "setTimeout",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, cb: Function<'js>, ms: f64| -> u32 {
                let id = ids.get();
                ids.set(id.wrapping_add(1));
                let cancelled = cancelled_ids.clone();
                let ch = ch.clone();
                let timer_ctx = ctx.clone();
                ctx.spawn(async move {
                    tokio::time::sleep(ms_to_duration(ms)).await;
                    let aborted = cancelled.borrow().contains(&id);
                    if aborted {
                        return;
                    }
                    if let Err(e) = cb.call::<_, Value>(()) {
                        let caught = rquickjs::CaughtError::from_error(&timer_ctx, e);
                        crate::runner::log_caught(&ch, &caught);
                    }
                });
                id
            },
        )?,
    )?;

    let ids = next_id.clone();
    let cancelled_ids = cancelled.clone();
    let ch = console.clone();
    globals.set(
        "setInterval",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, cb: Function<'js>, ms: f64| -> u32 {
                let id = ids.get();
                ids.set(id.wrapping_add(1));
                let cancelled = cancelled_ids.clone();
                let ch = ch.clone();
                let timer_ctx = ctx.clone();
                // Clamp to 1ms: a zero-delay interval would spin the scheduler.
                let period = ms_to_duration(ms).max(Duration::from_millis(1));
                ctx.spawn(async move {
                    loop {
                        tokio::time::sleep(period).await;
                        let aborted = cancelled.borrow().contains(&id);
                        if aborted {
                            return;
                        }
                        if let Err(e) = cb.call::<_, Value>(()) {
                            let caught = rquickjs::CaughtError::from_error(&timer_ctx, e);
                            crate::runner::log_caught(&ch, &caught);
                            return;
                        }
                    }
                });
                id
            },
        )?,
    )?;

    let cancelled_ids = cancelled.clone();
    let clear = Function::new(ctx.clone(), move |id: Value<'js>| {
        if let Some(id) = id.as_int() {
            cancelled_ids.borrow_mut().insert(id as u32);
        }
    })?;
    globals.set("clearTimeout", clear.clone())?;
    globals.set("clearInterval", clear)?;

    Ok(())
}

/// Coerce a JS delay to a duration; negative or non-finite delays fire
/// immediately (`setTimeout(fn, -1)` behaves like `setTimeout(fn, 0)`).
fn ms_to_duration(ms: f64) -> Duration {
    if ms.is_finite() && ms > 0.0 {
        Duration::from_millis(ms as u64)
    } else {
        Duration::ZERO
    }
}

/// Best-effort `String(value)` conversion for console output.
fn format_value<'js>(v: &Value<'js>) -> String {
    use rquickjs::FromJs;
    let ctx = v.ctx().clone();
    // Try direct string first (avoids quoting).
    if let Ok(s) = String::from_js(&ctx, v.clone()) {
        return s;
    }
    // Fall back to JS String() coercion.
    if let Ok(to_str) = ctx.globals().get::<_, Function>("String")
        && let Ok(s) = to_str.call::<_, String>((v.clone(),))
    {
        return s;
    }
    "[object]".into()
}

// ── Sync functions ─────────────────────────────────────────────────────────

fn read_file(path: String) -> Result<String> {
    std::fs::read_to_string(&path).map_err(Error::Io)
}

fn write_file(path: String, content: String) -> Result<()> {
    std::fs::write(&path, content).map_err(Error::Io)
}

fn list_dir(path: String) -> Result<Vec<String>> {
    let entries = std::fs::read_dir(&path).map_err(Error::Io)?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(Error::Io)?;
        out.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(out)
}

fn env_var(key: String) -> Result<Option<String>> {
    Ok(std::env::var(&key).ok())
}

/// `exit(code)` — throws a sentinel string the runner detects.
fn exit_fn<'js>(ctx: Ctx<'js>, code: i32) -> Result<()> {
    let msg = format!("__scripter_exit_{}__", code);
    let val = msg.into_js(&ctx)?;
    Err(ctx.throw(val))
}

// ── Interaction functions ──────────────────────────────────────────────────

/// Install the host interaction APIs onto the existing `__scripter` object.
///
/// Must run *after* [`install`] (via `HostRegistry::install_globals`), which is
/// what creates `__scripter`; the loader's re-export module reads that same
/// object when a script does `import { confirm } from 'scripter'`.
pub fn install_interaction<'js>(ctx: &Ctx<'js>, bridge: &InteractionBridge) -> Result<()> {
    let scripter: Object = ctx.globals().get("__scripter")?;

    let b = bridge.clone();
    scripter.set(
        "typeStr",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>,
                  text: String,
                  delay_ms: Opt<f64>,
                  label: Opt<String>|
                  -> Result<()> {
                let delay_ms = delay_ms
                    .0
                    .map(|v| v.max(0.0) as u64)
                    .unwrap_or(crate::typer::DEFAULT_DELAY_MS);
                let label = label.0.as_deref();
                crate::typer::type_str(&text, delay_ms, label, &b)
                    .map_err(|msg| throw_error(&ctx, msg))
            },
        )?,
    )?;

    let b = bridge.clone();
    scripter.set(
        "requestFocus",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, hint: Opt<String>| -> Result<()> {
                let hint = hint.0.unwrap_or_else(default_focus_hint);
                b.request_focus(hint).map_err(|msg| throw_error(&ctx, msg))
            },
        )?,
    )?;

    let b = bridge.clone();
    scripter.set(
        "confirm",
        Function::new(
            ctx.clone(),
            Async(move |ctx: Ctx<'js>, message: String| -> _ {
                let b = b.clone();
                async move {
                    b.confirm(message)
                        .await
                        .map_err(|msg| throw_error(&ctx, msg))
                }
            }),
        )?,
    )?;

    let b = bridge.clone();
    scripter.set(
        "pickPath",
        Function::new(
            ctx.clone(),
            Async(
                move |ctx: Ctx<'js>, prompt: Opt<String>, kind: Opt<String>| -> _ {
                    let b = b.clone();
                    async move {
                        let kind = match kind.0.as_deref() {
                            None | Some("file") => PickKind::File,
                            Some("dir") => PickKind::Dir,
                            Some(other) => {
                                return Err(throw_error(
                                    &ctx,
                                    format!("kind 只支持 \"file\" / \"dir\"，收到\"{other}\""),
                                ));
                            }
                        };
                        let prompt = prompt.0.unwrap_or_else(|| default_pick_prompt(kind));
                        match b.pick_path(prompt, kind).await {
                            Ok(Some(path)) => path.to_string_lossy().into_owned().into_js(&ctx),
                            Ok(None) => Ok(Value::new_null(ctx.clone())),
                            Err(msg) => Err(throw_error(&ctx, msg)),
                        }
                    }
                },
            ),
        )?,
    )?;

    let b = bridge.clone();
    scripter.set(
        "scanQr",
        Function::new(
            ctx.clone(),
            Async(move |ctx: Ctx<'js>| -> _ {
                let b = b.clone();
                async move {
                    // Capturing and decoding block; keep them off the runtime thread.
                    let scanned = tokio::task::spawn_blocking(move || crate::qr::scan_qr(&b))
                        .await
                        .map_err(|e| throw_error(&ctx, format!("截屏任务失败：{e}")))?;
                    match scanned {
                        Ok(Some(text)) => text.into_js(&ctx),
                        Ok(None) => Ok(Value::new_null(ctx.clone())),
                        Err(msg) => Err(throw_error(&ctx, msg)),
                    }
                }
            }),
        )?,
    )?;

    Ok(())
}

fn default_focus_hint() -> String {
    crate::typer::DEFAULT_FOCUS_HINT.to_string()
}

fn default_pick_prompt(kind: PickKind) -> String {
    match kind {
        PickKind::File => "选择文件".to_string(),
        PickKind::Dir => "选择文件夹".to_string(),
    }
}

/// Build an `Error` object for `msg` and set it as the pending exception.
fn throw_error<'js>(ctx: &Ctx<'js>, msg: impl Into<String>) -> Error {
    let message = msg.into();
    match rquickjs::Exception::from_message(ctx.clone(), &message) {
        Ok(exception) => exception.throw(),
        Err(e) => e,
    }
}

// ── Async functions ─────────────────────────────────────────────────────────

/// `sleep(ms) -> Promise<void>`
///
/// 走 tokio 定时器，不占用线程；等待期间引擎会继续驱动其他 JS 任务，
/// 因此 `Promise.all` 里的多个 `sleep` 是并行的。
pub async fn sleep<'js>(ctx: Ctx<'js>, ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

async fn fetch_url<'js>(ctx: Ctx<'js>, url: String) -> Result<Value<'js>> {
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    let status = resp.status().as_u16() as i32;
    let body = resp
        .text()
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;

    let obj = Object::new(ctx.clone())?;
    obj.set("status", status)?;
    obj.set("body", body)?;
    obj.into_js(&ctx)
}

async fn exec_cmd<'js>(ctx: Ctx<'js>, cmd: String) -> Result<Value<'js>> {
    let out = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        // An aborted run drops this future; without kill_on_drop the child
        // would outlive the script as an orphan.
        .kill_on_drop(true)
        .output()
        .await?;

    let obj = Object::new(ctx.clone())?;
    obj.set("code", out.status.code().unwrap_or(-1))?;
    obj.set("stdout", String::from_utf8_lossy(&out.stdout).into_owned())?;
    obj.set("stderr", String::from_utf8_lossy(&out.stderr).into_owned())?;
    obj.into_js(&ctx)
}
