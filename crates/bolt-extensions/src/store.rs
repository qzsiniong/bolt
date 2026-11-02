//! 扩展运行时表：宿主拿到的唯一跨线程调用入口。
//!
//! 命令要在工作线程里跑（不能卡住宿主主线程），而注册表 [`crate::ExtensionRegistry`]
//! 是 `&mut` 单线程的：它只持本表的 `Arc`，宿主也从这里调 [`ExtensionStore::invoke`]
//! ——注册表负责生命周期与元数据，本表负责执行。
//!
//! [`ExtensionStore::ensure_loaded`] **不阻塞**：它只把工人线程拉起来就返回，引擎构建
//! 与入口求值发生在扩展线程上；等待就绪统一压到 [`ExtensionStore::invoke`] 里，那里
//! 本来就在工作线程上，且已经有超时与取消。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, bounded};
use parking_lot::Mutex;

use crate::cancel::CancelToken;
use crate::env::{JsEnv, Scope};
use crate::error::JsError;
use crate::worker::{ExtensionWorker, POLL, WorkerSpec};

/// 命令的默认超时。扩展里一个卡死的 `while(true)` 不该把应用拖住。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// 请求中断后等工人线程收尾的宽限：等它回一条（会被丢弃的）回复，确认它已经
/// 回到消息循环，下一条命令不会被前一条的残留中断标志误伤。
const ABORT_GRACE: Duration = Duration::from_millis(500);

/// 卸载时等工人线程收尾的宽限。
///
/// 正常停在消息循环里的线程毫秒级退出；**加载中**的线程最多等这么久，之后放手
/// ——删扩展 / 重建运行时不该把主线程拖住 30s（QuickJS 线程不可强杀）。
const UNLOAD_GRACE: Duration = Duration::from_millis(500);

/// 宿主可见的运行时状态投影。
///
/// 三态而不是布尔：`Loading` 要放行（交给 `invoke` 的 `wait_ready` 去等），
/// `Dead` 才需要宿主重建——两者混成一个 `bool` 正是"线程死了却永不重建"的成因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeState {
    Loading,
    Ready,
    Dead,
}

/// 一次命令调用。
pub struct InvokeRequest {
    /// 清单里声明的命令 id。
    pub command: String,
    /// 命令参数：就是 JS 侧看到的 `ctx.argv`。
    ///
    /// 收已解析的值而不是 JSON 文本：类型本身就是校验，非法 JSON 这个概念消失，
    /// worker 侧也不用再做一次「解析失败就静默当 null」的兜底。
    pub args: Option<Vec<serde_json::Value>>,
    /// 执行预算（含等待运行时就绪的时间）。
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

