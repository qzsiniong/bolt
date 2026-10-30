//! 每插件一个专用线程，线程里跑一个常驻的 QuickJS 运行时。
//!
//! 为什么是线程 + 消息而不是 `AsyncRuntime::spawn`：plux 的 `Function::call` 与
//! `Manager` 的方法都是同步的，而宿主的定时器、`fetch`、`Async(...)` 宿主函数又都
//! 依赖 tokio 反应堆。给每个插件一条线程、线程内自带一个 current-thread 运行时，
//! 让同步的 `invoke` 与异步的引擎能干净地待在一起，也顺带把"引擎不跨线程"这条
//! QuickJS 约束变成了结构性保证。
//!
//! 超时与取消靠 `AsyncRuntime` 的 interrupt handler：宿主置位
//! [`PluginWorker::abort`]，QuickJS 在下一个中断点抛不可捕获异常，线程回到消息
//! 循环。标志只由工人线程在每条命令开头清零，因此不存在跨线程竞争。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module, Value};

use crate::env::JsEnv;
use crate::error::JsError;
use crate::loader::{PluginLoader, PluginResolver};
use crate::store::InvokeRequest;
use crate::value::{format_caught, value_to_json};

/// 与普通脚本一致：栈上限 8 MiB（`while(true)` 递归也不会立刻踩穿）。
const MAX_STACK_SIZE: usize = 8 * 1024 * 1024;
/// 单个插件的内存上限。
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;
/// 命令结束后驱动残留任务的预算。不能裸等 `idle()`：插件里一个没清掉的
/// `setInterval` 会让它永不返回。
const IDLE_BUDGET: Duration = Duration::from_millis(200);

/// 加载（建引擎 + 求值入口模块）的超时。
pub(crate) const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// 命令派发脚本。三档回退顺序：具名导出 → default 对象 → `globalThis.plux.commands`。
const DISPATCH: &str = r#"
globalThis.__plux_result = undefined;
globalThis.__plux_missing = undefined;
const call = globalThis.__plux_call || {};
const name = call.command;
const ns = globalThis.__plux_ns || {};
const fallback = globalThis.__plux_default;
let handler = (typeof ns[name] === "function") ? ns[name] : undefined;
if (!handler && fallback && typeof fallback[name] === "function") {
  handler = fallback[name].bind(fallback);
}
if (!handler) {
  const root = globalThis.plux;
  if (root && root.commands && typeof root.commands[name] === "function") {
    handler = root.commands[name];
  }
}
if (!handler) {
  globalThis.__plux_missing = name;
} else {
  const argv = Array.isArray(call.argv) ? call.argv : [];
  globalThis.__plux_result = await handler({
    command: name,
    plugin: { id: globalThis.__plux_plugin_id, version: globalThis.__plux_plugin_version },
    root: call.root,
    argv,
  });
}
"#;

/// 工人线程需要知道的插件身份与路径。
#[derive(Clone, Debug)]
pub(crate) struct WorkerSpec {
    pub id: String,
    pub version: String,
    pub root: PathBuf,
    pub entry: PathBuf,
}

pub(crate) enum WorkerMsg {
    Invoke {
        request: InvokeRequest,
        reply: Sender<Result<Option<String>, JsError>>,
    },
    Shutdown,
}

/// 一个插件的工人线程句柄。
pub(crate) struct PluginWorker {
    tx: Sender<WorkerMsg>,
    abort: Arc<AtomicBool>,
    join: parking_lot::Mutex<Option<JoinHandle<()>>>,
}

impl PluginWorker {
    /// 起线程并等它把引擎与入口模块准备好。
    ///
    /// 返回时插件已可执行命令；失败时线程已退出，不需要再清理。
    pub(crate) fn spawn(spec: WorkerSpec, env: Arc<dyn JsEnv>) -> Result<Arc<Self>, JsError> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let abort = Arc::new(AtomicBool::new(false));
        let thread_abort = abort.clone();

