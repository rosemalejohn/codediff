//! Git Flow end to end: the real `Controller` and the real UI bridge against an
//! offscreen window, on a scratch repository with a bare `origin` in a temp dir.
//! The feature is driven only through the `Flow` global's callbacks and
//! properties; the repository is opened with the one `OpenRepo` command the app
//! itself sends at startup, and the test helpers stand in for the agent's commits.

mod common;

use codediff::controller::{Command, Controller, Event};
use codediff::ui::{AppWindow, Flow};
use codediff::ui_bridge;
use common::{git, origin_ref, with_origin};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Model, PhysicalSize};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Offscreen {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
}

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Applies worker events to the window until `done` holds, or fails after 15 s.
fn wait_for(app: &AppWindow, rx: &mpsc::Receiver<Event>, what: &str, done: impl Fn(&AppWindow) -> bool) {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        while let Ok(ev) = rx.try_recv() {
            ui_bridge::apply(app, ev);
        }
        slint::platform::update_timers_and_animations();
        if done(app) {
            return;
        }
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(ev) => ui_bridge::apply(app, ev),
            Err(_) => {}
        }
    }
}

/// Opens the Start dialog for `kind`, types `name` and submits it.
fn start(app: &AppWindow, rx: &mpsc::Receiver<Event>, kind: &str, name: &str, branch: &str) {
    let f = app.global::<Flow>();
    f.invoke_action(format!("start:{kind}").into());
    assert_eq!(f.get_dialog(), 2, "start dialog open");
    // A bad name first, so the reply to the good name is an observable change.
    f.invoke_name_edited("bad name".into());
    wait_for(app, rx, "start error for a bad name", |a| !a.global::<Flow>().get_start_error().is_empty());
    f.invoke_name_edited(name.into());
    wait_for(app, rx, "start error cleared", |a| a.global::<Flow>().get_start_error().is_empty());
    f.invoke_submit_start();
    wait_for(app, rx, "start dialog closed on the new branch", |a| {
        a.global::<Flow>().get_dialog() == 0 && a.get_branch_name() == branch
    });
}

/// Opens Finish for `key`, waits for an unblocked plan and returns the step texts.
fn open_finish(app: &AppWindow, rx: &mpsc::Receiver<Event>, key: &str) -> Vec<(String, bool)> {
    let f = app.global::<Flow>();
    f.invoke_action(key.into());
    assert_eq!(f.get_dialog(), 3, "finish dialog open");
    wait_for(app, rx, "finish plan", |a| {
        let f = a.global::<Flow>();
        f.get_blocker_level() == 0 && f.get_steps().row_count() > 0
    });
    steps(app)
}

fn steps(app: &AppWindow) -> Vec<(String, bool)> {
    app.global::<Flow>().get_steps().iter().map(|s| (s.text.to_string(), s.remote)).collect()
}

