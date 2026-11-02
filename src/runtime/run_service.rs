//! 应用级脚本运行服务。
//!
//! 持有整条运行链路——工作线程、取消令牌、交互窗口管理器——所以启动器和
//! 脚本管理器都能发起运行，而互不依赖对方的窗口。交互窗口本身（待命/进度/
//! 确认 + 原生选择器）在 [`InteractionWindowManager`] 里；这里只管记录
//! *有没有*东西在跑，并派生、监视工作线程。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::{Context, Entity, EventEmitter, Subscription, Task};

use super::interaction::InteractionBridge;
use super::interaction_window_manager::InteractionWindowManager;
use super::runner::{CancelToken, RunEnv};
use crate::host::HostRegistry;
use crate::ui::interaction_window::{InteractionEvent, InteractionWindow};
use bolt_console::ConsoleChannel;
use bolt_extensions::ExtensionStore;
use bolt_model::script::Language;

/// 一次正在进行中的脚本运行。
struct RunState {
    cancel: CancelToken,
    /// 工作线程在 `run_script` 返回后置位。
    done: Arc<AtomicBool>,
}

/// 运行开始（`true`）或结束（`false`）时发出，编辑器工具栏之类的 UI 因此可以
/// 跟着服务走而不必自己持有它。
pub struct RunStateChanged(pub bool);

/// 本轮要执行的东西：一个脚本文件，或一条扩展命令。
///
/// 两者共用同一条链路（工作线程 / 取消 / 交互窗口 / Console），区别只在工作
/// 线程里调谁：普通脚本走一次性 rquickjs 运行时，扩展命令走扩展自己的常驻
/// 运行时（`ExtensionStore`）。
pub enum RunTask {
    Script {
        content: String,
        language: Language,
        file_name: String,
    },
    Extension {
        extension_id: String,
        command: String,
        /// Console 里显示的名字（`扩展名 · 命令标题`）。
        label: String,
    },
}

impl RunTask {
    fn label(&self) -> &str {
        match self {
            Self::Script { file_name, .. } => file_name,
            Self::Extension { label, .. } => label,
        }
    }
}

/// 脚本与扩展命令共用的运行业务：发起、监视、取消，外加交互窗口同步。
pub struct RunService {
    scripts_dir: PathBuf,
    registry: Arc<HostRegistry>,
    /// 扩展命令的执行入口。脚本运行用不到，但两者共用同一条取消/交互链路，
    /// 所以放在同一个服务里。
    extensions: Arc<ExtensionStore>,
    /// 可克隆的句柄，好让运行器从工作线程往 Console 面板推输出。
    console_channel: ConsoleChannel,
    /// 空闲时为 `None`；同一时刻只允许一次运行。
    running: Option<RunState>,
    /// 交互浮窗与待答复请求（confirm / pickPath / requestFocus）。
    manager: InteractionWindowManager,
    /// 窗口可能在 drain 中途新开，订阅要跟着补。记当前已订阅的窗口实体，
    /// 与 manager 的不一致就重订（旧的 Subscription 随赋值被丢弃）。
    subscribed_window: Option<Entity<InteractionWindow>>,
    _sub_interaction: Option<Subscription>,
    /// 轮询 `running.done`，让监听方及时退出「运行中」状态。
    _run_task: Option<Task<()>>,
    /// 排空交互请求，并让交互窗口保持同步。
    _interaction_task: Option<Task<()>>,
}

impl EventEmitter<RunStateChanged> for RunService {}

impl RunService {
    /// 创建服务。此时没有任何运行在进行。
    pub fn new(
        scripts_dir: PathBuf,
        registry: Arc<HostRegistry>,
        console_channel: ConsoleChannel,
        extensions: Arc<ExtensionStore>,
    ) -> Self {
        Self {
            scripts_dir,
            registry,
            extensions,
            manager: InteractionWindowManager::new(console_channel.clone()),
            console_channel,
            running: None,
            subscribed_window: None,
            _sub_interaction: None,
            _run_task: None,
            _interaction_task: None,
        }
    }

    /// 当前是否有运行在进行中。
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// 执行一个脚本文件。
    pub fn start_script(
        &mut self,
        content: String,
        language: Language,
        file_name: String,
        cx: &mut Context<Self>,
    ) {
        self.start(
            RunTask::Script {
                content,
                language,
                file_name,
            },
            cx,
        );
    }

