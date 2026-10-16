//! Script execution engine.
//!
//! Runs a script (JS or TS) in a fresh `rquickjs::AsyncRuntime` on a worker
//! thread. TypeScript is type-stripped via swc first. Host modules are
//! installed via the `HostLoader`/`HostResolver`, and a custom `console`
//! global captures output into the app's Console panel.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module};

use super::interaction::InteractionBridge;
use super::ts_transform::strip_typescript;
use crate::console_channel::ConsoleChannel;
use crate::host::modules::scripter;
use crate::host::{HostLoader, HostRegistry, HostResolver};
use crate::model::script::Language;

/// Cooperative cancellation token shared between the UI and a running script.
///
/// Two independent mechanisms watch it: QuickJS's interrupt handler preempts
/// long-running JS, and an async wait breaks the run out of host functions
/// (`sleep`/`exec`) and `AsyncRuntime::idle`, where no JS executes.
#[derive(Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request an abort. Idempotent and callable from any thread.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Resolve once an abort has been requested (50ms polling).
    pub async fn wait(&self) {
        while !self.is_cancelled() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Host-side environment for one script execution, grouped so the run entry
/// point does not take a long positional argument list.
pub(crate) struct RunEnv {
    /// Base directory used to resolve relative module imports.
    pub(crate) scripts_dir: PathBuf,
    pub(crate) registry: Arc<HostRegistry>,
    pub(crate) console: ConsoleChannel,
    pub(crate) cancel: CancelToken,
    pub(crate) interaction: InteractionBridge,
}

/// Run a script to completion. Call from a worker thread — never on the GPUI
/// main thread.
pub fn run_script(code: &str, language: Language, file_name: &str, env: RunEnv) {
    let RunEnv {
        scripts_dir,
        registry,
        console,
        cancel,
        interaction,
    } = env;
    console.info(format!("▶ Running {}…", file_name));

    // 1. Type-strip TypeScript → JS (imports preserved).
    let js_code = match language {
        Language::TypeScript => match strip_typescript(code) {
            Ok(js) => js,
            Err(e) => {
                console.error(format!("TS transform failed:\n{e}"));
                return;
            }
        },
        Language::JavaScript => code.to_string(),
    };

    // 2. Create a tokio runtime on this thread for async host fns (fetch/exec).
    let tokio_rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            console.error(format!("Failed to start async runtime: {e}"));
            return;
        }
    };

    let body_cancel = cancel.clone();
    let file_name = file_name.to_string();
    // Race the run against the cancel token. A script blocked inside a host
    // function (`sleep`/`exec`) or waiting in `idle()` executes no JS, so the
    // interrupt handler never fires — dropping the future is the only way out.
    // `select!` drops the losing branch, tearing the runtime and every
    // spawned timer task down with it.
    tokio_rt.block_on(async move {
        tokio::select! {
            outcome = run_async(
                &js_code,
                &file_name,
                scripts_dir,
                &registry,
                &console,
                &body_cancel,
                &interaction,
            ) => {
                if cancel.is_cancelled() {
                    // run_async suppressed its own error log while cancelled.
                    console.warn(format!("⏹ Aborted {file_name}"));
                } else if let Err(e) = outcome {
                    console.error(e);
                } else {
                    console.info(format!("✔ Finished {file_name}"));
                }
            }
            _ = cancel.wait() => {
                console.warn(format!("⏹ Aborted {file_name}"));
            }
        }
    });
}