    pub fn with_args(mut self, args: Vec<serde_json::Value>) -> Self {
        self.args = Some(args);
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

/// 运行时表的键：扩展身份就是清单里的 `id`。
type Key = String;

/// 已注册的扩展运行时。可通过 `Arc` 跨线程共享。
pub struct ExtensionStore {
    env: Arc<dyn JsEnv>,
    workers: Mutex<HashMap<Key, Arc<ExtensionWorker>>>,
}

impl ExtensionStore {
    pub fn new(env: Arc<dyn JsEnv>) -> Arc<Self> {
        Arc::new(Self {
            env,
            workers: Mutex::new(HashMap::new()),
        })
    }

    /// 确保扩展已被安排加载；**不阻塞**。
    ///
    /// 已有条目一律 `Ok`（无论是否就绪）：重复调用不该重启运行时。加载成败通过
    /// [`Self::state`] 暴露，并由 [`Self::invoke`] 在真正执行前等待。
    ///
    /// `permissions` 是扩展清单里声明的权限（缺省为空 = 全关），运行时据此
    /// 过滤宿主模块的导出。
    pub fn ensure_loaded(
        &self,
        id: &str,
        version: &str,
        root: &Path,
        entry: &Path,
        permissions: &[String],
    ) -> Result<(), JsError> {
        // 查表与 spawn 在同一把锁内完成：spawn 现在只建线程（微秒级），可以持锁。
        // 这样彻底消掉了原来"锁外 spawn、锁内复查、败者回收"的路径——败者的 Drop
        // 现在会 join 一个正在加载的线程，而且两个引擎会各跑一遍扩展顶层副作用。
        let mut workers = self.workers.lock();
        if workers.contains_key(id) {
            return Ok(());
        }
        let spec = WorkerSpec {
            id: id.to_string(),
            version: version.to_string(),
            root: root.to_path_buf(),
            entry: entry.to_path_buf(),
            permissions: permissions.to_vec(),
        };
        workers.insert(
            id.to_string(),
            ExtensionWorker::spawn(spec, self.env.clone())?,
        );
        Ok(())
    }

    /// 运行时状态；`None` 表示从未加载或已经卸载。
    pub fn state(&self, id: &str) -> Option<RuntimeState> {
        self.workers.lock().get(id).map(|worker| worker.state())
    }

    /// 上一次加载 / 运行失败的原因，供宿主展示"为什么死了"。
    pub fn last_error(&self, id: &str) -> Option<JsError> {
        self.workers
            .lock()
            .get(id)
            .and_then(|worker| worker.last_error())
    }

    /// 执行一条命令。阻塞到扩展返回、超时或用户取消。
    pub fn invoke(&self, id: &str, request: InvokeRequest) -> Result<Option<String>, JsError> {
        let worker = self
            .workers
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| JsError::NotLoaded(id.to_string()))?;

        let InvokeRequest {
            command,
            args,
            timeout,
            cancel,
            scope,
        } = request;

        let (reply_tx, reply_rx) = bounded(1);
        // 先等就绪再投递：加载中的扩展不该收到命令，否则消息会压在一个还没建好的
        // 引擎前面排队，超时也只能报"线程已退出"这种含糊话。
        let started = Instant::now();
        worker.wait_ready(timeout, &cancel)?;

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

        // 加载已经花掉的时间要从本次预算里扣掉。
        let deadline = started + timeout;
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
                    return Err(JsError::Runtime("扩展线程已退出".to_string()));
                }
            }
        }
    }

    /// 请求中断并短暂等工人线程回到消息循环。
    fn give_up(
        &self,
        worker: &Arc<ExtensionWorker>,
        reply_rx: &crossbeam_channel::Receiver<Result<Option<String>, JsError>>,
        error: JsError,
    ) -> JsError {
        worker.abort();
        let _ = reply_rx.recv_timeout(ABORT_GRACE);
        error
    }

    /// 停止并卸载扩展运行时。先摘表（此后 `invoke` 得 `NotLoaded`），再让线程收尾；
    /// 收尾有界，卡住的加载最多耽误 [`UNLOAD_GRACE`]。
    pub fn unload(&self, id: &str) {
        let worker = self.workers.lock().remove(id);
        if let Some(worker) = worker {
            let _ = worker.shutdown(UNLOAD_GRACE);
        }
    }

    pub fn is_loaded(&self, id: &str) -> bool {
        self.workers.lock().contains_key(id)
    }

    pub fn loaded_count(&self) -> usize {
        self.workers.lock().len()
    }

    /// 测试专用：拿到工人线程句柄，用来模拟「运行时意外死亡」。
    #[cfg(test)]
    pub(crate) fn worker_for_test(&self, id: &str) -> Option<Arc<ExtensionWorker>> {
        self.workers.lock().get(id).cloned()
    }
}

impl Drop for ExtensionStore {
    fn drop(&mut self) {
        for (_, worker) in self.workers.lock().drain() {
            let _ = worker.shutdown(UNLOAD_GRACE);
        }
    }
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

        fn host_modules(&self, _permissions: &[String]) -> Vec<crate::env::HostModuleSpec> {
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

    /// 测试里唯一的扩展身份：一个字符串说明全部。
    fn demo() -> &'static str {
        "demo"
    }

