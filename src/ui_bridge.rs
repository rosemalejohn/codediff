//! Maps controller Events onto `AppWindow` properties and UI callbacks onto Commands.
use crate::branch::{BranchInfo, HeadInfo};
use crate::controller::{Command, DiffView, Event, RangeInfo, Selection};
use crate::flow::{next_actions, FlowConfig, FlowKind, FlowState};
use crate::flow_finish::{Blocker, FinishPlan, FinishRequest};
use crate::history::relative_time;
use crate::highlight::{StyledCell, StyledRow};
use crate::sbs::{ExpandDir, GapCtl};
use crate::model::{
    file_fingerprint, hunk_fingerprint, ChangeSide, CommitInfo, DiffBody, FileEntry, FileStatus, LineKind,
};
use crate::settings::{Appearance, Settings};
use crate::switcher::{project_name, rank, rank_names};
use crate::ui::{AppWindow, CellData, CommitRow, FileRow, Flow, FlowStep, RowData, SegData, SwitcherItem};
use git2::Oid;
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

const SIDE_STAGED: i32 = 0;
const SIDE_UNSTAGED: i32 = 1;
const NO_SIDE: i32 = -1;

const MODE_TEXT: i32 = 0;
const MODE_BINARY: i32 = 1;
const MODE_TOO_LARGE: i32 = 2;
const MODE_EMPTY: i32 = 3;

// Directions of the diff pane's expand-gap callback.
const EXPAND_UP: i32 = 0;
const EXPAND_DOWN: i32 = 1;

const DISCARD_NONE: i32 = 0;
const DISCARD_FILE: i32 = 1;
const DISCARD_HUNK: i32 = 2;

// Row heights in logical px; keep in sync with Theme.diff-line-height and
// Theme.diff-header-height in ui/theme.slint.
const LINE_HEIGHT: f32 = 18.0;
const HEADER_HEIGHT: f32 = 30.0;
// Advance of one Menlo character at Theme.mono-font-size (12px): an estimate
// used only to size the horizontal scroll range.
const CHAR_WIDTH: f32 = 7.3;
const TEXT_END_PAD: f32 = 16.0;

const HIST_NONE: i32 = 0;
const HIST_COMMIT: i32 = 1;
const HIST_RANGE: i32 = 2;

const SWITCHER_BRANCHES: i32 = 1;
const SWITCHER_FLOW: i32 = 2;

/// Width of `cols` monospace columns of diff text plus its end padding.
fn text_width(cols: usize) -> f32 {
    cols as f32 * CHAR_WIDTH + TEXT_END_PAD
}

/// Columns of a cell's text as rendered (segments already tab-expanded).
fn cell_cols(c: &CellData) -> usize {
    c.segs.iter().map(|s| s.text.chars().count()).sum()
}

const STALE_MSG: &str = "File changed, refreshed. Try again.";

/// Applies one controller event to the window. Must run on the UI thread.
pub fn apply(window: &AppWindow, ev: Event) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    apply_at(window, ev, now);
}

/// `apply` with an explicit clock (unix seconds) for relative commit times.
fn apply_at(window: &AppWindow, ev: Event, now: i64) {
    match ev {
        Event::RepoOpened { name, path, recent } => {
            window.set_repo_name(name.into());
            window.set_repo_path(display_path(&path).into());
            window.set_branch_name(SharedString::new());
            window.set_branch_detached(false);
            window.set_branches(ModelRc::new(VecModel::<SwitcherItem>::default()));
            set_recent(window, &recent);
            window.set_has_repo(true);
            window.set_error_text(SharedString::new());
            window.set_staged(rows(&[], SIDE_STAGED));
            window.set_unstaged(rows(&[], SIDE_UNSTAGED));
            window.set_change_count(0);
            window.set_selected_side(NO_SIDE);
            window.set_selected_path(SharedString::new());
            clear_diff(window);
            window.set_left_title(SharedString::new());
            window.set_right_title(SharedString::new());
            clear_discard(window);
            window.set_commits(ModelRc::new(VecModel::<CommitRow>::default()));
            window.set_history_loading(false);
            window.set_history_exhausted(false);
            set_history_selection(window, HIST_NONE, "", "", "");
            window.set_range_banner(SharedString::new());
            clear_commit_files(window);
            reset_flow(window);
        }
        Event::Status(lists) => {
            let side = window.get_selected_side();
            let path = window.get_selected_path();
            let group = match side {
                SIDE_STAGED => Some(&lists.staged),
                SIDE_UNSTAGED => Some(&lists.unstaged),
                _ => None,
            };
            if !group.is_some_and(|g| g.iter().any(|e| e.path == path.as_str())) {
                window.set_selected_side(NO_SIDE);
                window.set_selected_path(SharedString::new());
            }
            let distinct: HashSet<&str> = lists
                .staged
                .iter()
                .chain(&lists.unstaged)
                .map(|e| e.path.as_str())
                .collect();
            window.set_change_count(distinct.len() as i32);
            window.set_staged(rows(&lists.staged, SIDE_STAGED));
            window.set_unstaged(rows(&lists.unstaged, SIDE_UNSTAGED));
            mark_selection(window);
        }
        Event::Error(msg) => {
            // An open flow dialog shows it too, instead of waiting silently.
            let f = window.global::<Flow>();
            if f.get_dialog() != FLOW_DIALOG_NONE {
                f.set_error(msg.as_str().into());
            }
            // The red banner takes over from the green one.
            f.set_notice(SharedString::new());
            window.set_error_text(msg.into());
        }
        Event::Head(head) => {
            let (name, detached) = match head {
                HeadInfo::Branch(b) => (b, false),
                HeadInfo::Detached(id) => (id, true),
            };
            window.set_branch_name(name.into());
            window.set_branch_detached(detached);
        }
        Event::Branches(list) => {
            let items: Vec<SwitcherItem> = list.iter().map(|b| branch_item(b, now)).collect();
            window.set_branches(ModelRc::new(VecModel::from(items)));
            if window.get_switcher_open() && window.get_switcher_mode() == SWITCHER_BRANCHES {
                filter_branches(window, &window.get_switcher_query());
            }
        }
        Event::Diff(Some(view)) => {
            let history = view.side.is_none();
            let path = view.file.path.clone();
            apply_diff(window, view);
            if history {
                window.set_commit_file(path.into());
                mark_files(window);
            }
        }
        Event::Diff(None) => clear_diff(window),
        Event::History { commits, append, new_ids } => {
            apply_history(window, &commits, append, &new_ids, now)
        }
        Event::Files(files) => {
            let keep = window.get_commit_file();
            let path = if files.iter().any(|f| f.path == keep.as_str()) {
                keep
            } else {
                files.first().map(|f| f.path.as_str().into()).unwrap_or_default()
            };
            window.set_commit_files(rows(&files, NO_SIDE));
            window.set_commit_file(path);
            mark_files(window);
        }
        Event::Range(Some(info)) => {
            if window.get_history_mode() == HIST_RANGE {
                // The head moves when new commits land on a range ending at HEAD.
                window.set_range_base(info.base.to_string().into());
                window.set_range_head(info.head.to_string().into());
                mark_commits(window);
            }
            window.set_range_banner(range_banner(&info).into());
        }
        // Also emitted when another tab takes over the controller selection;
        // keep the banner while the History tab still holds a range.
        Event::Range(None) => {
            if window.get_history_mode() != HIST_RANGE {
                window.set_range_banner(SharedString::new());
            }
        }
        Event::Flow(state) => apply_flow(window, &state, now),
        Event::FlowStartCheck { kind, name, error } => apply_start_check(window, kind, &name, error),
        Event::FlowDone(text) => {
            let f = window.global::<Flow>();
            f.set_dialog(FLOW_DIALOG_NONE);
            f.set_notice(text.into());
        }
        Event::FlowFailed(text) => window.global::<Flow>().set_error(text.into()),
        Event::FlowPlan { request, plan } => apply_finish_plan(window, &request, plan),
        Event::FlowPushing => {
            let f = window.global::<Flow>();
            f.set_dialog(FLOW_DIALOG_PUSHING);
            set_step_states(window, STEP_RUNNING);
        }
        Event::FlowPushFailed(reason) => {
            let f = window.global::<Flow>();
            f.set_dialog(FLOW_DIALOG_PUSH_FAILED);
            f.set_push_reason(reason.into());
            set_step_states(window, STEP_FAILED);
        }
    }
}

fn range_banner(info: &RangeInfo) -> String {
    let noun = if info.commit_count == 1 { "commit" } else { "commits" };
    format!(
        "Reviewing {} {noun} since {}  +{} -{}",
        info.commit_count,
        short(&info.base.to_string()),
        info.additions,
        info.deletions
    )
}

fn short(id: &str) -> &str {
    id.get(..7).unwrap_or(id)
}

fn apply_history(window: &AppWindow, commits: &[CommitInfo], append: bool, new_ids: &[Oid], now: i64) {
    let old = window.get_commits();
    let mut out: Vec<CommitRow> = Vec::new();
    // Marks stay until the row is clicked, across refreshes.
    let mut marked: HashSet<SharedString> = new_ids.iter().map(|i| i.to_string().into()).collect();
    if append {
        out.extend(old.iter());
    } else {
        marked.extend(old.iter().filter(|r| r.is_new).map(|r| r.id));
    }
    out.extend(commits.iter().map(|c| {
        let id: SharedString = c.id.to_string().into();
        CommitRow {
            is_new: marked.contains(&id),
            id,
            short_id: c.short_id.as_str().into(),
            summary: c.summary.as_str().into(),
            author: c.author.as_str().into(),
            initial: c.author.chars().next().map(|ch| ch.to_uppercase().to_string()).unwrap_or_default().into(),
            when: relative_time(now, c.time).into(),
            selected: false,
            in_range: false,
        }
    }));
    window.set_commits(ModelRc::new(VecModel::from(out)));
    window.set_history_loading(false);
    if append && commits.is_empty() {
        window.set_history_exhausted(true);
    }
    mark_commits(window);
}

/// Sets each commit row's `selected` / `in-range` flags from the history selection.
fn mark_commits(window: &AppWindow) {
    let model = window.get_commits();
    let mode = window.get_history_mode();
    let selected = window.get_history_commit();
    let (base, head) = (window.get_range_base(), window.get_range_head());
    let ids: Vec<SharedString> = model.iter().map(|r| r.id).collect();
    // Newest first: rows from head (inclusive) down to base (exclusive).
    let range = if mode == HIST_RANGE {
        ids.iter().position(|i| *i == head).map(|h| {
            let b = ids.iter().position(|i| *i == base).unwrap_or(ids.len());
            h..b.max(h)
        })
    } else {
        None
    };
    for (i, id) in ids.iter().enumerate() {
        let Some(mut row) = model.row_data(i) else { continue };
        let want_sel = mode == HIST_COMMIT && *id == selected;
        let want_range = range.as_ref().is_some_and(|r| r.contains(&i));
        if row.selected != want_sel || row.in_range != want_range {
            row.selected = want_sel;
            row.in_range = want_range;
            model.set_row_data(i, row);
        }
    }
}

/// Sets each file-strip chip's `selected` flag from `commit-file`.
fn mark_files(window: &AppWindow) {
    let model = window.get_commit_files();
    let path = window.get_commit_file();
    for i in 0..model.row_count() {
        let Some(mut row) = model.row_data(i) else { continue };
        let want = row.path == path;
        if row.selected != want {
            row.selected = want;
            model.set_row_data(i, row);
        }
    }
}

fn clear_commit_files(window: &AppWindow) {
    window.set_commit_files(rows(&[], NO_SIDE));
    window.set_commit_file(SharedString::new());
}

fn set_history_selection(w: &AppWindow, mode: i32, commit: &str, base: &str, head: &str) {
    w.set_history_mode(mode);
    w.set_range_follows_head(false);
    w.set_history_commit(commit.into());
    w.set_range_base(base.into());
    w.set_range_head(head.into());
    w.set_commit_file(SharedString::new());
    mark_commits(w);
}

/// Full 40-char hex id from the UI; anything else is ignored.
fn parse_oid(s: &str) -> Option<Oid> {
    (s.len() == 40).then(|| Oid::from_str(s).ok()).flatten()
}

/// Clears the "new" marker of a clicked row.
fn clear_new(w: &AppWindow, id: &str) {
    let model = w.get_commits();
    for i in 0..model.row_count() {
        let Some(mut row) = model.row_data(i) else { continue };
        if row.id == id && row.is_new {
            row.is_new = false;
            model.set_row_data(i, row);
        }
    }
}

/// Row index of a commit id in the loaded list (0 is newest).
fn commit_index(w: &AppWindow, id: &str) -> Option<usize> {
    w.get_commits().iter().position(|r| r.id == id)
}

fn select_commit(w: &AppWindow, send: &impl Fn(Command), id: &str) {
    let Some(oid) = parse_oid(id) else { return };
    clear_new(w, id);
    set_history_selection(w, HIST_COMMIT, id, "", "");
    w.set_range_banner(SharedString::new());
    reset_diff_scroll(w);
    send(Command::Select(Selection::Commit { id: oid, path: None }));
}

/// `follows_head`: the controller keeps `head` at the current HEAD; the `head`
/// sent here is only what the UI shows until the controller's Range event.
fn select_range(w: &AppWindow, send: &impl Fn(Command), base: &str, head: &str, follows_head: bool) {
    let (Some(b), Some(h)) = (parse_oid(base), parse_oid(head)) else { return };
    set_history_selection(w, HIST_RANGE, "", base, head);
    w.set_range_follows_head(follows_head);
    reset_diff_scroll(w);
    send(Command::Select(Selection::Range { base: b, head: h, path: None, follows_head }));
}

/// The History tab's current selection, including the selected strip file.
fn history_selection(w: &AppWindow) -> Option<Selection> {
    let file = w.get_commit_file();
    let path = (!file.is_empty()).then(|| file.to_string());
    match w.get_history_mode() {
        HIST_COMMIT => Some(Selection::Commit { id: parse_oid(&w.get_history_commit())?, path }),
        HIST_RANGE => Some(Selection::Range {
            base: parse_oid(&w.get_range_base())?,
            head: parse_oid(&w.get_range_head())?,
            path,
            follows_head: w.get_range_follows_head(),
        }),
        _ => None,
    }
}

/// Column titles for a History diff: "Parent" | commit, or base | head.
fn history_titles(window: &AppWindow) {
    let (left, right) = match window.get_history_mode() {
        HIST_COMMIT => ("Parent".to_string(), short(&window.get_history_commit()).to_string()),
        HIST_RANGE => (
            short(&window.get_range_base()).to_string(),
            short(&window.get_range_head()).to_string(),
        ),
        _ => (String::new(), String::new()),
    };
    window.set_left_title(left.into());
    window.set_right_title(right.into());
}

/// Scrolls the diff pane back to its top-left corner.
fn reset_diff_scroll(w: &AppWindow) {
    w.set_diff_scroll_y(0.0);
    w.set_diff_scroll_x(0.0);
}

/// Empties the diff pane (column titles are left to the caller).
fn clear_diff(window: &AppWindow) {
    window.set_diff_rows(ModelRc::new(VecModel::<RowData>::default()));
    window.set_diff_mode(MODE_EMPTY);
    window.set_diff_path(SharedString::new());
    window.set_diff_side(NO_SIDE);
    window.set_diff_note(SharedString::new());
    window.set_diff_additions(0);
    window.set_diff_deletions(0);
    window.set_diff_untracked(false);
    window.set_hunk_actions(false);
    window.set_diff_fingerprint(SharedString::new());
    window.set_diff_content_width(0.0);
    reset_diff_scroll(window);
}

