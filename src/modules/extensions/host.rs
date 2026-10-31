//! plux 扩展宿主：`Loader`（生命周期/元数据）+ `ExtensionStore`（执行）。
//!
//! **`Loader` 必须先 `Box` 再注册扩展，之后永不移动。** plux 的 `Plugin` 里存的是
//! 指向 `loader.managers` Vec 槽位的裸指针（`Ptr`），在栈上建好 Loader 再 move 进
//! 结构体会让它立刻悬空。同理，manager 只在扩展注册之前注册——之后不再往
//! `managers` 里 push，Vec 不会 realloc，指针一直有效。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use plux_js_manager::{
    ExtensionMeta, ExtensionStore, JsEnv, JsManager, RuntimeState, UiSpec, dir_fingerprint,
};
use plux_rs::function::FunctionOutput;
use plux_rs::utils::{RegisterPluginError, UnloadPluginError, UnregisterPluginError};
use plux_rs::{Bundle, Info, Loader};

use super::manifest;
use super::prefs;

pub(crate) type ExtensionsLoader = Loader<'static, FunctionOutput, ExtensionMeta>;

/// 重建运行时的冷却期：坏扩展被连点时不该每次都起一条新线程再失败一次。
const REBUILD_COOLDOWN: Duration = Duration::from_secs(3);

/// Launcher 里的一条命令（已脱离 `Loader` 的借用，可 move 进 `on_confirm`）。
#[derive(Clone, Debug)]
pub(crate) struct CommandTarget {
    pub(crate) bundle: Bundle,
    pub(crate) extension_id: String,
    pub(crate) extension_name: String,
    /// 扩展目录：UI 命令用它做 gpui-shell 的应用根。
    pub(crate) root: PathBuf,
    pub(crate) command_id: String,
    pub(crate) title: String,
    pub(crate) description: Option<String>,
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
    /// 见模块头部：必须钉在 Box 里。
    loader: Box<ExtensionsLoader>,
    store: Arc<ExtensionStore>,
    extensions_dir: PathBuf,
    /// 注册 manager 失败的原因。此时宿主仍然可构造，但所有命令都不可用，
    /// 由 `refresh` 把这条错误报给 Console。
    init_error: Option<String>,
    /// 本次扫描时磁盘上真实存在的扩展。
    ///
    /// 由目录名解析得出，**与注册成败无关**：清单写坏的扩展也算存在，否则作者
    /// 保存一次坏清单就会把正在用的扩展判成"已删除"，连带杀掉它的运行时。
    /// `None` = 还没成功扫描过，此时不做任何过滤。
    live: Option<HashSet<Bundle>>,
    /// 每个扩展上次重建运行时的时刻，用来给重建加冷却。
    rebuilt_at: HashMap<Bundle, Instant>,
    /// 上次扫描时各扩展目录的内容指纹。
    ///
    /// plux 的扩展身份只有 `(id, version)`：同 id 同版本再注册只会回
    /// `AlreadyExistsIDAndVersion`，元数据与运行时都留在旧内容上。开发期改完代码
    /// 忘了升版本号是常事，指纹就是"磁盘变了"的唯一证据。
    fingerprints: HashMap<Bundle, u64>,
    /// 禁用列表文件的位置（`extensions-disabled.json`）。测试传临时路径，
    /// 免得读到开发机上真实的偏好。
    disabled_path: PathBuf,
    /// 被用户禁用的扩展 id。`refresh` 每轮从磁盘重读，这样手改文件也能生效。
    disabled: BTreeSet<String>,
    /// 每个扩展目录的绝对路径（按 bundle 键）。清单写坏的目录不在
    /// `loader` 里，[`ExtensionsHost::inventory`] 只能靠它把行补全。
    roots: HashMap<Bundle, PathBuf>,
}

/// 扩展管理窗口的一行：一个扩展目录（含清单写坏的）。
///
/// 全是快照值：拿到之后不再借用 `Loader`，可以放心 move 进 UI。
#[derive(Clone, Debug)]
pub(crate) struct ExtensionEntry {
    pub(crate) bundle: Bundle,
    pub(crate) root: PathBuf,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) command_count: usize,
    pub(crate) ui_command_count: usize,
    pub(crate) permissions: Vec<String>,
    pub(crate) enabled: bool,
    /// plux 注册表里有它。清单写坏 / 被禁用时为 `false`。
    pub(crate) registered: bool,
    /// 同 id 有更高版本：注册保留（给依赖解析），但命令与运行时让位。
    pub(crate) shadowed: bool,
    /// 声明依赖它的已注册扩展。
    pub(crate) dependents: Vec<Bundle>,
    pub(crate) runtime: Option<RuntimeState>,
    pub(crate) last_error: Option<String>,
    /// 清单读取 / 校验失败的原因（此时 `registered == false`）。
    pub(crate) manifest_error: Option<String>,
}

