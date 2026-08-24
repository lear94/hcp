//! Scrollable text viewer for large response bodies.
//!
//! The naive approach — handing a multi-megabyte `String` to a wrapping
//! `Paragraph` every frame — re-wraps the entire document on every keystroke.
//! This viewer instead lays the text out **once** per (width, wrap) pair into
//! byte ranges, then renders only the rows that are actually on screen, so
//! scrolling a 32 MB payload costs the same as scrolling an empty one.

use std::sync::Arc;
use unicode_width::UnicodeWidthChar;

/// Byte range of one display row inside the buffer.
type Row = (u32, u32);

#[derive(Default)]
pub struct TextViewer {
    text: Arc<str>,
    rows: Vec<Row>,
    /// Widest row of the current layout, in columns. Recomputing this per frame
    /// would make every repaint O(payload), which is exactly what this viewer
    /// exists to avoid.
    widest_row: usize,
    laid_out_width: u16,
    laid_out_wrap: bool,
    layout_valid: bool,
    /// First visible display row.
    pub scroll: usize,
    /// Horizontal offset in columns, used only when wrapping is off.
    pub h_scroll: usize,
    pub query: String,
    /// Byte ranges of the current search hits, in document order. Ranges are
    /// resolved at search time so rendering never has to re-derive the length
    /// of a match that started on an earlier wrapped row.
    pub matches: Vec<(usize, usize)>,
    pub current_match: usize,
}

impl TextViewer {
    pub fn set_text(&mut self, text: impl Into<Arc<str>>) {
        self.text = normalize(text.into());
        self.layout_valid = false;
        self.scroll = 0;
        self.h_scroll = 0;
        self.rerun_search();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.set_text("");
        self.query.clear();
        self.matches.clear();
    }

    /// Rebuilds the row index when the viewport geometry or wrap mode changed.
    pub fn ensure_layout(&mut self, width: u16, wrap: bool) {
        if self.layout_valid && self.laid_out_width == width && self.laid_out_wrap == wrap {
            return;
        }
        let cols = width.max(1) as usize;
        let mut rows = std::mem::take(&mut self.rows);
        rows.clear();

        let text = &self.text;
        let bytes = text.as_bytes();
        let mut line_start = 0usize;
        let mut idx = 0usize;
        while idx <= bytes.len() {
            let at_end = idx == bytes.len();
            if at_end || bytes[idx] == b'\n' {
                push_line(&mut rows, text, line_start, idx, cols, wrap);
                line_start = idx + 1;
                if at_end {
                    break;
                }
            }
            idx += 1;
        }
        if rows.is_empty() {
            rows.push((0, 0));
        }

        self.widest_row = rows
            .iter()
            .map(|&(a, b)| display_width(&self.text[a as usize..b as usize]))
            .max()
            .unwrap_or(0);
        self.rows = rows;
        self.laid_out_width = width;
        self.laid_out_wrap = wrap;
        self.layout_valid = true;
        // A narrower window can leave the old scroll position past the end.
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    pub fn total_rows(&self) -> usize {
        self.rows.len()
    }

    /// Widest row in columns — drives horizontal scrolling when wrap is off.
    /// Computed with the layout, so reading it is free.
    pub fn max_row_width(&self) -> usize {
        self.widest_row
    }

    pub fn row(&self, index: usize) -> Option<&str> {
        self.rows
            .get(index)
            .map(|&(a, b)| &self.text[a as usize..b as usize])
    }

    /// Byte offset where a display row begins — needed to map search hits.
    pub fn row_start(&self, index: usize) -> usize {
        self.rows.get(index).map(|&(a, _)| a as usize).unwrap_or(0)
    }

    pub fn max_scroll(&self, viewport_rows: usize) -> usize {
        self.rows.len().saturating_sub(viewport_rows.max(1))
    }

    pub fn scroll_by(&mut self, delta: isize, viewport_rows: usize) {
        let max = self.max_scroll(viewport_rows) as isize;
        let next = (self.scroll as isize + delta).clamp(0, max.max(0));
        self.scroll = next as usize;
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_to_bottom(&mut self, viewport_rows: usize) {
        self.scroll = self.max_scroll(viewport_rows);
    }

    pub fn scroll_h(&mut self, delta: isize) {
        let next = self.h_scroll as isize + delta;
        self.h_scroll = next.max(0) as usize;
    }

    // ---- search -------------------------------------------------------

    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.rerun_search();
    }

    fn rerun_search(&mut self) {
        self.matches.clear();
        self.current_match = 0;
        if self.query.is_empty() {
            return;
        }
        // Folding a whole copy of the buffer would both double peak memory and
        // shift byte offsets (case folding is not length-preserving in
        // Unicode), so the scan folds one character at a time against the
        // original text and reports offsets that are always valid slices.
        let needle: Vec<char> = self.query.chars().map(fold_char).collect();
        let Some(&first) = needle.first() else { return };

        for (idx, ch) in self.text.char_indices() {
            if fold_char(ch) != first {
                continue;
            }
            if let Some(len) = match_len_at(&self.text[idx..], &needle) {
                self.matches.push((idx, idx + len));
                if self.matches.len() >= MAX_MATCHES {
                    break; // Enough to navigate; keeps huge bodies responsive.
                }
            }
        }
    }

    /// Moves to the next/previous hit and returns the row it lives on.
    pub fn jump_match(&mut self, forward: bool) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let len = self.matches.len();
        self.current_match = if forward {
            (self.current_match + 1) % len
        } else {
            (self.current_match + len - 1) % len
        };
        self.row_of_offset(self.matches[self.current_match].0)
    }

