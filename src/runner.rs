//! Script execution engine.
//!
//! Runs a script (JS or TS) in a fresh `rquickjs::AsyncRuntime` on a worker
//! thread. TypeScript is type-stripped via swc first. Host modules are
//! installed via the `HostLoader`/`HostResolver`, and a custom `console`
//! global captures output into the app's Console panel.

use std::path::PathBuf;
use std::sync::Arc;

use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module};

use crate::console_channel::ConsoleChannel;
use crate::host_module::{HostLoader, HostRegistry, HostResolver};
use crate::host_modules::scripter;
use crate::script::Language;
use crate::ts_transform::strip_typescript;

/// Run a script to completion. Call from a worker thread — never on the GPUI
/// main thread.
pub fn run_script(
    code: &str,
    language: Language,
    file_name: &str,
    scripts_dir: PathBuf,
    registry: &Arc<HostRegistry>,
    console: ConsoleChannel,
) {
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

    tokio_rt.block_on(async move {
        if let Err(e) = run_async(&js_code, file_name, scripts_dir, registry, &console).await {
            console.error(e.to_string());
        }
    });
}

async fn run_async(
    js_code: &str,
    file_name: &str,
    scripts_dir: PathBuf,
    registry: &Arc<HostRegistry>,
    console: &ConsoleChannel,
) -> Result<(), String> {
    // 3. Create QuickJS runtime + install host loader/resolver.
    let rt = AsyncRuntime::new().map_err(|e| e.to_string())?;
    rt.set_max_stack_size(8 * 1024 * 1024).await;
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
        // host module globals
        if let Err(e) = registry.install_globals(&ctx) {
            console.error(format!("module install: {e}"));
        }

        // Evaluate as an ES module (resolves imports via the loader).
        let result = Module::evaluate(ctx.clone(), file_name, js_code);
        match result {
            Ok(promise) => {
                // Drive the module promise to completion.
                if let Err(e) = promise.into_future::<()>().await.catch(&ctx) {
                    log_caught(console, &e);
                }
            }
            Err(e) => {
                let caught = rquickjs::CaughtError::from_error(&ctx, e);
                log_caught(console, &caught);
            }
        }
    })
    .await;

    // 6. Drain remaining async jobs (fetch/exec Promises, setTimeout-like).
    rt.idle().await;

    Ok(())
}

fn log_caught(console: &ConsoleChannel, err: &rquickjs::CaughtError) {
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
    use crate::host_modules;

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
        host_modules::register_all(&mut registry);
        let registry = Arc::new(registry);
        let (channel, rx) = ConsoleChannel::new();
        (registry, channel, rx)
    }

    #[test]
    fn run_plain_js_console_log() {
        let (registry, channel, rx) = make_runner();
        run_script(
            "console.log('hello world'); console.info('info');",
            Language::JavaScript,
            "test.js",
            std::env::temp_dir(),
            &registry,
            channel,
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
        let code = r#"
            import { env } from 'scripter';
            const name: string = env('USER') ?? 'world';
            console.log(`hello, ${name}`);
        "#;
        run_script(
            code,
            Language::TypeScript,
            "demo.ts",
            std::env::temp_dir(),
            &registry,
            channel,
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
            std::env::temp_dir(),
            &registry,
            channel,
        );
        let lines = drain(&rx);
        assert!(
            lines.iter().any(|l| l.contains("hi from script")),
            "missing file content: {lines:?}"
        );
    }
}
