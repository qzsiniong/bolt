//! Script execution engine.
//!
//! Runs a script (JS or TS) in a fresh `rquickjs::AsyncRuntime` on a worker
//! thread. TypeScript is type-stripped via swc first. Host modules are
//! installed via the `HostLoader`/`HostResolver`, and a custom `console`
//! global captures output into the app's Console panel.

use std::path::PathBuf;
use std::sync::Arc;

use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module};

use super::interaction::InteractionBridge;
use super::ts_transform::strip_typescript;
use crate::console_channel::ConsoleChannel;
use crate::host::modules::bolt;
use crate::host::{HostLoader, HostRegistry, HostResolver};
use crate::model::script::Language;
use crate::modules::extensions::js_env::ExtensionRunScope;
use plux_js_manager::{ExtensionStore, InvokeRequest, JsError};
use plux_rs::Bundle;

/// 取消令牌由扩展运行时 crate 提供：命令执行与脚本执行共用同一个语义。
pub use plux_js_manager::CancelToken;

/// Host-side environment for one script or extension-command execution, grouped so
/// the run entry point does not take a long positional argument list.
pub(crate) struct RunEnv {
    /// Base directory used to resolve relative module imports.
    pub(crate) scripts_dir: PathBuf,
    pub(crate) registry: Arc<HostRegistry>,
    pub(crate) console: ConsoleChannel,
    pub(crate) cancel: CancelToken,
    pub(crate) interaction: InteractionBridge,
    /// 扩展运行时。普通脚本用不到；扩展命令缺了它就无法执行。
    pub(crate) extensions: Option<Arc<ExtensionStore>>,
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
        extensions: _,
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

/// 执行一条扩展命令。日志格式与脚本一致（▶ / ✔ / ⏹）；超时与取消共用同一个
/// `CancelToken`：用户中止后命令里的 `while(true)` 会被 QuickJS 中断。
pub fn run_extension(bundle: Bundle, command: &str, label: &str, env: RunEnv) {
    let RunEnv {
        console,
        cancel,
        interaction,
        extensions,
        ..
    } = env;
    console.info(format!("▶ Running {label}…"));

    let Some(extensions) = extensions else {
        console.error("扩展运行时不可用，无法执行扩展命令");
        return;
    };

    // 交互 API（confirm / pickPath / progress）挂在扩展常驻上下文上，
    // 每轮用本轮新建的 bridge 覆盖一次。
    let scope = Arc::new(ExtensionRunScope {
        bridge: interaction,
    });
    let request = InvokeRequest::new(command, cancel.clone()).with_scope(scope);

    match extensions.invoke(&bundle, request) {
        Ok(Some(output)) => {
            if !output.trim().is_empty() {
                console.info(output);
            }
            console.info(format!("✔ Finished {label}"));
        }
        Ok(None) => console.info(format!("✔ Finished {label}")),
        // 与脚本路径一致：exit() 是正常终止，退出码跟在 ✔ 后面。
        Err(JsError::Exit(code)) => console.info(format!("✔ Finished {label} (exit {code})")),
        Err(JsError::Cancelled) => console.warn(format!("⏹ Aborted {label}")),
        Err(JsError::Timeout(timeout)) => console.error(format!(
            "命令超时（{} ms），已中断：{label}",
            timeout.as_millis()
        )),
        // 运行时掉了（加载失败 / 线程退出）时，宿主下一次执行会先重建再跑；
        // 这里把"下一步该做什么"直接写清楚，别让人只看到一句线程错误。
        Err(JsError::NotLoaded(_)) => console.error(format!(
            "扩展运行时尚未就绪：{label}（重新执行该命令会自动加载）"
        )),
        Err(JsError::Runtime(message)) => console.error(format!(
            "{message}\n提示：重新执行该命令会自动重建扩展运行时；仍失败请查看上面的加载报错"
        )),
        Err(error) => console.error(error.to_string()),
    }
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
        if let Err(e) = bolt::install_console(&ctx, console) {
            console.error(format!("console install: {e}"));
        }
        // timer globals (setTimeout / clearTimeout / setInterval / clearInterval)
        if let Err(e) = bolt::install_timers(&ctx, console) {
            console.error(format!("timer install: {e}"));
        }
        // host module globals
        if let Err(e) = registry.install_globals(&ctx) {
            console.error(format!("module install: {e}"));
        }
        // Interaction APIs (typeStr / requestFocus / confirm / pickPath /
        // scanQr) extend the `__bolt` object that the line above created,
        // so they must be installed after it.
        if let Err(e) = bolt::install_interaction(&ctx, interaction) {
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
                        report_outcome(console, &e, file_name);
                    }
                }
            }
            Err(e) => {
                if !cancel.is_cancelled() {
                    let caught = rquickjs::CaughtError::from_error(&ctx, e);
                    report_outcome(console, &caught, file_name);
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

/// Report a failed evaluation. `bolt.exit(code)` unwinds by throwing the
/// sentinel `__bolt_exit_<code>__`; that is a normal termination, not an
/// error — report a clean finish with the code instead.
fn report_outcome(console: &ConsoleChannel, err: &rquickjs::CaughtError, file_name: &str) {
    if let rquickjs::CaughtError::Exception(ex) = err
        && let Some(msg) = ex.message()
        && let Some(code) = bolt::exit_code(&msg)
    {
        console.info(format!("✔ Finished {file_name} (exit {code})"));
        return;
    }
    log_caught(console, err);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::modules;
    use std::time::Duration;

    fn drain(channel: &ConsoleChannel) -> Vec<String> {
        channel
            .history()
            .entries()
            .into_iter()
            .map(|e| e.text)
            .collect()
    }

    fn make_runner() -> (Arc<HostRegistry>, ConsoleChannel) {
        let mut registry = HostRegistry::new();
        modules::register_all(&mut registry);
        let registry = Arc::new(registry);
        (registry, ConsoleChannel::new())
    }

    /// Bridge with no UI attached. Interaction APIs would fail through it, but
    /// none of the tests below call them.
    fn no_ui(cancel: &CancelToken) -> InteractionBridge {
        InteractionBridge::new(cancel.clone()).0
    }

    fn run_env(
        registry: Arc<HostRegistry>,
        channel: &ConsoleChannel,
        cancel: &CancelToken,
    ) -> RunEnv {
        RunEnv {
            scripts_dir: std::env::temp_dir(),
            registry,
            console: channel.clone(),
            cancel: cancel.clone(),
            interaction: no_ui(cancel),
            extensions: None,
        }
    }

    #[test]
    fn run_plain_js_console_log() {
        let (registry, channel) = make_runner();
        let cancel = CancelToken::new();
        run_script(
            "console.log('hello world'); console.info('info');",
            Language::JavaScript,
            "test.js",
            run_env(registry, &channel, &cancel),
        );
        let lines = drain(&channel);
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
        let (registry, channel) = make_runner();
        let cancel = CancelToken::new();
        let code = r#"
            import { env } from 'bolt';
            const name: string = env('USER') ?? 'world';
            console.log(`hello, ${name}`);
        "#;
        run_script(
            code,
            Language::TypeScript,
            "demo.ts",
            run_env(registry, &channel, &cancel),
        );
        let lines = drain(&channel);
        assert!(
            lines.iter().any(|l| l.contains("hello,")),
            "missing hello message: {lines:?}"
        );
    }

    #[test]
    fn run_sync_host_fn() {
        let (registry, channel) = make_runner();
        let cancel = CancelToken::new();
        let code = r#"
            import { writeFile, readFile } from 'bolt';
            writeFile('/tmp/bolt_test.txt', 'hi from script');
            const content = readFile('/tmp/bolt_test.txt');
            console.log(content);
        "#;
        run_script(
            code,
            Language::JavaScript,
            "test.js",
            run_env(registry, &channel, &cancel),
        );
        let lines = drain(&channel);
        assert!(
            lines.iter().any(|l| l.contains("hi from script")),
            "missing file content: {lines:?}"
        );
    }

    #[test]
    fn run_set_timeout() {
        let (registry, channel) = make_runner();
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
            run_env(registry, &channel, &cancel),
        );
        let lines = drain(&channel);
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
        let (registry, channel) = make_runner();
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
            run_env(registry, &channel, &cancel),
        );
        let lines = drain(&channel);
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
        let (registry, channel) = make_runner();
        let cancel = CancelToken::new();
        let killer = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            killer.cancel();
        });
        let started = std::time::Instant::now();
        run_script(
            "import { sleep } from 'bolt'; await sleep(60000); console.log('never');",
            Language::JavaScript,
            "sleep.js",
            run_env(registry, &channel, &cancel),
        );
        let lines = drain(&channel);
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
        let (registry, channel) = make_runner();
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
            run_env(registry, &channel, &cancel),
        );
        let lines = drain(&channel);
        assert!(
            lines.iter().any(|l| l.contains("Aborted")),
            "missing abort notice: {lines:?}"
        );
    }

    /// 真扩展 + 真 `ExtensionStore` 的 RunEnv：验证扩展路径与脚本路径的输出对齐。
    fn extension_env(
        tmp: &crate::test_support::TempDir,
        dir: &str,
        manifest: &str,
        entry: &str,
        registry: Arc<HostRegistry>,
        channel: &ConsoleChannel,
        cancel: &CancelToken,
    ) -> (RunEnv, Arc<plux_js_manager::ExtensionStore>) {
        let root = tmp.0.join(dir);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(plux_js_manager::MANIFEST_FILE), manifest).unwrap();
        std::fs::write(root.join("index.js"), entry).unwrap();

        let env = Arc::new(crate::modules::extensions::js_env::BoltJsEnv {
            registry: registry.clone(),
            console: channel.clone(),
        });
        let store = plux_js_manager::ExtensionStore::new(env);
        // 目录名就是 bundle（`{id}-v{version}.js`）：身份只在一处解析。
        let bundle = plux_rs::Bundle::from_filename(dir).expect("valid bundle");
        store
            .ensure_loaded(&bundle, &root, &root.join("index.js"), &[])
            .unwrap();

        let env = RunEnv {
            scripts_dir: tmp.0.clone(),
            registry,
            console: channel.clone(),
            cancel: cancel.clone(),
            interaction: no_ui(cancel),
            extensions: Some(store.clone()),
        };
        (env, store)
    }

    #[test]
    fn run_extension_reports_the_exit_sentinel_as_finished() {
        let tmp = crate::test_support::TempDir::new("runner-extension-exit");
        let (registry, channel) = make_runner();
        let cancel = CancelToken::new();
        let (env, _store) = extension_env(
            &tmp,
            "quit-v1.0.0.js",
            r#"{ "id": "quit", "version": "1.0.0", "commands": [{ "id": "quit", "title": "Quit" }] }"#,
            "import { exit } from 'bolt';\nexport function quit() { exit(2); }",
            registry,
            &channel,
            &cancel,
        );

        run_extension(
            Bundle::from_filename("quit-v1.0.0.js").unwrap(),
            "quit",
            "Quit · Quit",
            env,
        );

        let lines = drain(&channel);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("✔ Finished") && line.contains("(exit 2)")),
            "exit must be reported as a clean finish: {lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("__bolt_exit_")),
            "the sentinel must never leak into the console: {lines:?}"
        );
    }

    #[test]
    fn run_extension_hints_that_a_dead_runtime_is_rebuilt() {
        let tmp = crate::test_support::TempDir::new("runner-extension-dead");
        let (registry, channel) = make_runner();
        let cancel = CancelToken::new();
        let (env, _store) = extension_env(
            &tmp,
            "dead-v1.0.0.js",
            r#"{ "id": "dead", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "throw new Error('top-level boom');",
            registry,
            &channel,
            &cancel,
        );

        run_extension(
            Bundle::from_filename("dead-v1.0.0.js").unwrap(),
            "go",
            "Dead · Go",
            env,
        );

        let lines = drain(&channel);
        assert!(
            lines.iter().any(|line| line.contains("自动重建")),
            "a dead runtime must explain how to recover: {lines:?}"
        );
    }
}
