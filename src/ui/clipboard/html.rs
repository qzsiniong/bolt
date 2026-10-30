//! 剪切板 HTML → 纯文本。
//!
//! GPUI 没有 HTML 渲染能力，这里只做「保留结构」的提取：
//! - 丢弃 `<script>` / `<style>` 的内容；
//! - 块级标签（p/div/h1-6/li/br/tr/pre/blockquote）转成换行，`<li>` 加 `• `；
//! - `<pre>` 内保留原始空白，其余位置折叠连续空白；
//! - 解码常见实体（`&amp;` `&nbsp;` `&#39;` `&#x27;` 等）；
//! - 行尾空白与多余空行清理掉。

/// 剪切板 HTML → 纯文本（见模块文档的规则清单）。
pub(super) fn html_to_text(html: &str) -> String {
    let mut raw = String::with_capacity(html.len());
    let mut in_pre = false;
    let mut in_skip = false;
    let mut tag = String::new();
    let mut chars = html.chars();

    while let Some(c) = chars.next() {
        if c != '<' {
            if in_skip {
                continue;
            }
            if in_pre {
                raw.push(c);
            } else if c.is_whitespace() {
                // 折叠空白，但不要把行首的换行前面再塞空格。
                if !raw.ends_with(' ') && !raw.ends_with('\n') && !raw.is_empty() {
                    raw.push(' ');
                }
            } else {
                raw.push(c);
            }
            continue;
        }

        tag.clear();
        let mut closed = false;
        for c2 in chars.by_ref() {
            if c2 == '>' {
                closed = true;
                break;
            }
            tag.push(c2);
        }
        if !closed {
            break;
        }

        let is_closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .trim()
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        match name.as_str() {
            "script" | "style" => in_skip = !is_closing,
            "pre" => in_pre = !is_closing,
            "br" => raw.push('\n'),
            "li" => {
                trim_trailing_spaces(&mut raw);
                if !raw.is_empty() && !raw.ends_with('\n') {
                    raw.push('\n');
                }
                if !is_closing {
                    raw.push_str("• ");
                }
            }
            "p" | "div" | "section" | "article" | "blockquote" | "tr" | "table" | "h1" | "h2"
            | "h3" | "h4" | "h5" | "h6" => {
                trim_trailing_spaces(&mut raw);
                if !raw.is_empty() && !raw.ends_with('\n') {
                    raw.push('\n');
                }
            }
            "td" | "th" => raw.push(' '),
            _ => {}
        }
    }

    let decoded = decode_entities(&raw);
    let mut out = String::with_capacity(decoded.len());
    let mut blank_lines = 0;
    for line in decoded.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank_lines += 1;
            if blank_lines > 1 {
                continue;
            }
        } else {
            blank_lines = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

fn trim_trailing_spaces(text: &mut String) {
    while text.ends_with(' ') {
        text.pop();
    }
}

/// 解码 HTML 实体：命名的常见几个 + `&#NN;` / `&#xNN;`。
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let Some(end) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity.strip_prefix('#').and_then(|num| {
                let code = match num.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num.parse::<u32>().ok(),
                };
                code.and_then(char::from_u32)
            }),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_becomes_readable_text() {
        // 浏览器复制的富文本：段落/列表/换行结构要保留，脚本与样式要丢掉。
        let html = r#"<html><head><style>p { color: red; }</style></head><body>
    <h1>标题</h1><p>第一段 &amp; 第二段<br>换行</p>
    <script>console.log(1)</script>
    <ul><li>甲</li><li>乙</li></ul>
    <pre>  a
  b</pre></body></html>"#;
        let text = html_to_text(html);
        assert!(text.starts_with("标题"), "{text:?}");
        assert!(text.contains("第一段 & 第二段"), "{text:?}");
        assert!(text.contains("换行"), "{text:?}");
        assert!(!text.contains("color: red"), "{text:?}");
        assert!(!text.contains("console"), "{text:?}");
        assert!(text.contains("• 甲"), "{text:?}");
        assert!(text.contains("• 乙"), "{text:?}");
        // `<pre>` 内的原始空白与换行要保留。
        assert!(text.contains("  a\n  b"), "{text:?}");
    }

    #[test]
    fn html_entities_decode() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt;"), "a & b <c>");
        assert_eq!(decode_entities("A&nbsp;B"), "A B");
        assert_eq!(decode_entities("&#39;引号&#x4e2d;"), "'引号中");
        assert_eq!(decode_entities("100 & 200"), "100 & 200");
    }
}