impl ExtensionsHost {
    /// `disabled_path` 是启用/禁用列表的落盘位置；调用方给 `prefs::path()`，
    /// 测试给临时目录里的文件。
    pub(crate) fn new(extensions_dir: &Path, env: Arc<dyn JsEnv>, disabled_path: &Path) -> Self {
        let manager = JsManager::new(env);
        let store = manager.store().clone();

        let mut loader: Box<ExtensionsLoader> = Box::new(Loader::new());
        let init_error = loader
            .context(|mut ctx| ctx.register_manager(manager))
            .err()
            .map(|e| format!("注册 JS 扩展管理器失败：{e}"));

        // 先读一次：首轮 refresh 之前 `commands()` 就该知道谁被禁用。
        let (disabled, _) = prefs::load(disabled_path);

        Self {
            loader,
            store,
            extensions_dir: extensions_dir.to_path_buf(),
            init_error,
            live: None,
            rebuilt_at: HashMap::new(),
            fingerprints: HashMap::new(),
            disabled_path: disabled_path.to_path_buf(),
            disabled,
            roots: HashMap::new(),
        }
    }

    /// 重新扫描磁盘、注册新扩展、并让内容变过的扩展重新生效。
    pub(crate) fn refresh(&mut self) -> RefreshReport {
        let mut report = RefreshReport::default();
        if let Some(error) = &self.init_error {
            report.errors.push(error.clone());
            return report;
        }

        // 每轮重读：用户手改 `extensions-disabled.json` 也该生效。
        let (disabled, warning) = prefs::load(&self.disabled_path);
        if let Some(warning) = warning {
            report.notices.push(warning);
        }
        self.disabled = disabled;

        let (dirs, scan_errors) = manifest::scan(&self.extensions_dir);
        report.errors.extend(scan_errors);
        // 先按目录名算"磁盘上有谁"：注册失败（清单写坏）也算存在。
        let live: HashSet<Bundle> = dirs
            .iter()
            .filter_map(|dir| plux_js_manager::manifest::bundle_from_dir(dir).ok())
            .collect();
        let mut roots: HashMap<Bundle, PathBuf> = HashMap::with_capacity(dirs.len());

        for dir in dirs {
            let Some(path) = dir.to_str() else {
                report
                    .errors
                    .push(format!("扩展路径不是合法 UTF-8：{}", dir.display()));
                continue;
            };
            // 目录名不合规时 register_plugin 会报一条统一的错，这里不重复判断。
            let bundle = plux_js_manager::manifest::bundle_from_dir(&dir).ok();
            let fingerprint = dir_fingerprint(&dir);
            if let Some(bundle) = &bundle {
                roots.insert(bundle.clone(), dir.clone());
            }

            // 被禁用的扩展：不注册、已注册的注销（连带卸运行时）。
            // 指纹仍要记基线，否则重新启用时会被误判成"内容变更"。
            if let Some(bundle) = &bundle
                && self.disabled.contains(&bundle.id)
            {
                self.disable_bundle(bundle, &mut report);
                if let Some(fingerprint) = fingerprint {
                    self.fingerprints.insert(bundle.clone(), fingerprint);
                }
                continue;
            }

            let update_baseline = match &bundle {
                Some(bundle) => self.reload_if_changed(bundle, fingerprint, &mut report),
                None => true,
            };

            match self.loader.register_plugin(path) {
                // 内容没变时的重复注册是正常路径：Launcher 每打开一次都会走到这里。
                Ok(_) | Err(RegisterPluginError::AlreadyExistsIDAndVersion(..)) => {}
                Err(error) => report.errors.push(format!(
                    "扩展加载失败 {}：{}",
                    dir.display(),
                    describe(&error)
                )),
            }

            if let (Some(bundle), Some(fingerprint), true) = (bundle, fingerprint, update_baseline)
            {
                self.fingerprints.insert(bundle, fingerprint);
            }
        }

        self.live = Some(live);
        // 磁盘上已经消失的扩展不必再留指纹/冷却记录；它若回来，本来也该重新建基准。
        if let Some(live) = &self.live {
            self.rebuilt_at.retain(|bundle, _| live.contains(bundle));
            self.fingerprints.retain(|bundle, _| live.contains(bundle));
        }
        self.roots = roots;
        self.drop_stale(&mut report.errors);
        self.drop_shadowed();
        report
    }

    /// 把一个被禁用的扩展从注册表摘掉（连带卸运行时）。
    ///
    /// 被已加载的扩展依赖时 plux 拒绝卸载：不强拆，只把事实报出来。此时
    /// `commands()` 的禁用过滤仍然让它从 Launcher 里消失，重启后不再注册。
    fn disable_bundle(&mut self, bundle: &Bundle, report: &mut RefreshReport) {
        match self.loader.unregister_plugin_by_bundle(bundle) {
            // 本来就没注册（首轮扫描、或上次清单写坏）：无事可做。
            Ok(()) | Err(UnregisterPluginError::NotFound) => {}
            Err(UnregisterPluginError::UnloadError(UnloadPluginError::CurrentlyUsesDepend {
                plugin: extension,
                ..
            })) => report.notices.push(format!(
                "扩展 {} 已禁用，但被 {} v{} 依赖，本次运行仍会加载；重启后不再加载",
                bundle.id, extension.id, extension.version
            )),
            Err(error) => report
                .errors
                .push(format!("扩展 {} 禁用失败：{error}", bundle.id)),
        }
    }