    /// 建一个 `demo/` 扩展目录并返回 `(root, entry, store)`。
    fn extension(entry: &str) -> (TempDir, Arc<ExtensionStore>, Arc<TestEnv>) {
        let tmp = TempDir::new("store");
        let dir = "demo";
        tmp.write(&format!("{dir}/{MANIFEST_FILE}"), &manifest("demo"));
        tmp.write(&format!("{dir}/index.js"), entry);
        tmp.write(&format!("{dir}/{UI_ENTRY}"), "");

        let env = Arc::new(TestEnv::default());
        let store = ExtensionStore::new(env.clone());
        let root = tmp.0.join(dir);
        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .expect("extension should load");
        (tmp, store, env)
    }

    fn call(store: &ExtensionStore, command: &str) -> Result<Option<String>, JsError> {
        store.invoke(demo(), InvokeRequest::new(command, CancelToken::new()))
    }

    #[test]
    fn named_export_runs_and_returns_its_value() {
        let (_tmp, store, _env) = extension("export function hello() { return 'hi'; }");
        assert_eq!(call(&store, "hello").unwrap().as_deref(), Some("hi"));
    }

    #[test]
    fn async_export_is_awaited() {
        let (_tmp, store, _env) =
            extension("export async function hello() { await Promise.resolve(); return 'later'; }");
        assert_eq!(call(&store, "hello").unwrap().as_deref(), Some("later"));
    }

