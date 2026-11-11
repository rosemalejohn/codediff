use codediff::controller::{Command, Controller};
use codediff::recent::RecentRepos;
use codediff::ui::AppWindow;
use codediff::ui_bridge;
use slint::ComponentHandle;
use std::path::PathBuf;
use std::rc::Rc;

fn main() -> Result<(), slint::PlatformError> {
    let window = AppWindow::new()?;

    let recent_file = RecentRepos::default_file();
    let recent = RecentRepos::load_from(&recent_file);
    ui_bridge::set_recent(&window, &recent.paths);

    // The controller emits from its worker thread; hop to the UI thread to apply.
    let weak = window.as_weak();
    let controller = Rc::new(Controller::spawn(recent_file, move |ev| {
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = weak.upgrade() {
                ui_bridge::apply(&w, ev);
            }
        });
    }));

    let c = controller.clone();
    ui_bridge::wire(&window, move |cmd| c.send(cmd));

    // CODEDIFF_OPEN overrides the most recent repo (handy for manual testing).
    let initial = std::env::var_os("CODEDIFF_OPEN")
        .map(PathBuf::from)
        .or_else(|| recent.paths.first().cloned());
    if let Some(path) = initial {
        controller.send(Command::OpenRepo(path));
    }

    window.run()
}
