//! 扩展的管理动作：Launcher 的 Cmd+K 与管理窗口共用同一份定义。
//!
//! 动作闭包只拿 `&mut App`（[`LauncherAction`] 的约束），所以每个动作都是一行
//! 转调 [`ops`]；需要确认的（卸载）用 [`DialogSpec::Confirm`]，不需要窗口上下文。

use std::path::PathBuf;
use std::sync::Arc;

use plux_rs::Bundle;

use super::host::{CommandTarget, PluginEntry};
use super::ops;
use crate::app::launcher::{DialogSpec, ItemAction};

/// 一次管理动作作用的对象：全是快照值，可以 move 进闭包。
#[derive(Clone)]
pub(crate) struct ActionSubject {
    pub(crate) bundle: Bundle,
    pub(crate) plugin_id: String,
    pub(crate) plugin_name: String,
    pub(crate) root: PathBuf,
    pub(crate) enabled: bool,
    /// 声明依赖它的已注册插件（只有管理窗口能算出来；Launcher 侧为空）。
    pub(crate) dependents: Vec<Bundle>,
}

impl ActionSubject {
    pub(crate) fn from_target(target: &CommandTarget) -> Self {
        Self {
            bundle: target.bundle.clone(),
            plugin_id: target.plugin_id.clone(),
            plugin_name: target.plugin_name.clone(),
            root: target.root.clone(),
            // 能出现在 Launcher 里的插件一定是启用的（禁用项被 commands() 过滤）。
            enabled: true,
            dependents: Vec::new(),
        }
    }

    pub(crate) fn from_entry(entry: &PluginEntry) -> Self {
        Self {
            bundle: entry.bundle.clone(),
            plugin_id: entry.bundle.id.clone(),
            plugin_name: entry.name.clone(),
            root: entry.root.clone(),
            enabled: entry.enabled,
            dependents: entry.dependents.clone(),
        }
    }

    pub(crate) fn label(&self) -> String {
        format!("{} v{}", self.plugin_name, self.bundle.version)
    }
}

/// 插件级动作，顺序固定（便于单测断言）：
/// 重新加载 → 启用/禁用 → 在 Finder 中显示 → 打开数据目录 → 卸载。
pub(crate) fn extension_actions(subject: &ActionSubject) -> Vec<ItemAction> {
    let mut actions = Vec::with_capacity(5);

    {
        let subject = subject.clone();
        actions.push(ItemAction {
            title: "重新加载".to_string(),
            shortcut: None,
            run: Arc::new(move |cx| ops::reload(cx, &subject)),
            dialog: None,
        });
    }

    {
        let subject = subject.clone();
        let enable = !subject.enabled;
        actions.push(ItemAction {
            title: if enable {
                "启用插件"
            } else {
                "禁用插件"
            }
            .to_string(),
            shortcut: None,
            run: Arc::new(move |cx| ops::set_enabled(cx, &subject, enable)),
            dialog: None,
        });
    }

    {
        let subject = subject.clone();
        actions.push(ItemAction {
            title: "在 Finder 中显示".to_string(),
            shortcut: None,
            run: Arc::new(move |cx| ops::reveal(cx, &subject)),
            dialog: None,
        });
    }

    {
        let subject = subject.clone();
        actions.push(ItemAction {
            title: "打开数据目录".to_string(),
            shortcut: None,
            run: Arc::new(move |cx| ops::open_data_dir(cx, &subject)),
            dialog: None,
        });
    }

    {
        let subject = subject.clone();
        let description = ops::uninstall_description(&subject);
        actions.push(ItemAction {
            title: "卸载插件…".to_string(),
            shortcut: None,
            run: Arc::new(|_| {}),
            dialog: Some(DialogSpec::Confirm {
                title: format!("卸载插件 {}", subject.label()),
                description,
                ok_text: Some("移到废纸篓".to_string()),
                cancel_text: Some("取消".to_string()),
                on_ok: Arc::new(move |cx| ops::uninstall(cx, &subject)),
            }),
        });
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn subject(enabled: bool) -> ActionSubject {
        ActionSubject {
            bundle: Bundle::from_filename("demo-v1.0.0.js").expect("valid bundle"),
            plugin_id: "demo".to_string(),
            plugin_name: "Demo".to_string(),
            root: PathBuf::from("/tmp/demo-v1.0.0.js"),
            enabled,
            dependents: Vec::new(),
        }
    }

    fn titles(enabled: bool) -> Vec<String> {
        extension_actions(&subject(enabled))
            .into_iter()
            .map(|action| action.title)
            .collect()
    }

    #[test]
    fn actions_cover_every_operation_in_order() {
        assert_eq!(
            titles(true),
            vec![
                "重新加载",
                "禁用插件",
                "在 Finder 中显示",
                "打开数据目录",
                "卸载插件…",
            ]
        );
    }

    #[test]
    fn disabled_plugin_offers_enable() {
        assert_eq!(titles(false)[1], "启用插件");
    }

    #[test]
    fn uninstall_carries_a_confirm_dialog() {
        let actions = extension_actions(&subject(true));
        let uninstall = actions.last().expect("uninstall");
        assert!(matches!(uninstall.dialog, Some(DialogSpec::Confirm { .. })));
    }

    #[test]
    fn uninstall_description_mentions_dependents() {
        let mut subject = subject(true);
        subject.dependents = vec![Bundle::from_filename("app-v1.0.0.js").unwrap()];
        let text = ops::uninstall_description(&subject);
        assert!(text.contains("依赖"), "{text}");
        assert!(text.contains("app v1.0.0"), "{text}");
    }
}