    /// 扩展的清单与状态快照，供扩展管理窗口渲染。
    ///
    /// **纯内存、不扫盘**：数据来自上一次 `refresh` / `reload` 的结果，所以窗口
    /// 可以每帧调。想反映磁盘现状就先 `refresh`（窗口打开时一次、每次操作后一次）。
    pub(crate) fn inventory(&self) -> Vec<ExtensionEntry> {
        // 最高版本口径与 `commands()` / `drop_shadowed` 一致。
        let mut highest: HashMap<&str, &Bundle> = HashMap::new();
        for extension in self.loader.get_plugins() {
            let bundle = &extension.info().bundle;
            match highest.get(bundle.id.as_str()) {
                Some(kept) if kept.version >= bundle.version => {}
                _ => {
                    highest.insert(bundle.id.as_str(), bundle);
                }
            }
        }

        let mut entries = Vec::new();
        let mut registered: HashSet<Bundle> = HashSet::new();
        for extension in self.loader.get_plugins() {
            let info = extension.info();
            let bundle = &info.bundle;
            let meta = &info.info;
            registered.insert(bundle.clone());
            entries.push(ExtensionEntry {
                shadowed: highest
                    .get(bundle.id.as_str())
                    .is_some_and(|kept| kept.version > bundle.version),
                dependents: self.dependents_of(bundle),
                runtime: self.store.state(bundle),
                last_error: self.store.last_error(bundle).map(|e| e.to_string()),
                enabled: !self.disabled.contains(&bundle.id),
                registered: true,
                bundle: bundle.clone(),
                root: meta.root.clone(),
                name: meta.display_name().to_string(),
                description: meta.manifest.description.clone(),
                command_count: meta.commands().len(),
                ui_command_count: meta
                    .commands()
                    .iter()
                    .filter(|command| command.ui.is_some())
                    .count(),
                permissions: meta.manifest.permissions.clone(),
                manifest_error: None,
            });
        }

        // 磁盘上有、注册表里没有：清单写坏了（或者被 plux 拒了）。
        if let Some(live) = &self.live {
            for bundle in live {
                if registered.contains(bundle) {
                    continue;
                }
                let root = self.roots.get(bundle).cloned().unwrap_or_else(|| {
                    self.extensions_dir.join(format!(
                        "{}-v{}.{}",
                        bundle.id, bundle.version, bundle.format
                    ))
                });
                let manifest_error = plux_js_manager::manifest::read(&root)
                    .err()
                    .unwrap_or_else(|| "未注册（原因见 Console）".to_string());
                entries.push(ExtensionEntry {
                    shadowed: highest
                        .get(bundle.id.as_str())
                        .is_some_and(|kept| kept.version > bundle.version),
                    dependents: Vec::new(),
                    runtime: None,
                    last_error: None,
                    enabled: !self.disabled.contains(&bundle.id),
                    registered: false,
                    bundle: bundle.clone(),
                    root,
                    name: bundle.id.clone(),
                    description: None,
                    command_count: 0,
                    ui_command_count: 0,
                    permissions: Vec::new(),
                    manifest_error: Some(manifest_error),
                });
            }
        }

        entries.sort_by(|left, right| {
            left.bundle
                .id
                .cmp(&right.bundle.id)
                .then_with(|| right.bundle.version.cmp(&left.bundle.version))
        });
        entries
    }

    /// 谁声明依赖 `bundle`（只看已注册的扩展）。
    ///
    /// plux 只在目标**已加载**时才拦 `unregister`；不加载时它会静默摘下，依赖方
    /// 下次加载失败才发现。所以确认框要能提前说出这句话。
    fn dependents_of(&self, bundle: &Bundle) -> Vec<Bundle> {
        let mut dependents = Vec::new();
        for extension in self.loader.get_plugins() {
            let info = extension.info();
            let meta = &info.info;
            let depends = meta.depends().iter().chain(meta.optional_depends().iter());
            let hit = depends
                .into_iter()
                .any(|depend| *depend == *bundle && depend.version.matches(&bundle.version));
            if hit {
                dependents.push(info.bundle.clone());
            }
        }
        dependents
    }

    /// 重新读取清单并重建运行时。UI 窗口由调用方先关掉（`ops::reload`）。
    pub(crate) fn reload(&mut self, bundle: &Bundle) -> Result<RefreshReport, String> {
        if self.disabled.contains(&bundle.id) {
            return Err(format!("扩展 {} 已被禁用，先启用再重载", bundle.id));
        }
        let root = self
            .root_of(bundle)
            .ok_or_else(|| format!("找不到扩展目录：{} v{}", bundle.id, bundle.version))?;

        match self.loader.unregister_plugin_by_bundle(bundle) {
            Ok(()) | Err(UnregisterPluginError::NotFound) => {}
            Err(UnregisterPluginError::UnloadError(UnloadPluginError::CurrentlyUsesDepend {
                plugin: extension,
                ..
            })) => {
                return Err(format!(
                    "扩展 {} 被 {} v{} 依赖，无法重载",
                    bundle.id, extension.id, extension.version
                ));
            }
            Err(error) => return Err(format!("注销扩展 {} 失败：{error}", bundle.id)),
        }

        // 冷却与指纹都是"旧运行时"的账，重载后要重新记。
        self.rebuilt_at.remove(bundle);
        self.fingerprints.remove(bundle);

        let Some(path) = root.to_str() else {
            return Err(format!("扩展路径不是合法 UTF-8：{}", root.display()));
        };
        let mut report = RefreshReport::default();
        match self.loader.register_plugin(path) {
            Ok(_) | Err(RegisterPluginError::AlreadyExistsIDAndVersion(..)) => {}
            Err(error) => report.errors.push(format!(
                "扩展加载失败 {}：{}",
                root.display(),
                describe(&error)
            )),
        }
        let refreshed = self.refresh();
        report.errors.extend(refreshed.errors);
        report.notices.extend(refreshed.notices);
        Ok(report)
    }

