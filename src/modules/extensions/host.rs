//! plux 插件宿主：`Loader`（生命周期/元数据）+ `PluginStore`（执行）。
//!
//! **`Loader` 必须先 `Box` 再注册插件，之后永不移动。** plux 的 `Plugin` 里存的是
//! 指向 `loader.managers` Vec 槽位的裸指针（`Ptr`），在栈上建好 Loader 再 move 进
//! 结构体会让它立刻悬空。同理，manager 只在插件注册之前注册——之后不再往
//! `managers` 里 push，Vec 不会 realloc，指针一直有效。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use plux_js_manager::{JsEnv, JsManager, PluginMeta, PluginStore, RuntimeState, UiSpec};
use plux_rs::function::FunctionOutput;
use plux_rs::utils::{RegisterPluginError, UnloadPluginError, UnregisterPluginError};
use plux_rs::{Bundle, Loader};

use super::manifest;

pub(crate) type ExtensionsLoader = Loader<'static, FunctionOutput, PluginMeta>;

/// 重建运行时的冷却期：坏插件被连点时不该每次都起一条新线程再失败一次。
const REBUILD_COOLDOWN: Duration = Duration::from_secs(3);

/// Launcher 里的一条命令（已脱离 `Loader` 的借用，可 move 进 `on_confirm`）。
#[derive(Clone, Debug)]
pub(crate) struct CommandTarget {
    pub(crate) bundle: Bundle,
    pub(crate) plugin_id: String,
    pub(crate) plugin_name: String,
    /// 插件目录：UI 命令用它做 gpui-shell 的应用根。
    pub(crate) root: PathBuf,
    pub(crate) command_id: String,
    pub(crate) title: String,
    pub(crate) description: Option<String>,
    /// 给了就是 UI 命令。
    pub(crate) ui: Option<UiSpec>,
}

impl CommandTarget {
    /// Launcher item 的 id：`ext.<plugin-id>.<command-id>`。
    pub fn item_id(&self) -> String {
        format!("ext.{}.{}", self.plugin_id, self.command_id)
    }

    pub(crate) fn is_ui(&self) -> bool {
        self.ui.is_some()
    }
}

pub(crate) struct ExtensionsHost {
    /// 见模块头部：必须钉在 Box 里。
    loader: Box<ExtensionsLoader>,
    store: Arc<PluginStore>,
    extensions_dir: PathBuf,
    /// 注册 manager 失败的原因。此时宿主仍然可构造，但所有命令都不可用，
    /// 由 `refresh` 把这条错误报给 Console。
    init_error: Option<String>,
    /// 本次扫描时磁盘上真实存在的插件。
    ///
    /// 由目录名解析得出，**与注册成败无关**：清单写坏的插件也算存在，否则作者
    /// 保存一次坏清单就会把正在用的插件判成"已删除"，连带杀掉它的运行时。
    /// `None` = 还没成功扫描过，此时不做任何过滤。
    live: Option<HashSet<Bundle>>,
    /// 每个插件上次重建运行时的时刻，用来给重建加冷却。
    rebuilt_at: HashMap<(String, String), Instant>,
}

impl ExtensionsHost {
    pub(crate) fn new(extensions_dir: &Path, env: Arc<dyn JsEnv>) -> Self {
        let manager = JsManager::new(env);
        let store = manager.store().clone();

        let mut loader: Box<ExtensionsLoader> = Box::new(Loader::new());
        let init_error = loader
            .context(|mut ctx| ctx.register_manager(manager))
            .err()
            .map(|e| format!("注册 JS 插件管理器失败：{e}"));

        Self {
            loader,
            store,
            extensions_dir: extensions_dir.to_path_buf(),
            init_error,
            live: None,
            rebuilt_at: HashMap::new(),
        }
    }

