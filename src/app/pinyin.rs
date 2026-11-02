//! 汉语拼音派生：把含汉字的搜索字段转出「全拼」与「拼音首字母」两个变体。
//!
//! 只做派生、不做存储：Launcher 过滤时对 title / keywords 现场转换（逐
//! char 查表，量级可忽略），不把派生词写进 `item.keywords`——那是用户
//! 可编辑的数据（「编辑搜索关键词」会原样回显并存回覆盖表），混入派生
//! 词会让编辑框越改越长。

use pinyin::ToPinyin;

/// 含汉字时返回 `Some((全拼, 首字母))`，否则 `None`（纯 ASCII 字段没有
/// 可派生的变体，调用方无须追加）。
///
/// - 汉字逐字转拼音：全拼按字连写（「企业微信」→ `qiyeweixin`），首字母
///   一字一字母（→ `qywx`）；连写保证 `weixin` 这类子串查询能命中；
/// - 非汉字字符原样保留（「JSON编辑器」→ `JSONbianjiqi` + `JSONbjq`）；
/// - 大小写原样带出，过滤层统一转小写。
pub(crate) fn variants(text: &str) -> Option<(String, String)> {
    let mut full = String::with_capacity(text.len() * 2);
    let mut initials = String::with_capacity(text.len());
    let mut has_hanzi = false;
    for ch in text.chars() {
        match ch.to_pinyin() {
            Some(py) => {
                has_hanzi = true;
                full.push_str(py.plain());
                initials.push_str(py.first_letter());
            }
            None => {
                full.push(ch);
                initials.push(ch);
            }
        }
    }
    if has_hanzi {
        Some((full, initials))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hanzi_yield_full_spelling_and_initials() {
        assert_eq!(variants("微信"), Some(("weixin".into(), "wx".into())));
        assert_eq!(
            variants("企业微信"),
            Some(("qiyeweixin".into(), "qywx".into()))
        );
        assert_eq!(variants("拼音"), Some(("pinyin".into(), "py".into())));
    }

    #[test]
    fn non_han_chars_pass_through() {
        // 混排：汉字转换，ASCII/数字/符号原样保留。
        assert_eq!(
            variants("JSON编辑器"),
            Some(("JSONbianjiqi".into(), "JSONbjq".into()))
        );
        assert_eq!(
            variants("v1.0 发布"),
            Some(("v1.0 fabu".into(), "v1.0 fb".into()))
        );
    }

    #[test]
    fn pure_ascii_yields_none() {
        assert_eq!(variants("wechat"), None);
        assert_eq!(variants("v1.0"), None);
        assert_eq!(variants(""), None);
        assert_eq!(variants("　"), None);
    }
}
