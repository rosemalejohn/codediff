//! Maps controller Events onto `AppWindow` properties and UI callbacks onto Commands.
use crate::controller::{Command, DiffView, Event, RangeInfo, Selection};
use crate::history::relative_time;
use crate::highlight::{StyledCell, StyledRow};
use crate::model::{
    file_fingerprint, hunk_fingerprint, ChangeSide, CommitInfo, DiffBody, FileEntry, FileStatus, LineKind,
};
use crate::ui::{AppWindow, CellData, CommitRow, FileRow, RowData, SegData};
use git2::Oid;
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};
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

const DISCARD_NONE: i32 = 0;
const DISCARD_FILE: i32 = 1;
const DISCARD_HUNK: i32 = 2;

// Row heights in logical px; keep in sync with Theme.diff-line-height and
// Theme.diff-header-height in ui/theme.slint.
const LINE_HEIGHT: f32 = 18.0;
const HEADER_HEIGHT: f32 = 30.0;

const HIST_NONE: i32 = 0;
const HIST_COMMIT: i32 = 1;
const HIST_RANGE: i32 = 2;

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
        Event::RepoOpened { name, recent, .. } => {
            window.set_repo_name(name.into());
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
        Event::Error(msg) => window.set_error_text(msg.into()),
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
    w.set_diff_scroll_y(0.0);
    send(Command::Select(Selection::Commit { id: oid, path: None }));
}

/// `follows_head`: the controller keeps `head` at the current HEAD; the `head`
/// sent here is only what the UI shows until the controller's Range event.
fn select_range(w: &AppWindow, send: &impl Fn(Command), base: &str, head: &str, follows_head: bool) {
    let (Some(b), Some(h)) = (parse_oid(base), parse_oid(head)) else { return };
    set_history_selection(w, HIST_RANGE, "", base, head);
    w.set_range_follows_head(follows_head);
    w.set_diff_scroll_y(0.0);
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
    window.set_diff_scroll_y(0.0);
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

    let y = if same {
        let min_y = -(content_height - window.get_diff_visible_height()).max(0.0);
        window.get_diff_scroll_y().clamp(min_y, 0.0)
    } else {
        0.0
    };
    window.set_diff_scroll_y(y);
}

fn row_data(row: &StyledRow, fingerprints: &[String]) -> RowData {
    match row {
        StyledRow::HunkHeader { hunk_index, header } => RowData {
            is_header: true,
            hunk_index: *hunk_index as i32,
            header: header.as_str().into(),
            fingerprint: fingerprints
                .get(*hunk_index)
                .map(|f| f.as_str().into())
                .unwrap_or_default(),
            left: CellData::default(),
            right: CellData::default(),
        },
        StyledRow::Line { left, right } => RowData {
            is_header: false,
            hunk_index: -1,
            header: SharedString::new(),
            fingerprint: SharedString::new(),
            left: cell_data(left.as_ref()),
            right: cell_data(right.as_ref()),
        },
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

    let s = send.clone();
    let weak = window.as_weak();
    window.on_stage_file(move || {
        let Some(w) = weak.upgrade() else { return };
        if let Some(path) = shown_path(&w, SIDE_UNSTAGED) {
            s(Command::StageFile(path));
        }
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
        w.set_diff_scroll_y(0.0);
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
        w.set_diff_scroll_y(0.0);
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
            path: e.path.as_str().into(),
            letter: e.status.letter().to_string().into(),
            side,
            selected: false,
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
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
            },
            rows: vec![
                StyledRow::HunkHeader { hunk_index: 0, header: h.header.clone() },
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
}