#[test]
fn git_flow_end_to_end_through_the_ui_bridge() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen { window: window.clone(), start: Instant::now() })).unwrap();
    window.set_size(PhysicalSize::new(1200, 800));

    let (t, cfg, bare) = with_origin();
    let app = AppWindow::new().unwrap();
    app.show().unwrap();

    let recent = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel();
    let controller = Rc::new(Controller::spawn(recent.path().join("recent.json"), move |ev| {
        let _ = tx.send(ev);
    }));
    let c = controller.clone();
    ui_bridge::wire(&app, move |cmd| c.send(cmd));
    controller.send(Command::OpenRepo(t.path().to_path_buf()));

    let f = app.global::<Flow>();
    wait_for(&app, &rx, "repository opened with flow state", |a| {
        a.global::<Flow>().get_initialized() && a.get_branch_name() == "develop"
    });
    assert_eq!(f.get_production().as_str(), cfg.production);
    assert_eq!(f.get_develop(), "develop");

    // Start New Feature and finish it.
    start(&app, &rx, "feature", "demo", "feature/demo");
    t.commit_file("demo.txt", b"demo\n", "add demo");
    let plan = open_finish(&app, &rx, "finish:feature:feature/demo");
    assert!(plan.iter().all(|(_, remote)| !remote), "a feature finish has no remote step: {plan:?}");

    // The agent commits while the dialog is open: Finish is pinned to the tips
    // the owner saw, so it refuses and the fresh plan comes back.
    let late = t.commit_file("late.txt", b"late\n", "late commit");
    f.invoke_submit_finish();
    wait_for(&app, &rx, "finish refused because the branch moved", |a| {
        let f = a.global::<Flow>();
        f.get_error() == "feature/demo changed since you looked. Review the new commits, then finish again."
            && f.get_blocker_level() == 0
    });
    assert_eq!(f.get_dialog(), 3, "dialog stays open");
    assert!(t.has_branch("feature/demo"), "nothing was finished");
    f.invoke_submit_finish();
    wait_for(&app, &rx, "feature finished", |a| {
        a.global::<Flow>().get_dialog() == 0 && a.get_branch_name() == "develop"
    });
    assert_eq!(
        f.get_notice(),
        "Finished feature/demo. Merged into develop and deleted the branch."
    );
    assert!(!t.has_branch("feature/demo"));
    assert!(t.repo.graph_descendant_of(t.tip("develop"), late).unwrap() || t.tip("develop") == late);

    // Start a release and finish it with a push and the branch deleted.
    start(&app, &rx, "release", "0.1.0", "release/0.1.0");
    f.invoke_action("finish:release:release/0.1.0".into());
    assert_eq!(f.get_dialog(), 3);
    assert!(f.get_delete_branch(), "Delete branch is ticked by default");
    wait_for(&app, &rx, "release plan", |a| {
        let f = a.global::<Flow>();
        f.get_blocker_level() == 0 && f.get_steps().row_count() > 0
    });
    assert!(!f.get_push(), "nothing remembered yet, so Push to origin starts unticked");
    f.set_push(true);
    f.invoke_plan_changed();
    wait_for(&app, &rx, "plan with a push step", |a| {
        a.global::<Flow>().get_blocker_level() == 0 && steps(a).iter().any(|(_, remote)| *remote)
    });
    f.invoke_submit_finish();
    wait_for(&app, &rx, "release finished", |a| {
        a.global::<Flow>().get_dialog() == 0 && a.get_branch_name() == "develop"
    });
    let notice = f.get_notice().to_string();
    assert!(notice.ends_with("and the tag to origin."), "notice: {notice}");
    assert!(origin_ref(&bare, "refs/tags/0.1.0").is_some(), "tag 0.1.0 on origin");
    assert_eq!(origin_ref(&bare, &format!("refs/heads/{}", cfg.production)), Some(t.tip(&cfg.production)));
    assert_eq!(origin_ref(&bare, "refs/heads/develop"), Some(t.tip("develop")));
    assert!(!t.has_branch("release/0.1.0"), "release branch deleted locally");
    assert!(git(t.path(), &["tag", "--list", "0.1.0"]).contains("0.1.0"));

    // The push choice is remembered for the next release.
    start(&app, &rx, "release", "0.2.0", "release/0.2.0");
    open_finish(&app, &rx, "finish:release:release/0.2.0");
    assert!(f.get_push(), "Push to origin stays ticked after a push");
    f.invoke_cancel();
    assert_eq!(f.get_dialog(), 0);

    // An uncommitted change blocks Finish, and submitting leaves the dialog open.
    t.write("a.txt", b"edited\n");
    f.invoke_action("finish:release:release/0.2.0".into());
    wait_for(&app, &rx, "blocker", |a| a.global::<Flow>().get_blocker_level() > 0);
    assert_eq!(f.get_blocker_level(), 1);
    assert!(f.get_blocker_title().starts_with("1 uncommitted change"), "title: {}", f.get_blocker_title());
    f.invoke_submit_finish();
    // Give a wrongly sent command time to show up: a no-op round trip through the worker.
    f.invoke_plan_changed();
    wait_for(&app, &rx, "plan after the refused submit", |a| a.global::<Flow>().get_blocker_level() == 1);
    assert_eq!(f.get_dialog(), 3, "dialog stays open");
    assert!(t.has_branch("release/0.2.0"), "nothing was finished");
}