    /// 重新扫描磁盘并注册新插件；返回本次发现的错误（不含已注册插件的重复注册）。
    pub(crate) fn refresh(&mut self) -> Vec<String> {
        if let Some(error) = &self.init_error {
            return vec![error.clone()];
        }

        let (dirs, mut errors) = manifest::scan(&self.extensions_dir);
        // 先按目录名算"磁盘上有谁"：注册失败（清单写坏）也算存在。
        let live: HashSet<Bundle> = dirs
            .iter()
            .filter_map(|dir| plux_js_manager::manifest::bundle_from_dir(dir).ok())
            .collect();
        let live_keys: HashSet<(String, String)> = live
            .iter()
            .map(|bundle| (bundle.id.clone(), bundle.version.to_string()))
            .collect();

        for dir in dirs {
            let Some(path) = dir.to_str() else {
                errors.push(format!("插件路径不是合法 UTF-8：{}", dir.display()));
                continue;
            };
            match self.loader.register_plugin(path) {
                // 重复注册（同 id 同版本）是正常的：Launcher 每次打开都会刷新。
                Ok(_) | Err(RegisterPluginError::AlreadyExistsIDAndVersion(..)) => {}
                Err(error) => errors.push(format!(
                    "扩展加载失败 {}：{}",
                    dir.display(),
                    describe(&error)
                )),
            }
        }

        self.live = Some(live);
        self.rebuilt_at.retain(|key, _| live_keys.contains(key));
        self.drop_stale(&mut errors);
        self.drop_shadowed();
        errors
    }

    /// 磁盘上已经消失的插件：连注册一起摘掉，Launcher 里自然不再出现。
    fn drop_stale(&mut self, errors: &mut Vec<String>) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let stale: Vec<Bundle> = self
            .loader
            .get_plugins()
            .iter()
            .map(|plugin| plugin.info().bundle.clone())
            .filter(|bundle| !live.contains(bundle))
            .collect();

