//! 扩展宿主：`ExtensionRegistry`（登记/注销/懒加载编排）+ `ExtensionStore`（执行）。
//!
//! 注册表按值持有即可（无裸指针约束）；执行入口经 `Arc` 跨线程共享。磁盘扫描、
//! 禁用列表与冷却期由本模块负责，生命周期语义在 `bolt-extensions` 里。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bolt_extensions::{
    ExtensionRegistry, ExtensionStore, JsEnv, RegisterError, RegisterOutcome, RuntimeState, UiSpec,
};

use super::manifest;
use super::prefs;

/// 重建运行时的冷却期：坏扩展被连点时不该每次都起一条新线程再失败一次。
const REBUILD_COOLDOWN: Duration = Duration::from_secs(3);

/// Launcher 里的一条命令（已脱离注册表的借用，可 move 进 `on_confirm`）。
#[derive(Clone, Debug)]
pub(crate) struct CommandTarget {
    pub(crate) extension_id: String,
    pub(crate) extension_version: String,
    pub(crate) extension_name: String,
    /// 扩展目录：UI 命令用它做 gpui-shell 的应用根。
    pub(crate) root: PathBuf,
    pub(crate) command_id: String,
    pub(crate) title: String,
    pub(crate) description: Option<String>,
    /// Launcher 行图标（资产路径）：命令 `icon` 优先，扩展级 `icon` 兜底；
    /// 都没写为 `None`（渲染端再退到内置默认图标）。
    pub(crate) icon: Option<String>,
    /// 给了就是 UI 命令。
    pub(crate) ui: Option<UiSpec>,
}

impl CommandTarget {
    /// Launcher item 的 id：`ext.<extension-id>.<command-id>`。
    pub fn item_id(&self) -> String {
        format!("ext.{}.{}", self.extension_id, self.command_id)
    }

    pub(crate) fn is_ui(&self) -> bool {
        self.ui.is_some()
    }
}

/// 一次 [`ExtensionsHost::refresh`] 的结果。
///
/// 分两档：`errors` 是「扩展坏了」（清单写坏、注册失败、重载被拒），`notices` 是
/// 「扩展没坏，只是你改了内容却没升版本号」。都塞进 errors 会让开发期的正常迭代
/// 看起来像故障，作者很快就会学会忽略控制台里的红字。
#[derive(Debug, Default)]
pub(crate) struct RefreshReport {
    pub(crate) errors: Vec<String>,
    pub(crate) notices: Vec<String>,
}

pub(crate) struct ExtensionsHost {
    /// 注册表：登记、注销、懒加载编排（指纹基线也在 Registration 里）。
    registry: ExtensionRegistry,
    store: Arc<ExtensionStore>,
    extensions_dir: PathBuf,
    /// 本次扫描时磁盘上真实存在的扩展目录。
    ///
    /// **与注册成败无关**：清单写坏的扩展也算存在，否则作者保存一次坏清单就会把
    /// 正在用的扩展判成"已删除"，连带杀掉它的运行时。
    /// `None` = 还没成功扫描过，此时不做任何过滤。
    live: Option<HashSet<PathBuf>>,
    /// 每个扩展上次重建运行时的时刻，用来给重建加冷却。
    rebuilt_at: HashMap<String, Instant>,
    /// 禁用列表文件的位置（`extensions-disabled.json`）。测试传临时路径，
    /// 免得读到开发机上真实的偏好。
    disabled_path: PathBuf,
    /// 被用户禁用的扩展 id。`refresh` 每轮从磁盘重读，这样手改文件也能生效。
    disabled: BTreeSet<String>,
    /// 上轮扫描中未能注册的目录与原因（清单写坏 / 同 id 冲突），
    /// [`ExtensionsHost::inventory`] 用它补行。
    broken: Vec<(PathBuf, String)>,
}