async fn run_async(
    js_code: &str,
    file_name: &str,
    scripts_dir: PathBuf,
    registry: &Arc<HostRegistry>,
    console: &ConsoleChannel,
    cancel: &CancelToken,
    interaction: &InteractionBridge,
) -> Result<(), String> {
    // 3. Create QuickJS runtime + install host loader/resolver.
    let rt = AsyncRuntime::new().map_err(|e| e.to_string())?;
    rt.set_max_stack_size(8 * 1024 * 1024).await;
    // Preempt long-running JS (`while (true) {}`): QuickJS polls this handler
    // periodically and aborts with an uncatchable exception when it returns
    // true.
    let interrupted = cancel.clone();
    rt.set_interrupt_handler(Some(Box::new(move || interrupted.is_cancelled())))
        .await;
    let loader = HostLoader::new(registry.clone(), scripts_dir);
    rt.set_loader(HostResolver, loader).await;

    // 4. Create context.
    let ctx = AsyncContext::full(&rt).await.map_err(|e| e.to_string())?;

    // 5. Inside the context: install console + host module globals, then eval.
    ctx.async_with(async |ctx| {
        // console global
        if let Err(e) = scripter::install_console(&ctx, console) {
            console.error(format!("console install: {e}"));
        }
        // timer globals (setTimeout / clearTimeout / setInterval / clearInterval)
        if let Err(e) = scripter::install_timers(&ctx, console) {
            console.error(format!("timer install: {e}"));
        }
        // host module globals
        if let Err(e) = registry.install_globals(&ctx) {
            console.error(format!("module install: {e}"));
        }
        // Interaction APIs (typeStr / requestFocus / confirm / pickPath /
        // scanQr) extend the `__scripter` object that the line above created,
        // so they must be installed after it.
        if let Err(e) = scripter::install_interaction(&ctx, interaction) {
            console.error(format!("interaction install: {e}"));
        }

        // Evaluate as an ES module (resolves imports via the loader).
        let result = Module::evaluate(ctx.clone(), file_name, js_code);
        match result {
            Ok(promise) => {
                // Drive the module promise to completion.
                if let Err(e) = promise.into_future::<()>().await.catch(&ctx) {
                    // A cancelled run bails out with the interrupt exception;
                    // the caller reports the abort instead.
                    if !cancel.is_cancelled() {
                        log_caught(console, &e);
                    }
                }
            }
            Err(e) => {
                if !cancel.is_cancelled() {
                    let caught = rquickjs::CaughtError::from_error(&ctx, e);
                    log_caught(console, &caught);
                }
            }
        }
    })
    .await;

    // 6. Drain remaining async jobs (fetch/exec Promises, setTimeout-like) —
    // unless cancelled: a leftover `setInterval` or pending host call would
    // otherwise keep `idle()` waiting forever.
    if !cancel.is_cancelled() {
        rt.idle().await;
    }

    Ok(())
}