        for bundle in stale {
            match self.loader.unregister_plugin_by_bundle(&bundle) {
                Ok(()) => {}
                // 已经不在表里：目标状态已达成。
                Err(UnregisterPluginError::NotFound) => {}
                // 还被别的已加载插件依赖：plux 拒绝卸载。不强行杀运行时（那会让
                // 依赖方静默失效），只是不再出现在 Launcher（`commands` 按 live 过滤），
                // 下次启动彻底消失。
                Err(UnregisterPluginError::UnloadError(
                    UnloadPluginError::CurrentlyUsesDepend { .. },
                )) => {}
                Err(error) => errors.push(format!(
                    "插件卸载失败 {} v{}：{error}",
                    bundle.id, bundle.version
                )),
            }
        }
    }

    /// 同 id 多版本：只留最高版本的运行时。
    ///
    /// 两套引擎会让插件顶层副作用跑两遍，而 item id（`ext.<id>.<command>`）不含
    /// 版本，Launcher 里必然重复。**注册保留**——plux 的依赖解析仍可能按版本区间
    /// 挑到低版本，注册表要留给它。
    fn drop_shadowed(&mut self) {
        let mut highest: HashMap<&str, &Bundle> = HashMap::new();
        for plugin in self.loader.get_plugins() {
            let bundle = &plugin.info().bundle;
            match highest.get(bundle.id.as_str()) {
                Some(kept) if kept.version >= bundle.version => {}
                _ => {
                    highest.insert(bundle.id.as_str(), bundle);
                }
            }
        }

        let shadowed: Vec<(String, String)> = self
            .loader
            .get_plugins()
            .iter()
            .map(|plugin| plugin.info().bundle.clone())
            .filter(|bundle| {
                highest
                    .get(bundle.id.as_str())
                    .is_some_and(|kept| kept.version > bundle.version)
            })
            .map(|bundle| (bundle.id, bundle.version.to_string()))
            .collect();

        for (id, version) in shadowed {
            self.store.unload(&id, &version);
        }
    }

    /// 当前所有插件贡献的命令，按注册顺序（即插件目录名排序）。
    ///
    /// 两层过滤：磁盘上已不存在的插件不展示（plux 无 unregister 失败的兜底），
    /// 同 id 只展示最高版本（item id 不含版本，双份必然重复；口径与 plux 依赖
    /// 解析一致——它也挑最高匹配版本）。
    pub(crate) fn commands(&self) -> Vec<CommandTarget> {
        let mut highest: HashMap<&str, &Bundle> = HashMap::new();
        for plugin in self.loader.get_plugins() {
            let bundle = &plugin.info().bundle;
            match highest.get(bundle.id.as_str()) {
                Some(kept) if kept.version >= bundle.version => {}
                _ => {
                    highest.insert(bundle.id.as_str(), bundle);
                }
            }
        }

        let mut targets = Vec::new();
        for plugin in self.loader.get_plugins() {
            let info = plugin.info();
            let bundle = &info.bundle;
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
                    plugin_id: meta.id().to_string(),
                    plugin_name: meta.display_name().to_string(),
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

    /// 懒加载：第一次执行命令时才安排加载（立即返回，引擎在插件线程里建）。
    ///
    /// 运行时已死（加载失败 / 线程退出）时先复位 plux 的 `is_load` 再重新安排——
    /// 否则 `load_plugin_by_bundle` 会因为 `is_load == true` 直接 Ok，什么都不重建。
    pub(crate) fn ensure_loaded(&mut self, target: &CommandTarget) -> Result<(), String> {
        let id = target.plugin_id.clone();
        let version = target.bundle.version.to_string();
        let registered = self
            .loader
            .get_plugin_by_bundle(&target.bundle)
            .map(|plugin| plugin.is_load())
            .unwrap_or(false);

        if registered {
            match self.store.state(&id, &version) {
                // 已就绪或正在加载：交给 invoke 的 wait_ready，不会重复起线程。
                Some(RuntimeState::Ready) | Some(RuntimeState::Loading) => return Ok(()),
                Some(RuntimeState::Dead) => {
                    let key = (id.clone(), version.clone());
                    if let Some(at) = self.rebuilt_at.get(&key)
                        && at.elapsed() < REBUILD_COOLDOWN
                    {
                        let reason = self
                            .store
                            .last_error(&id, &version)
                            .map(|error| error.to_string())
                            .unwrap_or_default();
                        return Err(format!(
                            "插件运行时已退出（{reason}），{:.1}s 后可重新执行重试",
                            (REBUILD_COOLDOWN - at.elapsed()).as_secs_f32()
                        ));
                    }
                    self.rebuilt_at.insert(key, Instant::now());
                    // 必须先卸载：load_plugin_by_bundle 对已加载插件是 no-op。
                    if let Err(error) = self.loader.unload_plugin_by_bundle(&target.bundle) {
                        return Err(format!("插件运行时已退出，且无法重建（{error}）"));
                    }
                }
                // plux 说已加载、表里却没有运行时（曾被直接 unload）：同样走重建。
                None => {}
            }
        }

        self.loader
            .load_plugin_by_bundle(&target.bundle)
            .map_err(|e| e.to_string())
    }

    /// 执行入口的 `Arc` 副本（RunService 需要它才能在工作线程里调命令）。
    pub(crate) fn store(&self) -> Arc<PluginStore> {
        self.store.clone()
    }
}

fn describe(error: &RegisterPluginError) -> String {
    match error {
        // plux 的外层 Display 只有"manager 报错"，真正原因在里层。
        RegisterPluginError::RegisterPluginByManager(inner) => inner.to_string(),
        RegisterPluginError::BundleFromFailed(inner) => inner.to_string(),
        RegisterPluginError::NotFound => "插件目录不存在".to_string(),
        RegisterPluginError::UnknownManagerFormat(format) => {
            format!("未知插件格式 `{format}`（目录名必须以 `.js` 结尾）")
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
        let (console, _rx) = ConsoleChannel::new();
        let env = Arc::new(BoltJsEnv {
            registry: Arc::new(registry),
            console,
        });
        ExtensionsHost::new(&tmp.0, env)
    }

    fn write_plugin(tmp: &TempDir, dir: &str, manifest: &str, entry: &str) {
        let root = tmp.0.join(dir);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(MANIFEST_FILE), manifest).unwrap();
        std::fs::write(root.join("index.js"), entry).unwrap();
    }

    #[test]
    fn demo_plugin_contributes_script_and_ui_commands() {
        let tmp = TempDir::new("host-demo");
        manifest::ensure_demo_plugin(&tmp.0).expect("write demo plugin");

        let mut host = host(&tmp);
        let errors = host.refresh();
        assert!(errors.is_empty(), "{errors:?}");

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
                "demo",
                "1.0.0",
                InvokeRequest::new("hello", CancelToken::new()),
            )
            .expect("hello should run");
        assert!(
            output.unwrap_or_default().contains("demo v1.0.0"),
            "unexpected output"
        );
    }

    #[test]
    fn plugins_can_import_host_modules() {
        let tmp = TempDir::new("host-modules");
        write_plugin(
            &tmp,
            "probe-v1.0.0.js",
            r#"{ "id": "probe", "version": "1.0.0", "permissions": ["env"], "commands": [{ "id": "home", "title": "Home" }] }"#,
            "import { env } from 'bolt';\nexport function home() { return env('HOME') ?? 'unset'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.ensure_loaded(&target).expect("probe should load");

        let output = host
            .store()
            .invoke(
                "probe",
                "1.0.0",
                InvokeRequest::new("home", CancelToken::new()),
            )
            .expect("home should run");
        assert!(output.is_some_and(|text| !text.is_empty()));
    }

    #[test]
    fn undeclared_permissions_throw_at_call_time() {
        // fail-closed：清单没声明 fs，import 不炸，调用 writeFile 抛出点名报错。
        let tmp = TempDir::new("host-perm-denied");
        write_plugin(
            &tmp,
            "guard-v1.0.0.js",
            r#"{ "id": "guard", "version": "1.0.0", "commands": [{ "id": "write", "title": "Write" }] }"#,
            "import { writeFile } from 'bolt';\nexport function write() { writeFile('/tmp/nope', 'x'); }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().is_empty());
        let target = host.commands().into_iter().next().expect("one command");
        host.ensure_loaded(&target).expect("guard should load");

        let error = host
            .store()
            .invoke(
                "guard",
                "1.0.0",
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
    fn broken_plugin_is_reported_without_blocking_the_others() {
        let tmp = TempDir::new("host-broken");
        write_plugin(
            &tmp,
            "bad-v1.0.0.js",
            r#"{ "id": "bad", "version": "1.0.0", "commands": "nope" }"#,
            "",
        );
        write_plugin(
            &tmp,
            "good-v1.0.0.js",
            r#"{ "id": "good", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() { return 'ok'; }",
        );

        let mut host = host(&tmp);
        let errors = host.refresh();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("bad-v1.0.0.js"), "{errors:?}");

        let commands = host.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].plugin_id, "good");
    }

    #[test]
    fn refresh_is_idempotent() {
        let tmp = TempDir::new("host-refresh");
        write_plugin(
            &tmp,
            "one-v1.0.0.js",
            r#"{ "id": "one", "version": "1.0.0", "commands": [{ "id": "go", "title": "Go" }] }"#,
            "export function go() {}",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().is_empty());
        // 第二次扫描不该把已注册的插件当成错误报出来。
        assert!(host.refresh().is_empty());
        assert_eq!(host.commands().len(), 1);
    }

    fn wait_state(store: &PluginStore, id: &str, version: &str, want: RuntimeState) {
        for _ in 0..500 {
            if store.state(id, version) == Some(want) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "state of {id} v{version} never became {want:?} (last: {:?})",
            store.state(id, version)
        );
    }

    #[test]
    fn refresh_unregisters_plugins_deleted_from_disk() {
        let tmp = TempDir::new("host-stale");
        for id in ["one", "two"] {
            write_plugin(
                &tmp,
                &format!("{id}-v1.0.0.js"),
                &format!(
                    r#"{{ "id": "{id}", "version": "1.0.0", "commands": [{{ "id": "go", "title": "Go" }}] }}"#
                ),
                "export function go() {}",
            );
        }

        let mut host = host(&tmp);
        assert!(host.refresh().is_empty());
        assert_eq!(host.commands().len(), 2);

        std::fs::remove_dir_all(tmp.0.join("two-v1.0.0.js")).unwrap();
        assert!(
            host.refresh().is_empty(),
            "deleting a plugin is not an error"
        );
        assert_eq!(host.commands().len(), 1);
        assert_eq!(host.commands()[0].plugin_id, "one");

        // 摘掉之后要幂等：下一轮扫描不该再报 NotFound。
        assert!(host.refresh().is_empty());
        assert_eq!(host.commands().len(), 1);
    }

    #[test]
    fn commands_keep_only_the_highest_version() {
        let tmp = TempDir::new("host-versions");
        write_plugin(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() { return 'v1'; }",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().is_empty());
        let older = host.commands().into_iter().next().expect("v1 command");
        host.ensure_loaded(&older).expect("v1 loads");

        // 升级：两个版本目录并存。
        write_plugin(
            &tmp,
            "demo-v1.1.0.js",
            r#"{ "id": "demo", "version": "1.1.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() { return 'v2'; }",
        );
        assert!(host.refresh().is_empty());

        let commands = host.commands();
        assert_eq!(
            commands.len(),
            1,
            "same id must not show twice: {commands:?}"
        );
        assert_eq!(commands[0].bundle.version.to_string(), "1.1.0");
        assert_eq!(commands[0].item_id(), "ext.demo.hello");
        // 旧版本的运行时被停掉；注册保留给 plux 的依赖解析。
        assert_eq!(host.store().state("demo", "1.0.0"), None);
    }

    #[test]
    fn broken_plugin_is_rebuilt_after_the_file_is_fixed() {
        let tmp = TempDir::new("host-rebuild");
        write_plugin(
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
        assert!(host.refresh().is_empty());
        let target = host.commands().into_iter().next().expect("command");
        host.ensure_loaded(&target)
            .expect("spawn must not fail on a broken entry");
        let store = host.store();
        assert!(
            store
                .invoke(
                    "demo",
                    "1.0.0",
                    InvokeRequest::new("hello", CancelToken::new())
                )
                .is_err(),
            "broken entry must fail"
        );
        wait_state(&store, "demo", "1.0.0", RuntimeState::Dead);

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
                "demo",
                "1.0.0",
                InvokeRequest::new("hello", CancelToken::new()),
            )
            .expect("rebuilt plugin runs");
        assert_eq!(output.as_deref(), Some("fixed"));
    }

    #[test]
    fn rebuild_is_rate_limited() {
        let tmp = TempDir::new("host-rebuild-cooldown");
        write_plugin(
            &tmp,
            "demo-v1.0.0.js",
            r#"{ "id": "demo", "version": "1.0.0", "commands": [{ "id": "hello", "title": "Hello" }] }"#,
            "export function hello() {",
        );

        let mut host = host(&tmp);
        assert!(host.refresh().is_empty());
        let target = host.commands().into_iter().next().expect("command");
        host.ensure_loaded(&target)
            .expect("first load is scheduled");
        let store = host.store();
        wait_state(&store, "demo", "1.0.0", RuntimeState::Dead);

        host.ensure_loaded(&target).expect("first rebuild");
        wait_state(&store, "demo", "1.0.0", RuntimeState::Dead);

        let error = host
            .ensure_loaded(&target)
            .expect_err("second rebuild inside the cooldown must be refused");
        assert!(error.contains("后可重新执行"), "{error}");
    }
}