/// 扩展管理窗口的一行：一个扩展目录（含清单写坏的）。
///
/// 全是快照值：拿到之后不再借用注册表，可以放心 move 进 UI。
#[derive(Clone, Debug)]
pub(crate) struct ExtensionEntry {
    pub(crate) id: String,
    pub(crate) version: String,
    pub(crate) root: PathBuf,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    /// 扩展图标（资产路径）；`None` = 渲染端用内置默认。
    pub(crate) icon: Option<String>,
    pub(crate) command_count: usize,
    pub(crate) ui_command_count: usize,
    pub(crate) permissions: Vec<String>,
    pub(crate) enabled: bool,
    /// 注册表里有它。清单写坏 / 同 id 冲突 / 被禁用时为 `false`。
    pub(crate) registered: bool,
    pub(crate) runtime: Option<RuntimeState>,
    pub(crate) last_error: Option<String>,
    /// 清单读取 / 校验失败的原因（此时 `registered == false`）。
    pub(crate) manifest_error: Option<String>,
}

impl ExtensionsHost {
    /// `disabled_path` 是启用/禁用列表的落盘位置；调用方给 `prefs::path()`，
    /// 测试给临时目录里的文件。
    pub(crate) fn new(extensions_dir: &Path, env: Arc<dyn JsEnv>, disabled_path: &Path) -> Self {
        let store = ExtensionStore::new(env);
        let registry = ExtensionRegistry::new(store.clone());

        // 先读一次：首轮 refresh 之前 `commands()` 就该知道谁被禁用。
        let (disabled, _) = prefs::load(disabled_path);

        Self {
            registry,
            store,
            extensions_dir: extensions_dir.to_path_buf(),
            live: None,
            rebuilt_at: HashMap::new(),
            disabled_path: disabled_path.to_path_buf(),
            disabled,
            broken: Vec::new(),
        }
    }

    /// 重新扫描磁盘、注册新扩展、并让内容变过的扩展重新生效。
    ///
    /// 内容变更的重载已并入 `registry.register`（指纹变了返回
    /// [`RegisterOutcome::Reloaded`] 并卸掉旧运行时），这里只负责把结果翻译成
    /// Console 上的 errors / notices。
    pub(crate) fn refresh(&mut self) -> RefreshReport {
        let mut report = RefreshReport::default();

        // 每轮重读：用户手改 `extensions-disabled.json` 也该生效。
        let (disabled, warning) = prefs::load(&self.disabled_path);
        if let Some(warning) = warning {
            report.notices.push(warning);
        }
        self.disabled = disabled;

        let (dirs, scan_errors) = manifest::scan(&self.extensions_dir);
        report.errors.extend(scan_errors);
        let live: HashSet<PathBuf> = dirs.iter().cloned().collect();
        let mut broken = Vec::with_capacity(dirs.len());

        for dir in dirs {
            match self.registry.register(&dir) {
                Ok(outcome) => {
                    let Some(id) = self
                        .registry
                        .all()
                        .find(|(_, r)| r.package.root == dir)
                        .map(|(id, _)| id.to_string())
                    else {
                        continue;
                    };
                    // 被禁用的扩展：登记只为让清单校验与指纹基线走同一条路，
                    // 随后立即注销（连带卸运行时）——禁用的扩展不该留在表里。
                    if self.disabled.contains(&id) {
                        self.registry.unregister(&id);
                        self.rebuilt_at.remove(&id);
                        continue;
                    }
                    if outcome == RegisterOutcome::Reloaded {
                        let version = self
                            .registry
                            .get(&id)
                            .map(|r| r.package.version().to_string())
                            .unwrap_or_default();
                        report.notices.push(format!(
                            "扩展 {id} v{version} 的内容已变更，已重新加载（版本号未变，发布前记得升版本）"
                        ));
                    }
                }
                Err(RegisterError::Manifest { message, .. }) => {
                    report
                        .errors
                        .push(format!("扩展加载失败 {}：{message}", dir.display()));
                    broken.push((dir.clone(), message));
                }
                Err(error) => {
                    report
                        .errors
                        .push(format!("扩展加载失败 {}：{error}", dir.display()));
                    broken.push((dir.clone(), error.to_string()));
                }
            }
        }

        self.broken = broken;
        self.live = Some(live);
        // 磁盘上已经消失的扩展不必再留冷却记录；它若回来，本来也该重新建基准。
        if let Some(live) = &self.live {
            self.rebuilt_at.retain(|id, _| {
                self.registry
                    .get(id)
                    .is_some_and(|r| live.contains(&r.package.root))
            });
        }
        self.drop_stale();
        report
    }

