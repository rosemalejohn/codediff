# CodeDiff Git Flow: Design

Date: 2026-11-16
Status: Approved
Extends: `2026-11-11-codediff-design.md`

## 1. Intent

Add Sourcetree's Git Flow menu to CodeDiff: initialize a repository for git-flow, start and finish features, releases and hotfixes, and a context-aware "Next Action".

**Owner's words:** "I want to add a functionality same as in sourcetree. Basically git flow." The owner supplied a screenshot of the Sourcetree menu: Initialize Repository, Next Action (Option+Cmd+F), Start New Feature, Finish Feature, Start New Release, Finish Release, Start New Hotfix, Finish Hotfix.

**Why (Claude's reading, the owner did not state it):** the original spec's success test is that the owner never opens Sourcetree. Git Flow is a remaining reason to open it. The owner's choices during brainstorming (push on finish, remote branch cleanup, latest tag as a reference) point the same way: finishing a release should need no second tool.

**Deliberate change of scope:** the original spec lists merge, checkout, push and tags as non-goals, and calls "another heavy Git client" a failure. This feature is the first that writes history (merge commits, tags, branch deletion) and the first that touches the network. The owner was told this and chose to go ahead. Sections 3 and 4 keep the feature narrow.

## 2. Goals

1. A **Git Flow** button in the top bar that opens a menu with the same eight items, in the same order, as Sourcetree.
2. **Initialize Repository:** pick the production and development branches and the branch prefixes.
3. **Start** a feature, release or hotfix from one small dialog.
4. **Finish** a feature, release or hotfix from a dialog that shows the exact steps before anything runs.
5. **Finish Release / Hotfix** can tag, and can optionally push to `origin`.
6. **Next Action** (Option+Cmd+F): a context-aware list of what makes sense on the current branch.
7. **Interoperate** with Sourcetree and the git-flow CLI: a repository initialized by one is recognized by the others.
8. **Never leave the repository half done:** a finish either completes its local steps or changes nothing.

## 3. Non-goals

- Resolving merge conflicts in the app. A finish that would conflict is refused (section 8.4).
- Committing, pulling, fetching, or pushing anywhere other than the optional push at the end of Finish Release / Hotfix.
- Sourcetree extras: "start at a specific commit", "rebase on development branch", "force deletion".
- git-flow's `bugfix/` and `support/` branch types, and `feature publish` / `track`.
- Remotes other than `origin`.
- Signing merge commits or tags, and running Git hooks for the local merge steps (the library does not run them). The owner has neither configured today.
- Merging a hotfix into an open release branch instead of develop. Hotfixes always merge back into develop.
- Suggesting or auto-filling the next version number. The latest tag is shown as a reference only.

## 4. Anti-goals (failure even if it works)

- A repository left mid-merge, or with some branches moved and others not, with no clear message.
- Anything pushed without the tick box, or a push that overwrites or discards commits on origin, ever.
- Losing uncommitted work.
- Growing into a general branch, merge and push client. Every action here is one of the eight menu items.

## 5. Constraints

- Rust and Slint, macOS, as in the original spec.
- Git work stays off the UI thread (existing worker thread in `src/controller.rs`).
- Owner decision: local Git Flow steps use the `git2` library; the push step runs the installed `git` command line (section 6).
- Dark theme is the primary design target; every new color also needs a light value in `ui/theme.slint`.
- **Starting point (owner):** the branch switcher (Cmd+B, `src/branch.rs`, `Command::Checkout`, `Event::Branches`, the safe checkout) is finished but, at the time of writing, exists only as uncommitted changes (11 modified files, 2 new). It is committed first, as its own commit. Git Flow work starts from that commit and edits several of the same files.
- **Known failing test, unrelated:** `start_does_not_scan_large_trees` in `tests/watch_test.rs` fails on the owner's machine in debug builds (watcher start about 64 ms against a 50 ms limit). The watcher files are untouched by the switcher work. It is not a regression from this feature and is not in scope here.

## 6. Approaches

Three ways to build it were considered.

**A. Hybrid (chosen by the owner).** Initialize, start, test-merge, merge, tag, delete and switch go through `git2`, the same path as stage, discard and checkout today. Only the push step runs the installed `git` (`git push ...`), so it uses the login that already works in the owner's terminal (here: HTTPS to GitHub with the macOS keychain helper). Trade-off: the app's first dependency on the `git` command line, which the original spec avoided. If `git` is missing the push tick box is disabled with a reason.

**B. Everything through `git2`, push included.** One code path. But the app's `git2` is currently built with no network transport at all (no HTTPS, no SSH), so this means adding two network libraries, a larger binary, a slower build, and a credentials layer to write and debug. The keychain lookup would still shell out to `git`. Rejected: push login is where Git apps break, and this borrows nothing from a setup that already works.

**C. Drive the `git flow` command line for everything, as Sourcetree does.** `git-flow` is not installed on the owner's machine, so it would have to be bundled. It cannot test a merge first, so a conflict would leave the repository half merged with no way to resolve or commit in the app. Rejected.

Why A: local steps stay in the library, where the preview and the conflict check are cheap and testable; the network step borrows a login the owner already trusts.

## 7. Screens

Reference mockups, reviewed by the owner (local only, not committed): `.superpowers/brainstorm/51052-1794818570/content/gitflow-screens-v3.html`. Nine screens, drawn over real renders of the app. The mockups are the reference for look and wording; this section records what each screen is.

### 7.1 Top bar button and menu

A **Git Flow** button (icon, label, chevron) sits right of the branch button, styled like it, visible whenever a repository is open. It opens a popup menu:

```
 Initialize Repository...
 Next Action...              Opt Cmd F
 ---------------------------------
 Start New Feature...
 Finish Feature...
 ---------------------------------
 Start New Release...
 Finish Release...           none open
 ---------------------------------
 Start New Hotfix...
 Finish Hotfix...            none open
```

- Not initialized: only Initialize Repository is enabled.
- Initialized: Initialize Repository is disabled. Finish items are enabled only when a branch of that kind exists, otherwise greyed with "none open". Start New Release (and Hotfix) is greyed with "one open" while one exists, since git-flow allows one release and one hotfix at a time.

### 7.2 Initialize Repository

Modal dialog: Production branch (picker over local branches), Development branch (text), then prefixes for Feature, Release, Hotfix and Version tag. Buttons: Cancel, Initialize. A hint under Development branch says whether it is a new branch or an existing one.

### 7.3 Start New Feature / Release / Hotfix

One field (Feature name, or Version). The prefix is shown inside the field, so only the name is typed. Below it, a line showing base branch, an arrow, and the branch to be created, updating as the user types. Start New Release and Start New Hotfix also show **"Latest tag: 1.1.0, 3 weeks ago"** under the Version field (section 8.8). Empty, invalid or taken names disable the primary button and show an inline reason. Buttons: Cancel, Start Feature / Start Release / Start Hotfix.

### 7.4 Next Action

Not a new dialog style: a third mode of the existing search panel used for Cmd+K (projects) and Cmd+B (branches). Rows are flow actions; the row for finishing the current branch comes first and is preselected. Activating a row opens the matching dialog; nothing runs directly from the panel.

### 7.5 Finish Feature

```
 Finish Feature
 Feature      [ feature/login-form        v ]
 WHAT HAPPENS
  1  Merge feature/login-form into develop     3 commits
  2  Delete feature/login-form
  3  Switch to develop
 [x] Delete branch after merge
                          [Cancel] [Finish Feature]
```

The picker lists every local feature branch; the current branch is preselected. The step list is the real plan and changes with the tick box.

### 7.6 Finish Release / Hotfix

```
 Finish Release
 Release      [ release/1.2.0             v ]
 Tag          [ 1.2.0                       ]
              Latest tag: 1.1.0, 3 weeks ago
 Tag message  [ Release 1.2.0               ]
 WHAT HAPPENS
  1  Merge release/1.2.0 into main             5 commits
  2  Tag main as 1.2.0
  3  Merge release/1.2.0 back into develop
  4  Delete release/1.2.0
  5  Switch to develop
  6  Push main, develop and tag 1.2.0 to origin
  7  Delete release/1.2.0 on origin
 [x] Delete branch after merge
 [x] Push to origin
                          [Cancel] [Finish Release]
```

Steps 6 and 7 appear only with "Push to origin" ticked; step 7 also needs "Delete branch after merge" ticked and the branch to exist on origin. Finish Hotfix is the same dialog with "Hotfix" wording and a default tag message of "Hotfix 1.2.1".

While it runs, the dialog shows the steps with check marks and a spinner on the push step. If the push fails, the dialog changes to "Release 1.2.0 finished, not pushed" with the reason and buttons Close and Retry Push.

### 7.7 Blocked states

The finish dialog still opens when it cannot run, so the owner sees why. A warning box at the top explains, and the primary button is disabled (or, for a conflict, only Close is offered). Cases are listed in section 8.4.

### 7.8 Done

The dialog closes, the branch button shows the new branch, and a green notice appears in the slot used by today's red error banner, with Dismiss:

- "Finished feature/login-form. Merged into develop and deleted the branch."
- "Finished release/1.2.0. Tagged 1.2.0 and pushed main, develop and the tag to origin."
- "Finished release/1.2.0. Tagged 1.2.0. Not pushed."

Start and Initialize show no notice; the branch button changing is the feedback.

## 8. Behavior

### 8.1 Configuration and detection

- Git Flow settings live in the repository's local Git config under the standard keys: `gitflow.branch.master` (production), `gitflow.branch.develop`, `gitflow.prefix.feature`, `gitflow.prefix.release`, `gitflow.prefix.hotfix`, `gitflow.prefix.versiontag`.
- A repository counts as initialized when production and develop are set, differ, both exist as local branches, and the feature, release and hotfix prefixes are set. A missing version tag prefix means "none".
- Initialize also writes `gitflow.prefix.bugfix = bugfix/` and `gitflow.prefix.support = support/`, unused by CodeDiff, so that the git-flow CLI and Sourcetree see a complete setup (see section 11).
- Flow state (initialized or not, branch names, open feature, release and hotfix branches, what the current branch is) is recomputed on every refresh, so branches created by an agent, the terminal or Sourcetree show up without action.

### 8.2 Initialize

- Defaults match Sourcetree: `develop`, `feature/`, `release/`, `hotfix/`, no tag prefix. Production defaults to `main` if it exists, else `master`, else the current branch. If Git Flow config already exists but is incomplete, the dialog is pre-filled from it.
- Development branch: reused if it exists locally; otherwise created from `origin/<name>` if that exists (tracking it), else from the production branch tip.
- Production and development must differ. Prefixes must be non-empty (except version tag) and valid in a branch name.
- After writing the config, the app switches to the development branch using the existing safe checkout. If local changes block the switch, initialization still stands and the usual checkout error is shown.
- A repository with no commits cannot be initialized; the menu item is disabled with a reason. (Claude's call; git-flow would create an initial commit.)

### 8.3 Start

- Branch name is prefix plus the typed name. Feature and release start from the tip of local develop; hotfix starts from the tip of local production.
- Refused, with an inline reason, when: the name is empty or not a valid branch name; the branch already exists locally or on origin (as last fetched); the name nests with an existing local branch (`feature/login` and `feature/login/v2` cannot both exist); for release and hotfix, a tag named version-tag-prefix plus version already exists; a release (or hotfix) is already open.
- The new branch is checked out with the existing safe checkout. Uncommitted changes come along. If they would be overwritten, nothing is created and the existing "local changes would be overwritten" error shows. (Claude's call: git-flow demands a clean tree for release and hotfix start; one rule for all three is simpler and loses nothing.)
- No fetch, and no comparison with origin, on start.

### 8.4 Finish: checks before anything runs

The checks run when the finish dialog opens, when its branch picker changes, and again when Finish is clicked. All are read-only apart from writing unreferenced Git objects. Any failure blocks the finish and is explained in the dialog:

0. **Branches moved** (Finish clicked only): the finish is pinned to the branch tips of the plan the owner saw (the finished branch, develop, and production for release and hotfix). If any of them has changed since, nothing runs: "feature/x changed since you looked. Review the new commits, then finish again." The dialog then shows the fresh plan. The finish reads each tip once and builds every commit, reference move and push from that one snapshot. While the dialog is open, a change to the Git Flow state plans again. (Controller ruling after the branch review: the agent may commit at any moment.)
1. **Uncommitted changes:** staged or unstaged changes to tracked files block the finish ("13 uncommitted changes. Finish needs a clean working tree."). Untracked files do not, as in git-flow.
2. **Repository busy:** a merge, rebase or cherry-pick in progress blocks it.
3. **Identity:** `user.name` and `user.email` must be set (merge commits and tags need them).
4. **Other worktree:** a branch that would be moved or deleted is checked out in another linked worktree.
5. **Behind origin:** local develop (and production, for release and hotfix), or the branch being finished, is behind or has diverged from its `origin/` counterpart as last fetched. Message names the branch and says to pull it first. (Claude's call, same guard as git-flow; it prevents a merge on a stale base, a rejected push, and finishing a branch without commits that someone else pushed to it. The app does not fetch, so this is best effort; section 8.7 adds a hard guard for the remote delete.)
6. **Tag exists** (release and hotfix): the tag name is taken or invalid.
7. **Conflict:** every merge in the plan is test-merged in memory. If any would conflict, the dialog lists the target branch and the conflicting files, says nothing was changed, and offers only Close.
8. **Checkout collision:** the final tree is dry-run checked out; an untracked file in the way blocks the finish and is named.

### 8.5 Finish: what gets created

Feature `F`, release or hotfix `R`, production `M`, develop `D`:

- **Feature:** merge `F` into `D`. As git-flow does: a merge commit, except when `F` is exactly one commit ahead of `D` and `D` has not moved, in which case `D` is fast-forwarded. If `F` is already contained in `D`, the merge step shows "already merged" and is skipped.
- **Release / hotfix:**
  1. Merge `R` into `M`, always with a merge commit. Skipped as "already merged" if `M` already contains `R`.
  2. Annotated tag (name and message from the dialog) on the new tip of `M`.
  3. Merge that new tip of `M` (the tagged commit) into `D`, always with a merge commit. This is the git-flow AVH behavior: develop ends up containing production, and `git describe` on develop sees the tag. The dialog words it as "Merge release/1.2.0 back into develop". (Claude's call.)
- Merge commit messages follow Git's wording: "Merge branch 'feature/login-form' into develop", "Merge branch 'release/1.2.0' into main", "Merge tag '1.2.0' into develop". Author and committer are the repository's configured identity.
- Then: delete the finished branch locally if ticked, and end on develop in all three cases.

### 8.6 Finish: all or nothing

Required property: if any local step cannot complete, no branch, tag or HEAD has moved, and the working tree is untouched.

Recommended technique (Claude's call, the builder may choose another that keeps the property): never use the library's working-tree merge (which writes merge state and conflict markers). Instead:

1. Build every new commit object in the object database with in-memory merges. No reference changes yet.
2. Check out the final tree (develop's new tip) with a safe checkout while HEAD still points at the old commit, so the library compares the working tree against the right baseline.
3. Only then move references: production, tag, develop, HEAD, and delete the finished branch.

Step 3 is a handful of reference writes. If one of them fails, the error banner names the step that failed and the steps that already ran, and says that the working tree already shows develop's new content. The finished branch is deleted only while it still points at the commit that was merged; otherwise it is kept and the finish stops there.

### 8.7 Push

- Offered only in Finish Release and Finish Hotfix (owner decision). Finish Feature never pushes, as in Sourcetree (Claude's call).
- "Push to origin" is unticked the first time, then remembers the last choice per repository, shared by release and hotfix (owner decision).
- Disabled with a reason when there is no `origin` remote or no working `git` command.
- Runs only after every local step succeeded. One `git push` to `origin` with: the production branch, the development branch (each as the commit the finish produced, `<commit>:refs/heads/<branch>`, so a later commit on develop is not pushed by a Retry), the new tag, and, when "Delete branch after merge" is ticked and the finished branch exists on origin, deletion of that remote branch (owner decision).
- **All or nothing on origin** (Claude's call): the push is atomic (`git push --atomic`). Either every reference above is updated or none is, so origin never ends up with the tag but not the production branch. If origin does not support atomic pushes, the push fails with that reason and nothing is sent. GitHub supports it.
- **Guarded remote delete** (Claude's call): the delete only happens if origin's branch is still exactly the commit that was merged (a lease on that one reference, `--force-with-lease=<branch>:<merged commit>`). If someone pushed more commits to it, the whole push is refused and nothing is sent. The lease only restricts the delete; it forces nothing.
- Never overwrites or discards commits on origin: no forced update of any branch or tag.
- Exactly the references above: the user's Git config cannot widen the push (`--no-follow-tags`, `--recurse-submodules=no`).
- Must never wait for input: terminal prompts are disabled, and a push that cannot authenticate fails with Git's message. It must not hang forever.
- Failure: the local finish stays done and nothing was sent. The dialog shows Git's reason and Close / Retry Push. Retry runs the same push again. Close leaves the "Not pushed" notice.
- The UI stays responsive while pushing; the dialog shows progress.

### 8.8 Latest tag

- Owner request: show the latest tag under the Tag field of Finish Release / Hotfix, as a reference for the next version. Claude's call: also show it under the Version field of Start New Release / Hotfix, where the version is first typed.
- Definition (Claude's call): among tags reachable from the production branch tip (limited to tags starting with the version tag prefix when one is set), the one on the newest commit. Ties go to the higher version-like name.
- Shown as the full tag name and the age of its commit: "Latest tag: 1.1.0, 3 weeks ago". With no such tag: "No tags yet". Read-only; nothing is filled in from it.

### 8.9 Next Action

- Option+Cmd+F, when a repository is open and no dialog is showing. Also the second menu item.
- Not initialized: a single row, "Initialize repository".
- On a feature, release or hotfix branch: "Finish feature login-form" (or release, hotfix) first and preselected, tagged "current branch".
- Then, when allowed: "Finish release 1.2.0" / "Finish hotfix 1.2.1" for an open one that is not the current branch, "Finish feature..." when features exist (opens the dialog with its picker), "Start new feature", "Start new release", "Start new hotfix". Start rows for a kind that already has one open are left out.
- Typing filters the rows like the other switcher modes.

### 8.10 Dialogs, keys, feedback

- Dialogs are modal over a scrim, like the discard confirmation. Clicking outside does not close them.
- Return runs the primary action when it is enabled; Esc cancels; Tab moves between fields. (This differs on purpose from the discard dialog, where Return cancels.)
- Errors outside a dialog use the existing red banner. Success uses the new green notice (section 7.8), cleared by Dismiss, by the next notice or error, or by switching repository.
- After every flow action the app refreshes status, history, head and flow state, as other mutations do.

## 9. Technical direction (Claude's calls, open to change)

- New core module for flow logic beside `src/branch.rs`, pure functions over `git2::Repository` returning plain data (flow state, finish plan with steps and blockers), unit-testable without UI. A small separate module runs `git push` and reports success, or Git's reason for failure.
- New worker commands and events in `src/controller.rs` following the existing pattern (`Command::Checkout`, `Event::Branches`): flow state event, finish-plan request and reply, init, start, finish, retry push, progress, notice.
- UI: a form dialog component in `ui/` (text field with fixed prefix, branch picker, tick box, step list, warning box), the menu popup, a third mode for `ui/switcher.slint`, a notice banner, a flow icon in `Icons`.
- The remembered push choice is stored per repository in its local Git config.
- `git` is located on `PATH` with `/usr/bin/git` as fallback (apps launched from Finder get a minimal `PATH`).

## 10. Left to the builder

- Module and type names, command and event shapes, Slint component structure.
- Exact config key for the remembered push choice.
- How push output is parsed and how the no-hang rule is met (timeout or cancel).
- Whether push runs on the worker thread or its own.
- Keeping the top bar from overlapping at the 640 px minimum width (for example, collapsing the Git Flow label to its icon).
- The picker control inside dialogs.
- Icon glyph, exact spacing and colors, within the mockups' look, with light theme values.
- README: a Git Flow section and the new shortcut.
- Whether to extend `examples/screenshot.rs` to capture the new dialogs.

## 11. To verify during the build

These are Claude's recollections, not checked against sources:

- That git-flow AVH (and Sourcetree) treat a repository as initialized only when all six prefix keys exist, including `bugfix` and `support`. If not, writing them is harmless.
- That Sourcetree's finish matches the merge rules in section 8.5 closely enough that a repository can alternate between the two tools.
- How Slint reports Option+Cmd+F on macOS: Option changes the typed character (Option+F gives "ƒ"), so the key handler may need to accept both. The existing code notes that Slint reports Cmd as `control`.

## 12. Testing

Emphasis on the finish logic, since mistakes there damage history.

- Core, with temporary repositories (existing `tests/common` helper): config round trip and detection of a repository initialized in the standard format; each start variant and each refusal; finish feature (merge commit, single-commit fast-forward, already merged); finish release and hotfix (resulting commit graph, tag, final branch); every blocker in section 8.4, each asserting that references, HEAD and working tree are unchanged; latest tag selection.
- Push, against a local bare repository as `origin`, using the real `git`: success; rejected push (origin ahead) leaving the local finish intact and origin entirely unchanged; retry; remote branch deletion; deletion refused when origin's branch has extra commits; missing `origin`.
- Controller and UI bridge tests in the existing style for menu enablement, Next Action rows per branch kind, and dialog blocked states.

## 13. Decision log

Owner decisions:

- Feature set and menu: same as Sourcetree's Git Flow menu.
- Mockups reviewed across three versions; no changes requested beyond the items below.
- "Push to origin" tick box on Finish Release / Hotfix, off by default, remembered, never blocking the local finish.
- With push and delete both ticked, the finished branch is also deleted on origin.
- Approach A: library for local steps, installed `git` for the push.
- Latest tag shown under the Tag field as a reference.
- The branch switcher work is finished; it is committed before Git Flow work starts.

Claude's calls (not objected to, open to change): placement of the button; Next Action as a search-panel mode; refusing conflicts up front instead of resolving them; dropping "specific commit", "rebase" and "force delete"; no push on Finish Feature; one checkout rule for all starts; "behind origin" as a blocker, including the branch being finished; all-or-nothing push; guarded remote delete; back-merging the tagged production commit into develop; latest tag also on the Start dialogs and its definition; green notice banner; Return as the primary key in these dialogs; no initialization of an empty repository; hotfixes always merge back into develop.

Reviewed before handover by an independent builder check. Its two safety findings (remote delete could discard commits pushed by someone else; a partly accepted push could leave the tag on origin without the production branch) are resolved in sections 8.4 and 8.7.

## 14. Success looks like

The owner's agent has committed work on `feature/login-form`. In CodeDiff the owner presses Option+Cmd+F, picks "Finish feature login-form", reads three steps, and clicks Finish. Later they start `release/1.2.0` with the latest tag in view, and finish it with "Push to origin" ticked: main, develop and the tag are on GitHub, the release branch is gone locally and remotely, and Sourcetree was never opened. When a finish cannot run, CodeDiff says why and the repository is exactly as it was.