    /// 只注销、不碰磁盘：卸载的第一步。被依赖时返回 `Err`，调用方不得继续删目录。
    pub(crate) fn forget(&mut self, bundle: &Bundle) -> Result<(), String> {
        match self.loader.unregister_plugin_by_bundle(bundle) {
            Ok(()) | Err(UnregisterPluginError::NotFound) => {}
            Err(UnregisterPluginError::UnloadError(UnloadPluginError::CurrentlyUsesDepend {
                plugin: extension,
                ..
            })) => {
                return Err(format!(
                    "扩展 {} 被 {} v{} 依赖，先卸载依赖方",
                    bundle.id, extension.id, extension.version
                ));
            }
            Err(error) => return Err(format!("注销扩展 {} 失败：{error}", bundle.id)),
        }
        // plux 的 unregister 会先 unload；这里兜一次底，防止某条路径漏了。
        self.store.unload(bundle);
        self.rebuilt_at.remove(bundle);
        self.fingerprints.remove(bundle);
        self.roots.remove(bundle);
        if let Some(live) = &mut self.live {
            live.remove(bundle);
        }
        Ok(())
    }

    fn root_of(&self, bundle: &Bundle) -> Option<PathBuf> {
        self.roots.get(bundle).cloned().or_else(|| {
            self.loader
                .get_plugin_by_bundle(bundle)
                .map(|extension| extension.info().info.root.clone())
        })
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

    /// 内容变了、版本号没变：先注销旧条目，让调用方紧接着重新注册。
    ///
    /// 注销会连运行时一起卸掉（`unregister_plugin` 先 unload 再 remove），下次执行
    /// 命令时用新代码重建引擎。返回 `false` 表示重载没成功——此时**不要**记录新
    /// 指纹，下一轮扫描还要再试一次。
    fn reload_if_changed(
        &mut self,
        bundle: &Bundle,
        fingerprint: Option<u64>,
        report: &mut RefreshReport,
    ) -> bool {
        let (Some(current), Some(known)) = (fingerprint, self.fingerprints.get(bundle).copied())
        else {
            // 第一次见到它（或目录读不了）：没有基准可比，谈不上"变了"。
            return true;
        };
        if current == known {
            return true;
        }

        match self.loader.unregister_plugin_by_bundle(bundle) {
            // 上次注册就失败了（清单当时写坏）：没有旧条目可摘，直接重新注册。
            Err(UnregisterPluginError::NotFound) => {}
            Ok(()) => report.notices.push(format!(
                "扩展 {} v{} 的内容已变更，已重新加载（版本号未变，发布前记得升版本）",
                bundle.id, bundle.version
            )),
            // 还被别的已加载扩展依赖着：不能强拆，只能报出来让作者升版本号。
            Err(error) => {
                report.errors.push(format!(
                    "扩展 {} v{} 的内容已变更，但无法重新加载：{error}",
                    bundle.id, bundle.version
                ));
                return false;
            }
        }
        true
    }

    /// 磁盘上已经消失的扩展：连注册一起摘掉，Launcher 里自然不再出现。
    fn drop_stale(&mut self, errors: &mut Vec<String>) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let stale: Vec<Bundle> = self
            .loader
            .get_plugins()
            .iter()
            .map(|extension| extension.info().bundle.clone())
            .filter(|bundle| !live.contains(bundle))
            .collect();

        for bundle in stale {
            match self.loader.unregister_plugin_by_bundle(&bundle) {
                Ok(()) => {}
                // 已经不在表里：目标状态已达成。
                Err(UnregisterPluginError::NotFound) => {}
                // 还被别的已加载扩展依赖：plux 拒绝卸载。不强行杀运行时（那会让
                // 依赖方静默失效），只是不再出现在 Launcher（`commands` 按 live 过滤），
                // 下次启动彻底消失。
                Err(UnregisterPluginError::UnloadError(
                    UnloadPluginError::CurrentlyUsesDepend { .. },
                )) => {}
                Err(error) => errors.push(format!(
                    "扩展卸载失败 {} v{}：{error}",
                    bundle.id, bundle.version
                )),
            }
        }
    }

    /// 同 id 多版本：只留最高版本的运行时。
    ///
    /// 两套引擎会让扩展顶层副作用跑两遍，而 item id（`ext.<id>.<command>`）不含
    /// 版本，Launcher 里必然重复。**注册保留**——plux 的依赖解析仍可能按版本区间
    /// 挑到低版本，注册表要留给它。
    fn drop_shadowed(&mut self) {
        let mut highest: HashMap<&str, &Bundle> = HashMap::new();
        for extension in self.loader.get_plugins() {
            let bundle = &extension.info().bundle;
            match highest.get(bundle.id.as_str()) {
                Some(kept) if kept.version >= bundle.version => {}
                _ => {
                    highest.insert(bundle.id.as_str(), bundle);
                }
            }
        }

        let shadowed: Vec<Bundle> = self
            .loader
            .get_plugins()
            .iter()
            .map(|extension| extension.info().bundle.clone())
            .filter(|bundle| {
                highest
                    .get(bundle.id.as_str())
                    .is_some_and(|kept| kept.version > bundle.version)
            })
            .collect();

        for bundle in shadowed {
            self.store.unload(&bundle);
        }
    }