        let join = std::thread::Builder::new()
            .name(format!("plux-js:{}", spec.id))
            .spawn(move || worker_main(spec, env, thread_abort, rx, ready_tx))
            .map_err(|e| JsError::Runtime(format!("无法启动插件线程：{e}")))?;

        let worker = Arc::new(Self {
            tx,
            abort,
            join: parking_lot::Mutex::new(Some(join)),
        });

        match ready_rx.recv_timeout(LOAD_TIMEOUT) {
            Ok(Ok(())) => Ok(worker),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                worker.abort();
                Err(JsError::Timeout(LOAD_TIMEOUT))
            }
        }
    }

    /// 投递一条消息（超时/取消由 `PluginStore` 判定，这里只负责发）。
    pub(crate) fn send(&self, message: WorkerMsg) -> Result<(), JsError> {
        self.tx
            .send(message)
            .map_err(|_| JsError::Runtime("插件线程已退出".to_string()))
    }

    /// 请求中断当前命令：QuickJS 在下一个中断点抛出。
    pub(crate) fn abort(&self) {
        self.abort.store(true, Ordering::SeqCst);
    }

    /// 停线程并等它退出。
    pub(crate) fn shutdown(&self) {
        self.abort();
        let _ = self.tx.send(WorkerMsg::Shutdown);
        if let Some(join) = self.join.lock().take() {
            let _ = join.join();
        }
    }
}

impl Drop for PluginWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_main(
    spec: WorkerSpec,
    env: Arc<dyn JsEnv>,
    abort: Arc<AtomicBool>,
    rx: Receiver<WorkerMsg>,
    ready: Sender<Result<(), JsError>>,
) {
    let tokio_rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let _ = ready.send(Err(JsError::Runtime(format!("无法启动异步运行时：{e}"))));
            return;
        }
    };
    // 宿主模块的 setTimeout / sleep / fetch 都要求"当前线程有反应堆"。
    let _guard = tokio_rt.enter();

    let engine = tokio_rt.block_on(build_engine(&spec, &env, &abort));
    let (runtime, context) = match engine {
        Ok(engine) => {
            let _ = ready.send(Ok(()));
            engine
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    let mut seq: u64 = 0;
    while let Ok(message) = rx.recv() {
        match message {
            WorkerMsg::Invoke { request, reply } => {
                // 上一条命令的超时标志在这里清掉：只有工人线程会写 false，
                // 而它此刻一定已经从上次求值中返回。
                abort.store(false, Ordering::SeqCst);
                seq += 1;
                let result = tokio_rt.block_on(invoke(&context, &env, &request, seq));
                let _ = reply.send(result);
                // 让残留的定时器/异步任务跑完，但有上限。
                tokio_rt.block_on(async {
                    let _ = tokio::time::timeout(IDLE_BUDGET, runtime.idle()).await;
                });
            }
            WorkerMsg::Shutdown => break,
        }
    }

    drop(context);
    drop(runtime);
}

async fn build_engine(
    spec: &WorkerSpec,
    env: &Arc<dyn JsEnv>,
    abort: &Arc<AtomicBool>,
) -> Result<(AsyncRuntime, AsyncContext), JsError> {
    let runtime = AsyncRuntime::new().map_err(|e| JsError::Runtime(e.to_string()))?;
    runtime.set_max_stack_size(MAX_STACK_SIZE).await;
    runtime.set_memory_limit(MEMORY_LIMIT).await;

    let flag = abort.clone();
    runtime
        .set_interrupt_handler(Some(Box::new(move || flag.load(Ordering::SeqCst))))
        .await;

    let loader = PluginLoader::new(spec.root.clone(), env.clone());
    let resolver = PluginResolver::new(
        spec.root.clone(),
        env.host_modules().into_iter().map(|m| m.name).collect(),
    );
    runtime.set_loader(resolver, loader).await;

    let context = AsyncContext::full(&runtime)
        .await
        .map_err(|e| JsError::Runtime(e.to_string()))?;

    let entry = spec
        .entry
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| JsError::Invalid(format!("非法的入口文件名：{}", spec.entry.display())))?
        .to_string();

    let spec_id = spec.id.clone();
    let spec_version = spec.version.clone();
    let root = spec.root.to_string_lossy().into_owned();

    context
        .async_with(async |ctx| {
            env.install_base(&ctx)
                .map_err(|e| JsError::Script(format!("宿主注入失败：{e}")))?;

            ctx.globals()
                .set("__plux_plugin_id", spec_id)
                .map_err(runtime_error)?;
            ctx.globals()
                .set("__plux_plugin_version", spec_version)
                .map_err(runtime_error)?;
            ctx.globals()
                .set("__plux_root", root)
                .map_err(runtime_error)?;

            // 把入口模块的命名空间挂到全局，供后续每条命令的派发脚本读取。
            let bridge = format!(
                "import * as ns from {entry:?};\nglobalThis.__plux_ns = ns;\nglobalThis.__plux_default = (ns && ns.default) || undefined;\n",
            );
            let promise = Module::evaluate(ctx.clone(), "<plux:entry>", bridge)
                .map_err(|e| {
                    let caught = rquickjs::CaughtError::from_error(&ctx, e);
                    JsError::Script(format_caught(&caught))
                })?;
            promise
                .into_future::<()>()
                .await
                .catch(&ctx)
                .map_err(|e| JsError::Script(format_caught(&e)))?;

            Ok::<(), JsError>(())
        })
        .await?;

    Ok((runtime, context))
}

