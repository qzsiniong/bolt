//! Built-in `"scripter"` host module.
//!
//! Provides file I/O, HTTP, shell exec, env, and exit — the common building
//! blocks user scripts need. Sync functions are registered directly; `fetch`
//! and `exec` are async (return Promises) and run on the rquickjs scheduler.

use std::time::Duration;

use rquickjs::prelude::{Async, Rest};
use rquickjs::{Ctx, Error, Function, IntoJs, Object, Result, Value};

use crate::console_channel::ConsoleChannel;
use crate::host_module::{HostModule, HostRegistry};

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
        .output()
        .await?;

    let obj = Object::new(ctx.clone())?;
    obj.set("code", out.status.code().unwrap_or(-1))?;
    obj.set("stdout", String::from_utf8_lossy(&out.stdout).into_owned())?;
    obj.set("stderr", String::from_utf8_lossy(&out.stderr).into_owned())?;
    obj.into_js(&ctx)
}
