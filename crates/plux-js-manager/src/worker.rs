//! 每插件一个专用线程，线程里跑一个常驻的 QuickJS 运行时。
//!
//! 为什么是线程 + 消息而不是 `AsyncRuntime::spawn`：plux 的 `Function::call` 与
//! `Manager` 的方法都是同步的，而宿主的定时器、`fetch`、`Async(...)` 宿主函数又都
//! 依赖 tokio 反应堆。给每个插件一条线程、线程内自带一个 current-thread 运行时，
//! 让同步的 `invoke` 与异步的引擎能干净地待在一起，也顺带把"引擎不跨线程"这条
//! QuickJS 约束变成了结构性保证。
//!
//! **加载是非阻塞的**：`spawn` 只起线程就返回，引擎构建与入口求值都在工人线程里
//! 跑。就绪与否由 `LoadState` 记录——它在 gpui 主线程上被 `ensure_loaded` 调用，
//! 30s 的加载预算不该算在 UI 的响应时间上。
//!
//! 超时与取消靠 `AsyncRuntime` 的 interrupt handler：宿主置位
//! [`PluginWorker::abort`]，QuickJS 在下一个中断点抛不可捕获异常，线程回到消息
//! 循环。标志只由工人线程在每条命令开头清零，因此不存在跨线程竞争。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Condvar, Mutex};
use rquickjs::{AsyncContext, AsyncRuntime, CatchResultExt, Module, Value};

use crate::cancel::CancelToken;
use crate::env::{JsEnv, LogLevel};
use crate::error::JsError;
use crate::loader::{PluginLoader, PluginResolver};
use crate::store::{InvokeRequest, RuntimeState};
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

/// 等待插件回复 / 等待就绪的轮询间隔：既用来查超时，也用来查用户取消。
pub(crate) const POLL: Duration = Duration::from_millis(100);

/// 丢给 `Drop` 的宽限：正常停在消息循环里的线程毫秒级退出。
const DROP_GRACE: Duration = Duration::from_millis(500);

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

/// 工人线程需要知道的插件身份、路径与权限。
#[derive(Clone, Debug)]
pub(crate) struct WorkerSpec {
    pub id: String,
    pub version: String,
    pub root: PathBuf,
    pub entry: PathBuf,
    /// 清单里声明的权限（`extension.json` 的 `permissions`），缺省为空。
    pub permissions: Vec<String>,
}

pub(crate) enum WorkerMsg {
    Invoke {
        request: InvokeRequest,
        reply: Sender<Result<Option<String>, JsError>>,
    },
    Shutdown,
}

/// 运行时生命周期：一次性的就绪事件，但**结论要能被之后的每一次 invoke 反复读到**。
///
/// 这也是不用 ready 通道的原因：`bounded(1)` 只能被一个读者消费，第二个读者只会
/// 看到 `Disconnected`，分不清"还在加载"和"加载失败"。
#[derive(Clone, Debug)]
pub(crate) enum LoadState {
    Loading,
    Ready,
    /// 加载阶段失败：线程已（或即将）退出。错误留给之后每一次调用。
    Failed(JsError),
    /// 曾就绪，之后线程退出（收到 Shutdown 或 panic）。
    Exited(JsError),
}

impl LoadState {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Failed(_) | Self::Exited(_))
    }

    fn error(&self) -> Option<JsError> {
        match self {
            Self::Failed(error) | Self::Exited(error) => Some(error.clone()),
            _ => None,
        }
    }
}

/// 工人线程与等待者共享的状态。锁只保护这一个字段，`Condvar` 广播所有等待者。
struct Shared {
    state: Mutex<LoadState>,
    ready: Condvar,
    /// 起线程的时刻：加载超时按它算，不按每次 invoke 算（否则每次调用都能刷新 30s）。
    spawned_at: Instant,
}

impl Shared {
    fn mark_ready(&self) {
        *self.state.lock() = LoadState::Ready;
        self.ready.notify_all();
    }

    /// 先落 Console 再置状态：用户即使不再点这条命令，也能在面板里看到原因。
    fn fail(&self, error: JsError, env: &dyn JsEnv, label: &str) {
        let level = if error.is_control_flow() {
            LogLevel::Warn
        } else {
            LogLevel::Error
        };
        env.log(level, &format!("插件 {label} 加载失败：{error}"));
        *self.state.lock() = LoadState::Failed(error);
        self.ready.notify_all();
    }
}

/// 一个插件的工人线程句柄。
pub(crate) struct PluginWorker {
    tx: Sender<WorkerMsg>,
    abort: Arc<AtomicBool>,
    join: Mutex<Option<JoinHandle<()>>>,
    shared: Arc<Shared>,
    /// 是否已经请求过退出：`unload` 之后 `Drop` 会再调一次 `shutdown`，
    /// 不记住这一点就会把 500ms 的宽限等两遍。
    stopped: AtomicBool,
}

