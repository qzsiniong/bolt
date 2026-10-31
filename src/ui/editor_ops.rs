//! 编辑器快捷键背后的纯文本变换。
//!
//! 每个操作把「当前文本 + 字节选区」映射成一次区间替换，外加替换后要恢复的
//! 选区，gpui 那层只需照做 `set_selected_range` + `replace`。全程用字节偏移，
//! 行级辅助函数都建在 [`RopeExt`] 上。
//!
//! 注意：`RopeExt::line_end_offset` 实际把行尾收在 `\n` **之前**（尽管它自己的
//! 注释说的不是这样），而 `slice_line` 会保留行尾的 `\r`。真正把终止符算进
//! 行内的是 [`line_full_end`]。

use std::ops::Range;
use std::ops::RangeInclusive;

use gpui_kit::component::input::{Rope, RopeExt};

/// 一次替换：喂给 `EditorState::set_selected_range` + `replace`。
pub struct Edit {
    /// 当前文本里要被覆盖的字节区间。
    pub range: Range<usize>,
    /// 替换进去的文本。
    pub text: String,
    /// 编辑应用后要选中的字节区间。
    pub selection: Range<usize>,
}

// ── 行辅助函数 ─────────────────────────────────────────────────────────────

/// 一行正文的字节区间，不含行终止符（行尾的 `\r` 保留：它属于这一行，而不是
/// 属于换行）。
fn line_content_range(rope: &Rope, row: usize) -> Range<usize> {
    rope.line_start_offset(row)..rope.line_end_offset(row)
}

/// 一行终止符之后的字节偏移：下一行的开头，最后一行则是文本末尾。
fn line_full_end(rope: &Rope, row: usize) -> usize {
    if row + 1 < rope.lines_len() {
        rope.line_start_offset(row + 1)
    } else {
        rope.len()
    }
}

/// 该行后面是否跟着 `\n`。只有末尾没有换行的最后一行才是 false。
fn line_has_terminator(rope: &Rope, row: usize) -> bool {
    line_full_end(rope, row) > rope.line_end_offset(row)
}

/// 替换块最后一行该用的终止符，让文本「末尾有没有换行」的状态在编辑后保持不变。
fn final_terminator(rope: &Rope, row: usize) -> &'static str {
    if line_has_terminator(rope, row) {
        "\n"
    } else {
        ""
    }
}

/// `first..=last` 这几行的正文（不含 `\n`，保留 `\r`）。
fn line_contents(rope: &Rope, first: usize, last: usize) -> Vec<String> {
    (first..=last)
        .map(|row| rope.slice_line(row).to_string())
        .collect()
}

/// 把行拼回去：行间用 `\n` 分隔，最后一行用 `final_term`。
fn build_block(contents: &[String], final_term: &str) -> String {
    let mut out = String::new();
    for (i, content) in contents.iter().enumerate() {
        out.push_str(content);
        if i + 1 < contents.len() {
            out.push('\n');
        } else {
            out.push_str(final_term);
        }
    }
    out
}