fn apply_diff(window: &AppWindow, view: DiffView) {
    let file = &view.file;
    let side = match view.side {
        Some(ChangeSide::Staged) => SIDE_STAGED,
        Some(ChangeSide::Unstaged) => SIDE_UNSTAGED,
        None => NO_SIDE,
    };
    // Same file and side as shown: a refresh, so keep the scroll position.
    let same = window.get_diff_mode() != MODE_EMPTY
        && window.get_diff_path() == file.path.as_str()
        && window.get_diff_side() == side;

    let (mode, note, fingerprints) = match &file.body {
        DiffBody::Text(hunks) => {
            let note = if view.rows.is_empty() { "No content changes." } else { "" };
            let fps = hunks.iter().map(|h| format!("{:x}", hunk_fingerprint(h))).collect();
            (MODE_TEXT, note.to_string(), fps)
        }
        DiffBody::Binary { old_size, new_size } => (
            MODE_BINARY,
            format!("Binary file, no preview ({old_size} to {new_size} bytes)"),
            Vec::new(),
        ),
        DiffBody::TooLarge { lines } => (
            MODE_TOO_LARGE,
            format!("Large diff ({lines} lines). Click to load."),
            Vec::new(),
        ),
    };

    let rows: Vec<RowData> = view.rows.iter().map(|r| row_data(r, &fingerprints)).collect();
    let content_height: f32 = rows
        .iter()
        .map(|r| if r.is_header { HEADER_HEIGHT } else { LINE_HEIGHT })
        .sum();
    let max_cols = rows
        .iter()
        .map(|r| cell_cols(&r.left).max(cell_cols(&r.right)))
        .max()
        .unwrap_or(0);
    let content_width = if max_cols == 0 { 0.0 } else { text_width(max_cols) };

    match view.side {
        Some(ChangeSide::Unstaged) => {
            window.set_left_title("Index".into());
            window.set_right_title("Working copy".into());
        }
        Some(ChangeSide::Staged) => {
            window.set_left_title("HEAD".into());
            window.set_right_title("Staged".into());
        }
        // History: the event carries no commit ids; use the UI's history selection.
        None => history_titles(window),
    }

    window.set_diff_path(file.path.as_str().into());
    window.set_diff_side(side);
    window.set_diff_mode(mode);
    window.set_diff_note(note.into());
    window.set_diff_additions(file.additions as i32);
    window.set_diff_deletions(file.deletions as i32);
    window.set_diff_untracked(file.status == FileStatus::Untracked);
    window.set_diff_fingerprint(format!("{:x}", file_fingerprint(file)).into());
    window.set_hunk_actions(match view.side {
        Some(ChangeSide::Unstaged) => true,
        // Hunk ops on a staged rename always fail as stale; file-level only.
        Some(ChangeSide::Staged) => file.status != FileStatus::Renamed,
        None => false,
    });
    window.set_diff_rows(ModelRc::new(VecModel::from(rows)));

    window.set_diff_content_width(content_width);

    // The view clamps x to the visible text width; keep it within the content here.
    let (x, y) = if same {
        let min_y = -(content_height - window.get_diff_visible_height()).max(0.0);
        (
            window.get_diff_scroll_x().clamp(-content_width, 0.0),
            window.get_diff_scroll_y().clamp(min_y, 0.0),
        )
    } else {
        (0.0, 0.0)
    };
    window.set_diff_scroll_x(x);
    window.set_diff_scroll_y(y);
}

fn row_data(row: &StyledRow, fingerprints: &[String]) -> RowData {
    match row {
        StyledRow::HunkHeader { hunk_index, header, gap } => RowData {
            is_header: true,
            hunk_index: *hunk_index as i32,
            header: header.as_str().into(),
            fingerprint: fingerprints
                .get(*hunk_index)
                .map(|f| f.as_str().into())
                .unwrap_or_default(),
            ..gap_data(gap.as_ref())
        },
        StyledRow::Gap(g) => RowData {
            is_header: true,
            hunk_index: -1,
            ..gap_data(Some(g))
        },
        StyledRow::Line { left, right } => RowData {
            is_header: false,
            hunk_index: -1,
            left: cell_data(left.as_ref()),
            right: cell_data(right.as_ref()),
            ..gap_data(None)
        },
    }
}

/// Expand controls of a header or trailing gap row (gap -1 when none).
fn gap_data(gap: Option<&GapCtl>) -> RowData {
    let Some(g) = gap else {
        return RowData { gap: -1, ..RowData::default() };
    };
    RowData {
        gap: g.gap as i32,
        hidden: g.hidden as i32,
        expand_up: g.up,
        expand_down: g.down,
        expand_all: g.all,
        ..RowData::default()
    }
}

fn cell_data(cell: Option<&StyledCell>) -> CellData {
    let Some(c) = cell else {
        return CellData::default();
    };
    // `segs` may be empty (a blank line paired with a changed one).
    let segs: Vec<SegData> = c
        .segs
        .iter()
        .map(|s| SegData {
            text: s.text.trim_end_matches(['\n', '\r']).replace('\t', "    ").into(),
            // (0, 0, 0) is the highlighter's "unstyled" fallback: let the theme color it.
            color: match s.rgb {
                (0, 0, 0) => Color::from_argb_u8(0, 0, 0, 0),
                (r, g, b) => Color::from_rgb_u8(r, g, b),
            },
            changed: s.changed,
        })
        .collect();
    CellData {
        present: true,
        no: c.no as i32,
        kind: match c.kind {
            LineKind::Context => 0,
            LineKind::Added => 1,
            LineKind::Removed => 2,
        },
        segs: ModelRc::new(VecModel::from(segs)),
    }
}

fn clear_discard(window: &AppWindow) {
    window.set_discard_kind(DISCARD_NONE);
    window.set_discard_body(SharedString::new());
    window.set_discard_path(SharedString::new());
    window.set_discard_hunk_index(-1);
    window.set_discard_header(SharedString::new());
    window.set_discard_fingerprint(SharedString::new());
}

/// Parses the hex fingerprint carried on a hunk header row.
fn parse_fingerprint(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    u64::from_str_radix(s, 16).ok()
}

/// Validates a hunk action's arguments; on failure shows the stale-hunk error.
fn hunk_args(w: &AppWindow, index: i32, fingerprint: &str) -> Option<(usize, u64)> {
    let parsed = usize::try_from(index).ok().zip(parse_fingerprint(fingerprint));
    if parsed.is_none() {
        w.set_error_text(STALE_MSG.into());
    }
    parsed
}

/// The path shown in the diff pane when it is on `side`, else None.
fn shown_path(w: &AppWindow, side: i32) -> Option<String> {
    let path = w.get_diff_path();
    (w.get_diff_side() == side && !path.is_empty()).then(|| path.to_string())
}

/// Selects a Changes row: updates the window and sends the selection.
fn select_change(w: &AppWindow, send: &impl Fn(Command), side: i32, path: SharedString) {
    let change_side = if side == SIDE_STAGED {
        ChangeSide::Staged
    } else {
        ChangeSide::Unstaged
    };
    w.set_selected_side(side);
    w.set_selected_path(path.clone());
    mark_selection(w);
    send(Command::Select(Selection::Change {
        side: change_side,
        path: path.into(),
    }));
}

/// Fills the recent-repos list (used before any repo is open).
pub fn set_recent(window: &AppWindow, paths: &[PathBuf]) {
    let items: Vec<SharedString> = paths.iter().map(|p| display_path(p).into()).collect();
    window.set_recent(ModelRc::new(VecModel::from(items)));
}

/// Fills the Cmd+K list with the recent projects matching `query`, best first,
/// and highlights the first row. With no query the list keeps recency order
/// and highlights the most recent project other than the open one.
fn filter_projects(window: &AppWindow, query: &str) {
    let paths: Vec<String> = window.get_recent().iter().map(|p| p.to_string()).collect();
    let current = window.get_repo_path();
    let items: Vec<SwitcherItem> = rank(query, &paths)
        .into_iter()
        .map(|i| {
            let p = paths[i].as_str();
            SwitcherItem {
                name: project_name(p).into(),
                detail: p.into(),
                key: p.into(),
                current: window.get_has_repo() && current == p,
            }
        })
        .collect();
    show_items(window, query, items);
}

/// Fills the Cmd+B list with the branches matching `query`, best first. With
/// no query the list keeps the controller's order (local branches, newest
/// first) and highlights the first branch other than the current one.
fn filter_branches(window: &AppWindow, query: &str) {
    let all: Vec<SwitcherItem> = window.get_branches().iter().collect();
    let names: Vec<String> = all.iter().map(|b| b.name.to_string()).collect();
    let items = rank_names(query, &names).into_iter().map(|i| all[i].clone()).collect();
    show_items(window, query, items);
}

fn show_items(window: &AppWindow, query: &str, items: Vec<SwitcherItem>) {
    let index = if query.trim().is_empty() { items.iter().position(|i| !i.current).unwrap_or(0) } else { 0 };
    window.set_switcher_index(index as i32);
    window.set_switcher_items(ModelRc::new(VecModel::from(items)));
}

fn branch_item(b: &BranchInfo, now: i64) -> SwitcherItem {
    let mut detail = format!("{} \u{b7} {}", b.summary, relative_time(now, b.time));
    if b.remote {
        detail = format!("remote \u{b7} {detail}");
    }
    SwitcherItem { name: b.name.as_str().into(), detail: detail.into(), key: b.refname.as_str().into(), current: b.current }
}

// ---- git flow ----

const FLOW_NO_KIND: i32 = -1;
const FLOW_DIALOG_NONE: i32 = 0;
const FLOW_DIALOG_INIT: i32 = 1;
const FLOW_DIALOG_START: i32 = 2;
const FLOW_DIALOG_FINISH: i32 = 3;
const FLOW_DIALOG_PUSHING: i32 = 4;
const FLOW_DIALOG_PUSH_FAILED: i32 = 5;
// Flow.blocker-level: Finish disabled, or only Close.
const BLOCKER_NONE: i32 = 0;
const BLOCKER_WARN: i32 = 1;
const BLOCKER_CLOSE: i32 = 2;
// FlowStep.state.
const STEP_PENDING: i32 = 0;
const STEP_DONE: i32 = 1;
const STEP_RUNNING: i32 = 2;
const STEP_FAILED: i32 = 3;

fn flow_kind_index(kind: FlowKind) -> i32 {
    match kind {
        FlowKind::Feature => 0,
        FlowKind::Release => 1,
        FlowKind::Hotfix => 2,
    }
}

fn flow_kind_from_key(word: &str) -> Option<FlowKind> {
    match word {
        "feature" => Some(FlowKind::Feature),
        "release" => Some(FlowKind::Release),
        "hotfix" => Some(FlowKind::Hotfix),
        _ => None,
    }
}

fn string_model(items: &[String]) -> ModelRc<SharedString> {
    let items: Vec<SharedString> = items.iter().map(|s| s.as_str().into()).collect();
    ModelRc::new(VecModel::from(items))
}

/// Fills the repository-state half of the `Flow` global from `Event::Flow`.
fn apply_flow(window: &AppWindow, state: &FlowState, now: i64) {
    let f = window.global::<Flow>();
    let names = state.config.as_ref().unwrap_or(&state.draft);
    f.set_initialized(state.config.is_some());
    f.set_init_blocked(state.init_blocked.as_deref().unwrap_or_default().into());
    f.set_branches(string_model(&state.branches));
    f.set_production(names.production.as_str().into());
    f.set_develop(names.develop.as_str().into());
    f.set_feature_prefix(names.feature.as_str().into());
    f.set_release_prefix(names.release.as_str().into());
    f.set_hotfix_prefix(names.hotfix.as_str().into());
    f.set_tag_prefix(names.version_tag.as_str().into());
    f.set_features(string_model(&state.features));
    f.set_release(state.releases.first().map(String::as_str).unwrap_or_default().into());
    f.set_hotfix(state.hotfixes.first().map(String::as_str).unwrap_or_default().into());
    let (kind, branch) = match &state.current {
        Some((kind, branch)) => (flow_kind_index(*kind), branch.as_str()),
        None => (FLOW_NO_KIND, ""),
    };
    f.set_current_kind(kind);
    f.set_current_branch(branch.into());
    let tag = match &state.latest_tag {
        Some(t) => format!("Latest tag: {}, {}", t.name, relative_time(now, t.time)),
        None => "No tags yet".to_string(),
    };
    f.set_latest_tag(tag.into());
    f.set_push_blocked(state.push_blocked.as_deref().unwrap_or_default().into());
    f.set_push_default(state.push_default);
    let actions: Vec<SwitcherItem> = next_actions(state)
        .into_iter()
        .map(|a| SwitcherItem { name: a.name.into(), detail: a.detail.into(), key: a.key.into(), current: false })
        .collect();
    f.set_actions(ModelRc::new(VecModel::from(actions)));
    if window.get_switcher_open() && window.get_switcher_mode() == SWITCHER_FLOW {
        filter_flow_actions(window, &window.get_switcher_query());
    }
}

/// Puts every `Flow` property back to its default (another repository opened).
fn reset_flow(window: &AppWindow) {
    let f = window.global::<Flow>();
    let empty = SharedString::new;
    f.set_initialized(false);
    f.set_init_blocked(empty());
    f.set_branches(string_model(&[]));
    f.set_production(empty());
    f.set_develop(empty());
    f.set_feature_prefix(empty());
    f.set_release_prefix(empty());
    f.set_hotfix_prefix(empty());
    f.set_tag_prefix(empty());
    f.set_features(string_model(&[]));
    f.set_release(empty());
    f.set_hotfix(empty());
    f.set_current_kind(FLOW_NO_KIND);
    f.set_current_branch(empty());
    f.set_latest_tag(empty());
    f.set_push_blocked(empty());
    f.set_push_default(false);
    f.set_actions(ModelRc::new(VecModel::<SwitcherItem>::default()));
    f.set_dialog(FLOW_DIALOG_NONE);
    f.set_kind(0);
    f.set_error(empty());
    f.set_notice(empty());
    f.set_init_production(empty());
    f.set_init_develop(empty());
    f.set_init_develop_new(false);
    f.set_init_feature(empty());
    f.set_init_release(empty());
    f.set_init_hotfix(empty());
    f.set_init_tag(empty());
    f.set_name(empty());
    f.set_start_error(empty());
    f.set_branch(empty());
    f.set_branch_options(string_model(&[]));
    f.set_tag(empty());
    f.set_message(empty());
    f.set_delete_branch(true);
    f.set_push(false);
    f.set_steps(ModelRc::new(VecModel::<FlowStep>::default()));
    f.set_blocker_level(0);
    f.set_blocker_title(empty());
    f.set_blocker_body(empty());
    f.set_blocker_files(string_model(&[]));
    f.set_push_reason(empty());
}

/// Fills the Next Action list with the flow actions matching `query`.
fn filter_flow_actions(window: &AppWindow, query: &str) {
    let all: Vec<SwitcherItem> = window.global::<Flow>().get_actions().iter().collect();
    let names: Vec<String> = all.iter().map(|a| a.name.to_string()).collect();
    let items = rank_names(query, &names).into_iter().map(|i| all[i].clone()).collect();
    show_items(window, query, items);
}

/// Opens the dialog for an action key (`init`, `start:<kind>`,
/// `finish:<kind>[:<branch>]`). Finish fills its defaults and asks for a plan.
fn flow_action(window: &AppWindow, key: &str, send: &dyn Fn(Command)) {
    let f = window.global::<Flow>();
    let mut parts = key.splitn(3, ':');
    let (dialog, kind) = match (parts.next(), parts.next().and_then(flow_kind_from_key)) {
        (Some("init"), _) => (FLOW_DIALOG_INIT, None),
        (Some("start"), Some(kind)) => (FLOW_DIALOG_START, Some(kind)),
        (Some("finish"), Some(kind)) => (FLOW_DIALOG_FINISH, Some(kind)),
        _ => return,
    };
    if let Some(kind) = kind {
        f.set_kind(flow_kind_index(kind));
    }
    match dialog {
        // While not initialized, the names hold the draft config.
        FLOW_DIALOG_INIT => {
            f.set_init_production(f.get_production());
            f.set_init_develop(f.get_develop());
            f.set_init_feature(f.get_feature_prefix());
            f.set_init_release(f.get_release_prefix());
            f.set_init_hotfix(f.get_hotfix_prefix());
            f.set_init_tag(f.get_tag_prefix());
            f.set_init_develop_new(!flow_has_branch(window, &f.get_develop()));
            f.set_error(SharedString::new());
        }
        FLOW_DIALOG_START => {
            f.set_name(SharedString::new());
            f.set_start_error(SharedString::new());
            f.set_error(SharedString::new());
        }
        FLOW_DIALOG_FINISH => open_finish(window, parts.next()),
        _ => {}
    }
    f.set_dialog(dialog);
    if dialog == FLOW_DIALOG_FINISH {
        send(Command::FlowPlan(finish_request(window)));
    }
}

// ---- git flow: finish ----

/// Fills the Finish fields for the kind in `Flow.kind`: the given branch,
/// else the checked-out one of this kind, else the first open one.
fn open_finish(window: &AppWindow, branch: Option<&str>) {
    let f = window.global::<Flow>();
    let kind = flow_dialog_kind(window);
    let options: Vec<SharedString> = match kind {
        FlowKind::Feature => f.get_features().iter().collect(),
        FlowKind::Release => [f.get_release()].into_iter().filter(|b| !b.is_empty()).collect(),
        FlowKind::Hotfix => [f.get_hotfix()].into_iter().filter(|b| !b.is_empty()).collect(),
    };
    let current = (f.get_current_kind() == flow_kind_index(kind)).then(|| f.get_current_branch());
    let branch = match branch {
        Some(b) => b.into(),
        None => current.or_else(|| options.first().cloned()).unwrap_or_default(),
    };
    let (tag, message) = finish_defaults(window, kind, &branch);
    f.set_branch_options(ModelRc::new(VecModel::from(options)));
    f.set_branch(branch);
    f.set_tag(tag.into());
    f.set_message(message.into());
    f.set_delete_branch(true);
    f.set_push(kind != FlowKind::Feature && f.get_push_blocked().is_empty() && f.get_push_default());
    clear_finish_plan(window);
    f.set_error(SharedString::new());
}