impl PluginWorker {
    /// 起线程并**立即返回**：引擎构建与入口求值都在工人线程里跑。
    ///
    /// 只有"线程建不起来"会在这里失败；引擎加载的成败通过 [`Self::state`] 与
    /// [`Self::wait_ready`] 传递，并已由 `env.log` 报给 Console。
    pub(crate) fn spawn(spec: WorkerSpec, env: Arc<dyn JsEnv>) -> Result<Arc<Self>, JsError> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let abort = Arc::new(AtomicBool::new(false));
        let thread_abort = abort.clone();
        let shared = Arc::new(Shared {
            state: Mutex::new(LoadState::Loading),
            ready: Condvar::new(),
            spawned_at: Instant::now(),
        });
        let thread_shared = shared.clone();

        let join = std::thread::Builder::new()
            .name(format!("plux-js:{}", spec.id))
            .spawn(move || worker_main(spec, env, thread_abort, rx, thread_shared))
            .map_err(|e| JsError::Runtime(format!("无法启动插件线程：{e}")))?;

        Ok(Arc::new(Self {
            tx,
            abort,
            join: Mutex::new(Some(join)),
            shared,
            stopped: AtomicBool::new(false),
        }))
    }

    /// 等就绪。并发调用共享同一次等待与同一份结论。
    ///
    /// 加载也算进本次执行预算——否则最坏是 30s 加载 + 60s 执行。超时**不 abort**：
    /// 中断一次正在进行的加载会把它永久打成 Failed。
    pub(crate) fn wait_ready(
        &self,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<(), JsError> {
        let load_deadline = self.shared.spawned_at + LOAD_TIMEOUT;
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock();
        loop {
            match &*state {
                LoadState::Ready => return Ok(()),
                LoadState::Failed(error) => {
                    return Err(JsError::Runtime(format!("插件加载失败：{error}")));
                }
                LoadState::Exited(error) => return Err(error.clone()),
                LoadState::Loading => {}
            }
            if cancel.is_cancelled() {
                return Err(JsError::Cancelled);
            }
            let now = Instant::now();
            if now >= load_deadline {
                return Err(JsError::Runtime(format!(
                    "插件加载超时（{}s）",
                    LOAD_TIMEOUT.as_secs()
                )));
            }
            if now >= deadline {
                return Err(JsError::Timeout(timeout));
            }
            // 切片上限 POLL：用户点"停止"后 100ms 内能看到 Cancelled。
            let slice = (deadline.min(load_deadline) - now).min(POLL);
            self.shared.ready.wait_for(&mut state, slice);
        }
    }

    /// 健康查询：不阻塞、不消费结论。
    pub(crate) fn state(&self) -> RuntimeState {
        match &*self.shared.state.lock() {
            LoadState::Loading => RuntimeState::Loading,
            LoadState::Ready => RuntimeState::Ready,
            _ => RuntimeState::Dead,
        }
    }

    pub(crate) fn last_error(&self) -> Option<JsError> {
        self.shared.state.lock().error()
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

    /// 停线程，但**只在 `grace` 内等收尾**；超时就丢掉 JoinHandle 放手。
    ///
    /// QuickJS 线程无法被强杀：加载中的线程可能正卡在引擎构建或宿主阻塞调用上，
    /// 无条件 join 会把"删插件 / 重建运行时"变成主线程上的最长 30s 卡顿。
    /// 返回 true 表示线程已经收尾（此后资源全部释放）。
    pub(crate) fn shutdown(&self, grace: Duration) -> bool {
        let first_request = !self.stopped.swap(true, Ordering::SeqCst);
        self.abort();
        let _ = self.tx.send(WorkerMsg::Shutdown);

        // 已经请求过（unload 之后的 Drop）：不再重复等宽限，只在线程确实收尾时 join。
        if !first_request {
            return self.reap();
        }

        let settled = {
            let mut state = self.shared.state.lock();
            let deadline = Instant::now() + grace;
            loop {
                if state.is_terminal() {
                    break true;
                }
                let now = Instant::now();
                if now >= deadline {
                    break false;
                }
                self.shared.ready.wait_for(&mut state, deadline - now);
            }
        };

        if settled {
            self.reap();
        }
        settled
    }

    /// 线程已到终止态时 join（不会阻塞：终止态在引擎析构之后才置位）。
    fn reap(&self) -> bool {
        if !self.shared.state.lock().is_terminal() {
            return false;
        }
        if let Some(join) = self.join.lock().take() {
            let _ = join.join();
        }
        true
    }

    /// 测试专用：让线程退出，但**不摘表**，用来模拟"运行时意外死亡"。
    #[cfg(test)]
    pub(crate) fn exit_for_test(&self) {
        let _ = self.tx.send(WorkerMsg::Shutdown);
    }
}

impl Drop for PluginWorker {
    fn drop(&mut self) {
        self.shutdown(DROP_GRACE);
    }
}

/// 线程退出守卫：声明在 `worker_main` 最前面 → 最后被 drop，于是终止态一定在
/// context/runtime 析构之后才置位（`shutdown` 因此可以只等终态就 join），
/// panic 时也由 unwind 的 `Drop` 兜住。
struct ExitGuard {
    shared: Arc<Shared>,
    env: Arc<dyn JsEnv>,
    label: String,
}

impl Drop for ExitGuard {
    fn drop(&mut self) {
        let needs_log = {
            let mut state = self.shared.state.lock();
            match &*state {
                // 已经写明原因的失败不要被覆盖。
                LoadState::Failed(_) | LoadState::Exited(_) => false,
                LoadState::Loading => {
                    *state =
                        LoadState::Failed(JsError::Runtime("插件线程在就绪前退出".to_string()));
                    true
                }
                LoadState::Ready => {
                    *state = LoadState::Exited(JsError::Runtime("插件运行时已退出".to_string()));
                    false
                }
            }
        };
        self.shared.ready.notify_all();
        if needs_log {
            self.env.log(
                LogLevel::Error,
                &format!("插件 {} 加载失败：线程在就绪前退出", self.label),
            );
        }
    }
}

fn worker_main(
    spec: WorkerSpec,
    env: Arc<dyn JsEnv>,
    abort: Arc<AtomicBool>,
    rx: Receiver<WorkerMsg>,
    shared: Arc<Shared>,
) {
    let _exit = ExitGuard {
        shared: shared.clone(),
        env: env.clone(),
        label: format!("{} v{}", spec.id, spec.version),
    };

    let tokio_rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            shared.fail(
                JsError::Runtime(format!("无法启动异步运行时：{e}")),
                &*env,
                &format!("{} v{}", spec.id, spec.version),
            );
            return;
        }
    };
    // 宿主模块的 setTimeout / sleep / fetch 都要求"当前线程有反应堆"。
    let _guard = tokio_rt.enter();

    let engine = tokio_rt.block_on(build_engine(&spec, &env, &abort));
    let (runtime, context) = match engine {
        Ok(engine) => {
            shared.mark_ready();
            engine
        }
        Err(e) => {
            shared.fail(e, &*env, &format!("{} v{}", spec.id, spec.version));
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

    let loader = PluginLoader::new(spec.root.clone(), env.clone(), &spec.permissions);
    let resolver = PluginResolver::new(
        spec.root.clone(),
        env.host_modules(&spec.permissions)
            .into_iter()
            .map(|m| m.name)
            .collect(),
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
                .map_err(|e| caught(env, rquickjs::CaughtError::from_error(&ctx, e)))?;
            promise
                .into_future::<()>()
                .await
                .catch(&ctx)
                .map_err(|e| caught(env, e))?;

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
                // 缺省给空数组：DISPATCH 里 `Array.isArray(call.argv)` 必须成立。
                "argv": request.args.clone().unwrap_or_default(),
                "root": ctx.globals().get::<_, String>("__plux_root").unwrap_or_default(),
            });
            let call = ctx.json_parse(payload.to_string()).map_err(runtime_error)?;
            ctx.globals()
                .set("__plux_call", call)
                .map_err(runtime_error)?;

            let name = format!("<plux:call:{seq}>");
            let promise = Module::evaluate(ctx.clone(), name.as_str(), DISPATCH)
                .map_err(|e| caught(env, rquickjs::CaughtError::from_error(&ctx, e)))?;
            promise
                .into_future::<()>()
                .await
                .catch(&ctx)
                .map_err(|e| caught(env, e))?;

            if let Some(missing) = read_string(&ctx, "__plux_missing")? {
                return Err(JsError::MissingHandler(missing));
            }
            let value: Value = ctx.globals().get("__plux_result").map_err(runtime_error)?;
            value_to_json(&ctx, &value).map_err(|e| JsError::Script(e.to_string()))
        })
        .await
}

/// 异常 → `JsError`：先问宿主认不认这条控制流（`exit` 之类的正常终止），
/// 再退回普通脚本错误。哨兵格式属于宿主约定，crate 不硬编码。
fn caught(env: &Arc<dyn JsEnv>, error: rquickjs::CaughtError) -> JsError {
    if let rquickjs::CaughtError::Exception(exception) = &error
        && let Some(message) = exception.message()
        && let Some(flow) = env.classify_error(&message)
    {
        return flow;
    }
    JsError::Script(format_caught(&error))
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
