//! Syntax highlighting of side-by-side rows.
use crate::model::LineKind;
use crate::sbs::{Cell, GapCtl, Row};
use syntect::easy::HighlightLines;
use syntect::highlighting::{HighlightState, Theme, ThemeSet};
use syntect::parsing::{ParseState, SyntaxReference, SyntaxSet};

pub struct Highlighter {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StyledSeg {
    pub text: String,
    pub rgb: (u8, u8, u8),
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StyledCell {
    pub no: u32,
    pub kind: LineKind,
    pub segs: Vec<StyledSeg>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StyledRow {
    HunkHeader { hunk_index: usize, header: String, gap: Option<GapCtl> },
    Line { left: Option<StyledCell>, right: Option<StyledCell> },
    Gap(GapCtl),
}

impl Highlighter {
    /// Loads syntaxes and themes. Slow: construct once per app.
    ///
    /// Syntaxes are bat's extended set (via two-face): syntect's defaults plus
    /// TypeScript, TSX, Vue, Svelte, TOML, Dockerfile and many more.
    pub fn new() -> Self {
        Highlighter {
            syntaxes: two_face::syntax::extra_newlines(),
            themes: ThemeSet::load_defaults(),
        }
    }

    /// Detects the language of `path`: by file name (Dockerfile, Makefile,
    /// .bashrc), then extension (case-insensitive), then `first_line` (a
    /// shebang, `<?php`, a modeline). Falls back to plain text.
    pub fn syntax_for(&self, path: &str, first_line: Option<&str>) -> &SyntaxReference {
        let ss = &self.syntaxes;
        let p = std::path::Path::new(path);
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        let found = [name, name.trim_start_matches('.'), ext, &ext.to_ascii_lowercase()]
            .into_iter()
            .filter(|t| !t.is_empty())
            .find_map(|t| ss.find_syntax_by_extension(t));
        found
            .or_else(|| first_line.and_then(|l| ss.find_syntax_by_first_line(l)))
            .unwrap_or_else(|| ss.find_syntax_plain_text())
    }

    /// Name of the language detected for `path` ("Plain Text" when unknown).
    pub fn language(&self, path: &str, first_line: Option<&str>) -> &str {
        &self.syntax_for(path, first_line).name
    }

    /// Styles `rows` of `path`. `old_lines` is the old side of the file (empty
    /// when unknown); the parse is primed with the lines above each hunk so a
    /// hunk inside a Vue `<script>` or after PHP's `<?php` is parsed as code,
    /// not as the HTML those syntaxes start in.
    pub fn highlight_rows(&self, path: &str, rows: &[Row], old_lines: &[String], dark: bool) -> Vec<StyledRow> {
        let theme_name = if dark { "base16-ocean.dark" } else { "InspiredGitHub" };
        let theme = &self.themes.themes[theme_name];
        let syntax = self.syntax_for(path, first_line(rows).as_deref());
        let mut primer = Primer::new(syntax, theme);
        let (mut left, mut right) = primer.split(&self.syntaxes, old_lines, first_old_no(rows));
        let mut out = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            match row {
                Row::HunkHeader { hunk_index, header, gap } => {
                    // Lines hidden above: restart the parse. Fully expanded: keep it.
                    if gap.is_some() || *hunk_index == 0 {
                        (left, right) = primer.split(&self.syntaxes, old_lines, first_old_no(&rows[i + 1..]));
                    }
                    out.push(StyledRow::HunkHeader { hunk_index: *hunk_index, header: header.clone(), gap: *gap });
                }
                Row::Gap(g) => out.push(StyledRow::Gap(*g)),
                Row::Line { left: l, right: r } => {
                    let l = l.as_ref().map(|c| self.style_cell(&mut left, c));
                    let r = r.as_ref().map(|c| self.style_cell(&mut right, c));
                    out.push(StyledRow::Line { left: l, right: r });
                }
            }
        }
        out
    }

    fn style_cell(&self, hl: &mut HighlightLines, cell: &Cell) -> StyledCell {
        let text: String = cell.segs.iter().map(|s| s.text.as_str()).collect();
        let mut line = text.clone();
        line.push('\n');
        // On error, fall back to a single unstyled span so state stays usable.
        let spans = hl.highlight_line(&line, &self.syntaxes).unwrap_or_default();

        // Changed ranges as (start, end, changed) over `text`.
        let mut ranges = Vec::with_capacity(cell.segs.len());
        let mut pos = 0;
        for s in &cell.segs {
            ranges.push((pos, pos + s.text.len(), s.changed));
            pos += s.text.len();
        }

        let mut segs: Vec<StyledSeg> = Vec::new();
        let mut off = 0;
        for (style, piece) in spans {
            let (s0, s1) = (off, off + piece.len());
            off = s1;
            let rgb = (style.foreground.r, style.foreground.g, style.foreground.b);
            for &(r0, r1, changed) in &ranges {
                let (a, b) = (s0.max(r0), s1.min(r1));
                if a < b {
                    push_styled(&mut segs, &text[a..b], rgb, changed);
                }
            }
        }
        // Highlighting failed or lost text: keep the text intact.
        let covered: usize = segs.iter().map(|s| s.text.len()).sum();
        if covered != text.len() {
            segs = cell
                .segs
                .iter()
                .filter(|s| !s.text.is_empty())
                .map(|s| StyledSeg { text: s.text.clone(), rgb: (0, 0, 0), changed: s.changed })
                .collect();
        }
        StyledCell { no: cell.no, kind: cell.kind, segs }
    }
}

/// Parses the old side of the file forward, once, to hand out parser states
/// for the line where each hunk starts.
struct Primer<'a> {
    theme: &'a Theme,
    hs: HighlightState,
    ps: ParseState,
    /// Old lines already fed.
    at: usize,
}

impl<'a> Primer<'a> {
    fn new(syntax: &SyntaxReference, theme: &'a Theme) -> Self {
        let (hs, ps) = HighlightLines::new(syntax, theme).state();
        Primer { theme, hs, ps, at: 0 }
    }