/// Default tag and tag message for `branch`; both empty for a feature.
fn finish_defaults(window: &AppWindow, kind: FlowKind, branch: &str) -> (String, String) {
    let f = window.global::<Flow>();
    let (prefix, word) = match kind {
        FlowKind::Feature => return (String::new(), String::new()),
        FlowKind::Release => (f.get_release_prefix(), "Release"),
        FlowKind::Hotfix => (f.get_hotfix_prefix(), "Hotfix"),
    };
    let version = branch.strip_prefix(prefix.as_str()).unwrap_or(branch);
    (format!("{}{version}", f.get_tag_prefix()), format!("{word} {version}"))
}

/// The finish the dialog shows. A feature never has a tag or a push, and a
/// blocked push is never asked for.
fn finish_request(window: &AppWindow) -> FinishRequest {
    let f = window.global::<Flow>();
    let kind = flow_dialog_kind(window);
    let feature = kind == FlowKind::Feature;
    let text = |s: SharedString| if feature { String::new() } else { s.into() };
    FinishRequest {
        kind,
        branch: f.get_branch().into(),
        tag: text(f.get_tag()),
        message: text(f.get_message()),
        delete_branch: f.get_delete_branch(),
        push: !feature && f.get_push() && f.get_push_blocked().is_empty(),
    }
}

fn clear_finish_plan(window: &AppWindow) {
    let f = window.global::<Flow>();
    f.set_steps(ModelRc::new(VecModel::<FlowStep>::default()));
    f.set_blocker_level(BLOCKER_NONE);
    f.set_blocker_title(SharedString::new());
    f.set_blocker_body(SharedString::new());
    f.set_blocker_files(string_model(&[]));
}

/// Shows a plan, unless it answers a request that is no longer on screen.
fn apply_finish_plan(window: &AppWindow, request: &FinishRequest, plan: FinishPlan) {
    let f = window.global::<Flow>();
    if f.get_dialog() != FLOW_DIALOG_FINISH || *request != finish_request(window) {
        return;
    }
    let steps: Vec<FlowStep> = plan
        .steps
        .into_iter()
        .map(|s| FlowStep { text: s.text.into(), note: s.note.into(), remote: s.remote, state: STEP_PENDING })
        .collect();
    f.set_steps(ModelRc::new(VecModel::from(steps)));
    let (level, title, body, files) = match &plan.blocker {
        None => (BLOCKER_NONE, String::new(), String::new(), &[][..]),
        Some(b) => {
            let files = match b {
                Blocker::Conflict { files, .. } | Blocker::Collision(files) => files.as_slice(),
                _ => &[],
            };
            let level = if matches!(b, Blocker::Conflict { .. }) { BLOCKER_CLOSE } else { BLOCKER_WARN };
            (level, b.title(), b.body(), files)
        }
    };
    f.set_blocker_level(level);
    f.set_blocker_title(title.into());
    f.set_blocker_body(body.into());
    f.set_blocker_files(string_model(files));
}

/// Marks local steps done and remote steps `remote_state` (push running or failed).
fn set_step_states(window: &AppWindow, remote_state: i32) {
    let steps: Vec<FlowStep> = window
        .global::<Flow>()
        .get_steps()
        .iter()
        .map(|s| FlowStep { state: if s.remote { remote_state } else { STEP_DONE }, ..s })
        .collect();
    window.global::<Flow>().set_steps(ModelRc::new(VecModel::from(steps)));
}

fn flow_has_branch(window: &AppWindow, name: &str) -> bool {
    window.global::<Flow>().get_branches().iter().any(|b| b == name)
}

/// The kind the open dialog works on.
fn flow_dialog_kind(window: &AppWindow) -> FlowKind {
    match window.global::<Flow>().get_kind() {
        1 => FlowKind::Release,
        2 => FlowKind::Hotfix,
        _ => FlowKind::Feature,
    }
}

fn flow_init_config(window: &AppWindow) -> FlowConfig {
    let f = window.global::<Flow>();
    FlowConfig {
        production: f.get_init_production().into(),
        develop: f.get_init_develop().into(),
        feature: f.get_init_feature().into(),
        release: f.get_init_release().into(),
        hotfix: f.get_init_hotfix().into(),
        version_tag: f.get_init_tag().into(),
    }
}

/// Shows the reply to `Command::FlowCheckStart` unless the name changed
/// since. The empty-name error is left out: the disabled button says it.
fn apply_start_check(window: &AppWindow, kind: FlowKind, name: &str, error: Option<String>) {
    let f = window.global::<Flow>();
    if f.get_dialog() != FLOW_DIALOG_START || kind != flow_dialog_kind(window) || name != f.get_name().as_str() {
        return;
    }
    let error = if name.is_empty() { String::new() } else { error.unwrap_or_default() };
    f.set_start_error(error.into());
}

fn wire_flow(window: &AppWindow, send: impl Fn(Command) + Clone + 'static) {
    let f = window.global::<Flow>();
    // Branch of the last plan asked for, to tell when the picker moved.
    let planned = Rc::new(RefCell::new(SharedString::new()));

    let s = send.clone();
    let p = planned.clone();
    let weak = window.as_weak();
    f.on_action(move |key| {
        if let Some(w) = weak.upgrade() {
            flow_action(&w, &key, &s);
            *p.borrow_mut() = w.global::<Flow>().get_branch();
        }
    });

    let weak = window.as_weak();
    f.on_cancel(move || {
        if let Some(w) = weak.upgrade() {
            w.global::<Flow>().set_dialog(FLOW_DIALOG_NONE);
        }
    });

    let weak = window.as_weak();
    f.on_init_develop_edited(move |text| {
        if let Some(w) = weak.upgrade() {
            let new = !flow_has_branch(&w, &text);
            w.global::<Flow>().set_init_develop_new(new);
        }
    });

    let s = send.clone();
    let weak = window.as_weak();
    f.on_submit_init(move || {
        if let Some(w) = weak.upgrade() {
            s(Command::FlowInit(flow_init_config(&w)));
        }
    });

    let s = send.clone();
    let weak = window.as_weak();
    f.on_name_edited(move |text| {
        if let Some(w) = weak.upgrade() {
            w.global::<Flow>().set_name(text.clone());
            s(Command::FlowCheckStart { kind: flow_dialog_kind(&w), name: text.into() });
        }
    });

    let s = send.clone();
    let weak = window.as_weak();
    f.on_submit_start(move || {
        if let Some(w) = weak.upgrade() {
            s(Command::FlowStart { kind: flow_dialog_kind(&w), name: w.global::<Flow>().get_name().into() });
        }
    });

    // ---- finish, push and notice ----

    let s = send.clone();
    let weak = window.as_weak();
    f.on_plan_changed(move || {
        let Some(w) = weak.upgrade() else { return };
        let f = w.global::<Flow>();
        let branch = f.get_branch();
        if *planned.borrow() != branch {
            let (tag, message) = finish_defaults(&w, flow_dialog_kind(&w), &branch);
            f.set_tag(tag.into());
            f.set_message(message.into());
            *planned.borrow_mut() = branch;
        }
        s(Command::FlowPlan(finish_request(&w)));
    });

    // Only with a plan on screen and nothing blocking it.
    let s = send.clone();
    let weak = window.as_weak();
    f.on_submit_finish(move || {
        let Some(w) = weak.upgrade() else { return };
        let f = w.global::<Flow>();
        if f.get_dialog() == FLOW_DIALOG_FINISH && f.get_blocker_level() == BLOCKER_NONE && f.get_steps().row_count() > 0
        {
            s(Command::FlowFinish(finish_request(&w)));
        }
    });

    let s = send.clone();
    let weak = window.as_weak();
    f.on_retry_push(move || {
        if let Some(w) = weak.upgrade() {
            w.global::<Flow>().set_error(SharedString::new());
        }
        s(Command::FlowRetryPush);
    });

    let s = send;
    f.on_close_push_failed(move || s(Command::FlowDropPush));

    let weak = window.as_weak();
    f.on_dismiss_notice(move || {
        if let Some(w) = weak.upgrade() {
            w.global::<Flow>().set_notice(SharedString::new());
        }
    });
}

/// Applies a saved appearance choice (System follows macOS).
pub fn set_appearance(window: &AppWindow, appearance: Appearance) {
    window.set_appearance(appearance.index());
}

/// Handles the top-bar appearance switch: applies the choice and saves it.
/// The theme change itself reaches the controller through `dark-changed`.
pub fn wire_appearance(window: &AppWindow, settings_file: PathBuf) {
    let weak = window.as_weak();
    window.on_choose_appearance(move |i| {
        let appearance = Appearance::from_index(i);
        if let Some(w) = weak.upgrade() {
            set_appearance(&w, appearance);
        }
        let mut settings = Settings::load_from(&settings_file);
        settings.appearance = appearance;
        if let Err(e) = settings.save_to(&settings_file) {
            eprintln!("codediff: could not save settings: {e}");
        }
    });
}

/// Connects every UI callback to a controller command.
pub fn wire(window: &AppWindow, send: impl Fn(Command) + Clone + 'static) {
    let s = send.clone();
    window.on_open_repo_dialog(move || {
        // Native folder picker; runs modally on the UI thread.
        if let Some(dir) = rfd::FileDialog::new()
            .set_title("Open repository")
            .pick_folder()
        {
            s(Command::OpenRepo(dir));
        }
    });

    let s = send.clone();
    window.on_open_recent(move |path| s(Command::OpenRepo(PathBuf::from(path.as_str()))));

    let weak = window.as_weak();
    window.on_switcher_filter(move |query| {
        if let Some(w) = weak.upgrade() {
            if w.get_switcher_mode() == SWITCHER_FLOW {
                filter_flow_actions(&w, &query);
            } else if w.get_switcher_mode() == SWITCHER_BRANCHES {
                filter_branches(&w, &query);
            } else {
                filter_projects(&w, &query);
            }
        }
    });

    let s = send.clone();
    window.on_list_branches(move || s(Command::ListBranches));

    wire_flow(window, send.clone());

    let s = send.clone();
    window.on_checkout_branch(move |refname| s(Command::Checkout(refname.into())));

    let s = send.clone();
    let weak = window.as_weak();
    window.on_select_change(move |side, path| {
        let Some(w) = weak.upgrade() else { return };
        select_change(&w, &s, side, path);
    });

    // Up/Down in the sidebar: staged rows first, then unstaged.
    let s = send.clone();
    let weak = window.as_weak();
    window.on_move_selection(move |delta| {
        let Some(w) = weak.upgrade() else { return };
        let order: Vec<FileRow> = [w.get_staged(), w.get_unstaged()]
            .iter()
            .flat_map(|m| m.iter().collect::<Vec<_>>())
            .collect();
        if order.is_empty() {
            return;
        }
        let side = w.get_selected_side();
        let path = w.get_selected_path();
        let current = order.iter().position(|r| r.side == side && r.path == path);
        let next = match current {
            Some(i) => (i as i64 + delta as i64).clamp(0, order.len() as i64 - 1) as usize,
            None if delta < 0 => order.len() - 1,
            None => 0,
        };
        if Some(next) != current {
            let row = &order[next];
            select_change(&w, &s, row.side, row.path.clone());
        }
    });

    // Up/Down in History: newer (-1) or older (+1) commit; Changes untouched.
    let s = send.clone();
    let weak = window.as_weak();
    window.on_move_commit_selection(move |delta| {
        let Some(w) = weak.upgrade() else { return };
        let ids: Vec<SharedString> = w.get_commits().iter().map(|r| r.id).collect();
        if ids.is_empty() {
            return;
        }
        let current = (w.get_history_mode() == HIST_COMMIT)
            .then(|| commit_index(&w, &w.get_history_commit()))
            .flatten();
        let next = match current {
            Some(i) => (i as i64 + delta as i64).clamp(0, ids.len() as i64 - 1) as usize,
            None if delta < 0 => ids.len() - 1,
            None => 0,
        };
        if Some(next) != current {
            select_commit(&w, &s, &ids[next]);
        }
    });

    let s = send.clone();
    let weak = window.as_weak();
    window.on_stage_file(move || {
        let Some(w) = weak.upgrade() else { return };
        if let Some(path) = shown_path(&w, SIDE_UNSTAGED) {
            s(Command::StageFile(path));
        }
    });

    let s = send.clone();
    window.on_stage_path(move |side, path| {
        let path = path.to_string();
        s(if side == SIDE_STAGED { Command::UnstageFile(path) } else { Command::StageFile(path) });
    });

    let s = send.clone();
    let weak = window.as_weak();
    window.on_unstage_file(move || {
        let Some(w) = weak.upgrade() else { return };
        if let Some(path) = shown_path(&w, SIDE_STAGED) {
            s(Command::UnstageFile(path));
        }
    });

    let weak = window.as_weak();
    window.on_request_discard_file(move || {
        let Some(w) = weak.upgrade() else { return };
        let Some(path) = shown_path(&w, SIDE_UNSTAGED) else { return };
        // Captured now, from the diff the user is looking at.
        let fingerprint = w.get_diff_fingerprint();
        if parse_fingerprint(&fingerprint).is_none() {
            w.set_error_text(STALE_MSG.into());
            return;
        }
        let body = if w.get_diff_untracked() {
            format!("Delete untracked file {path}? This cannot be undone.")
        } else {
            format!("Discard all unstaged changes in {path}? This cannot be undone.")
        };
        clear_discard(&w);
        w.set_discard_path(path.into());
        w.set_discard_body(body.into());
        w.set_discard_fingerprint(fingerprint);
        w.set_discard_kind(DISCARD_FILE);
    });

    let s = send.clone();
    let weak = window.as_weak();
    window.on_stage_hunk(move |index, header, fingerprint| {
        let Some(w) = weak.upgrade() else { return };
        if !w.get_hunk_actions() {
            return;
        }
        let Some(path) = shown_path(&w, SIDE_UNSTAGED) else { return };
        let Some((hunk_index, fingerprint)) = hunk_args(&w, index, &fingerprint) else { return };
        s(Command::StageHunk { path, hunk_index, header: header.into(), fingerprint });
    });

    let s = send.clone();
    let weak = window.as_weak();
    window.on_unstage_hunk(move |index, header, fingerprint| {
        let Some(w) = weak.upgrade() else { return };
        if !w.get_hunk_actions() {
            return;
        }
        let Some(path) = shown_path(&w, SIDE_STAGED) else { return };
        let Some((hunk_index, fingerprint)) = hunk_args(&w, index, &fingerprint) else { return };
        s(Command::UnstageHunk { path, hunk_index, header: header.into(), fingerprint });
    });

    let weak = window.as_weak();
    window.on_request_discard_hunk(move |index, header, fingerprint| {
        let Some(w) = weak.upgrade() else { return };
        if !w.get_hunk_actions() {
            return;
        }
        let Some(path) = shown_path(&w, SIDE_UNSTAGED) else { return };
        if hunk_args(&w, index, &fingerprint).is_none() {
            return;
        }
        // Discarding a hunk of an untracked file deletes the whole file.
        let body = if w.get_diff_untracked() {
            format!("Delete untracked file {path}? This cannot be undone.")
        } else {
            format!("Discard this hunk in {path}? This cannot be undone.")
        };
        clear_discard(&w);
        w.set_discard_body(body.into());
        w.set_discard_path(path.into());
        w.set_discard_hunk_index(index);
        w.set_discard_header(header);
        w.set_discard_fingerprint(fingerprint);
        w.set_discard_kind(DISCARD_HUNK);
    });

    let s = send.clone();
    let weak = window.as_weak();
    window.on_confirm_discard(move || {
        let Some(w) = weak.upgrade() else { return };
        let kind = w.get_discard_kind();
        let path = w.get_discard_path().to_string();
        let index = w.get_discard_hunk_index();
        let header = w.get_discard_header().to_string();
        let fingerprint = w.get_discard_fingerprint();
        clear_discard(&w);
        match kind {
            DISCARD_FILE => match parse_fingerprint(&fingerprint) {
                Some(fingerprint) => s(Command::DiscardFile { path, fingerprint }),
                None => w.set_error_text(STALE_MSG.into()),
            },
            DISCARD_HUNK => {
                if let Some((hunk_index, fingerprint)) = hunk_args(&w, index, &fingerprint) {
                    s(Command::DiscardHunk { path, hunk_index, header, fingerprint });
                }
            }
            _ => {}
        }
    });

    let weak = window.as_weak();
    window.on_cancel_discard(move || {
        if let Some(w) = weak.upgrade() {
            clear_discard(&w);
        }
    });

    let s = send.clone();
    window.on_load_full_diff(move || s(Command::LoadFullDiff));

    let s = send.clone();
    window.on_expand_gap(move |gap, dir| {
        let dir = match dir {
            EXPAND_UP => ExpandDir::Up,
            EXPAND_DOWN => ExpandDir::Down,
            _ => ExpandDir::All,
        };
        if let Ok(gap) = usize::try_from(gap) {
            s(Command::ExpandGap { gap, dir });
        }
    });

    let weak = window.as_weak();
    window.on_dismiss_error(move || {
        if let Some(w) = weak.upgrade() {
            w.set_error_text(SharedString::new());
        }
    });

    let s = send.clone();
    let weak = window.as_weak();
    window.on_select_commit(move |id| {
        let Some(w) = weak.upgrade() else { return };
        select_commit(&w, &s, &id);
    });

    // Shift-click with one commit selected: older of the two is the base.
    let s = send.clone();
    let weak = window.as_weak();
    window.on_shift_select_commit(move |id| {
        let Some(w) = weak.upgrade() else { return };
        let current = w.get_history_commit();
        if w.get_history_mode() != HIST_COMMIT || current == id {
            return select_commit(&w, &s, &id);
        }
        let (Some(a), Some(b)) = (commit_index(&w, &current), commit_index(&w, &id)) else {
            return select_commit(&w, &s, &id);
        };
        clear_new(&w, &id);
        let (base, head) = if a > b { (current, id) } else { (id, current) };
        // Row 0 is the newest loaded commit, i.e. HEAD as the UI knows it.
        select_range(&w, &s, &base, &head, a.min(b) == 0);
    });

    // "Review since here": base exclusive, up to HEAD. The newest loaded row is
    // only a placeholder head; the controller resolves the real HEAD.
    let s = send.clone();
    let weak = window.as_weak();
    window.on_review_since(move |id| {
        let Some(w) = weak.upgrade() else { return };
        let Some(head) = w.get_commits().row_data(0).map(|r| r.id) else { return };
        clear_new(&w, &id);
        select_range(&w, &s, &id, &head, true);
    });

    let s = send.clone();
    let weak = window.as_weak();
    window.on_clear_range(move || {
        let Some(w) = weak.upgrade() else { return };
        set_history_selection(&w, HIST_NONE, "", "", "");
        w.set_range_banner(SharedString::new());
        clear_commit_files(&w);
        s(Command::Select(Selection::None));
    });

    let s = send.clone();
    let weak = window.as_weak();
    window.on_select_commit_file(move |path| {
        let Some(w) = weak.upgrade() else { return };
        w.set_commit_file(path);
        mark_files(&w);
        reset_diff_scroll(&w);
        if let Some(sel) = history_selection(&w) {
            s(Command::Select(sel));
        }
    });

    let s = send.clone();
    let weak = window.as_weak();
    window.on_history_scrolled_to_end(move || {
        let Some(w) = weak.upgrade() else { return };
        if w.get_history_loading() || w.get_history_exhausted() || w.get_commits().row_count() == 0 {
            return;
        }
        w.set_history_loading(true);
        s(Command::LoadMoreHistory);
    });

    // Re-show the newly active tab's own selection (the diff pane is shared).
    let s = send.clone();
    let weak = window.as_weak();
    window.on_tab_changed(move |tab| {
        let Some(w) = weak.upgrade() else { return };
        reset_diff_scroll(&w);
        let sel = if tab == 0 {
            let side = w.get_selected_side();
            let path = w.get_selected_path();
            if side == NO_SIDE || path.is_empty() {
                None
            } else {
                let side = if side == SIDE_STAGED { ChangeSide::Staged } else { ChangeSide::Unstaged };
                Some(Selection::Change { side, path: path.into() })
            }
        } else {
            history_selection(&w)
        };
        s(Command::Select(sel.unwrap_or(Selection::None)));
    });

    let s = send.clone();
    window.on_dark_changed(move |dark| s(Command::SetDark(dark)));

    send(Command::SetDark(window.get_dark()));
}