    /// 执行一条扩展命令（扩展必须已加载）。
    pub fn start_extension(
        &mut self,
        extension_id: String,
        command: String,
        label: String,
        cx: &mut Context<Self>,
    ) {
        self.start(
            RunTask::Extension {
                extension_id,
                command,
                label,
            },
            cx,
        );
    }

    /// 同一时刻只跑一个：忙的时候再来的请求直接拒绝并在 Console 里警告，而不是
    /// 排队（排队的运行会在用户早就忘了之后才触发）。
    fn start(&mut self, task: RunTask, cx: &mut Context<Self>) {
        if self.running.is_some() {
            self.console_channel
                .warn(format!("已有脚本在运行中，忽略本次执行：{}", task.label()));
            return;
        }
        let cancel = CancelToken::new();
        let done = Arc::new(AtomicBool::new(false));
        self.running = Some(RunState {
            cancel: cancel.clone(),
            done: done.clone(),
        });
        cx.emit(RunStateChanged(true));

        // 每轮新建一座交互桥：脚本侧经它与 UI 打交道（progress、confirm、
        // pickPath、requestFocus）。扩展命令也用它——每轮覆盖到扩展常驻上下文上。
        let (interaction, interaction_rx) = InteractionBridge::new(cancel.clone());
        self.manager
            .begin_run(interaction.clone(), interaction_rx, cancel.clone());

        let console = self.console_channel.clone();
        let scripts_dir = self.scripts_dir.clone();
        let registry = self.registry.clone();
        let extensions = self.extensions.clone();
        let env = RunEnv {
            scripts_dir,
            registry,
            console,
            cancel,
            interaction,
            extensions: Some(extensions),
        };
        std::thread::spawn(move || {
            match task {
                RunTask::Script {
                    content,
                    language,
                    file_name,
                } => super::runner::run_script(&content, language, &file_name, env),
                RunTask::Extension {
                    extension_id,
                    command,
                    label,
                } => super::runner::run_extension(extension_id, &command, &label, env),
            }
            done.store(true, Ordering::SeqCst);
        });

        // 工作线程碰不到 gpui 实体，只能轮询完成状态（与
        // EditorPanel::diagnostics_loop 同一套做法）。
        self._run_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                let finished = this.update(cx, |svc, cx| svc.poll_run(cx)).unwrap_or(true);
                if finished {
                    break;
                }
            }
        }));

        // 应答交互请求，并让交互窗口保持同步。比上面的运行轮询更快：待命释放
        // 检测和进度条都靠它推动。
        self._interaction_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(super::interaction_window_manager::INTERACTION_TICK)
                    .await;
                let finished = this
                    .update(cx, |svc, cx| svc.poll_interaction(cx))
                    .unwrap_or(true);
                if finished {
                    break;
                }
            }
        }));
    }

    /// 翻转取消令牌；运行器会在下一次中断检查、或竞争的等待先返回时中止。
    pub fn stop(&mut self) {
        if let Some(run) = &self.running {
            run.cancel.cancel();
        }
    }

    /// 本轮运行结束后返回 true；返回前先通知监听方、拆掉交互状态。
    fn poll_run(&mut self, cx: &mut Context<Self>) -> bool {
        let finished = self
            .running
            .as_ref()
            .map(|run| run.done.load(Ordering::SeqCst))
            .unwrap_or(true);
        if finished {
            self.running = None;
            cx.emit(RunStateChanged(false));
            // 脚本侧已经没了：丢弃所有待答复请求并关掉交互窗口，不留任何
            // 等在死运行上的东西。
            self.manager.end_run(cx);
        }
        finished
    }

    /// 排空交互请求，能就地解决的解决掉，并让交互窗口保持同步。本轮运行结束
    /// 后返回 true。
    fn poll_interaction(&mut self, cx: &mut Context<Self>) -> bool {
        let finished = self.running.is_none();
        self.manager.drain(cx);

        // drain 里可能刚开了一个新交互窗口：给它补上按钮事件订阅。窗口实体
        // 变化（新开或关闭）时重订，旧的 Subscription 随赋值被丢弃。
        let current = self.manager.window().cloned();
        if current != self.subscribed_window {
            if let Some(view) = &current {
                self._sub_interaction = Some(cx.subscribe(
                    view,
                    |this, _view, event: &InteractionEvent, _cx| {
                        this.manager.on_interaction_event(*event);
                    },
                ));
            }
            self.subscribed_window = current;
        }

        finished
    }
}