    /// Advances to just before old line `no` (1-based; `None` is line 1) and
    /// returns (left, right) highlighters starting from that state. Rows are
    /// in file order, so the primer only moves forward: each line parses once.
    fn split(&mut self, ss: &SyntaxSet, old_lines: &[String], no: Option<u32>) -> (HighlightLines<'a>, HighlightLines<'a>) {
        let target = no.map_or(0, |n| (n as usize).saturating_sub(1)).min(old_lines.len());
        if target > self.at {
            let mut hl = HighlightLines::from_state(self.theme, self.hs.clone(), self.ps.clone());
            for l in &old_lines[self.at..target] {
                let mut line = l.clone();
                line.push('\n');
                // A parse error leaves the state as it was; keep going.
                let _ = hl.highlight_line(&line, ss);
            }
            (self.hs, self.ps) = hl.state();
            self.at = target;
        }
        (
            HighlightLines::from_state(self.theme, self.hs.clone(), self.ps.clone()),
            HighlightLines::from_state(self.theme, self.hs.clone(), self.ps.clone()),
        )
    }
}

/// Old-side number of the first line in `rows` up to the next hunk header,
/// falling back to the new-side number when the hunk only adds lines.
fn first_old_no(rows: &[Row]) -> Option<u32> {
    let mut right = None;
    for r in rows {
        match r {
            Row::HunkHeader { .. } => break,
            Row::Line { left: Some(c), .. } => return Some(c.no),
            Row::Line { right: Some(c), .. } if right.is_none() => right = Some(c.no),
            _ => {}
        }
    }
    right
}

/// Text of line 1 when the rows show it (either side), for first-line detection.
fn first_line(rows: &[Row]) -> Option<String> {
    rows.iter().find_map(|r| match r {
        Row::Line { left, right } => [left, right]
            .into_iter()
            .flatten()
            .find(|c| c.no == 1)
            .map(|c| c.segs.iter().map(|s| s.text.as_str()).collect()),
        _ => None,
    })
}

fn push_styled(segs: &mut Vec<StyledSeg>, text: &str, rgb: (u8, u8, u8), changed: bool) {
    if let Some(last) = segs.last_mut() {
        if last.rgb == rgb && last.changed == changed {
            last.text.push_str(text);
            return;
        }
    }
    segs.push(StyledSeg { text: text.to_string(), rgb, changed });
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DiffLine, Hunk};
    use crate::sbs::build_rows;

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
        DiffLine { kind, old_no: old, new_no: new, text: text.to_string() }
    }

    fn hunk(lines: Vec<DiffLine>) -> Hunk {
        Hunk { header: "@@ -1 +1 @@".into(), old_start: 1, old_lines: 1, new_start: 1, new_lines: 1, lines }
    }

    fn cells(rows: &[StyledRow]) -> Vec<&StyledCell> {
        let mut out = Vec::new();
        for r in rows {
            if let StyledRow::Line { left, right } = r {
                out.extend(left.iter());
                out.extend(right.iter());
            }
        }
        out
    }

    #[test]
    fn rust_keyword_gets_color() {
        let rows = build_rows(&[hunk(vec![line(LineKind::Context, Some(1), Some(1), "fn main() {}")])]);
        let out = Highlighter::new().highlight_rows("x.rs", &rows, &[], true);
        let c = cells(&out)[0];
        let fn_seg = c.segs.iter().find(|s| s.text == "fn").expect("fn seg");
        let main_seg = c.segs.iter().find(|s| s.text == "main").expect("main seg");
        assert_ne!(fn_seg.rgb, main_seg.rgb);
        let joined: String = c.segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "fn main() {}");
    }

    #[test]
    fn detects_languages_by_name_extension_and_first_line() {
        let h = Highlighter::new();
        for (path, lang) in [
            ("src/schema.ts", "TypeScript"),
            ("App.tsx", "TypeScriptReact"),
            ("Cargo.toml", "TOML"),
            ("Dockerfile", "Dockerfile"),
            ("Makefile", "Makefile"),
            ("ui/app.vue", "Vue Component"),
            ("README.MD", "Markdown"),
            ("main.rs", "Rust"),
            ("app/User.php", "PHP"),
        ] {
            assert_eq!(h.language(path, None), lang, "{path}");
        }
        assert_eq!(h.language("bin/deploy", Some("#!/usr/bin/env python3")), "Python");
        assert_eq!(h.language("bin/run", Some("#!/bin/bash")), "Bourne Again Shell (bash)");
        assert_eq!(h.language("notes.zzz", Some("hello")), "Plain Text");
    }

    #[test]
    fn typescript_keyword_gets_color() {
        let rows = build_rows(&[hunk(vec![line(LineKind::Context, Some(1), Some(1), "export const x = 1;")])]);
        let out = Highlighter::new().highlight_rows("schema.ts", &rows, &[], true);
        let c = cells(&out)[0];
        assert!(c.segs.len() > 1, "{:?}", c.segs);
    }

    #[test]
    fn shebang_on_line_one_picks_language() {
        let rows = build_rows(&[hunk(vec![
            line(LineKind::Context, Some(1), Some(1), "#!/usr/bin/env python3"),
            line(LineKind::Context, Some(2), Some(2), "def main(): pass"),
        ])]);
        let out = Highlighter::new().highlight_rows("bin/tool", &rows, &[], true);
        assert!(cells(&out)[2].segs.len() > 1);
    }

    /// A one-line context hunk at `no` in a file whose old side is `file`.
    fn mid_file(file: &[&str], no: u32) -> (Vec<Row>, Vec<String>) {
        let text = file[no as usize - 1];
        let mut h = hunk(vec![line(LineKind::Context, Some(no), Some(no), text)]);
        h.old_start = no;
        h.new_start = no;
        let old: Vec<String> = file.iter().map(|s| s.to_string()).collect();
        (build_rows(&[h]), old)
    }

    #[test]
    fn vue_hunk_inside_script_is_highlighted() {
        let file = ["<template>", "  <div />", "</template>", "<script setup>", "const x = useRole()", "</script>"];
        let (rows, old) = mid_file(&file, 5);
        let out = Highlighter::new().highlight_rows("LoginForm.vue", &rows, &old, true);
        let c = cells(&out)[0];
        assert!(c.segs.len() > 1, "{:?}", c.segs);
    }

    #[test]
    fn php_hunk_after_open_tag_is_highlighted() {
        let file = ["<?php", "", "namespace App;", "", "function run() { return 1; }"];
        let (rows, old) = mid_file(&file, 5);
        let out = Highlighter::new().highlight_rows("app/User.php", &rows, &old, true);
        let c = cells(&out)[0];
        assert!(c.segs.len() > 1, "{:?}", c.segs);
    }

    #[test]
    fn unknown_extension_plain_text() {
        let rows = build_rows(&[hunk(vec![line(LineKind::Context, Some(1), Some(1), "fn main() {}")])]);
        let out = Highlighter::new().highlight_rows("x.zzzunknown", &rows, &[], false);
        for c in cells(&out) {
            assert_eq!(c.segs.len(), 1);
            assert_eq!(c.segs[0].text, "fn main() {}");
        }
    }

    #[test]
    fn changed_flag_survives_split() {
        let rows = build_rows(&[hunk(vec![
            line(LineKind::Removed, Some(1), None, "let value = 1;"),
            line(LineKind::Added, None, Some(1), "let value = 2;"),
        ])]);
        let out = Highlighter::new().highlight_rows("x.rs", &rows, &[], true);
        let right = match &out[1] {
            StyledRow::Line { right: Some(r), .. } => r,
            _ => panic!("expected line"),
        };
        assert!(right.segs.iter().any(|s| s.changed && s.text.contains('2')));
        assert!(right.segs.iter().any(|s| !s.changed));
        let joined: String = right.segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "let value = 2;");
    }

    #[test]
    fn empty_cell_does_not_panic() {
        let rows = build_rows(&[hunk(vec![
            line(LineKind::Removed, Some(1), None, ""),
            line(LineKind::Added, None, Some(1), "x"),
        ])]);
        let out = Highlighter::new().highlight_rows("x.rs", &rows, &[], true);
        assert_eq!(out.len(), rows.len());
    }
}