pub(crate) fn log_caught(console: &ConsoleChannel, err: &rquickjs::CaughtError) {
    match err {
        rquickjs::CaughtError::Exception(ex) => {
            console.error(ex.to_string());
        }
        rquickjs::CaughtError::Value(v) => {
            console.error(format!("{:?}", v));
        }
        rquickjs::CaughtError::Error(e) => {
            console.error(e.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules;

    fn drain(
        rx: &crossbeam_channel::Receiver<crate::console_channel::ConsoleEntry>,
    ) -> Vec<String> {
        rx.try_iter().map(|e| e.text).collect()
    }

    fn make_runner() -> (
        Arc<HostRegistry>,
        ConsoleChannel,
        crossbeam_channel::Receiver<crate::console_channel::ConsoleEntry>,
    ) {
        let mut registry = HostRegistry::new();
        modules::register_all(&mut registry);
        let registry = Arc::new(registry);
        let (channel, rx) = ConsoleChannel::new();
        (registry, channel, rx)
    }

    /// Bridge with no UI attached. Interaction APIs would fail through it, but
    /// none of the tests below call them.
    fn no_ui(cancel: &CancelToken) -> InteractionBridge {
        InteractionBridge::new(cancel.clone()).0
    }

    fn run_env(
        registry: Arc<HostRegistry>,
        channel: ConsoleChannel,
        cancel: &CancelToken,
    ) -> RunEnv {
        RunEnv {
            scripts_dir: std::env::temp_dir(),
            registry,
            console: channel,
            cancel: cancel.clone(),
            interaction: no_ui(cancel),
        }
    }

    #[test]
    fn run_plain_js_console_log() {
        let (registry, channel, rx) = make_runner();
        let cancel = CancelToken::new();
        run_script(
            "console.log('hello world'); console.info('info');",
            Language::JavaScript,
            "test.js",
            run_env(registry, channel, &cancel),
        );
        let lines = drain(&rx);
        assert!(
            lines.iter().any(|l| l.contains("hello world")),
            "missing hello world: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("info")),
            "missing info: {lines:?}"
        );
    }

    #[test]
    fn run_ts_with_import() {
        let (registry, channel, rx) = make_runner();
        let cancel = CancelToken::new();
        let code = r#"
            import { env } from 'scripter';
            const name: string = env('USER') ?? 'world';
            console.log(`hello, ${name}`);
        "#;
        run_script(
            code,
            Language::TypeScript,
            "demo.ts",
            run_env(registry, channel, &cancel),
        );
        let lines = drain(&rx);
        assert!(
            lines.iter().any(|l| l.contains("hello,")),
            "missing hello message: {lines:?}"
        );
    }

    #[test]
    fn run_sync_host_fn() {
        let (registry, channel, rx) = make_runner();
        let cancel = CancelToken::new();
        let code = r#"
            import { writeFile, readFile } from 'scripter';
            writeFile('/tmp/scripter_test.txt', 'hi from script');
            const content = readFile('/tmp/scripter_test.txt');
            console.log(content);
        "#;
        run_script(
            code,
            Language::JavaScript,
            "test.js",
            run_env(registry, channel, &cancel),
        );
        let lines = drain(&rx);
        assert!(
            lines.iter().any(|l| l.contains("hi from script")),
            "missing file content: {lines:?}"
        );
    }

    #[test]
    fn run_set_timeout() {
        let (registry, channel, rx) = make_runner();
        let cancel = CancelToken::new();
        let code = r#"
            console.log('start');
            setTimeout(() => console.log('timeout'), 30);
            console.log('end');
        "#;
        run_script(
            code,
            Language::JavaScript,
            "test.js",
            run_env(registry, channel, &cancel),
        );
        let lines = drain(&rx);
        // The callback must observe the full top-level execution first: the
        // run only finishes after pending timers have fired.
        let start = lines.iter().position(|l| l.contains("start")).unwrap();
        let end = lines.iter().position(|l| l.contains("end")).unwrap();
        let timeout = lines
            .iter()
            .position(|l| l.contains("timeout"))
            .unwrap_or_else(|| panic!("timeout callback never ran: {lines:?}"));
        assert!(start < end && end < timeout, "unexpected order: {lines:?}");
    }

    #[test]
    fn run_clear_interval_stops_repeating() {
        let (registry, channel, rx) = make_runner();
        let cancel = CancelToken::new();
        let code = r#"
            let n = 0;
            const id = setInterval(() => {
                n += 1;
                console.log(`tick ${n}`);
                if (n >= 2) clearInterval(id);
            }, 10);
        "#;
        run_script(
            code,
            Language::JavaScript,
            "test.js",
            run_env(registry, channel, &cancel),
        );
        let lines = drain(&rx);
        let ticks: Vec<&String> = lines.iter().filter(|l| l.contains("tick")).collect();
        assert_eq!(
            ticks.len(),
            2,
            "interval did not stop after clear: {lines:?}"
        );
        assert!(ticks[0].contains("tick 1") && ticks[1].contains("tick 2"));
    }

    #[test]
    fn abort_while_awaiting_host_call() {
        let (registry, channel, rx) = make_runner();
        let cancel = CancelToken::new();
        let killer = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            killer.cancel();
        });
        let started = std::time::Instant::now();
        run_script(
            "import { sleep } from 'scripter'; await sleep(60000); console.log('never');",
            Language::JavaScript,
            "sleep.js",
            run_env(registry, channel, &cancel),
        );
        let lines = drain(&rx);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "abort did not interrupt the host call: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("Aborted")),
            "missing abort notice: {lines:?}"
        );
    }

    #[test]
    fn abort_infinite_loop() {
        let (registry, channel, rx) = make_runner();
        let cancel = CancelToken::new();
        let killer = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            killer.cancel();
        });
        // Without a working interrupt handler this never returns.
        run_script(
            "while (true) {}",
            Language::JavaScript,
            "loop.js",
            run_env(registry, channel, &cancel),
        );
        let lines = drain(&rx);
        assert!(
            lines.iter().any(|l| l.contains("Aborted")),
            "missing abort notice: {lines:?}"
        );
    }
}
