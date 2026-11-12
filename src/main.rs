use codediff::controller::{Command, Controller};
use codediff::recent::RecentRepos;
use codediff::settings::Settings;
use codediff::ui::AppWindow;
use codediff::ui_bridge;
use slint::ComponentHandle;
use std::path::PathBuf;
use std::rc::Rc;

fn main() -> Result<(), slint::PlatformError> {
    let process_start = std::time::Instant::now();
    let window = AppWindow::new()?;

    // Apply the saved appearance before anything reads the theme.
    let settings_file = Settings::default_file();
    ui_bridge::set_appearance(&window, Settings::load_from(&settings_file).appearance);
    ui_bridge::wire_appearance(&window, settings_file);

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

    if std::env::var_os("CODEDIFF_STARTUP_TRACE").is_some() {
        window.show()?;
        slint::Timer::single_shot(std::time::Duration::from_millis(0), move || {
            eprintln!("startup: {} ms to first frame", process_start.elapsed().as_millis());
        });
    }

    window.run()
}
