//! Item 动作的构建与动作面板的 dialog。
//!
//! `actions_for` 给 item 补默认动作与关键词动作；`open_action_dialog` 在
//! 当前窗口渲染展示/编辑型 dialog（不关 launcher）。

use std::sync::Arc;

use gpui_kit::component::WindowExt;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::{App, AppContext as _, ParentElement as _, Window};

use super::item::{DialogSpec, ItemAction, LauncherAction, LauncherItem};
use crate::app::keywords;

/// item 没声明 actions 时回退成单个「打开」（执行 on_confirm）。
/// 无论回退还是声明，都统一追加「显示搜索关键词」——因为 keyword 是
/// Launcher 的核心概念，模块不用自己重复写这条。
pub(crate) fn actions_for(item: &LauncherItem) -> Vec<ItemAction> {
    let mut actions = if item.actions.is_empty() {
        vec![ItemAction {
            title: "打开".to_string(),
            shortcut: Some("↵"),
            run: item.on_confirm.clone(),
            dialog: None,
        }]
    } else {
        item.actions.clone()
    };

    // 搜索命中来源：label（title）、id、subtitle、item.keywords。过滤在
    // visible_indices 里做（title/id/subtitle/keywords 都参与）；把这几项
    // 列出来供「显示搜索关键词」调试动作展示。
    let item_id = item.id.clone();
    let title = item.title.clone();
    let subtitle = item.subtitle.clone();
    let keywords = item.keywords.clone();
    let mut lines = vec![format!("id: {item_id}"), format!("title: {title}")];
    if let Some(s) = &subtitle {
        lines.push(format!("subtitle: {s}"));
    }
    if keywords.is_empty() {
        lines.push("keywords: (无)".to_string());
    } else {
        lines.push(format!(
            "keywords: [{}]",
            keywords
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // dialog 字段非 None 表示这是展示/编辑型动作——Launcher 收到后直接在
    // 当前窗口 open dialog，不走 channel、不关窗口。
    let noop: LauncherAction = Arc::new(|_| {});
    actions.push(ItemAction {
        title: "显示搜索关键词".to_string(),
        shortcut: None,
        run: noop,
        dialog: Some(DialogSpec::Info {
            title: "搜索关键词".to_string(),
            description: lines.join("\n"),
            ok_text: Some("关闭".to_string()),
            show_cancel: false,
        }),
    });
    // 编辑动作：覆盖表按 item id 存盘（空输入恢复模块默认），保存后
    // items_revision++ 让下一次 collect 拿到新关键词。
    let edited_id = item.id.clone();
    let noop: LauncherAction = Arc::new(|_| {});
    actions.push(ItemAction {
        title: "编辑搜索关键词".to_string(),
        shortcut: None,
        run: noop,
        dialog: Some(DialogSpec::Edit {
            title: format!("编辑搜索关键词 — {title}"),
            initial: keywords.join(", "),
            ok_text: Some("保存".to_string()),
            on_submit: Arc::new(move |cx, text| keywords::set(cx, &edited_id, &text)),
        }),
    });
    actions
}

/// 在当前窗口打开动作的 dialog：展示型（Info）直接展示；编辑型（Edit）带
/// 输入框，OK 时把输入内容交给 `on_submit`。输入框 Entity 在闭包外创建——
/// `open_alert_dialog` 的 build 闭包与 content builder 每帧都会被调用，
/// 不能在里面 `cx.new`。
pub(crate) fn open_action_dialog(window: &mut Window, cx: &mut App, spec: &DialogSpec) {
    match spec {
        DialogSpec::Info {
            title,
            description,
            ok_text,
            show_cancel,
        } => {
            let (title, description, ok_text, show_cancel) = (
                title.clone(),
                description.clone(),
                ok_text.clone(),
                *show_cancel,
            );
            window.open_alert_dialog(cx, move |alert, _, _| {
                let mut dlg = alert
                    .title(title.clone())
                    .description(description.clone())
                    .show_cancel(show_cancel);
                if let Some(ok) = &ok_text {
                    dlg = dlg.ok_text(ok.clone());
                }
                dlg.on_ok(move |_, _, _| true)
            });
        }
        DialogSpec::Edit {
            title,
            initial,
            ok_text,
            on_submit,
        } => {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("逗号分隔，例如：微信, wechat（留空恢复默认）")
            });
            input.update(cx, |state, cx| state.set_value(initial.clone(), window, cx));
            let (title, ok_text, on_submit) = (title.clone(), ok_text.clone(), on_submit.clone());
            window.open_alert_dialog(cx, move |alert, _, _| {
                let mut dlg = alert
                    .title(title.clone())
                    // 内层闭包 own 一份 clone（外层是 Fn，构造内层 move 闭包
                    // 时只能动局部值，不能 move 捕获）。
                    .content({
                        let input = input.clone();
                        move |content, _, _| content.child(Input::new(&input))
                    })
                    .on_ok({
                        let input = input.clone();
                        let on_submit = on_submit.clone();
                        move |_, _, cx| {
                            let text = input.read(cx).value().to_string();
                            on_submit(cx, text);
                            true
                        }
                    });
                if let Some(ok) = &ok_text {
                    dlg = dlg.ok_text(ok.clone());
                }
                dlg
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> LauncherItem {
        LauncherItem {
            id: id.to_string(),
            title: id.to_string(),
            subtitle: None,
            icon: gpui_kit::assets::IconName::Folder.into(),
            group: "内置".to_string(),
            keywords: Vec::new(),
            on_confirm: Arc::new(|_| {}),
            actions: Vec::new(),
            paste: None,
        }
    }

    #[test]
    fn actions_fallback_has_open_plus_keyword_actions() {
        let mut it = item("a");
        let actions = actions_for(&it);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].title, "打开");
        assert_eq!(actions[0].shortcut, Some("↵"));
        // 两条关键词动作：展示（dialog 只读）与编辑（dialog 带输入框）。
        assert_eq!(actions[1].title, "显示搜索关键词");
        assert!(matches!(actions[1].dialog, Some(DialogSpec::Info { .. })));
        assert_eq!(actions[2].title, "编辑搜索关键词");
        assert!(matches!(actions[2].dialog, Some(DialogSpec::Edit { .. })));
        // 编辑框初始值是 item 当前的 keywords。
        it.keywords = vec!["foo".into(), "bar".into()];
        let actions = actions_for(&it);
        let Some(DialogSpec::Edit { initial, .. }) = actions[2].dialog.clone() else {
            panic!("编辑动作应携带 Edit dialog");
        };
        assert_eq!(initial, "foo, bar");
    }

    #[test]
    fn declared_actions_are_preserved_then_keyword_actions_appended() {
        let mut it = item("a");
        it.actions = vec![
            ItemAction {
                title: "第一个".to_string(),
                shortcut: None,
                run: Arc::new(|_| {}),
                dialog: None,
            },
            ItemAction {
                title: "第二个".to_string(),
                shortcut: None,
                run: Arc::new(|_| {}),
                dialog: None,
            },
        ];
        let actions = actions_for(&it);
        assert_eq!(actions.len(), 4);
        assert_eq!(actions[0].title, "第一个");
        assert_eq!(actions[1].title, "第二个");
        assert_eq!(actions[2].title, "显示搜索关键词");
        assert_eq!(actions[3].title, "编辑搜索关键词");
    }
}
