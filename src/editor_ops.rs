//! Pure text transforms behind the editor's editing shortcuts.
//!
//! Each op maps (current text, byte selection) to a single range replacement
//! plus the selection to restore afterwards, so the gpui layer only has to
//! spell out `set_selected_range` + `replace`. Byte offsets throughout, and all
//! line helpers are built on [`RopeExt`].
//!
//! Note: `RopeExt::line_end_offset` ends a line *before* its `\n` (despite the
//! doc comment, which claims otherwise), and `slice_line` keeps a trailing
//! `\r`. [`line_full_end`] is the one that includes the terminator.

use std::ops::Range;
use std::ops::RangeInclusive;

use gpui_kit::component::input::{Rope, RopeExt};

/// One replacement to feed `EditorState::set_selected_range` + `replace`.
pub struct Edit {
    /// Byte range in the current text to overwrite.
    pub range: Range<usize>,
    /// Replacement text.
    pub text: String,
    /// Byte range to select once the edit is applied.
    pub selection: Range<usize>,
}

// ── Line helpers ───────────────────────────────────────────────────────────

/// Byte range of a line's content, without its terminator (a trailing `\r` is
/// kept, since that belongs to the line rather than to the newline).
fn line_content_range(rope: &Rope, row: usize) -> Range<usize> {
    rope.line_start_offset(row)..rope.line_end_offset(row)
}

/// Byte offset just past a line's terminator: the next line's start, or the end
/// of the text for the last line.
fn line_full_end(rope: &Rope, row: usize) -> usize {
    if row + 1 < rope.lines_len() {
        rope.line_start_offset(row + 1)
    } else {
        rope.len()
    }
}

/// Whether the line is followed by a `\n`. False only for an unterminated last
/// line.
fn line_has_terminator(rope: &Rope, row: usize) -> bool {
    line_full_end(rope, row) > rope.line_end_offset(row)
}

/// The terminator text to use for the final line of a replacement block, so the
/// text's trailing-newline state survives the edit.
fn final_terminator(rope: &Rope, row: usize) -> &'static str {
    if line_has_terminator(rope, row) { "\n" } else { "" }
}

/// Content (no `\n`, `\r` kept) of the rows `first..=last`.
fn line_contents(rope: &Rope, first: usize, last: usize) -> Vec<String> {
    (first..=last).map(|row| rope.slice_line(row).to_string()).collect()
}

/// Join lines back up, separating them with `\n` and using `final_term` for the
/// last one.
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

/// Rows covered by a byte selection. A selection ending exactly at a line start
/// does not include that line (the usual "whole line" rule).
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

// ── Comment toggling ───────────────────────────────────────────────────────

/// Insert `// ` after the line's leading whitespace.
fn comment_line(line: &str) -> String {
    let indent = line.len() - line.trim_start().len();
    format!("{}// {}", &line[..indent], &line[indent..])
}

/// Remove one `//` and the single space that follows it.
fn uncomment_line(line: &str) -> String {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    format!("{}{}", &line[..indent], rest)
}

/// `Cmd + /` — comment out the selected lines, or uncomment them when every
/// non-blank line already starts with `//`. Blank lines are left alone.
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
    if text == rope.slice(block.clone()).to_string() {
        return None;
    }
    // Select the rewritten block, which may be longer than the original.
    let selection = block.start..block.start + text.len();
    Some(Edit {
        range: block,
        text,
        selection,
    })
}

/// `Cmd + Option + /` — wrap the selection (or the current line) in `/* */`, or
/// strip the delimiters when it is already wrapped.
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
        // Strip the delimiters plus one space of padding each, so both
        // `/*code*/` and `/* code */` come back as `code`.
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

// ── Whole-line edits ───────────────────────────────────────────────────────

/// `Cmd + Shift + K` — delete the selected lines, terminator included.
pub fn delete_lines(rope: &Rope, selection: Range<usize>) -> Option<Edit> {
    let rows = selected_rows(rope, &selection);
    let (first, last) = (*rows.start(), *rows.end());

    let mut range = rope.line_start_offset(first)..line_full_end(rope, last);
    // The last line of the text has no terminator: also swallow the newline
    // separating it from the line above, so no blank line is left behind.
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

/// `Option + Shift + Up/Down` — copy the selected lines above (`up`) or below
/// the original. Both directions select the fresh copy so the shortcut can be
/// pressed repeatedly.
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

/// `Option + Up/Down` — swap the selected lines with the adjacent line above
/// (`up`) or below. `None` when there is no line to swap with.
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
        // The block keeps the trailing-newline state of the region it leaves.
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

    /// Apply an edit the way the panel does, and return the resulting text.
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
        // The whole rewritten block stays selected, so the shortcut can be
        // pressed again right away.
        assert_eq!(edit.selection, 0..15);

        let edit = toggle_line_comment(&rope(&out), edit.selection.clone()).unwrap();
        assert_eq!(applied(&rope(&out), &edit), text);
        assert_eq!(edit.selection, 0..6);
    }

    #[test]
    fn toggle_comment_skips_blank_lines() {
        let text = "a\n\n  \nb\n";
        // Select the whole text so all four rows are covered.
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
        // Selecting "a\n" (0..2) must not pull in the following "b" line.
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
        // The text does not end with a newline, so the duplicate must not add
        // one either.
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
        // A trailing empty line is not a line to swap with either.
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