    #[test]
    fn default_object_fallback_works() {
        let (_tmp, store, _env) = extension("export default { hello() { return { ok: true }; } };");
        assert_eq!(
            call(&store, "hello").unwrap().as_deref(),
            Some(r#"{"ok":true}"#)
        );
    }

    #[test]
    fn global_commands_fallback_works() {
        let (_tmp, store, _env) =
            extension("globalThis.bolt = { commands: { hello() { return 'glob'; } } };");
        assert_eq!(call(&store, "hello").unwrap().as_deref(), Some("glob"));
    }

    #[test]
    fn command_receives_extension_context() {
        let (_tmp, store, _env) = extension(
            "export function who(ctx) { return `${ctx.command}|${ctx.extension.id}|${ctx.extension.version}`; }",
        );
        assert_eq!(
            call(&store, "who").unwrap().as_deref(),
            Some("who|demo|1.0.0")
        );
    }

    #[test]
    fn extension_state_survives_between_commands() {
        let (_tmp, store, _env) = extension(
            r#"
            let count = 0;
            export function bump() { count += 1; return count; }
            "#,
        );
        assert_eq!(call(&store, "bump").unwrap().as_deref(), Some("1"));
        assert_eq!(call(&store, "bump").unwrap().as_deref(), Some("2"));
    }

    #[test]
    fn module_imports_resolve_inside_the_extension() {
        let tmp = TempDir::new("store-import");
        tmp.write("demo/extension.json", &manifest("demo"));
        tmp.write("demo/index.js", "export { hello } from './lib/hello.js';");
        tmp.write(
            "demo/lib/hello.js",
            "export function hello() { return 'nested'; }",
        );

        let env = Arc::new(TestEnv::default());
        let store = ExtensionStore::new(env);
        let root = tmp.0.join("demo");
        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .expect("extension should load");

        assert_eq!(call(&store, "hello").unwrap().as_deref(), Some("nested"));
    }

    #[test]
    fn missing_handler_is_reported() {
        let (_tmp, store, _env) = extension("export function hello() { return 1; }");
        assert_eq!(
            call(&store, "nope").unwrap_err(),
            JsError::MissingHandler("nope".to_string())
        );
    }

    #[test]
    fn thrown_errors_carry_the_message() {
        let (_tmp, store, _env) =
            extension("export function boom() { throw new Error('kaboom'); }");
        let error = call(&store, "boom").unwrap_err();
        match error {
            JsError::Script(message) => assert!(message.contains("kaboom"), "{message}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn infinite_loops_time_out_and_the_runtime_stays_usable() {
        let (_tmp, store, _env) = extension(
            r#"
            export function spin() { while (true) {} }
            export function hello() { return 'still here'; }
            "#,
        );

        let timeout = store
            .invoke(
                demo(),
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
        let (_tmp, store, _env) = extension("export function spin() { while (true) {} }");

        let cancel = CancelToken::new();
        let killer = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            killer.cancel();
        });

        let started = std::time::Instant::now();
        let error = store
            .invoke(
                demo(),
                InvokeRequest::new("spin", cancel).with_timeout(Duration::from_secs(30)),
            )
            .unwrap_err();
        assert_eq!(error, JsError::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn unload_stops_the_worker() {
        let (_tmp, store, _env) = extension("export function hello() { return 'hi'; }");
        assert!(store.is_loaded(demo()));
        store.unload(demo());
        assert!(!store.is_loaded(demo()));
        assert!(matches!(call(&store, "hello"), Err(JsError::NotLoaded(_))));
    }

    #[test]
    fn console_output_reaches_the_host() {
        let (_tmp, store, env) =
            extension("export function hello() { console.log('from extension'); }");
        call(&store, "hello").unwrap();
        assert!(env.take_logs().iter().any(|line| line == "from extension"));
    }

    /// 带宿主模块的环境：`install_base` 只装基础函数，`install_scope` 才装
    /// `typeStr`——复现 bolt 的真实时序。`typeStr` 需要 `typeStr` 权限。
    struct ScopedEnv {
        logs: Arc<Mutex<Vec<String>>>,
    }

    impl Default for ScopedEnv {
        fn default() -> Self {
            Self {
                logs: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl JsEnv for ScopedEnv {
        fn install_base<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<()> {
            let g = Object::new(ctx.clone())?;
            g.set("base", Function::new(ctx.clone(), || "base")?)?;
            ctx.globals().set("__bolt", g)?;
            Ok(())
        }

        fn install_scope<'js>(&self, ctx: &Ctx<'js>, _scope: &dyn Scope) -> rquickjs::Result<()> {
            let g: Object = ctx.globals().get("__bolt")?;
            g.set("typeStr", Function::new(ctx.clone(), |text: String| text)?)?;
            Ok(())
        }

        fn host_modules(&self, permissions: &[String]) -> Vec<crate::env::HostModuleSpec> {
            let mut spec = crate::env::HostModuleSpec {
                name: "bolt".to_string(),
                exports: vec!["base".to_string()],
                denied: Vec::new(),
            };
            if permissions.iter().any(|p| p == "typeStr") {
                spec.exports.push("typeStr".to_string());
            } else {
                spec.denied
                    .push(("typeStr".to_string(), "typeStr".to_string()));
            }
            vec![spec]
        }

        fn prepare_source(&self, _path: &Path, code: &str) -> Result<String, String> {
            Ok(code.to_string())
        }

        fn log(&self, _level: LogLevel, message: &str) {
            self.logs.lock().push(message.to_string());
        }
    }

    struct TestScope;

    impl Scope for TestScope {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn host_module_exports_track_apis_installed_after_load() {
        let tmp = TempDir::new("store-scope-module");
        let dir = "demo";
        // 声明 typeStr 权限：转发模块里 typeStr 是真转发。
        tmp.write(
            &format!("{dir}/{MANIFEST_FILE}"),
            r#"{ "id": "demo", "name": "Demo", "version": "1.0.0", "entry": "index.js", "permissions": ["typeStr"] }"#,
        );
        tmp.write(
            &format!("{dir}/index.js"),
            "import * as bolt from 'bolt';\nexport function hello() { return `${typeof bolt.typeStr}|${bolt.typeStr('hi')}`; }",
        );

        let env = Arc::new(ScopedEnv::default());
        let store = ExtensionStore::new(env);
        let root = tmp.0.join(dir);
        store
            .ensure_loaded(
                demo(),
                "1.0.0",
                &root,
                &root.join("index.js"),
                &["typeStr".to_string()],
            )
            .expect("extension should load");

        // 调用时带上 scope，install_scope 才会把 typeStr 装上。
        let request = InvokeRequest::new("hello", CancelToken::new())
            .with_scope(Arc::new(TestScope) as Arc<dyn Scope>);
        assert_eq!(
            store.invoke(demo(), request).unwrap().as_deref(),
            Some("function|hi")
        );
    }

    #[test]
    fn undeclared_permission_throws_a_naming_error_at_call_time() {
        // fail-closed：清单不声明 typeStr，import 不炸，调用时抛出点名报错。
        let tmp = TempDir::new("store-perm-denied");
        let dir = "demo";
        tmp.write(&format!("{dir}/{MANIFEST_FILE}"), &manifest("demo"));
        tmp.write(
            &format!("{dir}/index.js"),
            "import { base, typeStr } from 'bolt';\nexport function hello() { return typeStr('x'); }",
        );

        let env = Arc::new(ScopedEnv::default());
        let store = ExtensionStore::new(env);
        let root = tmp.0.join(dir);
        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .expect("extension should load");

        let error = call(&store, "hello").unwrap_err();
        match error {
            JsError::Script(message) => {
                assert!(message.contains("未声明权限"), "{message}");
                assert!(message.contains("typeStr"), "{message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// 门控环境：`install_base` 先送 `started`，再阻塞等 `release`。测试因此能精确
    /// 停在「正在加载」上——不用 sleep，也没有阈值可 flaky。
    struct GateEnv {
        started: crossbeam_channel::Sender<()>,
        release: Mutex<crossbeam_channel::Receiver<()>>,
        logs: Arc<Mutex<Vec<String>>>,
    }

    type Gate = (
        Arc<GateEnv>,
        crossbeam_channel::Receiver<()>,
        crossbeam_channel::Sender<()>,
    );

    fn gate_env() -> Gate {
        let (started_tx, started_rx) = crossbeam_channel::bounded(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let env = Arc::new(GateEnv {
            started: started_tx,
            release: Mutex::new(release_rx),
            logs: Arc::new(Mutex::new(Vec::new())),
        });
        (env, started_rx, release_tx)
    }

    impl JsEnv for GateEnv {
        fn install_base<'js>(&self, ctx: &Ctx<'js>) -> rquickjs::Result<()> {
            // console 先装：扩展顶层的 console.log 要能落到 logs 里。
            let logs = self.logs.clone();
            let console = Object::new(ctx.clone())?;
            console.set(
                "log",
                Function::new(ctx.clone(), move |message: String| {
                    logs.lock().push(message);
                })?,
            )?;
            ctx.globals().set("console", console)?;

            let _ = self.started.send(());
            // 等测试放行；超时兜底，免得测试失败时把线程永久挂住。
            let _ = self.release.lock().recv_timeout(Duration::from_secs(5));
            Ok(())
        }

        fn install_scope<'js>(&self, _ctx: &Ctx<'js>, _scope: &dyn Scope) -> rquickjs::Result<()> {
            Ok(())
        }

        fn host_modules(&self, _permissions: &[String]) -> Vec<crate::env::HostModuleSpec> {
            Vec::new()
        }

        fn prepare_source(&self, _path: &Path, code: &str) -> Result<String, String> {
            Ok(code.to_string())
        }

        fn log(&self, _level: LogLevel, message: &str) {
            self.logs.lock().push(message.to_string());
        }
    }

    /// 建一个 `demo` 扩展目录并返回根路径。
    fn write_demo(tmp: &TempDir, entry: &str) -> std::path::PathBuf {
        tmp.write("demo/extension.json", &manifest("demo"));
        tmp.write("demo/index.js", entry);
        tmp.0.join("demo")
    }

    fn wait_for_state(store: &ExtensionStore, want: RuntimeState) {
        for _ in 0..500 {
            if store.state(demo()) == Some(want) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "state never became {want:?} (last: {:?})",
            store.state(demo())
        );
    }

    #[test]
    fn ensure_loaded_returns_before_the_worker_is_ready() {
        let tmp = TempDir::new("store-nonblocking");
        let root = write_demo(&tmp, "export function hello() { return 'hi'; }");
        let (env, started, release) = gate_env();
        let store = ExtensionStore::new(env);

        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .expect("spawn must not wait for the engine");
        // 关键断言：返回时引擎还没建好。
        assert_eq!(store.state(demo()), Some(RuntimeState::Loading));

        started
            .recv_timeout(Duration::from_secs(5))
            .expect("worker starts");
        let _ = release.send(());

        let output = call(&store, "hello").expect("hello should run");
        assert_eq!(output.as_deref(), Some("hi"));
        assert_eq!(store.state(demo()), Some(RuntimeState::Ready));
    }

    #[test]
    fn invoke_waits_for_a_loading_worker_and_loads_only_once() {
        let tmp = TempDir::new("store-wait-ready");
        let root = write_demo(
            &tmp,
            "console.log('boot');\nexport function hello() { return 'ready'; }",
        );
        let (env, _started, release) = gate_env();
        let logs = env.logs.clone();
        let store = ExtensionStore::new(env);

        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .unwrap();

        let unlocked = release.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let _ = unlocked.send(());
        });

        assert_eq!(
            call(&store, "hello").unwrap().as_deref(),
            Some("ready"),
            "invoke must wait for readiness instead of failing"
        );
        // 第二次调用不该重新加载：顶层副作用只跑一遍。
        call(&store, "hello").unwrap();
        let boots = logs.lock().iter().filter(|line| *line == "boot").count();
        assert_eq!(boots, 1, "extension top-level must run once");
    }

    #[test]
    fn load_failure_is_logged_once_and_replayed_to_every_invoke() {
        let tmp = TempDir::new("store-load-failure");
        let root = write_demo(&tmp, "throw new Error('boom');");
        let (env, _started, _release) = gate_env();
        let logs = env.logs.clone();
        let store = ExtensionStore::new(env);

        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .expect("spawn itself succeeds; the failure is asynchronous");
        wait_for_state(&store, RuntimeState::Dead);

        // 失败只报一次，但每一次 invoke 都能拿到原因。
        for _ in 0..2 {
            match call(&store, "hello").unwrap_err() {
                JsError::Runtime(message) => assert!(message.contains("boom"), "{message}"),
                other => panic!("unexpected error: {other:?}"),
            }
        }
        let failures = logs
            .lock()
            .iter()
            .filter(|line| line.contains("加载失败"))
            .count();
        assert_eq!(failures, 1, "load failure must be logged exactly once");
    }

    #[test]
    fn concurrent_ensure_loaded_starts_exactly_one_worker() {
        let tmp = TempDir::new("store-concurrent-load");
        let root = write_demo(
            &tmp,
            "console.log('boot');\nexport function hello() { return 'ok'; }",
        );
        let (env, _started, release) = gate_env();
        let logs = env.logs.clone();
        let store = ExtensionStore::new(env);

        let workers: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let root = root.clone();
                std::thread::spawn(move || {
                    store
                        .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
                        .expect("ensure_loaded");
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(store.loaded_count(), 1);

        let _ = release.send(());
        wait_for_state(&store, RuntimeState::Ready);
        let boots = logs.lock().iter().filter(|line| *line == "boot").count();
        assert_eq!(boots, 1, "only one engine may evaluate the entry module");
    }

    #[test]
    fn unload_during_load_is_bounded() {
        let tmp = TempDir::new("store-unload-loading");
        let root = write_demo(&tmp, "export function hello() { return 'hi'; }");
        let (env, started, release) = gate_env();
        let store = ExtensionStore::new(env);

        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();

        let started_at = Instant::now();
        store.unload(demo());
        assert!(
            started_at.elapsed() < Duration::from_millis(900),
            "unload must not wait for a stuck load (took {:?})",
            started_at.elapsed()
        );
        assert!(!store.is_loaded(demo()));
        assert!(matches!(call(&store, "hello"), Err(JsError::NotLoaded(_))));

        // 放行，让被放手的线程自己退出，别留给别的测试。
        let _ = release.send(());
    }

    #[test]
    fn worker_death_flips_state_to_dead() {
        let tmp = TempDir::new("store-death");
        let root = write_demo(&tmp, "export function hello() { return 'hi'; }");
        let (env, _started, _release) = gate_env();
        // 这个扩展不需要门：直接放行加载。
        let _ = _release.send(());
        let store = ExtensionStore::new(env);
        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .unwrap();
        wait_for_state(&store, RuntimeState::Ready);

        // 只让线程退出，不摘表：模拟运行时意外死亡。
        store
            .worker_for_test(demo())
            .expect("worker")
            .exit_for_test();
        wait_for_state(&store, RuntimeState::Dead);
        assert!(
            store
                .last_error(demo())
                .is_some_and(|error| error.to_string().contains("运行时已退出")),
            "death must carry a reason"
        );
        assert!(matches!(call(&store, "hello"), Err(JsError::Runtime(_))));
    }

    /// 认领 exit 哨兵的宿主，模拟 bolt 的 `classify_error`。
    struct SentryEnv;

    impl JsEnv for SentryEnv {
        fn install_base<'js>(&self, _ctx: &Ctx<'js>) -> rquickjs::Result<()> {
            Ok(())
        }

        fn install_scope<'js>(&self, _ctx: &Ctx<'js>, _scope: &dyn Scope) -> rquickjs::Result<()> {
            Ok(())
        }

        fn host_modules(&self, _permissions: &[String]) -> Vec<crate::env::HostModuleSpec> {
            Vec::new()
        }

        fn prepare_source(&self, _path: &Path, code: &str) -> Result<String, String> {
            Ok(code.to_string())
        }

        fn log(&self, _level: LogLevel, _message: &str) {}

        fn classify_error(&self, message: &str) -> Option<JsError> {
            message
                .strip_prefix("__bolt_exit_")?
                .strip_suffix("__")?
                .parse()
                .ok()
                .map(JsError::Exit)
        }
    }

    #[test]
    fn exit_sentinel_becomes_exit_error() {
        let tmp = TempDir::new("store-exit");
        let root = write_demo(
            &tmp,
            "export function quit() { throw new Error('__bolt_exit_7__'); }",
        );
        let store = ExtensionStore::new(Arc::new(SentryEnv));
        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .unwrap();

        assert_eq!(call(&store, "quit").unwrap_err(), JsError::Exit(7));
        assert!(JsError::Exit(7).is_control_flow());
    }

    #[test]
    fn argv_reaches_the_handler_as_an_array() {
        let tmp = TempDir::new("store-argv");
        let root = write_demo(
            &tmp,
            "export function echo(ctx) { return Array.isArray(ctx.argv) ? ctx.argv : 'not-array'; }",
        );
        let (env, _started, release) = gate_env();
        let _ = release.send(());
        let store = ExtensionStore::new(env);
        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .unwrap();

        let request = InvokeRequest::new("echo", CancelToken::new())
            .with_args(vec![serde_json::json!(1), serde_json::json!("a")]);
        assert_eq!(
            store.invoke(demo(), request).unwrap().as_deref(),
            Some(r#"[1,"a"]"#)
        );
    }

    #[test]
    fn argv_defaults_to_an_empty_array() {
        let tmp = TempDir::new("store-argv-default");
        let root = write_demo(
            &tmp,
            "export function size(ctx) { return Array.isArray(ctx.argv) ? ctx.argv.length : -1; }",
        );
        let (env, _started, release) = gate_env();
        let _ = release.send(());
        let store = ExtensionStore::new(env);
        store
            .ensure_loaded(demo(), "1.0.0", &root, &root.join("index.js"), &[])
            .unwrap();

        assert_eq!(call(&store, "size").unwrap().as_deref(), Some("0"));
    }
}