fn rows(entries: &[FileEntry], side: i32) -> ModelRc<FileRow> {
    let rows: Vec<FileRow> = entries
        .iter()
        .map(|e| FileRow {
            name: file_name(&e.path).into(),
            dir: parent_dir(&e.path).into(),
            path: e.path.as_str().into(),
            letter: e.status.letter().to_string().into(),
            side,
            selected: false,
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

/// Last path component, the part the sidebar shows first.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Everything before the last `/`, or empty for a top-level file.
fn parent_dir(path: &str) -> &str {
    path.rfind('/').map(|i| &path[..i]).unwrap_or("")
}

/// Sets each row's `selected` flag from the window's current selection.
fn mark_selection(window: &AppWindow) {
    let side = window.get_selected_side();
    let path = window.get_selected_path();
    for model in [window.get_staged(), window.get_unstaged()] {
        for i in 0..model.row_count() {
            let Some(mut row) = model.row_data(i) else { continue };
            let want = row.side == side && row.path == path;
            if row.selected != want {
                row.selected = want;
                model.set_row_data(i, row);
            }
        }
    }
}

fn display_path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileStatus, StatusLists};
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    fn window() -> AppWindow {
        thread_local! {
            static INIT: () = i_slint_backend_testing::init_no_event_loop();
        }
        INIT.with(|_| ());
        AppWindow::new().unwrap()
    }

    fn entry(path: &str, status: FileStatus) -> FileEntry {
        FileEntry {
            path: path.into(),
            old_path: None,
            status,
        }
    }

    fn status(staged: &[(&str, FileStatus)], unstaged: &[(&str, FileStatus)]) -> Event {
        Event::Status(StatusLists {
            staged: staged.iter().map(|(p, s)| entry(p, *s)).collect(),
            unstaged: unstaged.iter().map(|(p, s)| entry(p, *s)).collect(),
        })
    }

    fn recorder() -> (Rc<RefCell<Vec<Command>>>, impl Fn(Command) + Clone + 'static) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let l = log.clone();
        (log, move |c| l.borrow_mut().push(c))
    }

    #[test]
    fn status_event_fills_lists() {
        let w = window();
        apply(
            &w,
            status(
                &[("a.rs", FileStatus::Added)],
                &[("b.rs", FileStatus::Modified), ("c.rs", FileStatus::Untracked)],
            ),
        );
        assert_eq!(w.get_staged().row_count(), 1);
        assert_eq!(w.get_unstaged().row_count(), 2);
        assert_eq!(w.get_change_count(), 3);
        let row = w.get_unstaged().row_data(1).unwrap();
        assert_eq!(row.path, "c.rs");
        assert_eq!(row.letter, "A");
        assert_eq!(row.side, 1);
        assert_eq!(w.get_staged().row_data(0).unwrap().side, 0);
    }

    #[test]
    fn change_count_counts_distinct_paths() {
        let w = window();
        apply(
            &w,
            status(
                &[("a.rs", FileStatus::Modified)],
                &[("a.rs", FileStatus::Modified)],
            ),
        );
        assert_eq!(w.get_change_count(), 1);
    }

    #[test]
    fn error_event_sets_banner() {
        let w = window();
        apply(&w, Event::Error("not a git repository".into()));
        assert_eq!(w.get_error_text(), "not a git repository");
    }

    #[test]
    fn repo_opened_resets_state() {
        let w = window();
        apply(&w, status(&[("a.rs", FileStatus::Added)], &[]));
        w.set_selected_side(0);
        w.set_selected_path("a.rs".into());
        w.set_error_text("old".into());
        apply(
            &w,
            Event::RepoOpened {
                name: "proj".into(),
                path: PathBuf::from("/tmp/proj"),
                recent: vec![PathBuf::from("/tmp/proj"), PathBuf::from("/tmp/other")],
            },
        );
        assert!(w.get_has_repo());
        assert_eq!(w.get_repo_name(), "proj");
        assert_eq!(w.get_recent().row_count(), 2);
        assert_eq!(w.get_recent().row_data(1).unwrap(), "/tmp/other");
        assert_eq!(w.get_staged().row_count(), 0);
        assert_eq!(w.get_change_count(), 0);
        assert_eq!(w.get_selected_side(), -1);
        assert_eq!(w.get_selected_path(), "");
        assert_eq!(w.get_error_text(), "");
    }

    #[test]
    fn selection_survives_status_refresh_when_path_remains() {
        let w = window();
        let (_log, send) = recorder();
        wire(&w, send);
        apply(&w, status(&[], &[("a.rs", FileStatus::Modified), ("b.rs", FileStatus::Modified)]));
        w.invoke_select_change(1, "b.rs".into());
        apply(&w, status(&[], &[("b.rs", FileStatus::Modified), ("c.rs", FileStatus::Added)]));
        assert!(w.get_unstaged().row_data(0).unwrap().selected);
        assert!(!w.get_unstaged().row_data(1).unwrap().selected);
        assert_eq!(w.get_selected_path(), "b.rs");

        apply(&w, status(&[("b.rs", FileStatus::Modified)], &[]));
        assert!(!w.get_staged().row_data(0).unwrap().selected);
        assert_eq!(w.get_selected_side(), -1);
        assert_eq!(w.get_selected_path(), "");
    }

    #[test]
    fn select_change_sends_command_and_marks_row() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, status(&[("a.rs", FileStatus::Added)], &[("a.rs", FileStatus::Modified)]));
        log.borrow_mut().clear();
        w.invoke_select_change(0, "a.rs".into());
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Change { side: ChangeSide::Staged, path })] if path == "a.rs"
        ));
        assert!(w.get_staged().row_data(0).unwrap().selected);
        assert!(!w.get_unstaged().row_data(0).unwrap().selected);

        w.invoke_select_change(1, "a.rs".into());
        assert!(matches!(
            log.borrow().last(),
            Some(Command::Select(Selection::Change { side: ChangeSide::Unstaged, .. }))
        ));
        assert!(!w.get_staged().row_data(0).unwrap().selected);
        assert!(w.get_unstaged().row_data(0).unwrap().selected);
    }

    #[test]
    fn open_recent_sends_open_repo() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        log.borrow_mut().clear();
        w.invoke_open_recent("/tmp/proj".into());
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::OpenRepo(p)] if p == &PathBuf::from("/tmp/proj")
        ));
    }

    #[test]
    fn dismiss_error_clears_banner() {
        let w = window();
        let (_log, send) = recorder();
        wire(&w, send);
        w.set_error_text("boom".into());
        w.invoke_dismiss_error();
        assert_eq!(w.get_error_text(), "");
    }

    #[test]
    fn wire_sends_dark_mode_at_startup_and_on_change() {
        let w = window();
        let (log, send) = recorder();
        let dark = w.get_dark();
        wire(&w, send);
        assert!(matches!(log.borrow().as_slice(), [Command::SetDark(d)] if *d == dark));
        w.invoke_dark_changed(!dark);
        assert!(matches!(log.borrow().last(), Some(Command::SetDark(d)) if *d != dark));
    }

    // ---- appearance ----

    #[test]
    fn forced_light_and_dark_override_system() {
        let w = window();
        set_appearance(&w, Appearance::Dark);
        assert_eq!(w.get_appearance(), 2);
        assert!(w.get_dark());
        set_appearance(&w, Appearance::Light);
        assert_eq!(w.get_appearance(), 1);
        assert!(!w.get_dark());
    }

    #[test]
    fn choosing_appearance_saves_it_and_sends_set_dark() {
        let w = window();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let (log, send) = recorder();
        set_appearance(&w, Appearance::Light);
        wire(&w, send);
        wire_appearance(&w, file.clone());

        w.invoke_choose_appearance(2);
        // `changed dark` handlers run when Slint processes change trackers.
        slint::platform::update_timers_and_animations();
        assert_eq!(w.get_appearance(), 2);
        assert!(w.get_dark());
        assert_eq!(Settings::load_from(&file).appearance, Appearance::Dark);
        assert!(matches!(log.borrow().last(), Some(Command::SetDark(true))));

        w.invoke_choose_appearance(0);
        assert_eq!(Settings::load_from(&file).appearance, Appearance::System);
    }

    // ---- diff view ----

    use crate::controller::DiffView;
    use crate::highlight::{StyledCell, StyledRow, StyledSeg};
    use crate::model::{hunk_fingerprint, DiffBody, DiffLine, FileDiff, Hunk, LineKind};

    fn seg(text: &str) -> StyledSeg {
        StyledSeg { text: text.into(), rgb: (200, 100, 50), changed: false }
    }

    fn cell(no: u32, kind: LineKind, text: &str) -> StyledCell {
        StyledCell { no, kind, segs: vec![seg(text)] }
    }

    fn sample_hunk() -> Hunk {
        Hunk {
            header: "@@ -1,1 +1,2 @@".into(),
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 2,
            lines: vec![
                DiffLine { kind: LineKind::Context, old_no: Some(1), new_no: Some(1), text: "a".into() },
                DiffLine { kind: LineKind::Added, old_no: None, new_no: Some(2), text: "b".into() },
            ],
        }
    }

    fn text_view(path: &str, side: Option<ChangeSide>, status: FileStatus) -> DiffView {
        let h = sample_hunk();
        DiffView {
            file: FileDiff {
                path: path.into(),
                old_path: None,
                status,
                body: DiffBody::Text(vec![h.clone()]),
                additions: 1,
                deletions: 0,
                old_lines: Vec::new(),
            },
            rows: vec![
                StyledRow::HunkHeader { hunk_index: 0, header: h.header.clone(), gap: None },
                StyledRow::Line { left: None, right: Some(cell(2, LineKind::Added, "b")) },
                StyledRow::Line {
                    left: Some(cell(1, LineKind::Context, "a")),
                    right: Some(cell(1, LineKind::Context, "a")),
                },
            ],
            side,
        }
    }

    fn body_view(path: &str, body: DiffBody) -> DiffView {
        DiffView {
            file: FileDiff {
                path: path.into(),
                old_path: None,
                status: FileStatus::Modified,
                body,
                additions: 0,
                deletions: 0,
                old_lines: Vec::new(),
            },
            rows: Vec::new(),
            side: Some(ChangeSide::Unstaged),
        }
    }

    fn fp_hex() -> String {
        format!("{:x}", hunk_fingerprint(&sample_hunk()))
    }

    #[test]
    fn diff_event_maps_rows() {
        let w = window();
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        assert_eq!(w.get_diff_mode(), 0);
        assert_eq!(w.get_diff_path(), "a.rs");
        assert_eq!(w.get_diff_side(), 1);
        assert_eq!(w.get_left_title(), "Index");
        assert_eq!(w.get_right_title(), "Working copy");
        assert_eq!(w.get_diff_additions(), 1);
        assert!(w.get_hunk_actions());
        let rows = w.get_diff_rows();
        assert_eq!(rows.row_count(), 3);
        let h = rows.row_data(0).unwrap();
        assert!(h.is_header);
        assert_eq!(h.hunk_index, 0);
        assert_eq!(h.header, "@@ -1,1 +1,2 @@");
        assert_eq!(h.fingerprint, fp_hex().as_str());
        let r1 = rows.row_data(1).unwrap();
        assert!(!r1.is_header);
        assert!(!r1.left.present);
        assert!(r1.right.present);
        assert_eq!(r1.right.no, 2);
        assert_eq!(r1.right.kind, 1);
        assert_eq!(r1.right.segs.row_data(0).unwrap().text, "b");
        let r2 = rows.row_data(2).unwrap();
        assert!(r2.left.present);
        assert_eq!(r2.left.kind, 0);
    }

    #[test]
    fn staged_titles_and_renamed_hides_hunk_actions() {
        let w = window();
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Staged), FileStatus::Modified))));
        assert_eq!(w.get_diff_side(), 0);
        assert_eq!(w.get_left_title(), "HEAD");
        assert_eq!(w.get_right_title(), "Staged");
        assert!(w.get_hunk_actions());
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Staged), FileStatus::Renamed))));
        assert!(!w.get_hunk_actions());
        apply(&w, Event::Diff(Some(text_view("a.rs", None, FileStatus::Modified))));
        assert_eq!(w.get_diff_side(), -1);
        assert!(!w.get_hunk_actions());
    }

    #[test]
    fn binary_diff_sets_mode_and_note() {
        let w = window();
        apply(&w, Event::Diff(Some(body_view("img.png", DiffBody::Binary { old_size: 10, new_size: 20 }))));
        assert_eq!(w.get_diff_mode(), 1);
        assert_eq!(w.get_diff_note(), "Binary file, no preview (10 to 20 bytes)");
        assert_eq!(w.get_diff_rows().row_count(), 0);
    }

    #[test]
    fn gap_controls_map_to_rows_and_expand_sends_command() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        let mut v = text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified);
        let ctl = |gap, hidden, up, down, all| GapCtl { gap, hidden, up, down, all };
        v.rows[0] = StyledRow::HunkHeader {
            hunk_index: 0,
            header: "@@ -30 +30 @@".into(),
            gap: Some(ctl(0, 29, true, false, false)),
        };
        v.rows.push(StyledRow::Gap(ctl(1, 1, false, true, false)));
        apply(&w, Event::Diff(Some(v)));
        let rows = w.get_diff_rows();
        let h = rows.row_data(0).unwrap();
        assert_eq!((h.gap, h.hidden, h.expand_up, h.expand_down), (0, 29, true, false));
        assert_eq!(h.hunk_index, 0);
        assert!(!h.fingerprint.is_empty());
        assert_eq!(rows.row_data(1).unwrap().gap, -1);
        let t = rows.row_data(3).unwrap();
        assert!(t.is_header);
        assert_eq!((t.hunk_index, t.gap, t.hidden, t.expand_down), (-1, 1, 1, true));

        log.borrow_mut().clear();
        w.invoke_expand_gap(0, 0);
        w.invoke_expand_gap(1, 1);
        w.invoke_expand_gap(2, 2);
        w.invoke_expand_gap(-1, 2);
        assert!(matches!(
            log.borrow().as_slice(),
            [
                Command::ExpandGap { gap: 0, dir: ExpandDir::Up },
                Command::ExpandGap { gap: 1, dir: ExpandDir::Down },
                Command::ExpandGap { gap: 2, dir: ExpandDir::All },
            ]
        ));
    }

    #[test]
    fn too_large_diff_sets_mode_and_loads_full() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(body_view("big.txt", DiffBody::TooLarge { lines: 50000 }))));
        assert_eq!(w.get_diff_mode(), 2);
        assert_eq!(w.get_diff_note(), "Large diff (50000 lines). Click to load.");
        log.borrow_mut().clear();
        w.invoke_load_full_diff();
        assert!(matches!(log.borrow().as_slice(), [Command::LoadFullDiff]));
    }

    #[test]
    fn empty_segs_render_without_panic() {
        let w = window();
        let mut v = text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified);
        v.rows.push(StyledRow::Line {
            left: Some(StyledCell { no: 3, kind: LineKind::Removed, segs: vec![] }),
            right: None,
        });
        apply(&w, Event::Diff(Some(v)));
        let r = w.get_diff_rows().row_data(3).unwrap();
        assert!(r.left.present);
        assert_eq!(r.left.kind, 2);
        assert_eq!(r.left.segs.row_count(), 0);
    }

    #[test]
    fn diff_none_clears_pane() {
        let w = window();
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        apply(&w, Event::Diff(None));
        assert_eq!(w.get_diff_mode(), 3);
        assert_eq!(w.get_diff_path(), "");
        assert_eq!(w.get_diff_rows().row_count(), 0);
    }

    #[test]
    fn repo_opened_resets_diff_pane() {
        let w = window();
        let (_log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        w.invoke_request_discard_file();
        assert_ne!(w.get_discard_kind(), 0);
        apply(
            &w,
            Event::RepoOpened { name: "p".into(), path: PathBuf::from("/tmp/p"), recent: vec![] },
        );
        assert_eq!(w.get_diff_mode(), 3);
        assert_eq!(w.get_diff_path(), "");
        assert_eq!(w.get_diff_rows().row_count(), 0);
        assert_eq!(w.get_left_title(), "");
        assert_eq!(w.get_right_title(), "");
        assert_eq!(w.get_diff_side(), -1);
        assert_eq!(w.get_discard_kind(), 0);
        assert_eq!(w.get_discard_body(), "");
    }

    #[test]
    fn same_path_diff_keeps_scroll_new_path_resets() {
        let w = window();
        let mut v = text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified);
        for i in 0..200 {
            v.rows.push(StyledRow::Line {
                left: Some(cell(i, LineKind::Context, "x")),
                right: Some(cell(i, LineKind::Context, "x")),
            });
        }
        apply(&w, Event::Diff(Some(v.clone())));
        w.set_diff_scroll_y(-300.0);
        apply(&w, Event::Diff(Some(v.clone())));
        assert_eq!(w.get_diff_scroll_y(), -300.0);
        // Shorter content: clamped so the view never scrolls past the end.
        let short = text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified);
        apply(&w, Event::Diff(Some(short)));
        assert!(w.get_diff_scroll_y() > -300.0);
        assert!(w.get_diff_scroll_y() <= 0.0);
        w.set_diff_scroll_y(-300.0);
        apply(&w, Event::Diff(Some(v.clone())));
        let mut other = v;
        other.file.path = "b.rs".into();
        w.set_diff_scroll_y(-300.0);
        apply(&w, Event::Diff(Some(other)));
        assert_eq!(w.get_diff_scroll_y(), 0.0);
    }

    #[test]
    fn content_width_follows_longest_line() {
        let w = window();
        let mut v = text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified);
        // Longest line is on the left; a tab counts as four columns.
        v.rows.push(StyledRow::Line {
            left: Some(StyledCell {
                no: 9,
                kind: LineKind::Removed,
                segs: vec![seg("\tlet x"), seg(" = 1;")],
            }),
            right: Some(cell(9, LineKind::Added, "y")),
        });
        apply(&w, Event::Diff(Some(v)));
        let cols = 4 + "let x = 1;".len();
        assert!(text_width(cols) > text_width(cols - 1));
        assert_eq!(w.get_diff_content_width(), text_width(cols));

        apply(&w, Event::Diff(None));
        assert_eq!(w.get_diff_content_width(), 0.0);
    }

    #[test]
    fn horizontal_scroll_kept_on_refresh_reset_on_file_change() {
        let w = window();
        let (_log, send) = recorder();
        wire(&w, send);
        let v = text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified);
        apply(&w, Event::Diff(Some(v.clone())));
        w.set_diff_scroll_x(-10.0);
        apply(&w, Event::Diff(Some(v.clone())));
        assert_eq!(w.get_diff_scroll_x(), -10.0, "same file refresh keeps it");
        // Never further than the content is wide.
        w.set_diff_scroll_x(-500.0);
        apply(&w, Event::Diff(Some(v.clone())));
        assert_eq!(w.get_diff_scroll_x(), -w.get_diff_content_width());

        let mut other = v.clone();
        other.file.path = "b.rs".into();
        apply(&w, Event::Diff(Some(other)));
        assert_eq!(w.get_diff_scroll_x(), 0.0, "file change resets it");

        apply(&w, Event::Diff(Some(v)));
        w.set_diff_scroll_x(-10.0);
        w.invoke_tab_changed(1);
        assert_eq!(w.get_diff_scroll_x(), 0.0, "tab change resets it");
    }

    #[test]
    fn file_actions_send_commands_for_matching_side() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        log.borrow_mut().clear();
        w.invoke_unstage_file(); // wrong side: ignored
        w.invoke_stage_file();
        assert!(matches!(log.borrow().as_slice(), [Command::StageFile(p)] if p == "a.rs"));
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Staged), FileStatus::Modified))));
        log.borrow_mut().clear();
        w.invoke_stage_file(); // wrong side: ignored
        w.invoke_unstage_file();
        assert!(matches!(log.borrow().as_slice(), [Command::UnstageFile(p)] if p == "a.rs"));
    }

    #[test]
    fn hunk_actions_carry_fingerprint() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        log.borrow_mut().clear();
        let fp = hunk_fingerprint(&sample_hunk());
        w.invoke_stage_hunk(0, "@@ -1,1 +1,2 @@".into(), fp_hex().into());
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::StageHunk { path, hunk_index: 0, header, fingerprint }]
                if path == "a.rs" && header == "@@ -1,1 +1,2 @@" && *fingerprint == fp
        ));
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Staged), FileStatus::Modified))));
        log.borrow_mut().clear();
        w.invoke_unstage_hunk(0, "@@ -1,1 +1,2 @@".into(), fp_hex().into());
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::UnstageHunk { hunk_index: 0, fingerprint, .. }] if *fingerprint == fp
        ));
    }

    #[test]
    fn bad_fingerprint_sends_nothing_and_shows_error() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        log.borrow_mut().clear();
        w.invoke_stage_hunk(0, "@@".into(), "not-hex".into());
        w.invoke_request_discard_hunk(0, "@@".into(), "".into());
        assert!(log.borrow().is_empty());
        assert_eq!(w.get_error_text(), "File changed, refreshed. Try again.");
        assert_eq!(w.get_discard_kind(), 0);
    }

    #[test]
    fn discard_requires_confirm() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        log.borrow_mut().clear();
        w.invoke_request_discard_file();
        assert!(log.borrow().is_empty());
        assert_eq!(w.get_discard_kind(), 1);
        assert_eq!(
            w.get_discard_body(),
            "Discard all unstaged changes in a.rs? This cannot be undone."
        );
        // The view changes while the dialog is open: the captured fingerprint is sent.
        let shown = text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified);
        let fp = crate::model::file_fingerprint(&shown.file);
        let mut changed = shown.clone();
        changed.file.status = FileStatus::Added;
        apply(&w, Event::Diff(Some(changed)));
        w.invoke_confirm_discard();
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::DiscardFile { path, fingerprint }] if path == "a.rs" && *fingerprint == fp
        ));
        assert_eq!(w.get_discard_kind(), 0);
        // A second confirm without a new request sends nothing.
        w.invoke_confirm_discard();
        assert_eq!(log.borrow().len(), 1);
    }

    #[test]
    fn discard_file_without_fingerprint_sends_nothing() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        w.set_diff_fingerprint(SharedString::new());
        log.borrow_mut().clear();
        w.invoke_request_discard_file();
        assert_eq!(w.get_discard_kind(), 0);
        assert_eq!(w.get_error_text(), "File changed, refreshed. Try again.");
        w.invoke_confirm_discard();
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn discard_hunk_confirm_sends_hunk_command() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        log.borrow_mut().clear();
        w.invoke_request_discard_hunk(0, "@@ -1,1 +1,2 @@".into(), fp_hex().into());
        assert!(log.borrow().is_empty());
        assert_eq!(w.get_discard_body(), "Discard this hunk in a.rs? This cannot be undone.");
        w.invoke_confirm_discard();
        let fp = hunk_fingerprint(&sample_hunk());
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::DiscardHunk { path, hunk_index: 0, fingerprint, .. }] if path == "a.rs" && *fingerprint == fp
        ));
    }

    #[test]
    fn discard_untracked_body_and_cancel() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("new.rs", Some(ChangeSide::Unstaged), FileStatus::Untracked))));
        log.borrow_mut().clear();
        w.invoke_request_discard_file();
        assert_eq!(w.get_discard_body(), "Delete untracked file new.rs? This cannot be undone.");
        w.invoke_cancel_discard();
        assert_eq!(w.get_discard_kind(), 0);
        w.invoke_confirm_discard();
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn untracked_hunk_discard_says_delete() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("new.rs", Some(ChangeSide::Unstaged), FileStatus::Untracked))));
        log.borrow_mut().clear();
        w.invoke_request_discard_hunk(0, "@@ -1,1 +1,2 @@".into(), fp_hex().into());
        assert!(log.borrow().is_empty());
        assert_eq!(w.get_discard_kind(), 2);
        assert_eq!(w.get_discard_body(), "Delete untracked file new.rs? This cannot be undone.");
        w.invoke_confirm_discard();
        assert!(matches!(log.borrow().as_slice(), [Command::DiscardHunk { path, .. }] if path == "new.rs"));
    }

    #[test]
    fn hunk_requests_ignored_without_hunk_actions() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        w.set_hunk_actions(false);
        log.borrow_mut().clear();
        w.invoke_stage_hunk(0, "@@ -1,1 +1,2 @@".into(), fp_hex().into());
        w.invoke_request_discard_hunk(0, "@@ -1,1 +1,2 @@".into(), fp_hex().into());
        assert!(log.borrow().is_empty());
        assert_eq!(w.get_discard_kind(), 0);
    }

    #[test]
    fn discard_file_ignored_for_staged_side() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Staged), FileStatus::Modified))));
        log.borrow_mut().clear();
        w.invoke_request_discard_file();
        assert_eq!(w.get_discard_kind(), 0);
        w.invoke_confirm_discard();
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn move_selection_walks_staged_then_unstaged() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, status(&[("a.rs", FileStatus::Added)], &[("b.rs", FileStatus::Modified)]));
        log.borrow_mut().clear();
        w.invoke_move_selection(1); // nothing selected: first row
        assert_eq!((w.get_selected_side(), w.get_selected_path().to_string()), (0, "a.rs".into()));
        w.invoke_move_selection(1);
        assert_eq!((w.get_selected_side(), w.get_selected_path().to_string()), (1, "b.rs".into()));
        assert!(w.get_unstaged().row_data(0).unwrap().selected);
        w.invoke_move_selection(1); // at the end: stays, sends nothing new
        assert_eq!(w.get_selected_path(), "b.rs");
        assert_eq!(log.borrow().len(), 2);
        w.invoke_move_selection(-1);
        assert!(matches!(
            log.borrow().last(),
            Some(Command::Select(Selection::Change { side: ChangeSide::Staged, path })) if path == "a.rs"
        ));
    }

    fn opened(w: &AppWindow, path: &str, recent: &[&str]) {
        apply(
            w,
            Event::RepoOpened {
                name: project_name(path).into(),
                path: PathBuf::from(path),
                recent: recent.iter().map(PathBuf::from).collect(),
            },
        );
    }

    #[test]
    fn switcher_lists_recent_and_highlights_previous_project() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        opened(&w, "/w/diffchecker", &["/w/diffchecker", "/w/wannahookup", "/w/api"]);
        w.invoke_switcher_filter("".into());
        let items = w.get_switcher_items();
        assert_eq!(items.row_count(), 3);
        let first = items.row_data(0).unwrap();
        assert_eq!((first.name.as_str(), first.current), ("diffchecker", true));
        assert_eq!(items.row_data(1).unwrap().key, "/w/wannahookup");
        assert_eq!(w.get_switcher_index(), 1);

        w.invoke_switcher_filter("whk".into());
        let items = w.get_switcher_items();
        assert_eq!(items.row_count(), 1);
        assert_eq!(items.row_data(0).unwrap().name, "wannahookup");
        assert_eq!(w.get_switcher_index(), 0);

        w.invoke_switcher_filter("zzz".into());
        assert_eq!(w.get_switcher_items().row_count(), 0);

        log.borrow_mut().clear();
        w.invoke_open_recent("/w/api".into());
        assert!(matches!(log.borrow().as_slice(), [Command::OpenRepo(p)] if p == Path::new("/w/api")));
    }

    #[test]
    fn cmd_k_opens_switcher_even_without_repo() {
        let w = window();
        let (_log, send) = recorder();
        wire(&w, send);
        w.show().unwrap();
        set_recent(&w, &[PathBuf::from("/w/a"), PathBuf::from("/w/b")]);
        press(&w, "k".into(), true);
        assert!(w.get_switcher_open());
        assert_eq!(w.get_switcher_items().row_count(), 2);
        // Nothing open: no row is current, the first is highlighted.
        assert_eq!(w.get_switcher_index(), 0);
        assert!(!w.get_switcher_items().row_data(0).unwrap().current);
    }

    fn branch(name: &str, remote: bool, current: bool) -> BranchInfo {
        let refname = if remote { format!("refs/remotes/{name}") } else { format!("refs/heads/{name}") };
        BranchInfo { name: name.into(), refname, remote, current, summary: "msg".into(), time: 0 }
    }

    #[test]
    fn head_event_shows_branch_or_detached_commit() {
        let w = window();
        apply(&w, Event::Head(HeadInfo::Branch("main".into())));
        assert_eq!((w.get_branch_name().as_str(), w.get_branch_detached()), ("main", false));
        apply(&w, Event::Head(HeadInfo::Detached("abc1234".into())));
        assert_eq!((w.get_branch_name().as_str(), w.get_branch_detached()), ("abc1234", true));
    }

    #[test]
    fn cmd_b_lists_filters_and_checks_out_branches() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        w.show().unwrap();
        opened(&w, "/w/proj", &["/w/proj"]);
        log.borrow_mut().clear();
        press(&w, "b".into(), true);
        assert!(w.get_switcher_open());
        assert_eq!(w.get_switcher_mode(), SWITCHER_BRANCHES);
        assert!(matches!(log.borrow().as_slice(), [Command::ListBranches]));

        // The list arrives while the switcher is open.
        apply(
            &w,
            Event::Branches(vec![
                branch("main", false, true),
                branch("feature/login", false, false),
                branch("origin/fix-typo", true, false),
            ]),
        );
        let items = w.get_switcher_items();
        assert_eq!(items.row_count(), 3);
        assert_eq!(w.get_switcher_index(), 1);
        let remote = items.row_data(2).unwrap();
        assert!(remote.detail.starts_with("remote"));

        w.invoke_switcher_filter("fxt".into());
        let items = w.get_switcher_items();
        assert_eq!(items.row_count(), 1);
        assert_eq!(items.row_data(0).unwrap().key, "refs/remotes/origin/fix-typo");

        log.borrow_mut().clear();
        w.invoke_checkout_branch("refs/remotes/origin/fix-typo".into());
        assert!(matches!(log.borrow().as_slice(), [Command::Checkout(r)] if r == "refs/remotes/origin/fix-typo"));
    }

    #[test]
    fn set_recent_fills_list() {
        let w = window();
        set_recent(&w, &[PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(w.get_recent().row_count(), 2);
    }

    // ---- history ----

    use crate::controller::RangeInfo;
    use crate::model::CommitInfo;
    use git2::Oid;

    const NOW: i64 = 1_800_000_000;

    /// A deterministic oid whose hex starts with `prefix` (hex chars only).
    fn oid(prefix: &str) -> Oid {
        Oid::from_str(&format!("{prefix:0<40}")).unwrap()
    }

    fn commit(prefix: &str, summary: &str, age_secs: i64) -> CommitInfo {
        let id = oid(prefix);
        CommitInfo {
            id,
            short_id: id.to_string()[..7].to_string(),
            summary: summary.into(),
            author: "Ana".into(),
            time: NOW - age_secs,
        }
    }

    /// Newest first: c3 (HEAD), c2, c1.
    fn three() -> Vec<CommitInfo> {
        vec![commit("c3", "third", 30), commit("c2", "second", 7200), commit("c1", "first", 3 * 86400)]
    }

    fn history(commits: Vec<CommitInfo>, append: bool, new_ids: Vec<Oid>) -> Event {
        Event::History { commits, append, new_ids }
    }

    fn row(w: &AppWindow, i: usize) -> crate::ui::CommitRow {
        w.get_commits().row_data(i).unwrap()
    }

    fn id(prefix: &str) -> SharedString {
        oid(prefix).to_string().into()
    }

    #[test]
    fn history_event_fills_rows() {
        let w = window();
        apply_at(&w, history(three(), false, vec![]), NOW);
        assert_eq!(w.get_commits().row_count(), 3);
        let r = row(&w, 1);
        assert_eq!(r.id, id("c2"));
        assert_eq!(r.short_id, "c200000");
        assert_eq!(r.summary, "second");
        assert_eq!(r.author, "Ana");
        assert_eq!(r.when, "2h ago");
        assert!(!r.is_new && !r.selected && !r.in_range);
        assert_eq!(row(&w, 0).when, "just now");
    }

    #[test]
    fn history_event_marks_new() {
        let w = window();
        apply_at(&w, history(three(), false, vec![oid("c3")]), NOW);
        assert!(row(&w, 0).is_new);
        assert!(!row(&w, 1).is_new);
    }

    #[test]
    fn new_marks_survive_refresh_until_clicked() {
        let w = window();
        let (_log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three()[1..].to_vec(), false, vec![]), NOW);
        apply_at(&w, history(three(), false, vec![oid("c3")]), NOW);
        let mut four = vec![commit("c4", "fourth", 5)];
        four.extend(three());
        // A later refresh with only c4 new must keep c3's mark.
        apply_at(&w, history(four.clone(), false, vec![oid("c4")]), NOW);
        assert!(row(&w, 0).is_new);
        assert!(row(&w, 1).is_new);
        w.invoke_select_commit(id("c3"));
        assert!(!row(&w, 1).is_new);
        assert!(row(&w, 0).is_new);
        apply_at(&w, history(four, false, vec![]), NOW);
        assert!(!row(&w, 1).is_new);
        assert!(row(&w, 0).is_new);
    }

    #[test]
    fn history_append_extends_list() {
        let w = window();
        apply_at(&w, history(three()[..2].to_vec(), false, vec![]), NOW);
        w.set_history_loading(true);
        apply_at(&w, history(three()[2..].to_vec(), true, vec![]), NOW);
        assert_eq!(w.get_commits().row_count(), 3);
        assert_eq!(row(&w, 2).summary, "first");
        assert!(!w.get_history_loading());
        assert!(!w.get_history_exhausted());
        apply_at(&w, history(vec![], true, vec![]), NOW);
        assert!(w.get_history_exhausted());
    }

    #[test]
    fn scrolled_to_end_sends_load_more_once() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        log.borrow_mut().clear();
        w.invoke_history_scrolled_to_end();
        w.invoke_history_scrolled_to_end();
        assert!(matches!(log.borrow().as_slice(), [Command::LoadMoreHistory]));
        apply_at(&w, history(vec![], true, vec![]), NOW);
        w.invoke_history_scrolled_to_end();
        assert_eq!(log.borrow().len(), 1, "exhausted: no more requests");
    }

    #[test]
    fn select_commit_sends_commit_and_marks_row() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        log.borrow_mut().clear();
        w.invoke_select_commit(id("c2"));
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Commit { id, path: None })] if *id == oid("c2")
        ));
        assert!(row(&w, 1).selected);
        assert!(!row(&w, 0).selected);
        // Invalid ids are ignored.
        w.invoke_select_commit("zz".into());
        assert_eq!(log.borrow().len(), 1);
    }

    #[test]
    fn range_event_sets_banner() {
        let w = window();
        apply_at(
            &w,
            Event::Range(Some(RangeInfo {
                base: oid("a1b2c3d"),
                head: oid("ff"),
                commit_count: 7,
                additions: 48,
                deletions: 12,
            })),
            NOW,
        );
        assert_eq!(w.get_range_banner(), "Reviewing 7 commits since a1b2c3d  +48 -12");
    }

    #[test]
    fn review_since_sends_range_to_head() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        log.borrow_mut().clear();
        w.invoke_review_since(id("c1"));
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Range { base, head, path: None, follows_head: true })]
                if *base == oid("c1") && *head == oid("c3")
        ));
        assert_eq!(w.get_history_mode(), 2);
        // Base excluded, head included.
        assert!(row(&w, 0).in_range);
        assert!(row(&w, 1).in_range);
        assert!(!row(&w, 2).in_range);
    }

    #[test]
    fn shift_click_orders_base_and_head() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        w.invoke_select_commit(id("c1"));
        log.borrow_mut().clear();
        w.invoke_shift_select_commit(id("c2"));
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Range { base, head, path: None, follows_head: false })]
                if *base == oid("c1") && *head == oid("c2")
        ));
        assert!(!row(&w, 0).in_range);
        assert!(row(&w, 1).in_range);
        assert!(!row(&w, 2).in_range);
    }

    #[test]
    fn shift_click_to_newest_row_follows_head() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        w.invoke_select_commit(id("c3"));
        log.borrow_mut().clear();
        w.invoke_shift_select_commit(id("c1"));
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Range { base, head, follows_head: true, .. })]
                if *base == oid("c1") && *head == oid("c3")
        ));
    }

    #[test]
    fn tab_change_resends_following_range() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        w.invoke_review_since(id("c1"));
        w.invoke_tab_changed(0);
        log.borrow_mut().clear();
        w.invoke_tab_changed(1);
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Range { follows_head: true, .. })]
        ));
        // A fixed range stays fixed.
        w.invoke_select_commit(id("c1"));
        w.invoke_shift_select_commit(id("c2"));
        log.borrow_mut().clear();
        w.invoke_tab_changed(1);
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Range { follows_head: false, .. })]
        ));
    }

    #[test]
    fn shift_click_without_selection_selects_commit() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        log.borrow_mut().clear();
        w.invoke_shift_select_commit(id("c2"));
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Commit { id, path: None })] if *id == oid("c2")
        ));
    }

    #[test]
    fn clear_range_and_single_select_exit_range() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        w.invoke_review_since(id("c1"));
        w.set_range_banner("Reviewing".into());
        w.set_commit_files(rows(&[entry("a.rs", FileStatus::Modified)], NO_SIDE));
        log.borrow_mut().clear();
        w.invoke_clear_range();
        assert!(matches!(log.borrow().as_slice(), [Command::Select(Selection::None)]));
        assert_eq!(w.get_range_banner(), "");
        assert_eq!(w.get_history_mode(), 0);
        assert_eq!(w.get_commit_files().row_count(), 0);
        assert!(!row(&w, 0).in_range);

        w.invoke_review_since(id("c1"));
        w.set_range_banner("Reviewing".into());
        w.invoke_select_commit(id("c2"));
        assert_eq!(w.get_range_banner(), "");
        assert_eq!(w.get_history_mode(), 1);
        assert!(!row(&w, 0).in_range);
    }

    #[test]
    fn range_none_keeps_banner_while_range_selected() {
        let w = window();
        let (_log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        w.invoke_review_since(id("c1"));
        w.set_range_banner("Reviewing".into());
        // Emitted when the Changes tab takes over the controller selection.
        apply_at(&w, Event::Range(None), NOW);
        assert_eq!(w.get_range_banner(), "Reviewing");
        w.set_history_mode(1);
        apply_at(&w, Event::Range(None), NOW);
        assert_eq!(w.get_range_banner(), "");
    }

    #[test]
    fn range_event_updates_head() {
        let w = window();
        let (_log, send) = recorder();
        wire(&w, send);
        let mut four = vec![commit("c4", "fourth", 5)];
        four.extend(three());
        apply_at(&w, history(four, false, vec![]), NOW);
        w.invoke_review_since(id("c1"));
        w.set_range_head(id("c3"));
        apply_at(
            &w,
            Event::Range(Some(RangeInfo { base: oid("c1"), head: oid("c4"), commit_count: 3, additions: 1, deletions: 0 })),
            NOW,
        );
        assert_eq!(w.get_range_head(), id("c4"));
        assert!(row(&w, 0).in_range);
        assert_eq!(w.get_range_banner(), "Reviewing 3 commits since c100000  +1 -0");
    }

    #[test]
    fn files_event_fills_strip_and_selects_first() {
        let w = window();
        apply_at(
            &w,
            Event::Files(vec![entry("a.rs", FileStatus::Added), entry("b.rs", FileStatus::Deleted)]),
            NOW,
        );
        let files = w.get_commit_files();
        assert_eq!(files.row_count(), 2);
        assert_eq!(files.row_data(1).unwrap().letter, "D");
        assert_eq!(w.get_commit_file(), "a.rs");
        assert!(files.row_data(0).unwrap().selected);
        // A refresh that still lists the selected file keeps it.
        w.set_commit_file("b.rs".into());
        apply_at(
            &w,
            Event::Files(vec![entry("a.rs", FileStatus::Added), entry("b.rs", FileStatus::Deleted)]),
            NOW,
        );
        assert_eq!(w.get_commit_file(), "b.rs");
        assert!(w.get_commit_files().row_data(1).unwrap().selected);
    }

    #[test]
    fn select_commit_file_sends_path() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        w.invoke_select_commit(id("c2"));
        apply_at(&w, Event::Files(vec![entry("a.rs", FileStatus::Added), entry("b.rs", FileStatus::Modified)]), NOW);
        log.borrow_mut().clear();
        w.invoke_select_commit_file("b.rs".into());
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Commit { id, path: Some(p) })] if *id == oid("c2") && p == "b.rs"
        ));
        assert!(w.get_commit_files().row_data(1).unwrap().selected);

        w.invoke_review_since(id("c1"));
        log.borrow_mut().clear();
        w.invoke_select_commit_file("a.rs".into());
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Range { base, head, path: Some(p), .. })]
                if *base == oid("c1") && *head == oid("c3") && p == "a.rs"
        ));
    }

    #[test]
    fn history_diff_titles() {
        let w = window();
        let (_log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        w.invoke_select_commit(id("c2"));
        apply_at(&w, Event::Diff(Some(text_view("a.rs", None, FileStatus::Modified))), NOW);
        assert_eq!(w.get_left_title(), "Parent");
        assert_eq!(w.get_right_title(), "c200000");
        assert_eq!(w.get_commit_file(), "a.rs");
        assert!(!w.get_hunk_actions());
        assert_eq!(w.get_diff_side(), -1);
        w.invoke_review_since(id("c1"));
        apply_at(&w, Event::Diff(Some(text_view("a.rs", None, FileStatus::Modified))), NOW);
        assert_eq!(w.get_left_title(), "c100000");
        assert_eq!(w.get_right_title(), "c300000");
    }

    #[test]
    fn tab_change_resends_tab_selection() {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        apply(&w, status(&[], &[("x.rs", FileStatus::Modified)]));
        apply_at(&w, history(three(), false, vec![]), NOW);
        w.invoke_select_change(1, "x.rs".into());
        w.invoke_select_commit(id("c2"));
        w.set_commit_file("a.rs".into());
        log.borrow_mut().clear();
        w.invoke_tab_changed(0);
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::Select(Selection::Change { side: ChangeSide::Unstaged, path })] if path == "x.rs"
        ));
        w.invoke_tab_changed(1);
        assert!(matches!(
            log.borrow().last(),
            Some(Command::Select(Selection::Commit { id, path: Some(p) })) if *id == oid("c2") && p == "a.rs"
        ));
    }

    #[test]
    fn repo_opened_clears_history() {
        let w = window();
        let (_log, send) = recorder();
        wire(&w, send);
        apply_at(&w, history(three(), false, vec![]), NOW);
        w.invoke_review_since(id("c1"));
        w.set_range_banner("Reviewing".into());
        w.set_commit_files(rows(&[entry("a.rs", FileStatus::Modified)], NO_SIDE));
        w.set_commit_file("a.rs".into());
        w.set_history_exhausted(true);
        apply(&w, Event::RepoOpened { name: "p".into(), path: PathBuf::from("/tmp/p"), recent: vec![] });
        assert_eq!(w.get_commits().row_count(), 0);
        assert_eq!(w.get_commit_files().row_count(), 0);
        assert_eq!(w.get_range_banner(), "");
        assert_eq!(w.get_history_mode(), 0);
        assert_eq!(w.get_range_base(), "");
        assert_eq!(w.get_commit_file(), "");
        assert!(!w.get_history_exhausted());
    }

    // ---- keyboard ----

    use slint::platform::{Key, WindowEvent};

    /// Presses and releases `text` on the window, with Command (Slint's
    /// `control`) held when `cmd`.
    fn press(w: &AppWindow, text: SharedString, cmd: bool) {
        let win = w.window();
        if cmd {
            win.dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        }
        win.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        win.dispatch_event(WindowEvent::KeyReleased { text });
        if cmd {
            win.dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
        }
    }

    /// A shown window with a repo open, an unstaged diff of a.rs and history.
    fn keyboard_window() -> (AppWindow, Rc<RefCell<Vec<Command>>>) {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        w.show().unwrap();
        w.set_has_repo(true);
        apply(&w, status(&[], &[("a.rs", FileStatus::Modified)]));
        apply_at(&w, history(three(), false, vec![]), NOW);
        apply(&w, Event::Diff(Some(text_view("a.rs", Some(ChangeSide::Unstaged), FileStatus::Modified))));
        log.borrow_mut().clear();
        (w, log)
    }

    #[test]
    fn up_down_move_commit_selection_in_history() {
        let (w, log) = keyboard_window();
        w.set_active_tab(1);
        log.borrow_mut().clear();
        press(&w, Key::DownArrow.into(), false); // nothing selected: newest
        assert_eq!(w.get_history_commit(), id("c3"));
        press(&w, Key::DownArrow.into(), false);
        assert_eq!(w.get_history_commit(), id("c2"));
        assert!(row(&w, 1).selected);
        press(&w, Key::DownArrow.into(), false);
        press(&w, Key::DownArrow.into(), false); // at the end: stays
        assert_eq!(w.get_history_commit(), id("c1"));
        press(&w, Key::UpArrow.into(), false);
        // Select(None) from the tab change aside, one commit select per move.
        let commits: Vec<Oid> = log
            .borrow()
            .iter()
            .filter_map(|c| match c {
                Command::Select(Selection::Commit { id, path: None }) => Some(*id),
                Command::Select(Selection::None) => None,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(commits, vec![oid("c3"), oid("c2"), oid("c1"), oid("c2")]);
        // The Changes selection is untouched.
        assert_eq!(w.get_selected_side(), -1);
    }

    #[test]
    fn file_shortcuts_only_on_changes_tab() {
        let (w, log) = keyboard_window();
        w.set_active_tab(1);
        log.borrow_mut().clear();
        press(&w, "s".into(), false);
        press(&w, "u".into(), false);
        press(&w, Key::Backspace.into(), true);
        // The diff pane still shows the unstaged a.rs until the controller answers.
        assert!(
            log.borrow().iter().all(|c| matches!(c, Command::Select(_))),
            "{:?}",
            log.borrow()
        );
        assert_eq!(w.get_discard_kind(), 0);

        w.set_active_tab(0);
        log.borrow_mut().clear();
        press(&w, "s".into(), false);
        assert!(log.borrow().iter().any(|c| matches!(c, Command::StageFile(p) if p == "a.rs")));
        press(&w, Key::Backspace.into(), true);
        assert_eq!(w.get_discard_kind(), 1);
    }

    // ---- git flow ----

    use crate::flow::{FlowConfig, FlowKind, FlowState, TagInfo};
    use crate::ui::Flow;

    /// Presses and releases `text` with Option and Command (Slint's `alt` and
    /// `control`) held.
    fn press_opt_cmd(w: &AppWindow, text: SharedString) {
        let win = w.window();
        win.dispatch_event(WindowEvent::KeyPressed { text: Key::Control.into() });
        win.dispatch_event(WindowEvent::KeyPressed { text: Key::Alt.into() });
        win.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        win.dispatch_event(WindowEvent::KeyReleased { text });
        win.dispatch_event(WindowEvent::KeyReleased { text: Key::Alt.into() });
        win.dispatch_event(WindowEvent::KeyReleased { text: Key::Control.into() });
    }

    fn flow_config() -> FlowConfig {
        FlowConfig {
            production: "main".into(),
            develop: "develop".into(),
            feature: "feature/".into(),
            release: "release/".into(),
            hotfix: "hotfix/".into(),
            version_tag: String::new(),
        }
    }

    /// Initialized, production `main`, on `feature/login-form`, nothing else open.
    fn flow_state() -> FlowState {
        FlowState {
            config: Some(flow_config()),
            draft: flow_config(),
            init_blocked: None,
            branches: vec!["develop".into(), "feature/login-form".into(), "main".into()],
            features: vec!["feature/login-form".into()],
            releases: vec![],
            hotfixes: vec![],
            current: Some((FlowKind::Feature, "feature/login-form".into())),
            latest_tag: None,
            push_blocked: None,
            push_default: false,
        }
    }

    /// A shown, wired window with a repo open and `state` applied.
    fn flow_window(state: FlowState) -> (AppWindow, Rc<RefCell<Vec<Command>>>) {
        let w = window();
        let (log, send) = recorder();
        wire(&w, send);
        w.show().unwrap();
        opened(&w, "/w/proj", &["/w/proj"]);
        apply(&w, Event::Flow(state));
        log.borrow_mut().clear();
        (w, log)
    }

    #[test]
    fn flow_event_fills_global() {
        let w = window();
        apply(&w, Event::Flow(flow_state()));
        let f = w.global::<Flow>();
        assert!(f.get_initialized());
        assert_eq!(
            (f.get_production().as_str(), f.get_develop().as_str(), f.get_feature_prefix().as_str()),
            ("main", "develop", "feature/")
        );
        assert_eq!((f.get_current_kind(), f.get_current_branch().as_str()), (0, "feature/login-form"));
        assert_eq!(f.get_latest_tag(), "No tags yet");
        assert_eq!((f.get_release().as_str(), f.get_hotfix().as_str()), ("", ""));
        assert_eq!(f.get_features().row_count(), 1);
        assert_eq!(f.get_actions().row_count(), 4);
        assert!(f.get_actions().iter().all(|a| !a.current));
    }

    #[test]
    fn latest_tag_text_has_name_and_age() {
        let w = window();
        let mut state = flow_state();
        state.latest_tag = Some(TagInfo { name: "1.1.0".into(), time: NOW - 2 * 86400 });
        apply_at(&w, Event::Flow(state), NOW);
        assert_eq!(w.global::<Flow>().get_latest_tag(), "Latest tag: 1.1.0, 2d ago");
    }

    #[test]
    fn opt_cmd_f_opens_next_action_with_finish_first() {
        let (w, _log) = flow_window(flow_state());
        press_opt_cmd(&w, "f".into());
        assert!(w.get_switcher_open());
        assert_eq!(w.get_switcher_mode(), SWITCHER_FLOW);
        assert_eq!(w.get_switcher_items().row_data(0).unwrap().key, "finish:feature:feature/login-form");
        assert_eq!(w.get_switcher_index(), 0);
    }

    #[test]
    fn opt_cmd_f_accepts_option_character() {
        let (w, _log) = flow_window(flow_state());
        press_opt_cmd(&w, "\u{192}".into());
        assert!(w.get_switcher_open());
        assert_eq!(w.get_switcher_mode(), SWITCHER_FLOW);
    }

    #[test]
    fn opt_cmd_f_ignored_while_a_flow_dialog_shows() {
        let (w, _log) = flow_window(flow_state());
        w.global::<Flow>().set_dialog(2);
        press_opt_cmd(&w, "f".into());
        assert!(!w.get_switcher_open());
    }

    #[test]
    fn next_action_filter_narrows_rows() {
        let (w, _log) = flow_window(flow_state());
        press_opt_cmd(&w, "f".into());
        w.invoke_switcher_filter("hot".into());
        assert_eq!(w.get_switcher_items().row_count(), 1);
        assert_eq!(w.get_switcher_items().row_data(0).unwrap().key, "start:hotfix");
    }

    #[test]
    fn return_on_next_action_row_opens_its_dialog() {
        let (w, log) = flow_window(flow_state());
        press_opt_cmd(&w, "f".into());
        press(&w, Key::Return.into(), false);
        assert!(!w.get_switcher_open());
        let f = w.global::<Flow>();
        assert_eq!((f.get_dialog(), f.get_kind()), (3, 0));
        assert!(matches!(log.borrow().as_slice(), [Command::FlowPlan(r)] if r.branch == "feature/login-form"));
    }

    #[test]
    fn uninitialized_offers_only_init() {
        let mut state = flow_state();
        state.config = None;
        state.current = None;
        let (w, _log) = flow_window(state);
        assert!(!w.global::<Flow>().get_initialized());
        press_opt_cmd(&w, "f".into());
        let items = w.get_switcher_items();
        assert_eq!(items.row_count(), 1);
        assert_eq!(items.row_data(0).unwrap().key, "init");
    }

    #[test]
    fn uninitialized_shows_draft_names() {
        let mut state = flow_state();
        state.config = None;
        state.draft.production = "master".into();
        state.draft.hotfix = "fix/".into();
        let w = window();
        apply(&w, Event::Flow(state));
        let f = w.global::<Flow>();
        assert_eq!((f.get_production().as_str(), f.get_hotfix_prefix().as_str()), ("master", "fix/"));
    }

    #[test]
    fn action_opens_dialog_and_sends_nothing() {
        let (w, log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("start:release".into());
        assert_eq!((f.get_dialog(), f.get_kind()), (2, 1));
        f.invoke_action("init".into());
        assert_eq!(f.get_dialog(), 1);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn repo_opened_resets_flow() {
        let (w, _log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.set_dialog(3);
        f.set_notice("Finished feature/login-form".into());
        opened(&w, "/w/other", &["/w/other"]);
        assert!(!f.get_initialized());
        assert_eq!(f.get_dialog(), 0);
        assert_eq!(f.get_notice(), "");
        assert_eq!(f.get_current_kind(), -1);
        assert!(f.get_delete_branch());
        assert_eq!(f.get_actions().row_count(), 0);
    }

    /// Not initialized: default draft names, only `main` exists.
    fn uninitialized_state() -> FlowState {
        let mut state = flow_state();
        state.config = None;
        state.current = None;
        state.features = vec![];
        state.branches = vec!["main".into()];
        state
    }

    /// Types `text` one character at a time into the focused element.
    fn type_text(w: &AppWindow, text: &str) {
        for c in text.chars() {
            press(w, c.to_string().into(), false);
        }
    }

    #[test]
    fn init_dialog_prefills_and_submits_config() {
        let (w, log) = flow_window(uninitialized_state());
        let f = w.global::<Flow>();
        f.invoke_action("init".into());
        assert_eq!(
            (f.get_dialog(), f.get_init_production().as_str(), f.get_init_develop().as_str(), f.get_init_feature().as_str()),
            (1, "main", "develop", "feature/")
        );
        assert_eq!((f.get_init_release().as_str(), f.get_init_hotfix().as_str(), f.get_init_tag().as_str()), ("release/", "hotfix/", ""));
        assert!(f.get_init_develop_new());
        f.set_init_tag("v".into());
        f.invoke_submit_init();
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::FlowInit(c)] if c.production == "main" && c.develop == "develop" && c.version_tag == "v"
        ));
    }

    #[test]
    fn init_action_clears_an_old_error() {
        let (w, _log) = flow_window(uninitialized_state());
        let f = w.global::<Flow>();
        f.set_error("could not write config".into());
        f.invoke_action("init".into());
        assert_eq!(f.get_error(), "");
    }

    #[test]
    fn init_develop_hint_tracks_existing_branches() {
        let (w, _log) = flow_window(uninitialized_state());
        let f = w.global::<Flow>();
        f.invoke_action("init".into());
        f.invoke_init_develop_edited("main".into());
        assert!(!f.get_init_develop_new());
        f.invoke_init_develop_edited("dev".into());
        assert!(f.get_init_develop_new());
    }

    #[test]
    fn start_dialog_checks_name_and_submits() {
        let (w, log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("start:release".into());
        f.invoke_name_edited("1.2.0".into());
        assert_eq!(f.get_name(), "1.2.0");
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::FlowCheckStart { kind: FlowKind::Release, name }] if name == "1.2.0"
        ));
        apply(&w, Event::FlowStartCheck { kind: FlowKind::Release, name: "1.2".into(), error: Some("stale".into()) });
        assert_eq!(f.get_start_error(), "");
        apply(&w, Event::FlowStartCheck { kind: FlowKind::Feature, name: "1.2.0".into(), error: Some("other kind".into()) });
        assert_eq!(f.get_start_error(), "");
        apply(&w, Event::FlowStartCheck {
            kind: FlowKind::Release,
            name: "1.2.0".into(),
            error: Some("Tag 1.2.0 already exists.".into()),
        });
        assert_eq!(f.get_start_error(), "Tag 1.2.0 already exists.");
        apply(&w, Event::FlowStartCheck { kind: FlowKind::Release, name: "1.2.0".into(), error: None });
        assert_eq!(f.get_start_error(), "");
        f.invoke_submit_start();
        assert!(matches!(
            log.borrow().last(),
            Some(Command::FlowStart { kind: FlowKind::Release, name }) if name == "1.2.0"
        ));
    }

    #[test]
    fn start_check_enter_a_name_shows_as_empty_error() {
        let (w, _log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("start:feature".into());
        f.invoke_name_edited("".into());
        apply(&w, Event::FlowStartCheck { kind: FlowKind::Feature, name: "".into(), error: Some("Enter a name.".into()) });
        assert_eq!(f.get_start_error(), "");
    }

    #[test]
    fn start_action_clears_name_and_errors() {
        let (w, _log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.set_name("old".into());
        f.set_start_error("feature/old already exists.".into());
        f.set_error("failed".into());
        f.invoke_action("start:feature".into());
        assert_eq!((f.get_name().as_str(), f.get_start_error().as_str(), f.get_error().as_str()), ("", "", ""));
    }

    #[test]
    fn flow_done_closes_dialog_and_sets_notice() {
        let (w, _log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("start:feature".into());
        apply(&w, Event::FlowDone("Finished feature/x. Merged into develop.".into()));
        assert_eq!((f.get_dialog(), f.get_notice().as_str()), (0, "Finished feature/x. Merged into develop."));
    }

    #[test]
    fn flow_failed_keeps_dialog_open_with_error() {
        let (w, _log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("start:feature".into());
        apply(&w, Event::FlowFailed("feature/x already exists.".into()));
        assert_eq!((f.get_dialog(), f.get_error().as_str()), (2, "feature/x already exists."));
    }

    #[test]
    fn cancel_closes_dialog() {
        let (w, _log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("start:feature".into());
        f.invoke_cancel();
        assert_eq!(f.get_dialog(), 0);
    }

    #[test]
    fn error_event_shows_in_open_flow_dialog() {
        let (w, _log) = flow_window(uninitialized_state());
        let f = w.global::<Flow>();
        apply(&w, Event::Error("could not read config".into()));
        assert_eq!(f.get_error(), "");
        f.invoke_action("init".into());
        apply(&w, Event::Error("could not read config".into()));
        assert_eq!((f.get_dialog(), f.get_error().as_str()), (1, "could not read config"));
        assert_eq!(w.get_error_text(), "could not read config");
    }

    #[test]
    fn typing_in_start_dialog_checks_name_and_return_starts() {
        let (w, log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("start:feature".into());
        type_text(&w, "x");
        assert_eq!(f.get_name(), "x");
        assert!(matches!(
            log.borrow().last(),
            Some(Command::FlowCheckStart { kind: FlowKind::Feature, name }) if name == "x"
        ));
        press(&w, Key::Return.into(), false);
        assert!(matches!(
            log.borrow().last(),
            Some(Command::FlowStart { kind: FlowKind::Feature, name }) if name == "x"
        ));
    }

    #[test]
    fn return_does_nothing_while_start_is_disabled() {
        let (w, log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("start:feature".into());
        press(&w, Key::Return.into(), false);
        type_text(&w, "x");
        apply(&w, Event::FlowStartCheck { kind: FlowKind::Feature, name: "x".into(), error: Some("feature/x already exists.".into()) });
        press(&w, Key::Return.into(), false);
        assert!(!log.borrow().iter().any(|c| matches!(c, Command::FlowStart { .. })));
        assert_eq!(f.get_dialog(), 2);
    }

    #[test]
    fn return_in_init_dialog_initializes() {
        let (w, log) = flow_window(uninitialized_state());
        w.global::<Flow>().invoke_action("init".into());
        press(&w, Key::Return.into(), false);
        assert!(matches!(log.borrow().as_slice(), [Command::FlowInit(c)] if c.develop == "develop"));
    }

    #[test]
    fn return_does_nothing_while_init_is_disabled() {
        let (w, log) = flow_window(uninitialized_state());
        let f = w.global::<Flow>();
        f.invoke_action("init".into());
        f.set_init_develop("main".into());
        press(&w, Key::Return.into(), false);
        f.set_init_develop("develop".into());
        f.set_init_feature("".into());
        press(&w, Key::Return.into(), false);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn escape_cancels_dialog_and_gives_shortcuts_back() {
        let (w, _log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("start:feature".into());
        press(&w, Key::Escape.into(), false);
        assert_eq!(f.get_dialog(), 0);
        press_opt_cmd(&w, "f".into());
        assert!(w.get_switcher_open());
    }

    #[test]
    fn dialog_from_next_action_focuses_its_field() {
        let (w, log) = flow_window(flow_state());
        press_opt_cmd(&w, "f".into());
        w.invoke_switcher_filter("hot".into());
        press(&w, Key::Return.into(), false);
        let f = w.global::<Flow>();
        assert_eq!((f.get_dialog(), f.get_kind()), (2, 2));
        type_text(&w, "1");
        assert!(matches!(
            log.borrow().last(),
            Some(Command::FlowCheckStart { kind: FlowKind::Hotfix, name }) if name == "1"
        ));
    }

    // ---- git flow: finish, push and notice ----

    use crate::flow_finish::{Blocker, FinishPlan, FinishRequest, Step};

    /// Tag prefix `v`, `release/1.2.0` open and checked out, push on by default.
    fn release_state() -> FlowState {
        let mut state = flow_state();
        if let Some(c) = state.config.as_mut() {
            c.version_tag = "v".into();
        }
        state.branches.push("release/1.2.0".into());
        state.releases = vec!["release/1.2.0".into()];
        state.current = Some((FlowKind::Release, "release/1.2.0".into()));
        state.push_default = true;
        state
    }

    /// Two open features, on `feature/login-form`.
    fn two_features_state() -> FlowState {
        let mut state = flow_state();
        state.features = vec!["feature/login-form".into(), "feature/other".into()];
        state.branches.push("feature/other".into());
        state
    }

    fn step(text: &str, note: &str, remote: bool) -> Step {
        Step { text: text.into(), note: note.into(), remote }
    }

    fn feature_request(branch: &str) -> FinishRequest {
        FinishRequest {
            kind: FlowKind::Feature,
            branch: branch.into(),
            tag: String::new(),
            message: String::new(),
            delete_branch: true,
            push: false,
        }
    }

    fn release_request() -> FinishRequest {
        FinishRequest {
            kind: FlowKind::Release,
            branch: "release/1.2.0".into(),
            tag: "v1.2.0".into(),
            message: "Release 1.2.0".into(),
            delete_branch: true,
            push: true,
        }
    }

    /// The seven steps of a release finish with push; the last two run on origin.
    fn release_steps() -> Vec<Step> {
        vec![
            step("Merge release/1.2.0 into main", "5 commits", false),
            step("Tag main as v1.2.0", "", false),
            step("Merge release/1.2.0 back into develop", "", false),
            step("Delete release/1.2.0", "", false),
            step("Switch to develop", "", false),
            step("Push main, develop and tag v1.2.0 to origin", "", true),
            step("Delete release/1.2.0 on origin", "", true),
        ]
    }

    fn feature_plan(blocker: Option<Blocker>) -> FinishPlan {
        FinishPlan {
            steps: vec![
                step("Merge feature/login-form into develop", "3 commits", false),
                step("Delete feature/login-form", "", false),
                step("Switch to develop", "", false),
            ],
            blocker,
        }
    }

    /// A Finish Feature dialog on `feature/login-form` with a clean plan shown.
    fn finish_feature_window(blocker: Option<Blocker>) -> (AppWindow, Rc<RefCell<Vec<Command>>>) {
        let (w, log) = flow_window(flow_state());
        w.global::<Flow>().invoke_action("finish:feature".into());
        apply(&w, Event::FlowPlan { request: feature_request("feature/login-form"), plan: feature_plan(blocker) });
        log.borrow_mut().clear();
        (w, log)
    }

    #[test]
    fn finish_action_fills_defaults_and_requests_plan() {
        let (w, log) = flow_window(release_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:release:release/1.2.0".into());
        assert_eq!((f.get_dialog(), f.get_kind()), (3, 1));
        assert_eq!(f.get_branch(), "release/1.2.0");
        assert_eq!(f.get_branch_options().iter().collect::<Vec<_>>(), vec![SharedString::from("release/1.2.0")]);
        assert_eq!((f.get_tag().as_str(), f.get_message().as_str()), ("v1.2.0", "Release 1.2.0"));
        assert!(f.get_delete_branch() && f.get_push());
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::FlowPlan(r)] if r.branch == "release/1.2.0" && r.tag == "v1.2.0"
                && r.message == "Release 1.2.0" && r.delete_branch && r.push
        ));
    }

    #[test]
    fn finish_hotfix_message_says_hotfix() {
        let mut state = flow_state();
        state.hotfixes = vec!["hotfix/1.2.1".into()];
        let (w, log) = flow_window(state);
        let f = w.global::<Flow>();
        f.invoke_action("finish:hotfix".into());
        assert_eq!((f.get_kind(), f.get_branch().as_str()), (2, "hotfix/1.2.1"));
        assert_eq!((f.get_tag().as_str(), f.get_message().as_str()), ("1.2.1", "Hotfix 1.2.1"));
        // push_default is false in flow_state().
        assert!(!f.get_push());
        assert!(matches!(
            log.borrow().as_slice(),
            [Command::FlowPlan(r)] if r.kind == FlowKind::Hotfix && r.message == "Hotfix 1.2.1" && !r.push
        ));
    }

    #[test]
    fn finish_feature_defaults_to_current_branch_without_tag_or_push() {
        let mut state = two_features_state();
        state.push_default = true;
        let (w, log) = flow_window(state);
        let f = w.global::<Flow>();
        f.invoke_action("finish:feature".into());
        assert_eq!((f.get_branch().as_str(), f.get_tag().as_str(), f.get_push()), ("feature/login-form", "", false));
        assert_eq!(f.get_message(), "");
        assert_eq!(f.get_branch_options().row_count(), 2);
        assert!(matches!(log.borrow().as_slice(), [Command::FlowPlan(r)] if *r == feature_request("feature/login-form")));
    }

    #[test]
    fn finish_feature_off_a_feature_branch_picks_the_first_option() {
        let mut state = two_features_state();
        state.features = vec!["feature/other".into(), "feature/login-form".into()];
        state.current = None;
        let (w, _log) = flow_window(state);
        let f = w.global::<Flow>();
        f.invoke_action("finish:feature".into());
        assert_eq!(f.get_branch(), "feature/other");
        f.invoke_action("finish:feature:feature/login-form".into());
        assert_eq!(f.get_branch(), "feature/login-form");
    }

    #[test]
    fn finish_action_clears_old_plan_blocker_and_error() {
        let (w, _log) = finish_feature_window(Some(Blocker::Dirty(2)));
        let f = w.global::<Flow>();
        f.set_error("failed".into());
        f.invoke_action("finish:feature".into());
        assert_eq!(f.get_steps().row_count(), 0);
        assert_eq!((f.get_blocker_level(), f.get_blocker_title().as_str(), f.get_error().as_str()), (0, "", ""));
    }

    #[test]
    fn push_tick_off_when_push_is_blocked() {
        let mut state = release_state();
        state.push_blocked = Some("No origin remote.".into());
        let (w, log) = flow_window(state);
        let f = w.global::<Flow>();
        f.invoke_action("finish:release".into());
        assert!(!f.get_push());
        assert_eq!(f.get_push_blocked(), "No origin remote.");
        assert!(matches!(log.borrow().as_slice(), [Command::FlowPlan(r)] if !r.push));
    }

    #[test]
    fn push_is_never_requested_when_blocked_or_for_a_feature() {
        let mut state = release_state();
        state.push_blocked = Some("No origin remote.".into());
        let (w, log) = flow_window(state);
        let f = w.global::<Flow>();
        f.invoke_action("finish:release".into());
        f.set_push(true);
        f.invoke_plan_changed();
        assert!(matches!(log.borrow().last(), Some(Command::FlowPlan(r)) if !r.push));
        f.invoke_action("finish:feature".into());
        f.set_push(true);
        f.invoke_plan_changed();
        assert!(matches!(log.borrow().last(), Some(Command::FlowPlan(r)) if !r.push && r.tag.is_empty()));
    }

    #[test]
    fn plan_event_fills_steps_and_blocker() {
        let (w, _log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:feature".into());
        let req = feature_request("feature/login-form");
        apply(&w, Event::FlowPlan {
            request: req.clone(),
            plan: FinishPlan {
                steps: vec![step("Merge feature/x into develop", "3 commits", false)],
                blocker: Some(Blocker::Dirty(13)),
            },
        });
        let first = f.get_steps().row_data(0).unwrap();
        assert_eq!((first.text.as_str(), first.note.as_str(), first.state), ("Merge feature/x into develop", "3 commits", 0));
        assert_eq!((f.get_blocker_level(), f.get_blocker_title().as_str()), (1, "13 uncommitted changes"));
        assert_eq!(f.get_blocker_body(), Blocker::Dirty(13).body());
        assert_eq!(f.get_blocker_files().row_count(), 0);
    }

    #[test]
    fn clean_plan_clears_an_earlier_blocker() {
        let (w, _log) = finish_feature_window(Some(Blocker::Dirty(2)));
        let f = w.global::<Flow>();
        assert_eq!(f.get_blocker_level(), 1);
        apply(&w, Event::FlowPlan { request: feature_request("feature/login-form"), plan: feature_plan(None) });
        assert_eq!((f.get_blocker_level(), f.get_blocker_title().as_str()), (0, ""));
        assert_eq!(f.get_steps().row_count(), 3);
    }

    #[test]
    fn conflict_plan_is_level_two_with_files() {
        let conflict = Blocker::Conflict {
            target: "develop".into(),
            branch: "feature/login-form".into(),
            files: vec!["src/controller.rs".into(), "ui/app.slint".into()],
        };
        let (w, _log) = finish_feature_window(Some(conflict));
        let f = w.global::<Flow>();
        assert_eq!(f.get_blocker_level(), 2);
        assert_eq!(f.get_blocker_title(), "Cannot merge cleanly");
        assert_eq!(f.get_blocker_files().row_count(), 2);
        assert_eq!(f.get_blocker_files().row_data(1).unwrap(), "ui/app.slint");
    }

    #[test]
    fn collision_plan_is_level_one_with_files() {
        let (w, _log) = finish_feature_window(Some(Blocker::Collision(vec!["build.log".into()])));
        let f = w.global::<Flow>();
        assert_eq!(f.get_blocker_level(), 1);
        assert_eq!(f.get_blocker_files().row_count(), 1);
    }

    #[test]
    fn stale_plan_is_ignored() {
        let (w, _log) = flow_window(two_features_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:feature".into());
        apply(&w, Event::FlowPlan { request: feature_request("feature/other"), plan: feature_plan(Some(Blocker::Busy)) });
        assert_eq!(f.get_steps().row_count(), 0);
        assert_eq!(f.get_blocker_level(), 0);
        // Same branch but a different tick: also stale.
        let mut req = feature_request("feature/login-form");
        req.delete_branch = false;
        apply(&w, Event::FlowPlan { request: req, plan: feature_plan(None) });
        assert_eq!(f.get_steps().row_count(), 0);
    }

    #[test]
    fn plan_for_a_closed_dialog_is_ignored() {
        let (w, _log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:feature".into());
        f.invoke_cancel();
        apply(&w, Event::FlowPlan { request: feature_request("feature/login-form"), plan: feature_plan(None) });
        assert_eq!(f.get_steps().row_count(), 0);
    }

    #[test]
    fn ticks_and_picker_resend_plan() {
        let (w, log) = flow_window(two_features_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:feature".into());
        f.set_delete_branch(false);
        f.invoke_plan_changed();
        assert!(matches!(log.borrow().last(), Some(Command::FlowPlan(r)) if !r.delete_branch));

        f.set_branch("feature/other".into());
        f.set_tag("stray".into());
        f.set_message("stray".into());
        f.invoke_plan_changed();
        assert_eq!((f.get_tag().as_str(), f.get_message().as_str()), ("", ""));
        assert!(matches!(
            log.borrow().last(),
            Some(Command::FlowPlan(r)) if r.branch == "feature/other" && r.tag.is_empty() && !r.delete_branch
        ));
    }

    #[test]
    fn tag_edits_keep_the_typed_tag() {
        let (w, log) = flow_window(release_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:release".into());
        f.set_tag("v1.2.0-rc".into());
        f.invoke_plan_changed();
        assert_eq!(f.get_tag(), "v1.2.0-rc");
        assert!(matches!(log.borrow().last(), Some(Command::FlowPlan(r)) if r.tag == "v1.2.0-rc"));
    }

    #[test]
    fn typing_a_tag_requests_a_plan_per_edit() {
        let (w, log) = flow_window(release_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:release".into());
        // The Tag field has the focus with the caret after the default.
        type_text(&w, "x");
        assert_eq!(f.get_tag(), "v1.2.0x");
        assert!(matches!(log.borrow().last(), Some(Command::FlowPlan(r)) if r.tag == "v1.2.0x"));
    }

    #[test]
    fn submit_finish_sends_request() {
        let (w, log) = flow_window(release_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:release".into());
        let req = release_request();
        apply(&w, Event::FlowPlan { request: req.clone(), plan: FinishPlan { steps: release_steps(), blocker: None } });
        f.invoke_submit_finish();
        assert!(matches!(log.borrow().last(), Some(Command::FlowFinish(r)) if *r == req));
    }

    #[test]
    fn submit_finish_refused_while_blocked_or_unplanned() {
        let (w, log) = finish_feature_window(Some(Blocker::Dirty(1)));
        let f = w.global::<Flow>();
        f.invoke_submit_finish();
        f.invoke_action("finish:feature".into());
        log.borrow_mut().clear();
        // No plan has arrived for this request yet.
        f.invoke_submit_finish();
        assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    }

    #[test]
    fn return_finishes_when_allowed() {
        let (w, log) = finish_feature_window(None);
        press(&w, Key::Return.into(), false);
        assert!(matches!(log.borrow().last(), Some(Command::FlowFinish(r)) if *r == feature_request("feature/login-form")));
    }

    #[test]
    fn return_and_escape_close_a_conflict() {
        let conflict = Blocker::Conflict { target: "develop".into(), branch: "feature/login-form".into(), files: vec![] };
        let (w, log) = finish_feature_window(Some(conflict.clone()));
        let f = w.global::<Flow>();
        press(&w, Key::Return.into(), false);
        assert_eq!(f.get_dialog(), 0);
        f.invoke_action("finish:feature".into());
        apply(&w, Event::FlowPlan { request: feature_request("feature/login-form"), plan: feature_plan(Some(conflict)) });
        press(&w, Key::Escape.into(), false);
        assert_eq!(f.get_dialog(), 0);
        assert!(!log.borrow().iter().any(|c| matches!(c, Command::FlowFinish(_))));
    }

    #[test]
    fn return_does_nothing_while_finish_is_blocked() {
        let (w, log) = finish_feature_window(Some(Blocker::Dirty(3)));
        press(&w, Key::Return.into(), false);
        assert!(log.borrow().is_empty(), "{:?}", log.borrow());
        assert_eq!(w.global::<Flow>().get_dialog(), 3);
    }

    #[test]
    fn shortcuts_are_ignored_behind_a_finish_dialog() {
        let (w, log) = keyboard_window();
        apply(&w, Event::Flow(flow_state()));
        w.global::<Flow>().invoke_action("finish:feature".into());
        log.borrow_mut().clear();
        press(&w, "s".into(), false);
        press(&w, "u".into(), false);
        press(&w, "k".into(), true);
        assert!(log.borrow().is_empty(), "{:?}", log.borrow());
        assert!(!w.get_switcher_open());

        // The dialog lost the focus to the window shortcuts (closing a
        // discard prompt focuses them): still nothing behind the dialog acts.
        w.set_discard_kind(1);
        slint::platform::update_timers_and_animations();
        w.set_discard_kind(0);
        slint::platform::update_timers_and_animations();
        press(&w, "s".into(), false);
        press(&w, "u".into(), false);
        press(&w, "k".into(), true);
        assert!(log.borrow().is_empty(), "{:?}", log.borrow());
        assert!(!w.get_switcher_open());
        assert_eq!(w.global::<Flow>().get_dialog(), 3);
    }

    #[test]
    fn pushing_then_failed_then_retry_and_close() {
        let (w, log) = flow_window(release_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:release".into());
        apply(&w, Event::FlowPlan { request: release_request(), plan: FinishPlan { steps: release_steps(), blocker: None } });
        apply(&w, Event::FlowPushing);
        assert_eq!(f.get_dialog(), 4);
        let states: Vec<i32> = f.get_steps().iter().map(|s| s.state).collect();
        assert_eq!(states, vec![1, 1, 1, 1, 1, 2, 2]);
        apply(&w, Event::FlowPushFailed("rejected".into()));
        assert_eq!((f.get_dialog(), f.get_push_reason().as_str()), (5, "rejected"));
        let states: Vec<i32> = f.get_steps().iter().map(|s| s.state).collect();
        assert_eq!(states, vec![1, 1, 1, 1, 1, 3, 3]);
        f.invoke_retry_push();
        assert!(matches!(log.borrow().last(), Some(Command::FlowRetryPush)));
        apply(&w, Event::FlowPushing);
        assert_eq!(f.get_dialog(), 4);
        assert_eq!(f.get_steps().row_data(6).unwrap().state, 2);
        apply(&w, Event::FlowPushFailed("main [rejected] (fetch first)".into()));
        f.invoke_close_push_failed();
        assert!(matches!(log.borrow().last(), Some(Command::FlowDropPush)));
    }

    #[test]
    fn retry_clears_an_old_error() {
        let (w, _log) = flow_window(release_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:release".into());
        apply(&w, Event::FlowPushing);
        apply(&w, Event::FlowPushFailed("rejected".into()));
        apply(&w, Event::Error("could not refresh".into()));
        assert_eq!(f.get_error(), "could not refresh");
        f.invoke_retry_push();
        assert_eq!(f.get_error(), "");
    }

    #[test]
    fn pushing_ignores_escape_and_return() {
        let (w, log) = flow_window(release_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:release".into());
        apply(&w, Event::FlowPlan { request: release_request(), plan: FinishPlan { steps: release_steps(), blocker: None } });
        apply(&w, Event::FlowPushing);
        log.borrow_mut().clear();
        press(&w, Key::Escape.into(), false);
        press(&w, Key::Return.into(), false);
        press(&w, "s".into(), false);
        assert_eq!(f.get_dialog(), 4);
        assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    }

    #[test]
    fn push_failed_escape_closes_and_return_retries() {
        let (w, log) = flow_window(release_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:release".into());
        apply(&w, Event::FlowPushing);
        apply(&w, Event::FlowPushFailed("rejected".into()));
        log.borrow_mut().clear();
        press(&w, Key::Return.into(), false);
        assert!(matches!(log.borrow().as_slice(), [Command::FlowRetryPush]));
        press(&w, Key::Escape.into(), false);
        assert!(matches!(log.borrow().last(), Some(Command::FlowDropPush)));
    }

    #[test]
    fn repo_opened_closes_a_push_dialog() {
        let (w, _log) = flow_window(release_state());
        let f = w.global::<Flow>();
        f.invoke_action("finish:release".into());
        apply(&w, Event::FlowPushing);
        opened(&w, "/w/other", &["/w/other"]);
        assert_eq!(f.get_dialog(), 0);
        apply(&w, Event::Flow(release_state()));
        f.invoke_action("finish:release".into());
        apply(&w, Event::FlowPushing);
        apply(&w, Event::FlowPushFailed("rejected".into()));
        opened(&w, "/w/other", &["/w/other"]);
        assert_eq!((f.get_dialog(), f.get_push_reason().as_str()), (0, ""));
        assert_eq!(f.get_steps().row_count(), 0);
    }

    #[test]
    fn notice_is_dismissed_and_cleared_by_errors() {
        let (w, _log) = flow_window(flow_state());
        let f = w.global::<Flow>();
        apply(&w, Event::FlowDone("Finished feature/x. Merged into develop.".into()));
        assert_eq!(f.get_notice(), "Finished feature/x. Merged into develop.");
        f.invoke_dismiss_notice();
        assert_eq!(f.get_notice(), "");
        apply(&w, Event::FlowDone("x".into()));
        assert_eq!(f.get_notice(), "x");
        apply(&w, Event::Error("boom".into()));
        assert_eq!(f.get_notice(), "");
        assert_eq!(w.get_error_text(), "boom");
    }
}
