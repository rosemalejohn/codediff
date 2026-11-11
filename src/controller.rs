//! Background worker that owns the repository and turns Commands into Events.
use crate::diff::{diff_stats, file_diff, files_in_target};
use crate::highlight::{Highlighter, StyledRow};
use crate::history::list_commits;
use crate::model::{
    file_fingerprint, hunk_fingerprint, ChangeSide, CommitInfo, DiffBody, DiffTarget, FileDiff, FileEntry, Hunk,
    StatusLists,
};
use crate::recent::RecentRepos;
use crate::repo::{open_repo, repo_name};
use crate::sbs::build_rows;
use crate::stage;
use crate::status::list_status;
use crate::watch::RepoWatcher;
use crate::{CoreError, Result};
use git2::{Oid, Repository};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;

const HISTORY_PAGE: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    None,
    Change {
        side: ChangeSide,
        path: String,
    },
    Commit {
        id: Oid,
        path: Option<String>,
    },
    Range {
        base: Oid,
        head: Oid,
        path: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum Command {
    OpenRepo(PathBuf),
    Refresh,
    Select(Selection),
    LoadFullDiff,
    LoadMoreHistory,
    StageFile(String),
    UnstageFile(String),
    /// `fingerprint` is `file_fingerprint` of the unstaged diff the user saw.
    DiscardFile {
        path: String,
        fingerprint: u64,
    },
    StageHunk {
        path: String,
        hunk_index: usize,
        header: String,
        fingerprint: u64,
    },
    UnstageHunk {
        path: String,
        hunk_index: usize,
        header: String,
        fingerprint: u64,
    },
    DiscardHunk {
        path: String,
        hunk_index: usize,
        header: String,
        fingerprint: u64,
    },
    SetDark(bool),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffView {
    pub file: FileDiff,
    pub rows: Vec<StyledRow>,
    pub side: Option<ChangeSide>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeInfo {
    pub base: Oid,
    pub head: Oid,
    pub commit_count: usize,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone)]
pub enum Event {
    RepoOpened {
        name: String,
        path: PathBuf,
        recent: Vec<PathBuf>,
    },
    Status(StatusLists),
    History {
        commits: Vec<CommitInfo>,
        append: bool,
        new_ids: Vec<Oid>,
    },
    Files(Vec<FileEntry>),
    Range(Option<RangeInfo>),
    Diff(Option<DiffView>),
    Error(String),
}

/// Handle to the worker thread. Dropping it shuts the worker down.
pub struct Controller {
    tx: Sender<Command>,
    handle: Option<JoinHandle<()>>,
}

impl Controller {
    pub fn spawn(recent_file: PathBuf, emit: impl Fn(Event) + Send + 'static) -> Controller {
        let (tx, rx) = channel();
        let worker_tx = tx.clone();
        let handle = std::thread::Builder::new()
            .name("codediff-controller".into())
            .spawn(move || {
                let mut w = Worker {
                    tx: worker_tx,
                    emit: Box::new(emit),
                    recent_file,
                    hl: Highlighter::new(),
                    dark: false,
                    open: None,
                    selection: Selection::None,
                    force_full: false,
                    view: None,
                };
                w.run(rx);
            })
            .expect("spawn controller thread");
        Controller {
            tx,
            handle: Some(handle),
        }
    }

    /// Queues a command; never blocks on Git work.
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(h) = self.handle.take() {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }
}

/// State tied to one open repository.
struct OpenRepo {
    repo: Repository,
    _watcher: Option<RepoWatcher>,
    head: Option<Oid>,
    loaded: usize,
    seen: HashSet<Oid>,
}

struct Worker {
    tx: Sender<Command>,
    emit: Box<dyn Fn(Event) + Send>,
    recent_file: PathBuf,
    hl: Highlighter,
    dark: bool,
    open: Option<OpenRepo>,
    selection: Selection,
    force_full: bool,
    view: Option<DiffView>,
}

/// A hunk command as the UI sent it.
struct HunkReq {
    path: String,
    idx: usize,
    header: String,
    fingerprint: u64,
}

#[derive(Clone, Copy)]
enum HunkOp {
    Stage,
    Unstage,
    Discard,
}

fn head_id(repo: &Repository) -> Option<Oid> {
    repo.head().ok()?.peel_to_commit().ok().map(|c| c.id())
}

fn range_info(repo: &Repository, base: Oid, head: Oid) -> Result<RangeInfo> {
    let mut walk = repo.revwalk()?;
    walk.push(head)?;
    walk.hide(base)?;
    let commit_count = walk.count();
    let (additions, deletions) = diff_stats(repo, &DiffTarget::Range { base, head })?;
    Ok(RangeInfo {
        base,
        head,
        commit_count,
        additions,
        deletions,
    })
}

impl Worker {
    fn run(&mut self, rx: Receiver<Command>) {
        let mut pending: Option<Command> = None;
        loop {
            let cmd = match pending.take() {
                Some(c) => c,
                None => match rx.recv() {
                    Ok(c) => c,
                    Err(_) => break,
                },
            };
            if matches!(cmd, Command::Refresh) {
                // Coalesce queued refreshes (watcher bursts) into one.
                while let Ok(next) = rx.try_recv() {
                    if !matches!(next, Command::Refresh) {
                        pending = Some(next);
                        break;
                    }
                }
            }
            if matches!(cmd, Command::Shutdown) {
                break;
            }
            self.handle(cmd);
        }
    }

    fn emit(&self, e: Event) {
        (self.emit)(e);
    }

    fn report<T>(&self, r: Result<T>) -> Option<T> {
        r.map_err(|e| self.emit(Event::Error(e.to_string()))).ok()
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::OpenRepo(p) => self.open_repo(p),
            Command::Refresh => self.refresh(),
            Command::Select(sel) => self.select(sel),
            Command::LoadFullDiff => {
                self.force_full = true;
                self.emit_diff();
            }
            Command::LoadMoreHistory => self.load_more_history(),
            Command::StageFile(p) => self.mutate(|r| stage::stage_file(r, &p)),
            Command::UnstageFile(p) => self.mutate(|r| stage::unstage_file(r, &p)),
            Command::DiscardFile { path, fingerprint } => self.discard_file(path, fingerprint),
            Command::StageHunk {
                path,
                hunk_index,
                header,
                fingerprint,
            } => self.hunk(
                HunkOp::Stage,
                HunkReq {
                    path,
                    idx: hunk_index,
                    header,
                    fingerprint,
                },
            ),
            Command::UnstageHunk {
                path,
                hunk_index,
                header,
                fingerprint,
            } => self.hunk(
                HunkOp::Unstage,
                HunkReq {
                    path,
                    idx: hunk_index,
                    header,
                    fingerprint,
                },
            ),
            Command::DiscardHunk {
                path,
                hunk_index,
                header,
                fingerprint,
            } => self.hunk(
                HunkOp::Discard,
                HunkReq {
                    path,
                    idx: hunk_index,
                    header,
                    fingerprint,
                },
            ),
            Command::SetDark(dark) => {
                self.dark = dark;
                if self.view.is_some() {
                    self.emit_diff();
                }
            }
            Command::Shutdown => {}
        }
    }

    fn open_repo(&mut self, path: PathBuf) {
        let Some(repo) = self.report(open_repo(&path)) else {
            return;
        };
        let root: PathBuf = repo.workdir().unwrap_or(&path).components().collect();
        let tx = self.tx.clone();
        let watcher = self.report(RepoWatcher::start(root.clone(), move || {
            let _ = tx.send(Command::Refresh);
        }));
        let mut recent = RecentRepos::load_from(&self.recent_file);
        recent.touch(root.clone());
        let _ = recent.save_to(&self.recent_file);
        let head = head_id(&repo);
        self.open = Some(OpenRepo {
            repo,
            _watcher: watcher,
            head,
            loaded: 0,
            seen: HashSet::new(),
        });
        self.selection = Selection::None;
        self.force_full = false;
        self.view = None;
        let name = repo_name(&self.open.as_ref().unwrap().repo);
        self.emit(Event::RepoOpened {
            name,
            path: root,
            recent: recent.paths,
        });
        self.emit_status();
        self.emit_history_head(true);
        self.emit(Event::Diff(None));
    }

    fn emit_status(&self) {
        let Some(o) = &self.open else { return };
        if let Some(s) = self.report(list_status(&o.repo)) {
            self.emit(Event::Status(s));
        }
    }

    /// Re-reads the loaded window of history from HEAD and emits it (append false).
    fn emit_history_head(&mut self, first: bool) {
        let Some(o) = &mut self.open else { return };
        let limit = o.loaded.max(HISTORY_PAGE);
        let commits = match list_commits(&o.repo, 0, limit) {
            Ok(c) => c,
            Err(e) => return self.emit(Event::Error(e.to_string())),
        };
        let new_ids: Vec<Oid> = if first {
            Vec::new()
        } else {
            commits
                .iter()
                .map(|c| c.id)
                .filter(|id| !o.seen.contains(id))
                .collect()
        };
        o.seen.extend(commits.iter().map(|c| c.id));
        o.loaded = commits.len();
        self.emit(Event::History {
            commits,
            append: false,
            new_ids,
        });
    }

    fn load_more_history(&mut self) {
        let Some(o) = &mut self.open else { return };
        let commits = match list_commits(&o.repo, o.loaded, HISTORY_PAGE) {
            Ok(c) => c,
            Err(e) => return self.emit(Event::Error(e.to_string())),
        };
        o.seen.extend(commits.iter().map(|c| c.id));
        o.loaded += commits.len();
        self.emit(Event::History {
            commits,
            append: true,
            new_ids: Vec::new(),
        });
    }

    fn refresh(&mut self) {
        let Some(o) = &mut self.open else { return };
        let old_head = o.head;
        let new_head = head_id(&o.repo);
        o.head = new_head;
        self.emit_status();
        self.emit_history_head(false);
        if let (Selection::Range { head, .. }, Some(nh)) = (&mut self.selection, new_head) {
            if Some(*head) == old_head && *head != nh {
                *head = nh;
                self.emit_range_and_files();
            }
        }
        if !matches!(self.selection, Selection::None) && !self.emit_diff() {
            // The selected file has no diff any more.
            self.view = None;
            match &mut self.selection {
                Selection::Commit { path, .. } | Selection::Range { path, .. } => *path = None,
                s => *s = Selection::None,
            }
        }
    }

    fn select(&mut self, sel: Selection) {
        let was_range = matches!(self.selection, Selection::Range { .. });
        self.selection = sel;
        self.force_full = false;
        match &self.selection {
            Selection::Range { path: None, .. } => {
                let files = self.emit_range_and_files();
                self.auto_select_first(files);
            }
            Selection::Range { base, head, .. } => {
                let (base, head) = (*base, *head);
                if let Some(o) = &self.open {
                    if let Some(info) = self.report(range_info(&o.repo, base, head)) {
                        self.emit(Event::Range(Some(info)));
                    }
                }
                self.emit_diff();
            }
            other => {
                let needs_files = matches!(other, Selection::Commit { path: None, .. });
                if was_range {
                    self.emit(Event::Range(None));
                }
                if needs_files {
                    let files = self.emit_files();
                    self.auto_select_first(files);
                } else {
                    self.emit_diff();
                }
            }
        }
    }

    fn target(&self) -> Option<(DiffTarget, Option<ChangeSide>, Option<String>)> {
        Some(match &self.selection {
            Selection::None => return None,
            Selection::Change {
                side: ChangeSide::Staged,
                path,
            } => (
                DiffTarget::Staged,
                Some(ChangeSide::Staged),
                Some(path.clone()),
            ),
            Selection::Change {
                side: ChangeSide::Unstaged,
                path,
            } => (
                DiffTarget::Unstaged,
                Some(ChangeSide::Unstaged),
                Some(path.clone()),
            ),
            Selection::Commit { id, path } => (DiffTarget::Commit(*id), None, path.clone()),
            Selection::Range { base, head, path } => (
                DiffTarget::Range {
                    base: *base,
                    head: *head,
                },
                None,
                path.clone(),
            ),
        })
    }

    /// Files of the current Commit/Range selection, emitted; returns them.
    fn emit_files(&self) -> Vec<FileEntry> {
        let (Some(o), Some((target, _, _))) = (&self.open, self.target()) else {
            return Vec::new();
        };
        let files = self
            .report(files_in_target(&o.repo, &target))
            .unwrap_or_default();
        self.emit(Event::Files(files.clone()));
        files
    }

    fn emit_range_and_files(&mut self) -> Vec<FileEntry> {
        let Selection::Range { base, head, .. } = self.selection else {
            return Vec::new();
        };
        let Some(o) = &self.open else {
            return Vec::new();
        };
        let info = self.report(range_info(&o.repo, base, head));
        self.emit(Event::Range(info));
        self.emit_files()
    }

    /// Selects the first file of the current Commit/Range listing and emits its diff.
    fn auto_select_first(&mut self, files: Vec<FileEntry>) {
        let first = files.into_iter().next().map(|f| f.path);
        match &mut self.selection {
            Selection::Commit { path, .. } | Selection::Range { path, .. } => *path = first,
            _ => return,
        }
        self.emit_diff();
    }

    /// Emits the diff of the current selection. Returns false when the selected
    /// path has no diff (Diff(None) emitted in that case too).
    fn emit_diff(&mut self) -> bool {
        let built = self.build_view();
        let ok = built.is_some();
        self.view = built.clone();
        self.emit(Event::Diff(built));
        ok
    }

    fn build_view(&self) -> Option<DiffView> {
        let o = self.open.as_ref()?;
        let (target, side, path) = self.target()?;
        let path = path?;
        let file = self.report(file_diff(&o.repo, &target, &path, self.force_full))??;
        let rows = match &file.body {
            DiffBody::Text(hunks) => self.hl.highlight_rows(&path, &build_rows(hunks), self.dark),
            _ => Vec::new(),
        };
        Some(DiffView { file, rows, side })
    }

    fn mutate(&mut self, op: impl FnOnce(&Repository) -> Result<()>) {
        let Some(o) = &self.open else { return };
        self.report(op(&o.repo));
        self.refresh();
    }

    /// The hunk the user saw, if the request still matches the current view.
    fn seen_hunk(&self, op: HunkOp, req: &HunkReq) -> Option<Hunk> {
        let v = self.view.as_ref()?;
        let side_ok = match op {
            HunkOp::Stage | HunkOp::Discard => v.side == Some(ChangeSide::Unstaged),
            HunkOp::Unstage => v.side == Some(ChangeSide::Staged),
        };
        let DiffBody::Text(hunks) = &v.file.body else {
            return None;
        };
        let h = hunks.get(req.idx)?;
        let same = v.file.path == req.path
            && h.header == req.header
            && hunk_fingerprint(h) == req.fingerprint;
        (side_ok && same).then(|| h.clone())
    }

    /// Discards the file only when its unstaged diff, recomputed now, still has
    /// the fingerprint of the diff the user confirmed.
    fn discard_file(&mut self, path: String, fingerprint: u64) {
        let Some(o) = &self.open else { return };
        let current = match file_diff(&o.repo, &DiffTarget::Unstaged, &path, self.force_full) {
            Ok(f) => f,
            Err(e) => {
                self.emit(Event::Error(e.to_string()));
                return self.refresh();
            }
        };
        if current.as_ref().map(file_fingerprint) != Some(fingerprint) {
            self.emit(Event::Error(CoreError::Stale.to_string()));
            return self.refresh();
        }
        self.mutate(|r| stage::discard_file(r, &path));
    }

    fn hunk(&mut self, op: HunkOp, req: HunkReq) {
        let Some(expected) = self.seen_hunk(op, &req) else {
            self.emit(Event::Error(CoreError::Stale.to_string()));
            return self.refresh();
        };
        let (path, idx) = (req.path, req.idx);
        self.mutate(|r| match op {
            HunkOp::Stage => stage::stage_hunk(r, &path, idx, &expected),
            HunkOp::Unstage => stage::unstage_hunk(r, &path, idx, &expected),
            HunkOp::Discard => stage::discard_hunk(r, &path, idx, &expected),
        });
    }
}