async fn invoke(
    context: &AsyncContext,
    env: &Arc<dyn JsEnv>,
    request: &InvokeRequest,
    seq: u64,
) -> Result<Option<String>, JsError> {
    context
        .async_with(async |ctx| {
            if let Some(scope) = request.scope.as_deref() {
                env.install_scope(&ctx, scope)
                    .map_err(|e| JsError::Script(format!("交互 API 注入失败：{e}")))?;
            }

            let payload = serde_json::json!({
                "command": request.command,
                "argv": request
                    .args
                    .as_deref()
                    .map(|args| serde_json::from_str::<serde_json::Value>(args).unwrap_or(serde_json::Value::Null))
                    .unwrap_or(serde_json::Value::Null),
                "root": ctx.globals().get::<_, String>("__plux_root").unwrap_or_default(),
            });
            let call = ctx.json_parse(payload.to_string()).map_err(runtime_error)?;
            ctx.globals().set("__plux_call", call).map_err(runtime_error)?;

            let name = format!("<plux:call:{seq}>");
            let promise = Module::evaluate(ctx.clone(), name.as_str(), DISPATCH)
                .map_err(|e| {
                    let caught = rquickjs::CaughtError::from_error(&ctx, e);
                    JsError::Script(format_caught(&caught))
                })?;
            promise
                .into_future::<()>()
                .await
                .catch(&ctx)
                .map_err(|e| JsError::Script(format_caught(&e)))?;

            if let Some(missing) = read_string(&ctx, "__plux_missing")? {
                return Err(JsError::MissingHandler(missing));
            }
            let value: Value = ctx.globals().get("__plux_result").map_err(runtime_error)?;
            value_to_json(&ctx, &value).map_err(|e| JsError::Script(e.to_string()))
        })
        .await
}

fn read_string(ctx: &rquickjs::Ctx<'_>, name: &str) -> Result<Option<String>, JsError> {
    let value: Value = ctx.globals().get(name).map_err(runtime_error)?;
    if value.is_undefined() || value.is_null() {
        return Ok(None);
    }
    match value.as_string().and_then(|text| text.to_string().ok()) {
        Some(text) if !text.is_empty() => Ok(Some(text)),
        _ => Ok(None),
    }
}

fn runtime_error(error: rquickjs::Error) -> JsError {
    JsError::Runtime(error.to_string())
}