    /// 扩展的清单与状态快照，供扩展管理窗口渲染。
    ///
    /// **纯内存、不扫盘**：数据来自上一次 `refresh` / `reload` 的结果，所以窗口
    /// 可以每帧调。想反映磁盘现状就先 `refresh`（窗口打开时一次、每次操作后一次）。
    pub(crate) fn inventory(&self) -> Vec<ExtensionEntry> {
        let mut entries = Vec::new();
        for (id, r) in self.registry.all() {
            let package = &r.package;
            entries.push(ExtensionEntry {
                id: id.to_string(),
                version: package.version().to_string(),
                root: package.root.clone(),
                name: package.display_name().to_string(),
                description: package.manifest.description.clone(),
                icon: package.manifest.icon_path(),
                command_count: package.manifest.commands.len(),
                ui_command_count: package
                    .manifest
                    .commands
                    .iter()
                    .filter(|command| command.ui.is_some())
                    .count(),
                permissions: package.manifest.permissions.clone(),
                enabled: !self.disabled.contains(id),
                registered: true,
                runtime: self.store.state(id),
                last_error: self.store.last_error(id).map(|e| e.to_string()),
                manifest_error: None,
            });
        }

        // 磁盘上有、注册表里没有：清单写坏 / 同 id 冲突 / 被禁用。
        if let Some(live) = &self.live {
            for dir in live {
                if self.registry.all().any(|(_, r)| r.package.root == *dir) {
                    continue;
                }
                // 能读出清单就展示 id（被禁用的扩展就是这种），读不出只能报目录名。
                let package = bolt_extensions::manifest::read(dir).ok();
                let manifest_error = match &package {
                    Some(_) => None,
                    None => self
                        .broken
                        .iter()
                        .find(|(broken_dir, _)| broken_dir == dir)
                        .map(|(_, message)| message.clone()),
                };
                // 目录名就是扩展 id；清单读不出来时只能退回目录名。
                let dir_name = dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                let (id, enabled) = match &package {
                    Some(package) => (
                        package.id().to_string(),
                        !self.disabled.contains(package.id()),
                    ),
                    None => (dir_name.clone(), true),
                };
                let name = package
                    .as_ref()
                    .map(|p| p.display_name().to_string())
                    .unwrap_or_else(|| dir_name.clone());
                entries.push(ExtensionEntry {
                    id,
                    version: String::new(),
                    root: dir.clone(),
                    name,
                    description: None,
                    icon: None,
                    command_count: 0,
                    ui_command_count: 0,
                    permissions: Vec::new(),
                    enabled,
                    registered: false,
                    runtime: None,
                    last_error: None,
                    manifest_error,
                });
            }
        }

        entries.sort_by(|left, right| left.id.cmp(&right.id));
        entries
    }

    /// 重新读取清单并重建运行时。UI 窗口由调用方先关掉（`ops::reload`）。
    pub(crate) fn reload(&mut self, id: &str) -> Result<RefreshReport, String> {
        if self.disabled.contains(id) {
            return Err(format!("扩展 {id} 已被禁用，先启用再重载"));
        }
        let root = self
            .registry
            .get(id)
            .map(|r| r.package.root.clone())
            .ok_or_else(|| format!("找不到扩展目录：{id}"))?;

        // 注销会连运行时一起卸掉；冷却也是"旧运行时"的账，重载后重新记。
        self.registry.unregister(id);
        self.rebuilt_at.remove(id);

        let mut report = RefreshReport::default();
        match self.registry.register(&root) {
            Ok(_) => {}
            Err(error) => report
                .errors
                .push(format!("扩展加载失败 {}：{error}", root.display())),
        }
        let refreshed = self.refresh();
        report.errors.extend(refreshed.errors);
        report.notices.extend(refreshed.notices);
        Ok(report)
    }

