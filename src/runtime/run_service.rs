//! Application-level script run service.
//!
//! Owns the run chain — worker thread, cancel token and the interaction
//! window manager — so both the launcher and the script manager can start
//! runs without depending on each other's windows. The interaction window
//! itself (standby/progress/confirm + native picker) lives in
//! [`InteractionWindowManager`]; this type only tracks *whether* something
//! is running and spawns/monitors the worker thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::{Context, Entity, EventEmitter, Subscription, Task};

use super::interaction::InteractionBridge;
use super::interaction_window_manager::InteractionWindowManager;
use super::runner::{CancelToken, RunEnv};
use crate::console_channel::ConsoleChannel;
use crate::host::HostRegistry;
use crate::model::script::Language;
use crate::ui::interaction_window::{InteractionEvent, InteractionWindow};
use plux_js_manager::ExtensionStore;
use plux_rs::Bundle;

/// A single script run currently in flight.
struct RunState {
    cancel: CancelToken,
    /// Set by the worker thread once `run_script` returns.
    done: Arc<AtomicBool>,
}

/// Emitted when a run starts (`true`) or finishes (`false`), so UI like the
/// editor toolbar can follow the service without owning it.
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
        bundle: Bundle,
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

pub struct RunService {
    scripts_dir: PathBuf,
    registry: Arc<HostRegistry>,
    /// 扩展命令的执行入口。脚本运行用不到，但两者共用同一条取消/交互链路，
    /// 所以放在同一个服务里。
    extensions: Arc<ExtensionStore>,
    /// Cloneable handle so the runner can push output to the Console panel
    /// from a worker thread.
    console_channel: ConsoleChannel,
    /// `None` while idle; only one run at a time.
    running: Option<RunState>,
    /// 交互浮窗与待答复请求（confirm / pickPath / requestFocus）。
    manager: InteractionWindowManager,
    /// 窗口可能在 drain 中途新开，订阅要跟着补。记当前已订阅的窗口实体，
    /// 与 manager 的不一致就重订（旧的 Subscription 随赋值被丢弃）。
    subscribed_window: Option<Entity<InteractionWindow>>,
    _sub_interaction: Option<Subscription>,
    /// Polls `running.done` so listeners leave the "running" state.
    _run_task: Option<Task<()>>,
    /// Drains interaction requests and keeps the interaction window in sync.
    _interaction_task: Option<Task<()>>,
}

impl EventEmitter<RunStateChanged> for RunService {}

impl RunService {
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
        bundle: Bundle,
        command: String,
        label: String,
        cx: &mut Context<Self>,
    ) {
        self.start(
            RunTask::Extension {
                bundle,
                command,
                label,
            },
            cx,
        );
    }

    /// One run at a time: a second request while busy is refused with a Console
    /// warning rather than queued (a queued run would fire long after the user
    /// asked for it).
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

        // Fresh interaction bridge per run: the script side talks to the UI
        // through it (progress, confirm, pickPath, requestFocus). 扩展命令也用它
        // ——每轮覆盖到扩展常驻上下文上。
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
                    bundle,
                    command,
                    label,
                } => super::runner::run_extension(bundle, &command, &label, env),
            }
            done.store(true, Ordering::SeqCst);
        });

        // The worker thread cannot reach gpui entities, so poll for
        // completion (same pattern as EditorPanel::diagnostics_loop).
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

        // Answer interaction requests and keep the interaction window in
        // sync. Runs faster than the run poll above: it drives the standby
        // release check and the progress bar.
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

    /// Flip the token; the runner aborts at the next interrupt check or when
    /// the racing wait wins.
    pub fn stop(&mut self) {
        if let Some(run) = &self.running {
            run.cancel.cancel();
        }
    }

    /// Returns true once the in-flight run has finished, after notifying
    /// listeners and tearing down interaction state.
    fn poll_run(&mut self, cx: &mut Context<Self>) -> bool {
        let finished = self
            .running
            .as_ref()
            .map(|run| run.done.load(Ordering::SeqCst))
            .unwrap_or(true);
        if finished {
            self.running = None;
            cx.emit(RunStateChanged(false));
            // The script side is gone: drop every pending request and close the
            // interaction window, so nothing is left waiting on a dead run.
            self.manager.end_run(cx);
        }
        finished
    }

    /// Drain interaction requests, resolve what the UI can resolve and keep
    /// the interaction window in sync. Returns true once the run has ended.
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