    /// 当前所有扩展贡献的命令，按注册顺序（即扩展目录名排序）。
    ///
    /// 两层过滤：磁盘上已不存在的扩展不展示（plux 无 unregister 失败的兜底），
    /// 同 id 只展示最高版本（item id 不含版本，双份必然重复；口径与 plux 依赖
    /// 解析一致——它也挑最高匹配版本）。
    pub(crate) fn commands(&self) -> Vec<CommandTarget> {
        let mut highest: HashMap<&str, &Bundle> = HashMap::new();
        for extension in self.loader.get_plugins() {
            let bundle = &extension.info().bundle;
            match highest.get(bundle.id.as_str()) {
                Some(kept) if kept.version >= bundle.version => {}
                _ => {
                    highest.insert(bundle.id.as_str(), bundle);
                }
            }
        }

        let mut targets = Vec::new();
        for extension in self.loader.get_plugins() {
            let info = extension.info();
            let bundle = &info.bundle;
            // 禁用的扩展不出现在任何地方：即使因为「被依赖不能卸」还留在注册表里。
            if self.disabled.contains(&bundle.id) {
                continue;
            }
            if self
                .live
                .as_ref()
                .is_some_and(|live| !live.contains(bundle))
            {
                continue;
            }
            if highest
                .get(bundle.id.as_str())
                .is_some_and(|kept| kept.version != bundle.version)
            {
                continue;
            }
            let meta = &info.info;
            for command in meta.commands() {
                // UI 标题在清单里可省；省了就用命令标题，免得下游到处判断空串。
                let ui = command.ui.clone().map(|mut ui| {
                    if ui.title.is_empty() {
                        ui.title = command.title.clone();
                    }
                    ui
                });
                targets.push(CommandTarget {
                    bundle: info.bundle.clone(),
                    extension_id: meta.id().to_string(),
                    extension_name: meta.display_name().to_string(),
                    root: meta.root.clone(),
                    command_id: command.id.clone(),
                    title: command.title.clone(),
                    description: command.description.clone(),
                    ui,
                });
            }
        }
        targets
    }

    /// 懒加载：第一次执行命令时才安排加载（立即返回，引擎在扩展线程里建）。
    ///
    /// 运行时已死（加载失败 / 线程退出）时先复位 plux 的 `is_load` 再重新安排——
    /// 否则 `load_plugin_by_bundle` 会因为 `is_load == true` 直接 Ok，什么都不重建。
    pub(crate) fn ensure_loaded(&mut self, target: &CommandTarget) -> Result<(), String> {
        let bundle = &target.bundle;
        // 兜底：残留 item、RunService 直接投递都不该把被禁用的扩展拉起来。
        if self.disabled.contains(&bundle.id) {
            return Err(format!("扩展 {} 已被禁用", bundle.id));
        }
        let registered = self
            .loader
            .get_plugin_by_bundle(bundle)
            .map(|extension| extension.is_load())
            .unwrap_or(false);

        if registered {
            match self.store.state(bundle) {
                // 已就绪或正在加载：交给 invoke 的 wait_ready，不会重复起线程。
                Some(RuntimeState::Ready) | Some(RuntimeState::Loading) => return Ok(()),
                Some(RuntimeState::Dead) => {
                    if let Some(at) = self.rebuilt_at.get(bundle)
                        && at.elapsed() < REBUILD_COOLDOWN
                    {
                        let reason = self
                            .store
                            .last_error(bundle)
                            .map(|error| error.to_string())
                            .unwrap_or_default();
                        return Err(format!(
                            "扩展运行时已退出（{reason}），{:.1}s 后可重新执行重试",
                            (REBUILD_COOLDOWN - at.elapsed()).as_secs_f32()
                        ));
                    }
                    self.rebuilt_at.insert(bundle.clone(), Instant::now());
                    // 必须先卸载：load_plugin_by_bundle 对已加载扩展是 no-op。
                    if let Err(error) = self.loader.unload_plugin_by_bundle(bundle) {
                        return Err(format!("扩展运行时已退出，且无法重建（{error}）"));
                    }
                }
                // plux 说已加载、表里却没有运行时（曾被直接 unload）：同样走重建。
                None => {}
            }
        }

        self.loader
            .load_plugin_by_bundle(bundle)
            .map_err(|e| e.to_string())
    }

    /// 执行入口的 `Arc` 副本（RunService 需要它才能在工作线程里调命令）。
    pub(crate) fn store(&self) -> Arc<ExtensionStore> {
        self.store.clone()
    }
}