    /// 只注销、不碰磁盘：卸载的第一步。
    pub(crate) fn forget(&mut self, id: &str) -> Result<(), String> {
        let root = self.registry.get(id).map(|r| r.package.root.clone());
        // unregister 内部已经先卸运行时。
        self.registry.unregister(id);
        self.rebuilt_at.remove(id);
        if let (Some(live), Some(root)) = (&mut self.live, root) {
            live.remove(&root);
        }
        Ok(())
    }

    /// 启用 / 禁用某个扩展 id，立即生效并落盘。
    ///
    /// 先写盘再改内存：`refresh` 每轮都会重读文件，内存里"只改了一半"的状态
    /// 下一轮就会被磁盘覆盖，反而更难排查。写盘失败 = 操作整体失败（不改内存）。
    pub(crate) fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<RefreshReport, String> {
        let mut next = self.disabled.clone();
        if enabled {
            next.remove(id);
        } else {
            next.insert(id.to_string());
        }
        prefs::save(&self.disabled_path, &next)?;
        self.disabled = next;
        Ok(self.refresh())
    }

    /// 磁盘上已经消失的扩展：连注册一起摘掉，Launcher 里自然不再出现。
    fn drop_stale(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let stale: Vec<String> = self
            .registry
            .all()
            .filter(|(_, r)| !live.contains(&r.package.root))
            .map(|(id, _)| id.to_string())
            .collect();
        for id in stale {
            self.registry.unregister(&id);
        }
    }

    /// 当前所有扩展贡献的命令，按 id 排序（注册表本身有序）。
    ///
    /// 两层过滤：被禁用的扩展不出现在任何地方，磁盘上已不存在的扩展不展示。
    pub(crate) fn commands(&self) -> Vec<CommandTarget> {
        let mut targets = Vec::new();
        for (id, r) in self.registry.all() {
            // 禁用的扩展不出现在任何地方。
            if self.disabled.contains(id) {
                continue;
            }
            if self
                .live
                .as_ref()
                .is_some_and(|live| !live.contains(&r.package.root))
            {
                continue;
            }
            let package = &r.package;
            for command in &package.manifest.commands {
                // UI 标题在清单里可省；省了就用命令标题，免得下游到处判断空串。
                let ui = command.ui.clone().map(|mut ui| {
                    if ui.title.is_empty() {
                        ui.title = command.title.clone();
                    }
                    ui
                });
                targets.push(CommandTarget {
                    extension_id: id.to_string(),
                    extension_version: package.version().to_string(),
                    extension_name: package.display_name().to_string(),
                    root: package.root.clone(),
                    command_id: command.id.clone(),
                    title: command.title.clone(),
                    description: command.description.clone(),
                    // 命令图标覆盖扩展图标，都没写就留给渲染端用默认。
                    icon: command.icon_path().or_else(|| package.manifest.icon_path()),
                    ui,
                });
            }
        }
        targets
    }

    /// 懒加载：第一次执行命令时才安排加载（立即返回，引擎在扩展线程里建）。
    ///
    /// 运行时已死（加载失败 / 线程退出）时先卸掉再重新安排——否则表里的旧工人
    /// 线程会一直占着位置，永远不重建。
    pub(crate) fn ensure_loaded(&mut self, target: &CommandTarget) -> Result<(), String> {
        let id = target.extension_id.as_str();
        // 兜底：残留 item、RunService 直接投递都不该把被禁用的扩展拉起来。
        if self.disabled.contains(id) {
            return Err(format!("扩展 {id} 已被禁用"));
        }

        if self.registry.get(id).is_some() {
            match self.store.state(id) {
                // 已就绪或正在加载：交给 invoke 的 wait_ready，不会重复起线程。
                Some(RuntimeState::Ready) | Some(RuntimeState::Loading) => return Ok(()),
                Some(RuntimeState::Dead) => {
                    if let Some(at) = self.rebuilt_at.get(id)
                        && at.elapsed() < REBUILD_COOLDOWN
                    {
                        let reason = self
                            .store
                            .last_error(id)
                            .map(|error| error.to_string())
                            .unwrap_or_default();
                        return Err(format!(
                            "扩展运行时已退出（{reason}），{:.1}s 后可重新执行重试",
                            (REBUILD_COOLDOWN - at.elapsed()).as_secs_f32()
                        ));
                    }
                    self.rebuilt_at.insert(id.to_string(), Instant::now());
                    // 必须先卸载：ensure_loaded 对已有条目是 no-op。
                    self.store.unload(id);
                }
                // 表里没有运行时（曾被直接 unload）：同样走重建。
                None => {}
            }
        }

        self.registry.load(id).map_err(|e| e.to_string())
    }

