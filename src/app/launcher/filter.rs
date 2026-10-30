//! Launcher 的过滤逻辑：查询匹配与粘贴能力过滤。
//!
//! 全部是纯函数（不碰 gpui 实体），可以直接单测。

use gpui_kit::ImageFormat;
use gpui_kit::base::input::InlineTokenSpan;

use super::item::LauncherItem;
use crate::ui::clipboard::{PasteContent, PendingPaste};

/// 把 token 覆盖的字节区间从文本里剥掉，剩下的手输片段拼空格。
/// token spans 已按 range 有序且与文本同步（InputContent 一致性保证）。
pub(crate) fn strip_token_text(text: &str, spans: &[InlineTokenSpan]) -> String {
    if spans.is_empty() {
        return text.trim().to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for span in spans {
        let range = span.range();
        if range.start > last {
            out.push_str(&text[last..range.start]);
            out.push(' ');
        }
        last = last.max(range.end);
    }
    if last < text.len() {
        out.push_str(&text[last..]);
    }
    out.trim().to_string()
}

/// item 是否命中查询：大小写不敏感的 contains 匹配，空查询匹配一切。
/// 风格对齐 gpui-component 的 `CommandItem::matches`（逐字段 OR），
/// 参与字段：title、id、subtitle、keywords。
fn matches_query(item: &LauncherItem, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }

    let query = query.to_lowercase();

    item.title.to_lowercase().contains(&query)
        || item.id.to_lowercase().contains(&query)
        || item
            .subtitle
            .as_ref()
            .is_some_and(|subtitle| subtitle.to_lowercase().contains(&query))
        || item
            .keywords
            .iter()
            .any(|keyword| keyword.to_lowercase().contains(&query))
}

/// 按「有效查询」（[`matches_query`]）与粘贴类型能力过滤 items，返回可见
/// items 的下标（渲染顺序）。这份列表同时是 `visible_indices`：选中/确认
/// 的 `IndexPath.row` 都是它在列表里的位置。
pub(crate) fn visible_indices(
    items: &[LauncherItem],
    query: &str,
    paste: Option<&PendingPaste>,
) -> Vec<usize> {
    // 图片是惰性识别（还没读字节）：`accepts` 的契约是只按变体声明能力
    // （不看字段值），空字节探针即可判定；确认时读出的才是真内容。
    let image_probe;
    let paste: Option<&PasteContent> = match paste {
        Some(PendingPaste::Ready(content)) => Some(content),
        Some(PendingPaste::Image(_)) => {
            image_probe = PasteContent::Image {
                bytes: Vec::new(),
                format: ImageFormat::Png,
                width: 0,
                height: 0,
            };
            Some(&image_probe)
        }
        None => None,
    };
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            if let Some(content) = paste {
                let Some(handler) = &item.paste else {
                    return false;
                };
                if !(handler.accepts)(content) {
                    return false;
                }
            }
            matches_query(item, query)
        })
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::item::PasteHandler;
    use super::*;
    use gpui_kit::base::input::{InlineToken, InputContent};
    use std::sync::Arc;

    fn item(id: &str, group: &str) -> LauncherItem {
        LauncherItem {
            id: id.to_string(),
            title: id.to_string(),
            subtitle: None,
            icon: gpui_kit::assets::IconName::Folder.into(),
            group: group.to_string(),
            keywords: Vec::new(),
            on_confirm: Arc::new(|_| {}),
            actions: Vec::new(),
            paste: None,
        }
    }

    #[test]
    fn visible_indices_filters_by_query_and_paste_capability() {
        let mut pinnable = item("pin", "内置");
        pinnable.paste = Some(PasteHandler {
            accepts: |content: &PasteContent| matches!(content, PasteContent::Image { .. }),
            run: Arc::new(|_, _| {}),
        });
        let plain = item("app", "应用");
        let items = vec![pinnable, plain];

        // 无过滤：全部可见。
        assert_eq!(visible_indices(&items, "", None), vec![0, 1]);
        // 文本过滤：title/id/keywords 任一包含（大小写不敏感）。
        assert_eq!(visible_indices(&items, "APP", None), vec![1]);
        // 粘贴图片（惰性：只有类型、空字节探针）：只有声明能处理图片的 pin
        // 可见——顺带验证 accepts 按变体匹配的契约。
        assert_eq!(
            visible_indices(&items, "", Some(&PendingPaste::Image(None))),
            vec![0]
        );
        // 粘贴 + 文本叠加。
        assert_eq!(
            visible_indices(&items, "pin", Some(&PendingPaste::Image(None))),
            vec![0]
        );
        // 已读出的非图片内容（Ready）：pin 不接受。
        assert!(
            visible_indices(
                &items,
                "",
                Some(&PendingPaste::Ready(PasteContent::Text("x".into())))
            )
            .is_empty()
        );
    }

    #[test]
    fn keywords_and_subtitles_participate_in_matching() {
        let mut it = item("some-id", "内置");
        it.subtitle = Some("副标题".into());
        it.keywords = vec!["wechat".into()];
        let items = vec![it];

        assert_eq!(visible_indices(&items, "wechat", None), vec![0]);
        assert_eq!(visible_indices(&items, "副标题", None), vec![0]);
        assert!(visible_indices(&items, "nomatch", None).is_empty());
    }

    #[test]
    fn strip_token_text_keeps_hand_typed_parts() {
        // 无 token：trim 后的原文。
        assert_eq!(strip_token_text(" pin ", &[]), "pin");
        // token 覆盖「图片」（6 字节）+ 手输 "pin"：剥掉 token 段。
        let content = InputContent::new("图片")
            .with_token(0..6, InlineToken::new("json", "图片").with_label("JSON"));
        let input = content.expect("token covers its own text");
        assert_eq!(strip_token_text("图片pin", input.tokens()), "pin");
        // 只有 token：空查询。
        let only = InputContent::new("图片")
            .with_token(0..6, InlineToken::new("json", "图片").with_label("JSON"));
        assert_eq!(strip_token_text("图片", only.expect("valid").tokens()), "");
    }
}
