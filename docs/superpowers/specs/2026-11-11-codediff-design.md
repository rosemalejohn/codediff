# CodeDiff: Design

Date: 2026-11-11
Status: Approved

## 1. Intent

A lightweight macOS desktop app, written in Rust with a Slint UI, whose only job is reviewing changes in a local Git repository.

**Why:** AI coding agents now write and commit large amounts of code. The owner needs a fast, focused way to see what an agent changed, keep the good parts, and throw away the bad parts. Existing options miss:

- Sourcetree: too heavy.
- VS Code: means opening a full editor just to look at diffs.

The app focuses on diffs and nothing else. The agent commits; the human reviews.

**Workflow (owner's answer):** the agent commits as it works. The owner reviews afterward, mostly through History, including a combined view of "everything since commit X". Uncommitted work in progress (agent mid-task, or the owner's own edits) is reviewed and cleaned up in the Changes tab with stage/discard. Bad parts that are already committed are not undone in the app; the owner tells the agent to fix them.

## 2. Goals

1. Open a local Git repository from disk, and remember recently opened repos for quick switching.
2. Show working tree changes (staged and unstaged, including untracked files) with a side-by-side diff.
3. Stage, unstage, and discard changes, both per whole file and per hunk.
4. Browse commit history of the current branch and view each commit's diff side by side (read only).
5. Review a range: the combined diff of everything from a chosen base commit up to HEAD (read only).
6. Auto-refresh when files in the repo change on disk, or new commits land, so an agent's work appears without manual action.
7. Feel light: fast startup, low memory, smooth scrolling.
8. Ship as an installable `.dmg` for macOS.

## 3. Non-goals

- Committing, amending, or writing commit messages (the agent does that).
- Branch switching, checkout, merge, rebase, revert, cherry-pick, stash, push, pull, fetch. In particular, no undoing committed changes (the agent fixes those on request).
- Editing file contents.
- Remote/hosted repos (GitHub, GitLab). Local folders only.
- Windows and Linux builds in v1. Cross-platform is a bonus later, not a goal. Avoid macOS-only code where a portable option is equally good.

## 4. Anti-goals (failure even if it works)

- Becoming another heavy Git client. Every feature beyond the list in section 2 needs a strong reason.
- Slow to open, laggy on large diffs, or high memory use.
- Losing user work: discard must never happen without confirmation.
- Requiring an editor or IDE to be open.

## 5. Constraints (decided by owner)

- Language: Rust.
- UI: Slint.
- Platform: macOS first.
- Distribution: `.dmg` installer.
- Diff display: side by side (old on left, new on right).
- Layout: two panes with tabs on top (see section 6).
- Stage/discard granularity: whole file and per hunk.

## 6. The screen

Two-pane layout, one file's diff visible at a time. Reference mockup (option B in the file): `.superpowers/brainstorm/67264-1794402163/content/layout-v2.html`. Rough proportions: sidebar about 220 to 280 px wide (resizable), diff pane takes the rest, diff split 50/50.

```
+---------------------------------------------------------------+
| my-app v   [ Changes 4 ]  [ History ]                         |
+--------------+------------------------------------------------+
| STAGED (1)   | src/main.rs            [Stage file][Discard file]|
|  A auth.rs   |  HEAD                 | Working copy           |
| UNSTAGED (3) | @@ -10,6 +10,8 @@  [Stage hunk][Discard hunk] |
| >M main.rs   | 10 let cfg = load();  | 10 let cfg = load();   |
|  M db.rs     | 11 let db = connect() | 11 let db = connect(..)|
|  D util.rs   |  //////////////////   | 12 db.migrate()?;      |
+--------------+------------------------------------------------+
History tab: sidebar = commit list; above diff = strip of the
commit's files as tabs; no action buttons.
```

### Top bar
- Repo name with a dropdown: list of recent repos, plus "Open repository..." (native folder picker). Selecting a folder that is not a Git repo shows a clear error.
- Two tabs: **Changes** (shows count of changed files) and **History**.

### Changes tab
- **Left sidebar:** two groups, **Staged** and **Unstaged**, each with a count. Each row: status letter (M modified, A added, D deleted, R renamed, ? untracked shown as A) and file path. Untracked files appear under Unstaged and diff as all-added.
- **Right pane:** side-by-side diff of the selected file.
  - Column headers: left "HEAD" (or "Index" when relevant), right "Working copy" (or "Staged").
  - Line numbers on both sides.
  - Removed lines tinted red on left, added lines tinted green on right, with a striped filler on the side that has no matching line.
  - Within a changed line pair, the changed words get a stronger highlight.
  - Syntax highlighting for common languages.
