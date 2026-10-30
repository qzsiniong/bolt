//! 插件运行时注册表：宿主拿到的唯一跨线程调用入口。
//!
//! plux 的 `Loader` 是 `&mut` 单线程的，命令却要在工作线程里跑（不能卡住 GPUI
//! 主线程）。所以 [`crate::JsManager`] 只持有本表的 `Arc`，宿主也从这里调
//! [`PluginStore::invoke`]：`Loader` 负责生命周期与元数据，本表负责执行。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, bounded};
use parking_lot::Mutex;
use plux_rs::Bundle;

use crate::cancel::CancelToken;
use crate::env::{JsEnv, Scope};
use crate::error::JsError;
use crate::worker::{PluginWorker, WorkerSpec};

/// 命令的默认超时。插件里一个卡死的 `while(true)` 不该把应用拖住。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// 等待插件回复时的轮询间隔：既用来查超时，也用来查用户取消。
const POLL: Duration = Duration::from_millis(100);

/// 请求中断后等工人线程收尾的宽限：等它回一条（会被丢弃的）回复，确认它已经
/// 回到消息循环，下一条命令不会被前一条的残留中断标志误伤。
const ABORT_GRACE: Duration = Duration::from_millis(500);

/// 一次命令调用。
pub struct InvokeRequest {
    /// 清单里声明的命令 id。
    pub command: String,
    /// 额外参数，JSON 数组文本（可选）。
    pub args: Option<String>,
    /// 执行预算。
    pub timeout: Duration,
    /// 用户中止 / 关闭窗口时置位。
    pub cancel: CancelToken,
    /// 本轮的交互 API 载体（`confirm` / `pickPath` / `progress` / `typeStr`）。
    pub scope: Option<Arc<dyn Scope>>,
}

impl InvokeRequest {
    pub fn new(command: impl Into<String>, cancel: CancelToken) -> Self {
        Self {
            command: command.into(),
            args: None,
            timeout: DEFAULT_TIMEOUT,
            cancel,
            scope: None,
        }
    }