    pub fn first_match_row(&self) -> Option<usize> {
        self.matches
            .first()
            .and_then(|&(start, _)| self.row_of_offset(start))
    }

    pub fn row_of_offset(&self, offset: usize) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        let idx = self
            .rows
            .partition_point(|&(start, _)| (start as usize) <= offset);
        Some(idx.saturating_sub(1))
    }

    /// Centres the viewport on a row, so a search hit is never glued to an edge.
    pub fn center_on(&mut self, row: usize, viewport_rows: usize) {
        let half = viewport_rows / 2;
        self.scroll = row.saturating_sub(half).min(self.max_scroll(viewport_rows));
    }
}

/// Splits one logical line into display rows, honouring the wrap width.
fn push_line(rows: &mut Vec<Row>, text: &str, start: usize, end: usize, width: usize, wrap: bool) {
    if !wrap || end == start {
        rows.push((start as u32, end as u32));
        return;
    }
    let slice = &text[start..end];
    let mut seg_start = start;
    let mut used = 0usize;
    for (offset, ch) in slice.char_indices() {
        let w = ch.width().unwrap_or(0);
        let abs = start + offset;
        if used + w > width && abs > seg_start {
            rows.push((seg_start as u32, abs as u32));
            seg_start = abs;
            used = 0;
        }
        used += w;
    }
    rows.push((seg_start as u32, end as u32));
}

/// Upper limit on tracked search hits; navigating more than this is not useful
/// and scanning further would stall the UI on huge payloads.
pub const MAX_MATCHES: usize = 10_000;

