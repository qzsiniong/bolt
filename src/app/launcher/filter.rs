//! Launcher 的过滤逻辑：查询匹配与粘贴能力过滤。
//!
//! 全部是纯函数（不碰 gpui 实体），可以直接单测。

use gpui_kit::ImageFormat;
use gpui_kit::base::input::InlineTokenSpan;

use super::item::LauncherItem;
use crate::app::pinyin;
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

/// 查询是否命中若干文本字段：**按空白分词，每个词都要命中（AND），词序无关**。
///
/// 单个词在任意一个字段里出现即算命中（大小写不敏感）。所以 `"微信 wechat"`
/// 可以由 title 命中"微信"、由 keywords 命中"wechat"——这是"多个关键字同时
/// 存在"的意图，不是"某个字段里同时有这两段"。
///
/// 空查询或只有空白的分词结果为空 → 匹配一切。全角空格（中文输入法下常见）
/// 也算分隔符：`split_whitespace` 走 `char::is_whitespace`。
pub(crate) fn matches_query_terms(query: &str, fields: &[&str]) -> bool {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return true;
    }
    let haystacks: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();
    terms.iter().all(|term| {
        haystacks
            .iter()
            .any(|haystack| haystack.contains(term.as_str()))
    })
}

/// item 是否命中查询：见 [`matches_query_terms`]，参与字段是
/// title、id、subtitle、keywords（逐字段 OR，逐词 AND）。
///
/// 含汉字的字段会先经 [`pinyin::variants`] 派生出「全拼」与「首字母」
/// 两个额外 haystack（「微信」可被 `weixin` / `wx` 命中）。派生只发生在
/// 这里，不回写 `item.keywords`——那是用户可编辑的数据。
fn matches_query(item: &LauncherItem, query: &str) -> bool {
    if query.trim().is_empty() {
        return true;
    }

    let mut fields: Vec<&str> = Vec::with_capacity(2 + item.keywords.len());
    fields.push(item.title.as_str());
    fields.push(item.id.as_str());
    if let Some(subtitle) = &item.subtitle {
        fields.push(subtitle);
    }
    fields.extend(item.keywords.iter().map(String::as_str));

    // 拼音变体持有在本地，再以引用追加进 fields；owned 不再变长后推引用，
    // 借用检查是安全的。纯 ASCII 字段返回 None，不会产生重复 haystack。
    let mut variants: Vec<(String, String)> = Vec::new();
    for field in &fields {
        if let Some(variant) = pinyin::variants(field) {
            variants.push(variant);
        }
    }
    fields.extend(
        variants
            .iter()
            .flat_map(|(full, initials)| [full.as_str(), initials.as_str()]),
    );

    matches_query_terms(query, &fields)
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
    fn space_separated_terms_are_all_required_any_order() {
        let mut it = item("wecom", "应用");
        it.title = "企业微信".into();
        it.keywords = vec!["wechat".into(), "work".into()];
        let items = vec![it];

        // 两个词都在标题里：命中，且顺序无关。
        assert_eq!(visible_indices(&items, "企业 微信", None), vec![0]);
        assert_eq!(visible_indices(&items, "微信 企业", None), vec![0]);
        // 跨字段也算：一个词命中 title，另一个命中 keywords。
        assert_eq!(visible_indices(&items, "企业 wechat", None), vec![0]);
        // 只要有一个词无处可寻就不命中。
        assert!(visible_indices(&items, "企业 不存在", None).is_empty());
        assert!(visible_indices(&items, "企业 wechat nope", None).is_empty());
    }

    #[test]
    fn term_separators_accept_extra_and_full_width_spaces() {
        let mut it = item("wecom", "应用");
        it.title = "企业微信".into();
        let items = vec![it];

        // 连续空格、首尾空白、全角空格都当分隔符；不产生空词。
        assert_eq!(visible_indices(&items, "  企业   微信  ", None), vec![0]);
        assert_eq!(visible_indices(&items, "企业\u{3000}微信", None), vec![0]);
        // 只有空白 = 没输入：匹配一切，而不是匹配空串。
        assert_eq!(visible_indices(&items, "   ", None), vec![0]);
        assert_eq!(visible_indices(&items, "\u{3000}", None), vec![0]);
    }

    #[test]
    fn hanzi_fields_match_pinyin_queries() {
        let mut it = item("wecom", "应用");
        it.title = "企业微信".into();
        let items = vec![it];

        // 全拼（连写，子串命中）与首字母都能命中中文标题。
        assert_eq!(visible_indices(&items, "weixin", None), vec![0]);
        assert_eq!(visible_indices(&items, "qiyeweixin", None), vec![0]);
        assert_eq!(visible_indices(&items, "wx", None), vec![0]);
        assert_eq!(visible_indices(&items, "qywx", None), vec![0]);
        // 拼音 + 汉字跨词 AND 照常生效。
        assert_eq!(visible_indices(&items, "企业 weixin", None), vec![0]);
        assert!(visible_indices(&items, "wxyz", None).is_empty());
    }

    #[test]
    fn chinese_keywords_match_pinyin_queries() {
        let mut it = item("console", "内置");
        it.title = "控制台".into();
        it.keywords = vec!["微信".into(), "wechat".into()];
        let items = vec![it];

        assert_eq!(visible_indices(&items, "weixin", None), vec![0]);
        assert_eq!(visible_indices(&items, "wx", None), vec![0]);
        // 关键词原有的 ASCII 匹配不受影响。
        assert_eq!(visible_indices(&items, "wechat", None), vec![0]);
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