    pub fn with_args(mut self, args: impl Into<String>) -> Self {
        self.args = Some(args.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_scope(mut self, scope: Arc<dyn Scope>) -> Self {
        self.scope = Some(scope);
        self
    }
}

type Key = (String, String);

/// 已注册的插件运行时。可通过 `Arc` 跨线程共享。
pub struct PluginStore {
    env: Arc<dyn JsEnv>,
    workers: Mutex<HashMap<Key, Arc<PluginWorker>>>,
}

impl PluginStore {
    pub fn new(env: Arc<dyn JsEnv>) -> Arc<Self> {
        Arc::new(Self {
            env,
            workers: Mutex::new(HashMap::new()),
        })
    }

    /// 确保插件已有常驻运行时；已加载时直接返回。
    pub fn ensure_loaded(
        &self,
        id: &str,
        version: &str,
        root: &Path,
        entry: &Path,
    ) -> Result<(), JsError> {
        let key = (id.to_string(), version.to_string());
        if self.workers.lock().contains_key(&key) {
            return Ok(());
        }

        let spec = WorkerSpec {
            id: id.to_string(),
            version: version.to_string(),
            root: root.to_path_buf(),
            entry: entry.to_path_buf(),
        };
        let worker = PluginWorker::spawn(spec, self.env.clone())?;

        // 加载期间别的线程可能已经装好了同一个插件：以先到的为准。
        let mut workers = self.workers.lock();
        if let Some(existing) = workers.get(&key) {
            let _ = existing;
            return Ok(());
        }
        workers.insert(key, worker);
        Ok(())
    }

    /// 执行一条命令。阻塞到插件返回、超时或用户取消。
    pub fn invoke(
        &self,
        id: &str,
        version: &str,
        request: InvokeRequest,
    ) -> Result<Option<String>, JsError> {
        let worker = self
            .workers
            .lock()
            .get(&(id.to_string(), version.to_string()))
            .cloned()
            .ok_or_else(|| JsError::NotLoaded(format!("{id} v{version}")))?;

        let InvokeRequest {
            command,
            args,
            timeout,
            cancel,
            scope,
        } = request;

        let (reply_tx, reply_rx) = bounded(1);
        worker.send(crate::worker::WorkerMsg::Invoke {
            request: InvokeRequest {
                command,
                args,
                timeout,
                cancel: cancel.clone(),
                scope,
            },
            reply: reply_tx,
        })?;

        let deadline = Instant::now() + timeout;
        loop {
            match reply_rx.recv_timeout(POLL) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Timeout) => {
                    if cancel.is_cancelled() {
                        return Err(self.give_up(&worker, &reply_rx, JsError::Cancelled));
                    }
                    if Instant::now() >= deadline {
                        return Err(self.give_up(&worker, &reply_rx, JsError::Timeout(timeout)));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(JsError::Runtime("插件线程已退出".to_string()));
                }
            }
        }
    }

    /// 请求中断并短暂等工人线程回到消息循环。
    fn give_up(
        &self,
        worker: &Arc<PluginWorker>,
        reply_rx: &crossbeam_channel::Receiver<Result<Option<String>, JsError>>,
        error: JsError,
    ) -> JsError {
        worker.abort();
        let _ = reply_rx.recv_timeout(ABORT_GRACE);
        error
    }

    /// 停止并卸载插件运行时。
    pub fn unload(&self, id: &str, version: &str) {
        let worker = self
            .workers
            .lock()
            .remove(&(id.to_string(), version.to_string()));
        if let Some(worker) = worker {
            worker.shutdown();
        }
    }

    pub fn is_loaded(&self, id: &str, version: &str) -> bool {
        self.workers
            .lock()
            .contains_key(&(id.to_string(), version.to_string()))
    }

    pub fn loaded_count(&self) -> usize {
        self.workers.lock().len()
    }
}

impl Drop for PluginStore {
    fn drop(&mut self) {
        for (_, worker) in self.workers.lock().drain() {
            worker.shutdown();
        }
    }
}

/// bundle → `(id, version)`，与 [`PluginStore`] 的键一致。
pub fn bundle_key(bundle: &Bundle) -> (String, String) {
    (bundle.id.clone(), bundle.version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{JsEnv, LogLevel, Scope};
    use crate::manifest::{MANIFEST_FILE, UI_ENTRY};
    use crate::test_support::TempDir;
    use rquickjs::{Ctx, Function, Object};

    /// 最小宿主环境：只装一个把输出收进 Vec 的 console。
    #[derive(Default)]
    struct TestEnv {
        logs: Arc<Mutex<Vec<String>>>,
    }

    impl TestEnv {
        fn take_logs(&self) -> Vec<String> {
            std::mem::take(&mut self.logs.lock())
        }
    }

    impl JsEnv for TestEnv {
        fn install_base<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<()> {
            let logs = self.logs.clone();
            let console = Object::new(ctx.clone())?;
            console.set(
                "log",
                Function::new(ctx.clone(), move |message: String| {
                    logs.lock().push(message);
                })?,
            )?;
            ctx.globals().set("console", console)?;
            Ok(())
        }

        fn install_scope<'js>(&self, _ctx: &Ctx<'js>, _scope: &dyn Scope) -> rquickjs::Result<()> {
            Ok(())
        }

        fn host_modules(&self) -> Vec<crate::env::HostModuleSpec> {
            Vec::new()
        }

        fn prepare_source(&self, _path: &Path, code: &str) -> Result<String, String> {
            Ok(code.to_string())
        }

        fn log(&self, _level: LogLevel, message: &str) {
            self.logs.lock().push(message.to_string());
        }
    }

    fn manifest(id: &str) -> String {
        format!(r#"{{ "id": "{id}", "name": "Demo", "version": "1.0.0", "entry": "index.js" }}"#)
    }

    /// 建一个 `demo-v1.0.0.js/` 插件目录并返回 `(root, entry, store)`。
    fn plugin(entry: &str) -> (TempDir, Arc<PluginStore>, Arc<TestEnv>) {
        let tmp = TempDir::new("store");
        let dir = "demo-v1.0.0.js";
        tmp.write(&format!("{dir}/{MANIFEST_FILE}"), &manifest("demo"));
        tmp.write(&format!("{dir}/index.js"), entry);
        tmp.write(&format!("{dir}/{UI_ENTRY}"), "");

        let env = Arc::new(TestEnv::default());
        let store = PluginStore::new(env.clone());
        let root = tmp.0.join(dir);
        store
            .ensure_loaded("demo", "1.0.0", &root, &root.join("index.js"))
            .expect("plugin should load");
        (tmp, store, env)
    }

    fn call(store: &PluginStore, command: &str) -> Result<Option<String>, JsError> {
        store.invoke(
            "demo",
            "1.0.0",
            InvokeRequest::new(command, CancelToken::new()),
        )
    }

    #[test]
    fn named_export_runs_and_returns_its_value() {
        let (_tmp, store, _env) = plugin("export function hello() { return 'hi'; }");
        assert_eq!(call(&store, "hello").unwrap().as_deref(), Some("hi"));
    }

    #[test]
    fn async_export_is_awaited() {
        let (_tmp, store, _env) =
            plugin("export async function hello() { await Promise.resolve(); return 'later'; }");
        assert_eq!(call(&store, "hello").unwrap().as_deref(), Some("later"));
    }

    #[test]
    fn default_object_fallback_works() {
        let (_tmp, store, _env) = plugin("export default { hello() { return { ok: true }; } };");
        assert_eq!(
            call(&store, "hello").unwrap().as_deref(),
            Some(r#"{"ok":true}"#)
        );
    }

    #[test]
    fn global_commands_fallback_works() {
        let (_tmp, store, _env) =
            plugin("globalThis.plux = { commands: { hello() { return 'glob'; } } };");
        assert_eq!(call(&store, "hello").unwrap().as_deref(), Some("glob"));
    }

    #[test]
    fn command_receives_plugin_context() {
        let (_tmp, store, _env) = plugin(
            "export function who(ctx) { return `${ctx.command}|${ctx.plugin.id}|${ctx.plugin.version}`; }",
        );
        assert_eq!(
            call(&store, "who").unwrap().as_deref(),
            Some("who|demo|1.0.0")
        );
    }

    #[test]
    fn plugin_state_survives_between_commands() {
        let (_tmp, store, _env) = plugin(
            r#"
            let count = 0;
            export function bump() { count += 1; return count; }
            "#,
        );
        assert_eq!(call(&store, "bump").unwrap().as_deref(), Some("1"));
        assert_eq!(call(&store, "bump").unwrap().as_deref(), Some("2"));
    }

    #[test]
    fn module_imports_resolve_inside_the_plugin() {
        let tmp = TempDir::new("store-import");
        tmp.write("demo-v1.0.0.js/extension.json", &manifest("demo"));
        tmp.write(
            "demo-v1.0.0.js/index.js",
            "export { hello } from './lib/hello.js';",
        );
        tmp.write(
            "demo-v1.0.0.js/lib/hello.js",
            "export function hello() { return 'nested'; }",
        );

        let env = Arc::new(TestEnv::default());
        let store = PluginStore::new(env);
        let root = tmp.0.join("demo-v1.0.0.js");
        store
            .ensure_loaded("demo", "1.0.0", &root, &root.join("index.js"))
            .expect("plugin should load");

        assert_eq!(call(&store, "hello").unwrap().as_deref(), Some("nested"));
    }

    #[test]
    fn missing_handler_is_reported() {
        let (_tmp, store, _env) = plugin("export function hello() { return 1; }");
        assert_eq!(
            call(&store, "nope").unwrap_err(),
            JsError::MissingHandler("nope".to_string())
        );
    }

    #[test]
    fn thrown_errors_carry_the_message() {
        let (_tmp, store, _env) = plugin("export function boom() { throw new Error('kaboom'); }");
        let error = call(&store, "boom").unwrap_err();
        match error {
            JsError::Script(message) => assert!(message.contains("kaboom"), "{message}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn infinite_loops_time_out_and_the_runtime_stays_usable() {
        let (_tmp, store, _env) = plugin(
            r#"
            export function spin() { while (true) {} }
            export function hello() { return 'still here'; }
            "#,
        );

        let timeout = store
            .invoke(
                "demo",
                "1.0.0",
                InvokeRequest::new("spin", CancelToken::new())
                    .with_timeout(Duration::from_millis(300)),
            )
            .unwrap_err();
        assert_eq!(timeout, JsError::Timeout(Duration::from_millis(300)));
        assert_eq!(
            call(&store, "hello").unwrap().as_deref(),
            Some("still here")
        );
    }

    #[test]
    fn cancel_token_stops_a_running_command() {
        let (_tmp, store, _env) = plugin("export function spin() { while (true) {} }");

        let cancel = CancelToken::new();
        let killer = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            killer.cancel();
        });

        let started = std::time::Instant::now();
        let error = store
            .invoke(
                "demo",
                "1.0.0",
                InvokeRequest::new("spin", cancel).with_timeout(Duration::from_secs(30)),
            )
            .unwrap_err();
        assert_eq!(error, JsError::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn unload_stops_the_worker() {
        let (_tmp, store, _env) = plugin("export function hello() { return 'hi'; }");
        assert!(store.is_loaded("demo", "1.0.0"));
        store.unload("demo", "1.0.0");
        assert!(!store.is_loaded("demo", "1.0.0"));
        assert!(matches!(call(&store, "hello"), Err(JsError::NotLoaded(_))));
    }

    #[test]
    fn console_output_reaches_the_host() {
        let (_tmp, store, env) = plugin("export function hello() { console.log('from plugin'); }");
        call(&store, "hello").unwrap();
        assert!(env.take_logs().iter().any(|line| line == "from plugin"));
    }
}