    /// 执行入口的 `Arc` 副本（RunService 需要它才能在工作线程里调命令）。
    pub(crate) fn store(&self) -> Arc<ExtensionStore> {
        self.store.clone()
    }

    /// 测试辅助：扩展当前是否已注册。
    #[cfg(test)]
    fn is_registered(&self, id: &str) -> bool {
        self.registry.get(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::HostRegistry;
    use crate::modules::extensions::js_env::BoltJsEnv;
    use crate::modules::extensions::manifest;
    use crate::test_support::TempDir;
    use bolt_console::ConsoleChannel;
    use bolt_extensions::{CancelToken, InvokeRequest, JsError, MANIFEST_FILE};
    fn host(tmp: &TempDir) -> ExtensionsHost {
        let mut registry = HostRegistry::new();
        crate::host::modules::register_all(&mut registry);
        let console = ConsoleChannel::new();
        let env = Arc::new(BoltJsEnv {
            registry: Arc::new(registry),
            console,
        });
        ExtensionsHost::new(&tmp.0, env, &tmp.0.join("extensions-disabled.json"))
    }

    fn write_extension(tmp: &TempDir, dir: &str, manifest: &str, entry: &str) {
        let root = tmp.0.join(dir);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(MANIFEST_FILE), manifest).unwrap();
        std::fs::write(root.join("index.js"), entry).unwrap();
    }

    #[test]
    fn demo_extension_contributes_script_and_ui_commands() {
        let tmp = TempDir::new("host-demo");
        manifest::ensure_demo_extension(&tmp.0).expect("write demo extension");

        let mut host = host(&tmp);
        let report = host.refresh();
        assert!(report.errors.is_empty(), "{:?}", report.errors);

        let commands = host.commands();
        assert_eq!(commands.len(), 6, "{commands:?}");
        let hello = commands
            .iter()
            .find(|command| command.command_id == "hello")
            .expect("hello command");
        assert!(!hello.is_ui());
        assert_eq!(hello.item_id(), "ext.demo.hello");
        let panel = commands
            .iter()
            .find(|command| command.command_id == "panel")
            .expect("panel command");
        assert_eq!(
            panel.ui.as_ref().map(|ui| ui.title.as_str()),
            Some("演示面板")
        );
        // 标题栏动作：内建与脚本命令混排，带自定义图标与提示。
        let actions = &panel.ui.as_ref().expect("ui spec").actions;
        assert_eq!(actions.len(), 5, "{actions:?}");

        host.ensure_loaded(hello).expect("demo should load");
        let output = host
            .store()
            .invoke("demo", InvokeRequest::new("hello", CancelToken::new()))
            .expect("hello should run");
        assert!(
            output.unwrap_or_default().contains("demo v1.0.0"),
            "unexpected output"
        );
    }

    #[test]
    fn extensions_can_import_host_modules() {
        let tmp = TempDir::new("host-modules");
        write_extension(
            &tmp,
            "probe",
            r#"{ "id": "probe", "version": "1.0.0", "permissions": ["env"], "commands": [{ "id": "home", "title": "Home" }] }"#,
            "import { env } from 'bolt';\nexport function home() { return env('HOME') ?? 'unset'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.ensure_loaded(&target).expect("probe should load");

        let output = host
            .store()
            .invoke("probe", InvokeRequest::new("home", CancelToken::new()))
            .expect("home should run");
        assert!(output.is_some_and(|text| !text.is_empty()));
    }

    #[test]
    fn undeclared_permissions_throw_at_call_time() {
        // fail-closed：清单没声明 fs，import 不炸，调用 writeFile 抛出点名报错。
        let tmp = TempDir::new("host-perm-denied");
        write_extension(
            &tmp,
            "guard",
            r#"{ "id": "guard", "version": "1.0.0", "commands": [{ "id": "write", "title": "Write" }] }"#,
            "import { writeFile } from 'bolt';\nexport function write() { writeFile('/tmp/nope', 'x'); }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.ensure_loaded(&target).expect("guard should load");

        let error = host
            .store()
            .invoke("guard", InvokeRequest::new("write", CancelToken::new()))
            .expect_err("denied call must throw");
        match error {
            JsError::Script(message) => {
                assert!(message.contains("未声明权限"), "{message}");
                assert!(message.contains("fs"), "{message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn broken_extension_is_reported_without_blocking_the_others() {
        let tmp = TempDir::new("host-broken");
        write_extension(
            &tmp,
            "bad",
            r#"{ "id": "bad", "version": "1.0.0", "commands": "nope" }"#,
            "",
        );
        write_extension(
            &tmp,
            "good",
            r#"{ "id": "good", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'ok'; }",
        );

        let mut host = host(&tmp);
        let report = host.refresh();
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(report.errors[0].contains("bad"), "{:?}", report.errors);

        let commands = host.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].extension_id, "good");
    }

    #[test]
    fn refresh_is_idempotent() {
        let tmp = TempDir::new("host-refresh");
        write_extension(
            &tmp,
            "one",
            r#"{ "id": "one", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() {}",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        // 第二次扫描不该把已注册的扩展当成错误报出来。
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands().len(), 1);
    }

    fn wait_state(store: &ExtensionStore, id: &str, want: RuntimeState) {
        for _ in 0..500 {
            if store.state(id) == Some(want) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "state of {id} never became {want:?} (last: {:?})",
            store.state(id)
        );
    }

    #[test]
    fn refresh_unregisters_extension_deleted_from_disk() {
        let tmp = TempDir::new("host-stale");
        for id in ["one", "two"] {
            write_extension(
                &tmp,
                id,
                &format!(
                    r#"{{ "id": "{id}", "version": "1.0.0", "commands": [{{ "id": "go", "title": "Go" }}] }}"#
                ),
                "export function go() {}",
            );
        }

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands().len(), 2);

        std::fs::remove_dir_all(tmp.0.join("two")).unwrap();
        assert!(
            host.refresh().errors.is_empty(),
            "deleting a extension is not an error"
        );
        assert_eq!(host.commands().len(), 1);
        assert_eq!(host.commands()[0].extension_id, "one");

        // 摘掉之后要幂等：下一轮扫描不该再报 NotFound。
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands().len(), 1);
    }

    #[test]
    fn a_second_directory_with_the_same_manifest_id_is_reported() {
        let tmp = TempDir::new("host-dup-id");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() { return 'v1'; }",
        );
        // 另一个目录声明了同一个 id：目录名不参与身份，这是作者错误。
        write_extension(
            &tmp,
            "demo-copy",
            r#"{ "id": "demo", "version": "1.1.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() { return 'v2'; }",
        );

        let mut host = host(&tmp);
        let report = host.refresh();
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(report.errors[0].contains("已注册"), "{:?}", report.errors);

        let commands = host.commands();
        assert_eq!(commands.len(), 1, "same id must not show twice");
        assert_eq!(commands[0].extension_id, "demo");
        assert_eq!(commands[0].item_id(), "ext.demo.hello");
    }

    #[test]
    fn broken_extension_is_rebuilt_after_the_file_is_fixed() {
        let tmp = TempDir::new("host-rebuild");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() { return 'ok'; }",
        );
        // 入口写成语法错误：加载失败，运行时死掉。
        std::fs::write(
            tmp.0.join("demo").join("index.js"),
            "export function hello() {",
        )
        .unwrap();

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("command");
        host.ensure_loaded(&target)
            .expect("spawn must not fail on a broken entry");
        let store = host.store();
        assert!(
            store
                .invoke("demo", InvokeRequest::new("hello", CancelToken::new()))
                .is_err(),
            "broken entry must fail"
        );
        wait_state(&store, "demo", RuntimeState::Dead);

        // 作者修好文件后再点一次：健康检查要重建运行时，而不是永远短路。
        std::fs::write(
            tmp.0.join("demo").join("index.js"),
            "export function hello() { return 'fixed'; }",
        )
        .unwrap();
        host.ensure_loaded(&target)
            .expect("dead runtime must rebuild");

        let output = store
            .invoke("demo", InvokeRequest::new("hello", CancelToken::new()))
            .expect("rebuilt extension runs");
        assert_eq!(output.as_deref(), Some("fixed"));
    }