/// Single-character case folding. Unlike `str::to_lowercase` this is
/// length-preserving in characters, which keeps search offsets aligned with
/// the original buffer.
fn fold_char(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Byte length of the match at the start of `haystack`, or `None` if the
/// needle does not match there.
fn match_len_at(haystack: &str, needle: &[char]) -> Option<usize> {
    let mut len = 0usize;
    let mut chars = haystack.chars();
    for &n in needle {
        match chars.next() {
            Some(c) if fold_char(c) == n => len += c.len_utf8(),
            _ => return None,
        }
    }
    Some(len)
}

/// Tabs and CRs wreck fixed-width layout; normalising once at ingest keeps
/// every byte offset stable for the lifetime of the buffer.
fn normalize(text: Arc<str>) -> Arc<str> {
    if !text.contains('\t') && !text.contains('\r') {
        return text; // The common case allocates nothing.
    }
    let mut out = String::with_capacity(text.len() + 16);
    for ch in text.chars() {
        match ch {
            '\t' => out.push_str("    "),
            '\r' => {}
            c => out.push(c),
        }
    }
    Arc::from(out)
}

pub fn display_width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Takes the slice of `s` covering columns `[skip, skip + width)` along with
/// its byte offset inside `s`. Scanning stops as soon as the window closes, so
/// the cost is bounded by the viewport rather than by the line length — which
/// matters when a minified payload puts megabytes on a single line.
pub fn column_slice_at(s: &str, skip: usize, width: usize) -> (&str, usize) {
    if skip == 0 && s.len() <= width {
        // Every byte is at most one column wide, so this cannot overflow.
        return (s, 0);
    }
    let mut col = 0usize;
    let mut start: Option<usize> = None;
    let mut end = s.len();
    for (i, ch) in s.char_indices() {
        if start.is_none() && col >= skip {
            start = Some(i);
        }
        if start.is_some() && col >= skip + width {
            end = i;
            break;
        }
        col += ch.width().unwrap_or(0);
    }
    match start {
        Some(start) => (&s[start..end.max(start)], start),
        None => ("", s.len()),
    }
}

pub fn column_slice(s: &str, skip: usize, width: usize) -> &str {
    column_slice_at(s, skip, width).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewer(text: &str) -> TextViewer {
        let mut v = TextViewer::default();
        v.set_text(text);
        v
    }

    #[test]
    fn unwrapped_layout_is_one_row_per_line() {
        let mut v = viewer("alpha\nbeta\ngamma");
        v.ensure_layout(3, false);
        assert_eq!(v.total_rows(), 3);
        assert_eq!(v.row(0), Some("alpha"));
        assert_eq!(v.row(2), Some("gamma"));
    }

    #[test]
    fn wrapping_splits_on_width_and_loses_nothing() {
        let mut v = viewer("abcdefghij");
        v.ensure_layout(4, true);
        assert_eq!(v.total_rows(), 3);
        let joined: String = (0..v.total_rows()).filter_map(|i| v.row(i)).collect();
        assert_eq!(joined, "abcdefghij");
    }

    #[test]
    fn trailing_newline_yields_a_final_empty_row() {
        let mut v = viewer("a\nb\n");
        v.ensure_layout(80, true);
        assert_eq!(v.total_rows(), 3);
        assert_eq!(v.row(2), Some(""));
    }

    #[test]
    fn empty_buffer_still_has_a_row() {
        let mut v = viewer("");
        v.ensure_layout(80, true);
        assert_eq!(v.total_rows(), 1);
        assert_eq!(v.row(0), Some(""));
    }

    #[test]
    fn wide_characters_do_not_overflow_the_viewport() {
        let mut v = viewer("日本語テキスト");
        v.ensure_layout(4, true);
        for i in 0..v.total_rows() {
            assert!(display_width(v.row(i).unwrap()) <= 4);
        }
    }

    #[test]
    fn scroll_is_clamped_to_content() {
        let mut v = viewer("1\n2\n3\n4\n5");
        v.ensure_layout(80, false);
        v.scroll_by(100, 2);
        assert_eq!(v.scroll, 3, "cannot scroll past the last screenful");
        v.scroll_by(-100, 2);
        assert_eq!(v.scroll, 0, "cannot scroll above the first row");
    }

    #[test]
    fn tabs_and_crlf_are_normalized_at_ingest() {
        let v = viewer("a\tb\r\nc");
        assert_eq!(v.text(), "a    b\nc");
    }

    #[test]
    fn search_finds_case_insensitively_and_maps_to_rows() {
        let mut v = viewer("hello\nWORLD\nhello again");
        v.ensure_layout(80, false);
        v.set_query("hello".into());
        assert_eq!(v.matches.len(), 2);
        assert_eq!(v.row_of_offset(v.matches[1].0), Some(2));
        v.set_query("world".into());
        assert_eq!(v.matches.len(), 1);
    }

    #[test]
    fn match_navigation_wraps_around() {
        let mut v = viewer("x\nx\nx");
        v.ensure_layout(80, false);
        v.set_query("x".into());
        assert_eq!(v.jump_match(true), Some(1));
        assert_eq!(v.jump_match(true), Some(2));
        assert_eq!(v.jump_match(true), Some(0));
        assert_eq!(v.jump_match(false), Some(2));
    }

    #[test]
    fn relayout_after_width_change_keeps_scroll_in_range() {
        let mut v = viewer(&"word ".repeat(200));
        v.ensure_layout(10, true);
        v.scroll_to_bottom(5);
        let deep = v.scroll;
        v.ensure_layout(400, true);
        assert!(v.scroll <= v.total_rows().saturating_sub(1));
        assert!(v.scroll < deep.max(1) || v.total_rows() > deep);
    }

    #[test]
    fn search_offsets_stay_valid_with_unicode() {
        // 'İ' lowercases to two chars; a naive to_lowercase() scan would report
        // an offset that is not a char boundary in the original text.
        let mut v = viewer("İstanbul café CAFÉ");
        v.ensure_layout(80, false);
        v.set_query("café".into());
        for &(start, end) in &v.matches {
            assert!(v.text().is_char_boundary(start), "start {start} splits a char");
            assert!(v.text().is_char_boundary(end), "end {end} splits a char");
            assert_eq!(v.text()[start..end].to_lowercase(), "café");
        }
        assert_eq!(v.matches.len(), 2, "case-insensitive match on accented text");
    }

    #[test]
    fn the_widest_row_is_cached_with_the_layout() {
        let mut v = viewer("ab\nabcdefgh\nabc");
        v.ensure_layout(80, false);
        assert_eq!(v.max_row_width(), 8);
        // Wrapping caps every row at the viewport width.
        v.ensure_layout(4, true);
        assert_eq!(v.max_row_width(), 4);
    }

    #[test]
    fn setting_text_shares_the_buffer_instead_of_copying_it() {
        let shared: std::sync::Arc<str> = std::sync::Arc::from("payload");
        let mut v = TextViewer::default();
        v.set_text(shared.clone());
        assert_eq!(std::sync::Arc::strong_count(&shared), 2, "buffer was copied");
    }

    #[test]
    fn match_ranges_cover_multibyte_hits() {
        let mut v = viewer("naïve");
        v.ensure_layout(80, false);
        v.set_query("ïv".into());
        let (start, end) = v.matches[0];
        assert_eq!(&v.text()[start..end], "ïv", "range must span whole chars");
    }

    #[test]
    fn column_slice_windows_correctly() {
        assert_eq!(column_slice("abcdef", 0, 3), "abc");
        assert_eq!(column_slice("abcdef", 2, 3), "cde");
        assert_eq!(column_slice("abc", 10, 3), "");
        assert_eq!(column_slice("abc", 0, 10), "abc");
        assert_eq!(column_slice_at("abcdef", 2, 3), ("cde", 2));
    }

    #[test]
    fn column_slice_respects_wide_characters() {
        // Each CJK glyph is two columns wide.
        assert_eq!(column_slice("日本語", 0, 4), "日本");
        assert_eq!(column_slice_at("日本語", 2, 2), ("本", 3));
    }

    #[test]
    fn column_slice_offset_is_always_a_char_boundary() {
        let text = "héllo wörld";
        for skip in 0..12 {
            let (slice, off) = column_slice_at(text, skip, 4);
            assert!(text.is_char_boundary(off));
            assert!(text[off..].starts_with(slice));
        }
    }
}
