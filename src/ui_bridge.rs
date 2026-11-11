//! Maps controller Events onto `AppWindow` properties and UI callbacks onto Commands.
use crate::controller::{Command, Event, Selection};
use crate::model::{ChangeSide, FileEntry};
use crate::ui::{AppWindow, FileRow};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const SIDE_STAGED: i32 = 0;
const SIDE_UNSTAGED: i32 = 1;
const NO_SIDE: i32 = -1;

/// Applies one controller event to the window. Must run on the UI thread.
pub fn apply(window: &AppWindow, ev: Event) {
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
        // History, files, range and diff views arrive in later tasks.
        Event::History { .. } | Event::Files(_) | Event::Range(_) | Event::Diff(_) => {}
    }
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
        let change_side = if side == SIDE_STAGED {
            ChangeSide::Staged
        } else {
            ChangeSide::Unstaged
        };
        w.set_selected_side(side);
        w.set_selected_path(path.clone());
        mark_selection(&w);
        s(Command::Select(Selection::Change {
            side: change_side,
            path: path.into(),
        }));
    });

    let weak = window.as_weak();
    window.on_dismiss_error(move || {
        if let Some(w) = weak.upgrade() {
            w.set_error_text(SharedString::new());
        }
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

    #[test]
    fn set_recent_fills_list() {
        let w = window();
        set_recent(&w, &[PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(w.get_recent().row_count(), 2);
    }
}