    #[test]
    fn rebuild_is_rate_limited() {
        let tmp = TempDir::new("host-rebuild-cooldown");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() {",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("command");
        host.ensure_loaded(&target)
            .expect("first load is scheduled");
        let store = host.store();
        wait_state(&store, "demo", RuntimeState::Dead);

        host.ensure_loaded(&target).expect("first rebuild");
        wait_state(&store, "demo", RuntimeState::Dead);

        let error = host
            .ensure_loaded(&target)
            .expect_err("second rebuild inside the cooldown must be refused");
        assert!(error.contains("后可重新执行"), "{error}");
    }

    fn invoke(store: &ExtensionStore, id: &str, command: &str) -> Option<String> {
        store
            .invoke(id, InvokeRequest::new(command, CancelToken::new()))
            .expect("command should run")
    }

    #[test]
    fn same_version_content_change_reloads_and_notifies() {
        let tmp = TempDir::new("host-content-change");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() { return 'v1'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().notices.is_empty());
        let target = host.commands().into_iter().next().expect("command");
        host.ensure_loaded(&target).expect("loads");
        let store = host.store();
        assert_eq!(invoke(&store, "demo", "hello").as_deref(), Some("v1"));

        // 改代码但不动版本号：注册表靠目录指纹发现内容变更。
        std::fs::write(
            tmp.0.join("demo/index.js"),
            "export function hello() { return 'v2'; }",
        )
        .unwrap();

        let report = host.refresh();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.notices.len(), 1, "{:?}", report.notices);
        assert!(
            report.notices[0].contains("内容已变更"),
            "{:?}",
            report.notices
        );

        // 运行时被连着卸掉了：下一次执行要按新代码重建。
        let target = host.commands().into_iter().next().expect("command");
        host.ensure_loaded(&target).expect("reload");
        assert_eq!(invoke(&store, "demo", "hello").as_deref(), Some("v2"));
    }