/// 字节选区覆盖到的行。恰好结束在行首的选区不含那一行（常见的「整行」规则）。
fn selected_rows(rope: &Rope, selection: &Range<usize>) -> RangeInclusive<usize> {
    let first = rope.offset_to_position(selection.start).line as usize;
    if selection.end <= selection.start {
        return first..=first;
    }
    let last = rope.offset_to_position(selection.end).line as usize;
    let last = if last > first && rope.line_start_offset(last) == selection.end {
        last - 1
    } else {
        last
    };
    first..=last
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

// ── 注释开关 ───────────────────────────────────────────────────────────────

/// 在行首缩进之后插入 `// `。
fn comment_line(line: &str) -> String {
    let indent = line.len() - line.trim_start().len();
    format!("{}// {}", &line[..indent], &line[indent..])
}

/// 去掉一个 `//` 及紧随其后的一个空格。
fn uncomment_line(line: &str) -> String {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    format!("{}{}", &line[..indent], rest)
}

/// `Cmd + /` —— 注释掉选中的行；若每个非空行都已以 `//` 开头，则反向取消注释。
/// 空行原样保留。
pub fn toggle_line_comment(rope: &Rope, selection: Range<usize>) -> Option<Edit> {
    let rows = selected_rows(rope, &selection);
    let (first, last) = (*rows.start(), *rows.end());

    let contents = line_contents(rope, first, last);
    let uncomment = contents.iter().any(|line| !is_blank(line))
        && contents
            .iter()
            .filter(|line| !is_blank(line))
            .all(|line| line.trim_start().starts_with("//"));

    let transformed: Vec<String> = contents
        .iter()
        .map(|line| {
            if is_blank(line) {
                line.clone()
            } else if uncomment {
                uncomment_line(line)
            } else {
                comment_line(line)
            }
        })
        .collect();

    let block = rope.line_start_offset(first)..line_full_end(rope, last);
    let text = build_block(&transformed, final_terminator(rope, last));
    if text == rope.slice(block.clone()) {
        return None;
    }
    // 选中改写后的整块，它可能比原来更长。
    let selection = block.start..block.start + text.len();
    Some(Edit {
        range: block,
        text,
        selection,
    })
}

/// `Cmd + Option + /` —— 把选区（无选区时取当前行）包进 `/* */`，已经包住时
/// 则剥掉定界符。
pub fn toggle_block_comment(rope: &Rope, selection: Range<usize>) -> Option<Edit> {
    let range = if selection.is_empty() {
        let row = rope.offset_to_position(selection.start).line as usize;
        let mut range = line_content_range(rope, row);
        if range.end > range.start && rope.char_at(range.end - 1) == Some('\r') {
            range.end -= 1;
        }
        range
    } else {
        selection.clone()
    };

    let body = rope.slice(range.clone()).to_string();
    if body.trim().is_empty() {
        return None;
    }

    let open = body.len() - body.trim_start().len();
    let wrapped = body[open..].starts_with("/*")
        && match body.rfind("*/") {
            Some(close) => close >= open + 2,
            None => false,
        };
    if wrapped {
        let close = body.rfind("*/").unwrap();
        // 剥掉定界符，并各带走一个空格的内边距，这样 `/*code*/` 和
        // `/* code */` 都能还原成 `code`。
        let mut inner_start = open + 2;
        if body[inner_start..].starts_with(' ') {
            inner_start += 1;
        }
        let mut inner_end = close;
        if body[..inner_end].ends_with(' ') {
            inner_end -= 1;
        }
        let text = format!(
            "{}{}{}",
            &body[..open],
            &body[inner_start..inner_end],
            &body[close + 2..]
        );
        let selection = range.start..range.start + text.len();
        return Some(Edit {
            range,
            text,
            selection,
        });
    }

    let text = format!("/*{body}*/");
    let selection = range.start..range.start + text.len();
    Some(Edit {
        range,
        text,
        selection,
    })
}

// ── 整行编辑 ───────────────────────────────────────────────────────────────

/// `Cmd + Shift + K` —— 删除选中的行，连同行终止符。
pub fn delete_lines(rope: &Rope, selection: Range<usize>) -> Option<Edit> {
    let rows = selected_rows(rope, &selection);
    let (first, last) = (*rows.start(), *rows.end());

    let mut range = rope.line_start_offset(first)..line_full_end(rope, last);
    // 文本最后一行没有终止符：把与上一行之间的换行一起吞掉，免得留下空行。
    if range.end == rope.len() && !line_has_terminator(rope, last) && first > 0 {
        range.start = rope.line_end_offset(first - 1);
    }
    if range.is_empty() {
        return None;
    }
    Some(Edit {
        selection: range.start..range.start,
        range,
        text: String::new(),
    })
}

/// `Option + Shift + Up/Down` —— 把选中的行复制到原位置之上（`up`）或之下。
/// 两个方向都选中新复制的那份，方便连按。
pub fn duplicate_lines(rope: &Rope, selection: Range<usize>, up: bool) -> Option<Edit> {
    let rows = selected_rows(rope, &selection);
    let (first, last) = (*rows.start(), *rows.end());

    let contents = line_contents(rope, first, last);
    let copy = build_block(&contents, "\n");
    let original = build_block(&contents, final_terminator(rope, last));
    let text = format!("{copy}{original}");

    let block = rope.line_start_offset(first)..line_full_end(rope, last);
    let selection = if up {
        block.start..block.start + copy.len()
    } else {
        let start = block.start + copy.len();
        start..start + original.len()
    };
    Some(Edit {
        range: block,
        text,
        selection,
    })
}

/// `Option + Up/Down` —— 把选中的行与相邻的上一行（`up`）或下一行交换。
/// 没有可交换的行时返回 `None`。
pub fn move_lines(rope: &Rope, selection: Range<usize>, up: bool) -> Option<Edit> {
    let rows = selected_rows(rope, &selection);
    let (first, last) = (*rows.start(), *rows.end());

    if up {
        if first == 0 {
            return None;
        }
        let prev = first - 1;
        let mut contents = line_contents(rope, first, last);
        contents.push(rope.slice_line(prev).to_string());
        // 整块保持它离开的那片区域「末尾有无换行」的状态。
        let moved = build_block(&contents, final_terminator(rope, last));

        let prev_start = rope.line_start_offset(prev);
        let shift = line_full_end(rope, prev) - prev_start;
        let range = prev_start..line_full_end(rope, last);
        let selection = (selection.start - shift)..(selection.end - shift);
        Some(Edit {
            range,
            text: moved,
            selection,
        })
    } else {
        let next = last + 1;
        if next >= rope.lines_len() || !line_has_terminator(rope, next) {
            return None;
        }
        let mut contents = vec![rope.slice_line(next).to_string()];
        contents.extend(line_contents(rope, first, last));
        let moved = build_block(&contents, final_terminator(rope, next));

        let next_start = rope.line_start_offset(next);
        let shift = line_full_end(rope, next) - next_start;
        let range = rope.line_start_offset(first)..line_full_end(rope, next);
        let selection = (selection.start + shift)..(selection.end + shift);
        Some(Edit {
            range,
            text: moved,
            selection,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按面板的方式应用一次编辑，返回结果文本。
    fn applied(rope: &Rope, edit: &Edit) -> String {
        let mut out = rope.slice(0..edit.range.start).to_string();
        out.push_str(&edit.text);
        out.push_str(&rope.slice(edit.range.end..rope.len()).to_string());
        out
    }

    fn rope(text: &str) -> Rope {
        Rope::from(text)
    }

    #[test]
    fn toggle_comment_single_line() {
        let text = "const a = 1;\n";
        let edit = toggle_line_comment(&rope(text), 0..0).unwrap();
        let out = applied(&rope(text), &edit);
        assert_eq!(out, "// const a = 1;\n");

        let edit = toggle_line_comment(&rope(&out), 0..0).unwrap();
        assert_eq!(applied(&rope(&out), &edit), text);
    }

    #[test]
    fn toggle_comment_keeps_indent() {
        let text = "  const a = 1;\n";
        let edit = toggle_line_comment(&rope(text), 0..0).unwrap();
        assert_eq!(applied(&rope(text), &edit), "  // const a = 1;\n");
    }

    #[test]
    fn toggle_comment_block_and_back() {
        let text = "a\nb\nc\n";
        let edit = toggle_line_comment(&rope(text), 0..6).unwrap();
        let out = applied(&rope(text), &edit);
        assert_eq!(out, "// a\n// b\n// c\n");
        // 改写后的整块保持选中，方便立刻再按一次快捷键。
        assert_eq!(edit.selection, 0..15);

        let edit = toggle_line_comment(&rope(&out), edit.selection.clone()).unwrap();
        assert_eq!(applied(&rope(&out), &edit), text);
        assert_eq!(edit.selection, 0..6);
    }

    #[test]
    fn toggle_comment_skips_blank_lines() {
        let text = "a\n\n  \nb\n";
        // 选中整段文本，四行都覆盖到。
        let edit = toggle_line_comment(&rope(text), 0..text.len()).unwrap();
        assert_eq!(applied(&rope(text), &edit), "// a\n\n  \n// b\n");
    }

    #[test]
    fn toggle_comment_crlf() {
        let text = "a\r\n";
        let edit = toggle_line_comment(&rope(text), 0..0).unwrap();
        assert_eq!(applied(&rope(text), &edit), "// a\r\n");
    }

    #[test]
    fn selection_ending_at_column_zero_excludes_that_line() {
        // 选中 "a\n"（0..2）不能顺带把后面那行 "b" 卷进来。
        let text = "a\nb\n";
        let edit = toggle_line_comment(&rope(text), 0..2).unwrap();
        assert_eq!(applied(&rope(text), &edit), "// a\nb\n");
    }

    #[test]
    fn delete_middle_line() {
        let text = "a\nb\nc\n";
        let edit = delete_lines(&rope(text), 2..2).unwrap();
        assert_eq!(applied(&rope(text), &edit), "a\nc\n");
        assert_eq!(edit.selection, 2..2);
    }

    #[test]
    fn delete_last_line_without_newline() {
        let text = "a\nb";
        let edit = delete_lines(&rope(text), 2..2).unwrap();
        assert_eq!(applied(&rope(text), &edit), "a");
    }

    #[test]
    fn delete_last_line_with_newline() {
        let text = "a\nb\n";
        let edit = delete_lines(&rope(text), 2..2).unwrap();
        assert_eq!(applied(&rope(text), &edit), "a\n");
    }

    #[test]
    fn delete_selected_block() {
        let text = "a\nb\nc\nd\n";
        let edit = delete_lines(&rope(text), 2..6).unwrap();
        assert_eq!(applied(&rope(text), &edit), "a\nd\n");
    }

    #[test]
    fn duplicate_line_down() {
        let text = "a\n";
        let edit = duplicate_lines(&rope(text), 0..0, false).unwrap();
        assert_eq!(applied(&rope(text), &edit), "a\na\n");
        assert_eq!(edit.selection, 2..4);
    }

    #[test]
    fn duplicate_line_up() {
        let text = "a\n";
        let edit = duplicate_lines(&rope(text), 0..0, true).unwrap();
        assert_eq!(applied(&rope(text), &edit), "a\na\n");
        assert_eq!(edit.selection, 0..2);
    }

    #[test]
    fn duplicate_unterminated_last_line() {
        // 文本不以换行结尾，复制出来的那份也不能加。
        let text = "a\nb";
        let edit = duplicate_lines(&rope(text), 2..2, false).unwrap();
        assert_eq!(applied(&rope(text), &edit), "a\nb\nb");
    }

    #[test]
    fn move_line_up_at_top_is_noop() {
        assert!(move_lines(&rope("a\nb\n"), 0..0, true).is_none());
    }

    #[test]
    fn move_line_down_at_bottom_is_noop() {
        assert!(move_lines(&rope("a\nb"), 2..2, false).is_none());
        // 末尾的空行同样不算可交换的行。
        assert!(move_lines(&rope("a\nb\n"), 2..2, false).is_none());
    }

    #[test]
    fn move_line_down_swaps_with_next() {
        let text = "a\nb\n";
        let edit = move_lines(&rope(text), 0..0, false).unwrap();
        assert_eq!(applied(&rope(text), &edit), "b\na\n");
        assert_eq!(edit.selection, 2..2);
    }

    #[test]
    fn move_line_up_swaps_with_previous() {
        let text = "a\nb\n";
        let edit = move_lines(&rope(text), 2..2, true).unwrap();
        assert_eq!(applied(&rope(text), &edit), "b\na\n");
        assert_eq!(edit.selection, 0..0);
    }

    #[test]
    fn block_comment_wrap_and_unwrap() {
        let text = "code";
        let edit = toggle_block_comment(&rope(text), 0..4).unwrap();
        let out = applied(&rope(text), &edit);
        assert_eq!(out, "/*code*/");
        assert_eq!(edit.selection, 0..8);

        let edit = toggle_block_comment(&rope(&out), 0..8).unwrap();
        assert_eq!(applied(&rope(&out), &edit), "code");
    }

    #[test]
    fn block_comment_unwrap_strips_padding() {
        let text = "/* code */";
        let edit = toggle_block_comment(&rope(text), 0..10).unwrap();
        assert_eq!(applied(&rope(text), &edit), "code");
    }

    #[test]
    fn block_comment_uses_current_line_when_selection_is_empty() {
        let text = "let a = 1;\nlet b = 2;\n";
        let edit = toggle_block_comment(&rope(text), 0..0).unwrap();
        assert_eq!(applied(&rope(text), &edit), "/*let a = 1;*/\nlet b = 2;\n");
    }

    #[test]
    fn block_comment_noop_on_blank_line() {
        assert!(toggle_block_comment(&rope("   \n"), 0..0).is_none());
    }
}