- **File header actions:** "Stage file" and "Discard file" for an unstaged file; "Unstage file" for a staged file.
- **Hunk header actions:** each hunk header shows its `@@` range and buttons "Stage hunk" / "Discard hunk" (unstaged) or "Unstage hunk" (staged).
- **Discard** always shows a confirmation dialog naming the file (and hunk) and saying it cannot be undone. Discarding an untracked file deletes it.
- A file may appear in both groups when it is partially staged; each group shows its own part.

### History tab
- **Left sidebar:** commits on the current branch, newest first. Each row: first line of message, short hash, author, relative time. Loads more as the user scrolls.
- **Right pane:** a strip of the commit's changed files (status letter + file name) above a side-by-side diff of the selected file, diffed against the commit's first parent. Initial commit diffs against empty. Merge commits diff against first parent.
- **Range review (Claude's call on the interaction):** each commit row has a "Review since here" action (context menu, plus shift-click selecting a second commit). This shows the combined diff from that commit (exclusive) up to HEAD, or between the two selected commits. The sidebar shows a banner "Reviewing 7 commits since a1b2c3d [Clear]" and the file strip lists every file changed across the range, with combined +/- counts. Selecting a single commit again exits range mode.
- **New commits while viewing:** when the agent commits, new rows appear at the top with a subtle "new" marker until clicked. An open range review that ends at HEAD extends to include them.
- Read only: no action buttons.

### Behavior
- **Auto-refresh:** the app watches the repo working directory and `.git` (index, HEAD, refs). On change, it refreshes the file list, the history list (new commits from the agent), and the open diff. Events are debounced so a burst of writes causes one refresh. The selected file stays selected if it still exists; scroll position is kept where reasonable.
- **Keyboard:** up/down moves between files (or commits) in the sidebar; shortcuts for stage file, unstage file, discard file (discard still confirms). Exact keys left to the builder, following macOS conventions.
- **Binary files:** show "Binary file, no preview" with size change.
- **Very large files or diffs:** show a "Large diff, click to load" placeholder above a threshold, so the UI never freezes.
- **Theme:** follows macOS light/dark setting; dark theme is the primary design target.
- **Empty states:** no repo opened yet (welcome screen with "Open repository..."), clean working tree ("No changes").

## 7. Technical direction (builder's calls, made in brainstorming, open to change)

These were proposed by Claude and not objected to; they are defaults, not owner requirements:

- **Git access:** `git2` crate (libgit2). No dependency on the `git` CLI. Hunk staging/unstaging/discard via building partial patches and applying them to the index or working tree (`git2` apply with hunk filtering, or equivalent).
- **File watching:** `notify` crate (with debouncer).
- **Syntax highlighting:** `syntect`.
- **Word-level diff:** `similar` crate on paired changed lines.
- **Recent repos:** persisted in a small config file in the standard macOS app support directory.
- **Packaging (owner: personal use for now):** produce a `.app` bundle and wrap it in a `.dmg` (for example via `cargo-bundle` or `cargo-packager`). Unsigned, ad-hoc signed at most; the owner approves it once in System Settings > Privacy & Security. No notarization in v1. Slint used under its royalty-free license for now; revisit license, signing, and name if it is ever shared.
- **App name (owner):** CodeDiff (binary and bundle id: `codediff`).
- **Performance assumptions (Claude's call):** medium repos up to about 100k files, diffs up to a few thousand lines, Apple Silicon. File watching ignores `.gitignore`d paths (for example `node_modules`, `target`). The diff view virtualizes lines from day one. Targets: cold start under 1 second, idle memory under 100 MB, refresh after an agent write burst under 300 ms for a typical repo.

## 8. Left to the builder

- Internal architecture, module layout, threading model (Git and diff work must stay off the UI thread).
- Exact debounce interval, large-diff thresholds, history page size.
- Exact keyboard shortcuts.
- Visual details: colors, spacing, fonts, icons, within the look shown in the chosen mockup (layout B, side by side).
- Test strategy, with emphasis on the stage/unstage/discard hunk logic, since bugs there lose user work.

## 9. Success looks like

The owner finishes a session with an AI agent, opens CodeDiff (or already has it open and sees commits and changes appear live), picks "Review since here" on the commit before the agent started, reads every changed file side by side, cleans up any leftover uncommitted changes with stage/discard, tells the agent what to fix, and never opens Sourcetree or VS Code to do it.
