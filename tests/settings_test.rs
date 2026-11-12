use codediff::settings::{Appearance, Settings};

#[test]
fn default_appearance_is_system() {
    assert_eq!(Settings::default().appearance, Appearance::System);
}

#[test]
fn missing_or_corrupt_file_gives_default() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.json");
    assert_eq!(Settings::load_from(&missing).appearance, Appearance::System);

    let corrupt = dir.path().join("bad.json");
    std::fs::write(&corrupt, "{oops").unwrap();
    assert_eq!(Settings::load_from(&corrupt).appearance, Appearance::System);
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sub").join("settings.json");
    for a in [Appearance::Light, Appearance::Dark, Appearance::System] {
        Settings { appearance: a }.save_to(&file).unwrap();
        assert_eq!(Settings::load_from(&file).appearance, a);
    }
}

#[test]
fn stored_as_lowercase_names() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.json");
    Settings { appearance: Appearance::Dark }.save_to(&file).unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"dark\""), "{text}");
}

#[test]
fn appearance_index_round_trips() {
    for (a, i) in [(Appearance::System, 0), (Appearance::Light, 1), (Appearance::Dark, 2)] {
        assert_eq!(a.index(), i);
        assert_eq!(Appearance::from_index(i), a);
    }
    assert_eq!(Appearance::from_index(7), Appearance::System);
}

#[test]
fn default_file_sits_next_to_recent_json() {
    let f = Settings::default_file();
    assert!(f.ends_with("codediff/settings.json"), "{}", f.display());
}