fn describe(error: &RegisterPluginError) -> String {
    match error {
        // plux 的外层 Display 只有"manager 报错"，真正原因在里层。
        RegisterPluginError::RegisterPluginByManager(inner) => inner.to_string(),
        RegisterPluginError::BundleFromFailed(inner) => inner.to_string(),
        RegisterPluginError::NotFound => "扩展目录不存在".to_string(),
        RegisterPluginError::UnknownManagerFormat(format) => {
            format!("未知扩展格式 `{format}`（目录名必须以 `.js` 结尾）")
        }
        RegisterPluginError::AlreadyExistsIDAndVersion(id, version) => {
            format!("同 id 同版本已注册：{id} v{version}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console_channel::ConsoleChannel;
    use crate::host::HostRegistry;
    use crate::modules::extensions::js_env::BoltJsEnv;
    use crate::modules::extensions::manifest;
    use crate::test_support::TempDir;
    use plux_js_manager::{CancelToken, InvokeRequest, JsError, MANIFEST_FILE};

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

    /// 测试里的扩展身份：`(id, version)` 由 bundle 一处说清。
    fn bundle(id: &str, version: &str) -> Bundle {
        Bundle::from_filename(format!("{id}-v{version}.js").as_str()).expect("valid bundle")
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
        assert_eq!(commands.len(), 2, "{commands:?}");
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

        host.ensure_loaded(hello).expect("demo should load");
        let output = host
            .store()
            .invoke(
                &bundle("demo", "1.0.0"),
                InvokeRequest::new("hello", CancelToken::new()),
            )
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
            "probe-v1.0.0.js",
            r#"{ "id": "probe", "version": "1.0.0", "permissions": ["env"], "commands": [{ "id": "home", "title": "Home" }] }"#,
            "import { env } from 'bolt';\nexport function home() { return env('HOME') ?? 'unset'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.ensure_loaded(&target).expect("probe should load");

        let output = host
            .store()
            .invoke(
                &bundle("probe", "1.0.0"),
                InvokeRequest::new("home", CancelToken::new()),
            )
            .expect("home should run");
        assert!(output.is_some_and(|text| !text.is_empty()));
    }

    #[test]
    fn undeclared_permissions_throw_at_call_time() {
        // fail-closed：清单没声明 fs，import 不炸，调用 writeFile 抛出点名报错。
        let tmp = TempDir::new("host-perm-denied");
        write_extension(
            &tmp,
            "guard-v1.0.0.js",
            r#"{ "id": "guard", "version": "1.0.0", "commands": [{ "id": "write", "title": "Write" }] }"#,
            "import { writeFile } from 'bolt';\nexport function write() { writeFile('/tmp/nope', 'x'); }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.ensure_loaded(&target).expect("guard should load");

        let error = host
            .store()
            .invoke(
                &bundle("guard", "1.0.0"),
                InvokeRequest::new("write", CancelToken::new()),
            )
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
            "bad-v1.0.0.js",
            r#"{ "id": "bad", "version": "1.0.0", "commands": "nope" }"#,
            "",
        );
        write_extension(
            &tmp,
            "good-v1.0.0.js",
            r#"{ "id": "good", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'ok'; }",
        );

        let mut host = host(&tmp);
        let report = host.refresh();
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(
            report.errors[0].contains("bad-v1.0.0.js"),
            "{:?}",
            report.errors
        );

        let commands = host.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].extension_id, "good");
    }

    #[test]
    fn refresh_is_idempotent() {
        let tmp = TempDir::new("host-refresh");
        write_extension(
            &tmp,
            "one-v1.0.0.js",
            r#"{ "id": "one", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() {}",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        // 第二次扫描不该把已注册的扩展当成错误报出来。
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands().len(), 1);
    }

    fn wait_state(store: &ExtensionStore, bundle: &Bundle, want: RuntimeState) {
        for _ in 0..500 {
            if store.state(bundle) == Some(want) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "state of {bundle} never became {want:?} (last: {:?})",
            store.state(bundle)
        );
    }

    #[test]
    fn refresh_unregisters_extension_deleted_from_disk() {
        let tmp = TempDir::new("host-stale");
        for id in ["one", "two"] {
            write_extension(
                &tmp,
                &format!("{id}-v1.0.0.js"),
                &format!(
                    r#"{{ "id": "{id}", "version": "1.0.0", "commands": [{{ "id": "go", "title": "Go" }}] }}"#
                ),
                "export function go() {}",
            );
        }

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands().len(), 2);

        std::fs::remove_dir_all(tmp.0.join("two-v1.0.0.js")).unwrap();
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
    fn commands_keep_only_the_highest_version() {
        let tmp = TempDir::new("host-versions");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() { return 'v1'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let older = host.commands().into_iter().next().expect("v1 command");
        host.ensure_loaded(&older).expect("v1 loads");

        // 升级：两个版本目录并存。
        write_extension(
            &tmp,
            "demo-v1.1.0.js",
            r#"{ "id": "demo", "version": "1.1.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() { return 'v2'; }",
        );
        assert!(host.refresh().errors.is_empty());

        let commands = host.commands();
        assert_eq!(
            commands.len(),
            1,
            "same id must not show twice: {commands:?}"
        );
        assert_eq!(commands[0].bundle.version.to_string(), "1.1.0");
        assert_eq!(commands[0].item_id(), "ext.demo.hello");
        // 旧版本的运行时被停掉；注册保留给 plux 的依赖解析。
        assert_eq!(host.store().state(&bundle("demo", "1.0.0")), None);
    }

    #[test]
    fn broken_extension_is_rebuilt_after_the_file_is_fixed() {
        let tmp = TempDir::new("host-rebuild");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() { return 'ok'; }",
        );
        // 入口写成语法错误：加载失败，运行时死掉。
        std::fs::write(
            tmp.0.join("demo-v1.0.0.js").join("index.js"),
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
                .invoke(
                    &bundle("demo", "1.0.0"),
                    InvokeRequest::new("hello", CancelToken::new())
                )
                .is_err(),
            "broken entry must fail"
        );
        wait_state(&store, &bundle("demo", "1.0.0"), RuntimeState::Dead);

        // 作者修好文件后再点一次：健康检查要重建运行时，而不是永远短路。
        std::fs::write(
            tmp.0.join("demo-v1.0.0.js").join("index.js"),
            "export function hello() { return 'fixed'; }",
        )
        .unwrap();
        host.ensure_loaded(&target)
            .expect("dead runtime must rebuild");

        let output = store
            .invoke(
                &bundle("demo", "1.0.0"),
                InvokeRequest::new("hello", CancelToken::new()),
            )
            .expect("rebuilt extension runs");
        assert_eq!(output.as_deref(), Some("fixed"));
    }

    #[test]
    fn rebuild_is_rate_limited() {
        let tmp = TempDir::new("host-rebuild-cooldown");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() {",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("command");
        host.ensure_loaded(&target)
            .expect("first load is scheduled");
        let store = host.store();
        wait_state(&store, &bundle("demo", "1.0.0"), RuntimeState::Dead);

        host.ensure_loaded(&target).expect("first rebuild");
        wait_state(&store, &bundle("demo", "1.0.0"), RuntimeState::Dead);

        let error = host
            .ensure_loaded(&target)
            .expect_err("second rebuild inside the cooldown must be refused");
        assert!(error.contains("后可重新执行"), "{error}");
    }

    fn invoke(store: &ExtensionStore, id: &str, version: &str, command: &str) -> Option<String> {
        store
            .invoke(
                &bundle(id, version),
                InvokeRequest::new(command, CancelToken::new()),
            )
            .expect("command should run")
    }

    #[test]
    fn same_version_content_change_reloads_and_notifies() {
        let tmp = TempDir::new("host-content-change");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() { return 'v1'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().notices.is_empty());
        let target = host.commands().into_iter().next().expect("command");
        host.ensure_loaded(&target).expect("loads");
        let store = host.store();
        assert_eq!(
            invoke(&store, "demo", "1.0.0", "hello").as_deref(),
            Some("v1")
        );

        // 改代码但不动版本号：plux 只会回 AlreadyExistsIDAndVersion，宿主自己得发现。
        std::fs::write(
            tmp.0.join("demo-v1.0.0.js/index.js"),
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
        assert_eq!(
            invoke(&store, "demo", "1.0.0", "hello").as_deref(),
            Some("v2")
        );
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
            "demo-v1.0.0.js",
            &manifest("Hello"),
            "export function hello() {}",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands()[0].title, "Hello");

        std::fs::write(
            tmp.0.join("demo-v1.0.0.js/extension.json"),
            manifest("Hi there"),
        )
        .unwrap();
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
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "bump", "title": "Bump" }] }"#,
            "let n = 0;\nexport function bump() { n += 1; return String(n); }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("command");
        host.ensure_loaded(&target).expect("loads");
        let store = host.store();
        assert_eq!(
            invoke(&store, "demo", "1.0.0", "bump").as_deref(),
            Some("1")
        );
        assert_eq!(
            invoke(&store, "demo", "1.0.0", "bump").as_deref(),
            Some("2")
        );

        // Launcher 每打开一次都会 refresh：内容没变就不该动运行时。
        let report = host.refresh();
        assert!(
            report.errors.is_empty() && report.notices.is_empty(),
            "{report:?}"
        );
        assert_eq!(
            invoke(&store, "demo", "1.0.0", "bump").as_deref(),
            Some("3")
        );
    }

    #[test]
    fn content_change_is_reported_when_a_loaded_extension_depends_on_it() {
        let tmp = TempDir::new("host-content-dependent");
        write_extension(
            &tmp,
            "base-v1.0.0.js",
            r#"{ "id": "base", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'base'; }",
        );
        write_extension(
            &tmp,
            "app-v1.0.0.js",
            r#"{ "id": "app", "version": "1.0.0", "depends": ["base@*"], "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'app'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        for target in host.commands() {
            host.ensure_loaded(&target).expect("both load");
        }
        let base_target = host
            .commands()
            .into_iter()
            .find(|command| command.extension_id == "base")
            .expect("base command");
        host.ensure_loaded(&base_target).expect("base loads");

        std::fs::write(
            tmp.0.join("base-v1.0.0.js/index.js"),
            "export function go() { return 'base v2'; }",
        )
        .unwrap();

        // 被已加载的扩展依赖着，plux 拒绝卸载：只能报出来让作者升版本号。
        let report = host.refresh();
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(
            report.errors[0].contains("无法重新加载"),
            "{:?}",
            report.errors
        );
    }

    #[test]
    fn disabled_extension_is_never_registered() {
        let tmp = TempDir::new("host-disabled");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        let report = host.set_enabled("demo", false).expect("disable");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(host.commands().is_empty());
        assert!(
            host.loader
                .get_plugin_by_bundle(&bundle("demo", "1.0.0"))
                .is_none(),
            "disabled extension must not be registered"
        );
    }

    #[test]
    fn disabling_a_loaded_extension_unloads_its_runtime() {
        let tmp = TempDir::new("host-disable-loaded");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.ensure_loaded(&target).expect("loads");
        let store = host.store();
        wait_state(&store, &bundle("demo", "1.0.0"), RuntimeState::Ready);
        assert!(store.is_loaded(&bundle("demo", "1.0.0")));

        host.set_enabled("demo", false).expect("disable");
        assert!(
            store.state(&bundle("demo", "1.0.0")).is_none(),
            "runtime must be unloaded together with the registration"
        );
        assert!(host.commands().is_empty());
    }

    #[test]
    fn re_enabling_restores_commands_without_a_reload_notice() {
        let tmp = TempDir::new("host-reenable");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
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
    fn disabled_extension_with_a_loaded_dependent_is_hidden_and_reported() {
        let tmp = TempDir::new("host-disable-dependent");
        write_extension(
            &tmp,
            "base-v1.0.0.js",
            r#"{ "id": "base", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'base'; }",
        );
        write_extension(
            &tmp,
            "app-v1.0.0.js",
            r#"{ "id": "app", "version": "1.0.0", "depends": ["base@*"], "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'app'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        for target in host.commands() {
            host.ensure_loaded(&target).expect("both load");
        }

        // plux 会拒绝卸载被依赖的 base：注册表留着，但命令必须消失。
        let report = host.set_enabled("base", false).expect("disable");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.notices.len(), 1, "{:?}", report.notices);
        assert!(report.notices[0].contains("依赖"), "{:?}", report.notices);
        let ids: Vec<String> = host
            .commands()
            .into_iter()
            .map(|command| command.extension_id)
            .collect();
        assert_eq!(ids, vec!["app".to_string()]);

        // 启用回来后，命令也跟着回来。
        host.set_enabled("base", true).expect("enable");
        let ids: Vec<String> = host
            .commands()
            .into_iter()
            .map(|command| command.extension_id)
            .collect();
        assert_eq!(ids, vec!["app".to_string(), "base".to_string()]);
    }

    #[test]
    fn inventory_lists_broken_manifests() {
        let tmp = TempDir::new("host-inventory-broken");
        write_extension(
            &tmp,
            "bad-v1.0.0.js",
            r#"{ "id": "bad", "version": "1.0.0", "commands": "nope" }"#,
            "",
        );

        let mut host = host(&tmp);
        assert!(!host.refresh().errors.is_empty());
        let entries = host.inventory();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert!(!entries[0].registered);
        assert!(entries[0].enabled);
        assert_eq!(entries[0].bundle.id, "bad");
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
    fn inventory_marks_shadowed_versions_and_dependents() {
        let tmp = TempDir::new("host-inventory");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'v1'; }",
        );
        write_extension(
            &tmp,
            "demo-v2.0.0.js",
            r#"{ "id": "demo", "version": "2.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'v2'; }",
        );
        write_extension(
            &tmp,
            "app-v1.0.0.js",
            r#"{ "id": "app", "version": "1.0.0", "depends": ["demo@*"], "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'app'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let entries = host.inventory();

        let v1 = entries
            .iter()
            .find(|entry| entry.bundle.id == "demo" && entry.bundle.version.to_string() == "1.0.0")
            .expect("v1");
        let v2 = entries
            .iter()
            .find(|entry| entry.bundle.id == "demo" && entry.bundle.version.to_string() == "2.0.0")
            .expect("v2");
        assert!(v1.shadowed, "low version should be shadowed");
        assert!(!v2.shadowed);

        // 依赖方按 id+版本区间算：app 声明的 demo@* 覆盖两个版本。
        assert_eq!(v2.dependents.len(), 1, "{:?}", v2.dependents);
        assert_eq!(v2.dependents[0].id, "app");
        assert_eq!(v2.command_count, 1);
        assert_eq!(v2.ui_command_count, 0);
        assert!(v2.manifest_error.is_none());
    }

    #[test]
    fn reload_after_manifest_edit_updates_metadata() {
        let tmp = TempDir::new("host-reload");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Old" }] }"#,
            "export function go() { return 'v1'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands()[0].title, "Old");

        std::fs::write(
            tmp.0.join("demo-v1.0.0.js/extension.json"),
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "New" }] }"#,
        )
        .unwrap();

        let report = host.reload(&bundle("demo", "1.0.0")).expect("reload");
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(host.commands()[0].title, "New");
    }

    #[test]
    fn reload_is_refused_while_disabled() {
        let tmp = TempDir::new("host-reload-disabled");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        host.set_enabled("demo", false).expect("disable");
        let error = host
            .reload(&bundle("demo", "1.0.0"))
            .expect_err("must refuse");
        assert!(error.contains("已被禁用"), "{error}");
    }

    #[test]
    fn forget_unregisters_without_touching_the_disk() {
        let tmp = TempDir::new("host-forget");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        host.forget(&bundle("demo", "1.0.0")).expect("forget");

        assert!(host.commands().is_empty());
        assert!(
            host.loader
                .get_plugin_by_bundle(&bundle("demo", "1.0.0"))
                .is_none()
        );
        // 目录还在，下一轮 refresh 又把它捡回来（卸载的调用方负责真的删目录）。
        assert!(host.refresh().errors.is_empty());
        assert_eq!(host.commands().len(), 1);
    }

    #[test]
    fn ensure_loaded_refuses_a_disabled_extension() {
        let tmp = TempDir::new("host-disabled-load");
        write_extension(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'go'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().errors.is_empty());
        let mut target = host.commands().into_iter().next().expect("one command");
        host.set_enabled("demo", false).expect("disable");

        // 模拟"残留 item"：拿被禁用扩展的历史 target 直接要求加载。
        target.bundle = bundle("demo", "1.0.0");
        let error = host.ensure_loaded(&target).expect_err("must refuse");
        assert!(error.contains("已被禁用"), "{error}");
    }
}