    #[test]
    fn manifest_edit_without_version_bump_updates_metadata() {
        let tmp = TempDir::new("host-manifest-change");
        let manifest = |title: &str| {
            format!(
                r#"{{ "id": "demo", "version": "1.0.0", "commands": [{{ "id": "hello", "title": "{title}" }}] }}"#
            )
        };
        write_extension(
            &tmp,
            "demo",
            &manifest("Hello"),
            "export function hello() {}",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands()[0].title, "Hello");

        std::fs::write(tmp.0.join("demo/extension.json"), manifest("Hi there")).unwrap();
        let report = host.refresh();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.notices.len(), 1, "{:?}", report.notices);
        // 元数据必须重新读：注册表里的旧 ExtensionMeta 不能继续用。
        assert_eq!(host.commands()[0].title, "Hi there");
    }

    #[test]
    fn refresh_keeps_the_runtime_when_nothing_changed() {
        // 计数存在模块状态里：运行时一旦被重建，它会从 1 重来。
        let tmp = TempDir::new("host-runtime-kept");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "bump", "title": "Bump" }] }"#,
            "let n = 0;\nexport function bump() { n += 1; return String(n); }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("command");
        host.ensure_loaded(&target).expect("loads");
        let store = host.store();
        assert_eq!(invoke(&store, "demo", "bump").as_deref(), Some("1"));
        assert_eq!(invoke(&store, "demo", "bump").as_deref(), Some("2"));

        // Launcher 每打开一次都会 refresh：内容没变就不该动运行时。
        let report = host.refresh();
        assert!(
            report.errors.is_empty() && report.notices.is_empty(),
            "{report:?}"
        );
        assert_eq!(invoke(&store, "demo", "bump").as_deref(), Some("3"));
    }

    #[test]
    fn disabled_extension_is_never_registered() {
        let tmp = TempDir::new("host-disabled");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        let report = host.set_enabled("demo", false).expect("disable");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(host.commands().is_empty());
        assert!(
            !host.is_registered("demo"),
            "disabled extension must not be registered"
        );
    }

    #[test]
    fn disabling_a_loaded_extension_unloads_its_runtime() {
        let tmp = TempDir::new("host-disable-loaded");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.ensure_loaded(&target).expect("loads");
        let store = host.store();
        wait_state(&store, "demo", RuntimeState::Ready);
        assert!(store.is_loaded("demo"));

        host.set_enabled("demo", false).expect("disable");
        assert!(
            store.state("demo").is_none(),
            "runtime must be unloaded together with the registration"
        );
        assert!(host.commands().is_empty());
    }

    #[test]
    fn re_enabling_restores_commands_without_a_reload_notice() {
        let tmp = TempDir::new("host-reenable");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        host.set_enabled("demo", false).expect("disable");
        assert!(host.commands().is_empty());

        // 重新启用不该触发"内容已变更"：禁用期间指纹基线一直跟着更新。
        let report = host.set_enabled("demo", true).expect("enable");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.notices.is_empty(), "{:?}", report.notices);
        assert_eq!(host.commands().len(), 1);
    }

    #[test]
    fn inventory_lists_broken_manifests() {
        let tmp = TempDir::new("host-inventory-broken");
        write_extension(
            &tmp,
            "bad",
            r#"{ "id": "bad", "version": "1.0.0", "commands": "nope" }"#,
            "",
        );

        let mut host = host(&tmp);
        assert!(!host.refresh().errors.is_empty());
        let entries = host.inventory();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert!(!entries[0].registered);
        assert!(entries[0].enabled);
        assert_eq!(entries[0].id, "bad");
        assert_eq!(entries[0].name, "bad");
        assert!(
            entries[0]
                .manifest_error
                .as_deref()
                .is_some_and(|error| error.contains("解析 extension.json 失败")),
            "{:?}",
            entries[0].manifest_error
        );
    }

    #[test]
    fn inventory_shows_registered_extensions() {
        let tmp = TempDir::new("host-inventory");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.2.3", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'v1'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let entries = host.inventory();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "demo");
        assert_eq!(entries[0].version, "1.2.3");
        assert!(entries[0].registered);
        assert!(entries[0].enabled);
        assert_eq!(entries[0].command_count, 1);
        assert_eq!(entries[0].ui_command_count, 0);
        assert!(entries[0].manifest_error.is_none());
    }

    #[test]
    fn reload_after_manifest_edit_updates_metadata() {
        let tmp = TempDir::new("host-reload");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Old" }] }"#,
            "export function go() { return 'v1'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands()[0].title, "Old");

        std::fs::write(
            tmp.0.join("demo/extension.json"),
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "New" }] }"#,
        )
        .unwrap();

        let report = host.reload("demo").expect("reload");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(host.commands()[0].title, "New");
    }

    #[test]
    fn reload_is_refused_while_disabled() {
        let tmp = TempDir::new("host-reload-disabled");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        host.set_enabled("demo", false).expect("disable");
        let error = host.reload("demo").expect_err("must refuse");
        assert!(error.contains("已被禁用"), "{error}");
    }

    #[test]
    fn forget_unregisters_without_touching_the_disk() {
        let tmp = TempDir::new("host-forget");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        host.forget("demo").expect("forget");

        assert!(host.commands().is_empty());
        assert!(!host.is_registered("demo"));
        // 目录还在，下一轮 refresh 又把它捡回来（卸载的调用方负责真的删目录）。
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands().len(), 1);
    }

    #[test]
    fn ensure_loaded_refuses_a_disabled_extension() {
        let tmp = TempDir::new("host-disabled-load");
        write_extension(
            &tmp,
            "demo",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.set_enabled("demo", false).expect("disable");

        // 模拟"残留 item"：拿被禁用扩展的历史 target 直接要求加载。
        let error = host.ensure_loaded(&target).expect_err("must refuse");
        assert!(error.contains("已被禁用"), "{error}");
    }
}
