//! End-to-end UI tests, run headless with egui_kittest.
//!
//! Offline tests always run. Tests marked `live_` need the docker
//! environment (`dev/docker-compose.yml`) and `RR_LIVE=1`.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bson::{Bson, doc};
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use rustyrobo::app::App;
use rustyrobo::bsonfmt::FormatOptions;
use rustyrobo::shell::{OutputKind, ShellOutput};
use rustyrobo::settings::{AppSettings, ConnectionSettings, Credentials, HostPort, ViewMode};
use rustyrobo::ui::editor::code_view;
use rustyrobo::ui::icon;
use rustyrobo::ui::output::{PaneCtx, show_pane};
use rustyrobo::ui::query::OutputPane;
use rustyrobo::{fonts, theme};

/// The settings directory comes from an environment variable: tests that
/// create an App must not run concurrently.
static ENV: Mutex<()> = Mutex::new(());

struct Env {
    dir: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn env(name: &str, settings: &AppSettings) -> Env {
    let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("rustyrobo-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("settings.json"), serde_json::to_string(settings).unwrap()).unwrap();
    // SAFETY: serialised by ENV; the App reads it on the test thread.
    unsafe { std::env::set_var("RUSTYROBO_CONFIG_DIR", &dir) };
    Env { dir, _guard: guard }
}

fn harness() -> Harness<'static, App> {
    Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|cc| App::new(cc))
}

/// Runs frames (in real time, for background tasks) until `cond` holds.
fn wait_until(h: &mut Harness<'_, App>, what: &str, timeout: Duration, cond: impl Fn(&Harness<'_, App>) -> bool) {
    let start = Instant::now();
    loop {
        h.step();
        if cond(h) {
            return;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn has_label(h: &Harness<'_, App>, text: &str) -> bool {
    h.query_all_by_label_contains(text).next().is_some()
}

fn is_text_input(n: &egui_kittest::kittest::AccessKitNode<'_>) -> bool {
    matches!(n.role(), egui::accesskit::Role::TextInput | egui::accesskit::Role::MultilineTextInput | egui::accesskit::Role::PasswordInput)
}

/// True when a text field currently holds exactly `value`.
fn has_input(h: &Harness<'_, App>, value: &str) -> bool {
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some(value)).next().is_some()
}

/// Replaces the content of the (first) text field currently holding `value`.
fn replace_text(h: &mut Harness<'_, App>, value: &str, new: &str) {
    let v = value.to_string();
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some(v.as_str())).next().expect("text field").focus();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.key_press(Key::Backspace);
    h.run_steps(2);
    if !new.is_empty() {
        h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some("")).next().expect("cleared field").type_text(new);
    }
    h.run_steps(2);
}

fn local_mongo8() -> ConnectionSettings {
    ConnectionSettings {
        name: "Local Mongo 8".into(),
        hosts: vec![HostPort { host: "127.0.0.1".into(), port: 27117 }],
        credentials: Credentials {
            enabled: true,
            database: "admin".into(),
            username: "admin".into(),
            password: "secret".into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn saved_settings(env: &Env) -> AppSettings {
    serde_json::from_str(&std::fs::read_to_string(env.dir.join("settings.json")).unwrap()).unwrap()
}

// ------------------------------------------------------------- offline

#[test]
fn startup_shows_connection_manager() {
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let _env = env("startup", &s);
    let mut h = harness();
    h.run_steps(4);
    assert!(has_label(&h, "MongoDB Connections"));
    assert!(has_label(&h, "Local Mongo 8"));
    // Closing the manager shows the welcome screen.
    h.get_by_label("Close").click();
    h.run_steps(4);
    assert!(!has_label(&h, "MongoDB Connections"));
    assert!(has_label(&h, "Rustyrobo"));
    assert!(has_label(&h, "Manage connections"));
}

#[test]
fn empty_state_offers_creation() {
    let _env = env("empty", &AppSettings::default());
    let mut h = harness();
    h.run_steps(4);
    assert!(has_label(&h, "No connections yet"));
}

#[test]
fn create_connection_through_the_editor() {
    let env = env("create", &AppSettings::default());
    let mut h = harness();
    h.run_steps(4);
    h.get_by_label(&format!("{} Create", icon::PLUS)).click();
    h.run_steps(4);
    assert!(has_label(&h, "New Connection"));

    replace_text(&mut h, "New Connection", "Staging");
    replace_text(&mut h, "localhost", "db.staging.local");
    h.get_by_label("Save").click();
    h.run_steps(6);

    assert!(!has_label(&h, "Edit Connection"), "editor closed after save");
    assert!(has_label(&h, "Staging"), "listed in the manager");
    let saved = saved_settings(&env);
    assert_eq!(saved.connections.len(), 1);
    assert_eq!(saved.connections[0].name, "Staging");
    assert_eq!(saved.connections[0].hosts[0].host, "db.staging.local");
}

#[test]
fn validation_blocks_saving() {
    let _env = env("validation", &AppSettings::default());
    let mut h = harness();
    h.run_steps(4);
    h.get_by_label(&format!("{} Create", icon::PLUS)).click();
    h.run_steps(4);
    replace_text(&mut h, "New Connection", "");
    h.run_steps(4);
    assert!(has_label(&h, "A name is required"));
    assert!(has_label(&h, "issue(s) to fix"));
    // Save is disabled: clicking it keeps the editor open.
    h.get_by_label("Save").click();
    h.run_steps(4);
    assert!(has_label(&h, "New Connection"), "editor still open");
}

#[test]
fn paste_uri_fills_the_form() {
    let env = env("uri", &AppSettings::default());
    let mut h = harness();
    h.run_steps(4);
    h.get_by_label(&format!("{} Create", icon::PLUS)).click();
    h.run_steps(4);
    h.get_by_label("Paste URI…").click();
    h.run_steps(4);
    let current = h.state().conn_editor.as_ref().unwrap().uri_text.clone();
    replace_text(&mut h, &current, "mongodb://bob:pw@h1:27017,h2:27018/admin?replicaSet=rs0&tls=true");
    h.get_by_label("Apply").click();
    h.run_steps(4);
    assert!(has_label(&h, "Connection string applied"));
    let ed = h.state().conn_editor.as_ref().unwrap();
    assert_eq!(ed.conn.hosts.len(), 2);
    assert_eq!(ed.conn.replica_set_name, "rs0");
    assert!(ed.conn.tls.enabled);
    assert_eq!(ed.conn.credentials.username, "bob");
    h.get_by_label("Save").click();
    h.run_steps(4);
    assert_eq!(saved_settings(&env).connections[0].kind, rustyrobo::settings::ConnectionType::ReplicaSet);
}

#[test]
fn editor_sections_and_ssh_options() {
    let _env = env("sections", &AppSettings::default());
    let mut h = harness();
    h.run_steps(4);
    h.get_by_label(&format!("{} Create", icon::PLUS)).click();
    h.run_steps(4);
    h.get_by_label_contains("SSH Tunnel").click();
    h.run_steps(3);
    assert!(has_label(&h, "Use SSH tunnel"));
    h.get_by_label("Use SSH tunnel").click();
    h.run_steps(3);
    assert!(has_label(&h, "SSH host is required"));
    h.get_by_label("Private key").click();
    h.run_steps(3);
    assert!(has_label(&h, "Key passphrase"));
    h.get_by_label("SSH agent").click();
    h.run_steps(3);
    assert!(has_label(&h, "Agent"));
    h.get_by_label_contains("TLS / SSL").click();
    h.run_steps(3);
    assert!(has_label(&h, "Use TLS protocol"));
    h.get_by_label_contains("Advanced").click();
    h.run_steps(3);
    assert!(has_label(&h, "Manually specify visible databases"));
}

#[test]
fn cancel_with_changes_asks_to_discard() {
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let env = env("discard", &s);
    let mut h = harness();
    h.run_steps(4);
    h.get_by_label(&format!("{} Edit", icon::PENCIL_SIMPLE)).click();
    h.run_steps(4);
    replace_text(&mut h, "Local Mongo 8", "Renamed");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(has_label(&h, "Discard changes"));
    h.get_by_label("Discard changes").click();
    h.run_steps(3);
    assert!(!has_label(&h, "Edit Connection"));
    assert!(saved_settings(&env).connections[0].name == "Local Mongo 8", "not saved");
}

#[test]
fn remove_and_clone_connections() {
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let env = env("remove", &s);
    let mut h = harness();
    h.run_steps(4);
    h.get_by_label("Clone").click();
    h.run_steps(4);
    assert!(has_input(&h, "Copy of Local Mongo 8"));
    h.get_by_label("Save").click();
    h.run_steps(4);
    assert_eq!(saved_settings(&env).connections.len(), 2);

    h.get_by_label_contains("Remove").click();
    h.run_steps(3);
    assert!(has_label(&h, "Remove connection"));
    h.get_by_label("Remove").click();
    h.run_steps(4);
    assert_eq!(saved_settings(&env).connections.len(), 1);
}

#[test]
fn search_filters_the_list() {
    let mut other = local_mongo8();
    other.id = uuid::Uuid::new_v4();
    other.name = "Production".into();
    let s = AppSettings { connections: vec![local_mongo8(), other], ..Default::default() };
    let _env = env("search", &s);
    let mut h = harness();
    h.run_steps(4);
    assert!(has_label(&h, "Production") && has_label(&h, "Local Mongo 8"));
    h.state_mut().conn_list.as_mut().unwrap().filter = "prod".into();
    h.run_steps(3);
    assert!(has_label(&h, "Production"));
    assert!(!has_label(&h, "Local Mongo 8"));
}

#[test]
fn theme_menu_shows_the_hovered_theme_and_keeps_the_clicked_one() {
    let env = env("theme-hover", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(3);
    let initial = h.state().theme.name;
    let open_menu = |h: &mut Harness<'_, App>| {
        h.get_by_label("View").click();
        h.run_steps(2);
        h.get_by_label_contains("Theme").hover();
        h.run_steps(3);
    };
    let hover = |h: &mut Harness<'_, App>, theme: &str| {
        h.get_by_label(theme).hover();
        h.run_steps(2);
    };
    let not_saved = |h: &Harness<'_, App>| {
        assert_eq!(h.state().settings.theme, theme::DEFAULT_THEME);
        assert_eq!(saved_settings(&env).theme, theme::DEFAULT_THEME);
    };

    // Hovering shows each theme, without choosing it.
    open_menu(&mut h);
    hover(&mut h, "Nord");
    assert_eq!(h.state().theme.name, "Nord");
    hover(&mut h, "Solarized Light");
    assert_eq!(h.state().theme.name, "Solarized Light");
    assert!(!h.ctx.global_style().visuals.dark_mode, "the app follows");
    not_saved(&h);

    // Between two rows, the theme does not flash back.
    h.hover_at(egui::pos2(1000.0, 800.0));
    h.run_steps(2);
    assert_eq!(h.state().theme.name, "Solarized Light");

    // Closing the menu restores the theme.
    press(&mut h, Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().theme.name, initial);
    not_saved(&h);

    // So does opening another menu.
    open_menu(&mut h);
    hover(&mut h, "Nord");
    assert_eq!(h.state().theme.name, "Nord");
    h.get_by_label_contains("Font Size").hover();
    h.run_steps(3);
    assert_eq!(h.state().theme.name, initial);
    not_saved(&h);
    press(&mut h, Key::Escape);
    h.run_steps(2);

    // A click keeps it, and the menu closes.
    open_menu(&mut h);
    hover(&mut h, "Nord");
    h.get_by_label("Nord").click();
    h.run_steps(3);
    assert!(h.query_by_label("Solarized Light").is_none(), "the menu is closed");
    assert_eq!(h.state().theme.name, "Nord");
    assert_eq!(h.state().settings.theme, "Nord");
    assert_eq!(saved_settings(&env).theme, "Nord");

    // From now on, this is the theme to go back to.
    open_menu(&mut h);
    hover(&mut h, "Solarized Light");
    assert_eq!(h.state().theme.name, "Solarized Light");
    press(&mut h, Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().theme.name, "Nord");
    assert_eq!(saved_settings(&env).theme, "Nord");
}

#[test]
fn theme_menu_switches_theme() {
    let env = env("theme", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(3);
    h.get_by_label("View").click();
    h.run_steps(2);
    h.get_by_label_contains("Theme").hover();
    h.run_steps(3);
    h.get_by_label("Nord").click();
    h.run_steps(3);
    assert_eq!(h.state().theme.name, "Nord");
    assert_eq!(saved_settings(&env).theme, "Nord");
}

#[test]
fn keyboard_shortcuts_open_dialogs() {
    let _env = env("shortcuts", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::COMMAND, Key::N);
    h.run_steps(3);
    assert!(has_label(&h, "MongoDB Connections"), "Ctrl+N opens the manager");
    h.get_by_label("Close").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    h.run_steps(2);
    assert!(!h.state().settings.show_explorer, "Ctrl+E hides the explorer");
    h.key_press_modifiers(Modifiers::COMMAND, Key::L);
    h.run_steps(2);
    assert!(h.state().settings.show_logs, "Ctrl+L shows the logs");
    assert!(has_label(&h, "started"), "startup line in the logs");
}

#[test]
fn shortcuts_dialog_lists_the_copy_shortcuts() {
    let _env = env("shortcuts-help", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(3);
    h.state_mut().actions.push(rustyrobo::app::Action::Dialog(rustyrobo::ui::dialogs::Dialog::Shortcuts));
    h.run_steps(3);
    assert!(has_label(&h, "Keyboard Shortcuts"));
    assert!(has_label(&h, "Ctrl+C"));
    assert!(has_label(&h, "Ctrl+Shift+C"));
}

fn opened_url(h: &Harness<'_, App>) -> Option<String> {
    h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::OpenUrl(o) => Some(o.url.clone()),
        _ => None,
    })
}

fn text_field_shows(h: &Harness<'_, App>, text: &str) -> bool {
    h.query_all_by(|n| n.role() == egui::accesskit::Role::MultilineTextInput).any(|n| n.value().is_some_and(|v| v.contains(text)))
}

#[test]
fn new_version_shows_a_chip_and_a_dialog() {
    let _env = env("update", &AppSettings::default());
    let mut h = app_harness();
    assert!(h.query_all_by_label_contains("v9.9.9").next().is_none(), "no chip without a new version");
    h.state_mut().update = Some(std::sync::Arc::new(rustyrobo::update::Release {
        version: "9.9.9".into(),
        notes: "### Added\n- Things".into(),
        asset: Some("rustyrobo-v9.9.9-linux-x86_64.tar.gz".into()),
    }));
    h.run_steps(2);

    let open = |h: &mut Harness<'_, App>| {
        h.get_by_label_contains("v9.9.9").click();
        h.run_steps(4);
        assert!(has_label(h, "New version 9.9.9"));
    };
    open(&mut h);
    assert!(text_field_shows(&h, "- Things"), "the changelog");
    assert!(has_label(&h, "Full changelog"));
    assert!(!text_field_shows(&h, "SHA256SUMS"), "the commands start folded");

    // The commands, by system.
    h.get_by_label("Update manually").click();
    h.run_steps(4);
    for (os, command) in [("Linux", "tar xzf"), ("Windows", "Expand-Archive"), ("macOS", "ditto")] {
        h.get_by_label(os).click();
        h.run_steps(3);
        assert!(text_field_shows(&h, command), "{os}");
    }
    h.get_by_label_contains("Copy").click();
    h.step();
    assert!(copied(&h).is_some_and(|c| c.contains("ditto")));
    h.run_steps(2);

    // Cancel only closes, the chip stays.
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(!has_label(&h, "New version 9.9.9"));
    assert!(opened_url(&h).is_none());

    // Download: the file for this system, in the browser.
    open(&mut h);
    h.get_by_label_contains("Download update").click();
    h.step();
    assert_eq!(opened_url(&h).as_deref(), Some("https://github.com/Tatayoyoh/rustyrobo/releases/download/v9.9.9/rustyrobo-v9.9.9-linux-x86_64.tar.gz"));
    h.run_steps(3);
    assert!(!has_label(&h, "New version 9.9.9"));
}

#[test]
fn update_check_can_be_turned_off() {
    let env = env("update-off", &AppSettings::default());
    let mut h = app_harness();
    assert!(h.state().settings.check_updates, "on by default");
    h.state_mut().update = Some(std::sync::Arc::new(rustyrobo::update::Release { version: "9.9.9".into(), notes: String::new(), asset: None }));
    h.run_steps(2);
    assert!(h.query_all_by_label_contains("v9.9.9").next().is_some());

    h.get_by_label("Options").click();
    h.run_steps(2);
    h.get_by_label("Check for updates at startup").click();
    h.run_steps(3);
    assert!(h.query_all_by_label_contains("v9.9.9").next().is_none(), "the chip goes away");
    assert!(!saved_settings(&env).check_updates, "saved");

    // A check still running when the option was turned off.
    let late = rustyrobo::update::Release { version: "9.9.9".into(), notes: String::new(), asset: None };
    h.state().tx.send(rustyrobo::app::AppEvent::Update(Ok(Some(late)))).unwrap();
    h.run_steps(3);
    assert!(h.query_all_by_label_contains("v9.9.9").next().is_none(), "its answer is ignored");
}

#[test]
fn settings_without_the_update_option_keep_the_check_on() {
    let s: AppSettings = serde_json::from_str("{}").unwrap();
    assert!(s.check_updates);
}

#[test]
fn f10_rotates_both_ways() {
    use rustyrobo::settings::Orientation;
    let env = env("rotate", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(3);
    assert_eq!(h.state().settings.orientation, Orientation::Vertical);
    h.key_press(Key::F10);
    h.run_steps(2);
    assert_eq!(h.state().settings.orientation, Orientation::Horizontal);
    h.key_press(Key::F10);
    h.run_steps(2);
    assert_eq!(h.state().settings.orientation, Orientation::Vertical);
    assert_eq!(saved_settings(&env).orientation, Orientation::Vertical);
}

fn open_document(h: &mut Harness<'_, App>, text: &str) {
    use rustyrobo::ui::dialogs::{Dialog, DocMode};
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(2);
    h.state_mut().dialog = Some(Dialog::document(DocMode::Edit, 1, "db", "coll", text.into(), None, None));
    h.run_steps(3);
}

fn document_text(h: &Harness<'_, App>) -> String {
    match &h.state().dialog {
        Some(rustyrobo::ui::dialogs::Dialog::Document { text, .. }) => text.clone(),
        _ => panic!("document dialog closed"),
    }
}

#[test]
fn document_editor_validates_while_typing() {
    let _env = env("doc-validate", &AppSettings::default());
    let mut h = harness();
    open_document(&mut h, "{ a: 1 }");
    assert!(!has_label(&h, "Validate"), "no validate button");
    wait_until(&mut h, "valid", Duration::from_secs(5), |h| has_label(h, "Valid document"));
    replace_text(&mut h, "{ a: 1 }", "{ a: ");
    wait_until(&mut h, "error", Duration::from_secs(5), |h| has_label(h, icon::WARNING));
}

#[test]
fn document_editor_find_and_multi_cursor() {
    let _env = env("doc-find", &AppSettings::default());
    let mut h = harness();
    open_document(&mut h, "foo(foo, foo)");
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some("foo(foo, foo)")).next().unwrap().focus();
    h.run_steps(2);

    // Ctrl+F, type, Enter for the next match; Escape closes only the bar.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(2);
    h.query_by(|n| is_text_input(n) && n.value().as_deref() == Some("")).expect("find field").type_text("FOO");
    h.run_steps(3);
    assert!(has_label(&h, "1 of 3"), "case-insensitive matches");
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(has_label(&h, "2 of 3"));
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(!has_label(&h, "of 3"), "search bar closed");
    assert!(h.state().dialog.is_some(), "dialog still open");

    // Ctrl+D selects the word, then each next occurrence; typing edits all.
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some("foo(foo, foo)")).next().unwrap().focus();
    h.run_steps(2);
    for _ in 0..3 {
        h.key_press_modifiers(Modifiers::COMMAND, Key::D);
        h.run_steps(2);
    }
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some("foo(foo, foo)")).next().unwrap().type_text("x");
    h.run_steps(3);
    assert_eq!(document_text(&h), "x(x, x)");
    h.key_press(Key::Backspace);
    h.run_steps(3);
    assert_eq!(document_text(&h), "(, )");
    // Escape leaves multi-cursor mode without closing the dialog.
    h.key_press(Key::Escape);
    h.run_steps(3);
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some("(, )")).next().unwrap().type_text("y");
    h.run_steps(3);
    assert_eq!(document_text(&h).matches('y').count(), 1, "single cursor again");
}

#[test]
fn favorite_scripts_manager() {
    let env = env("favorites", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(2);
    h.get_by_label_contains("Favorite Scripts").click();
    h.run_steps(3);
    assert!(has_label(&h, "No favorite scripts yet"));

    // Add one through the form.
    h.get_by_label_contains("Add").click();
    h.run_steps(3);
    // Database first: `replace_text` types into the first empty field.
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some("")).next().expect("database field").focus();
    h.run_steps(2);
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some("")).next().unwrap().type_text("shop");
    h.run_steps(2);
    replace_text(&mut h, "New script", "Big orders");
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert!(has_label(&h, "shop") && has_label(&h, "Big orders"), "listed under its database");
    assert_eq!(saved_settings(&env).favorites.len(), 1);

    // Clone it, then delete the copy with the Delete key.
    h.get_by_label_contains("Big orders").click();
    h.run_steps(2);
    h.get_by_label("Clone").click();
    h.run_steps(3);
    assert!(has_label(&h, "Copy of Big orders"));
    h.key_press(Key::Delete);
    h.run_steps(3);
    assert!(has_label(&h, "Delete favorite script \"Copy of Big orders\"?"));
    h.get_by_label("Delete").click();
    h.run_steps(3);
    assert!(!has_label(&h, "Copy of Big orders"));
    let saved = saved_settings(&env).favorites;
    assert_eq!((saved.len(), saved[0].name.as_str(), saved[0].db.as_str()), (1, "Big orders", "shop"));
    assert!(has_label(&h, "Any connection"), "no shell open: not tied to a connection");
}

#[test]
fn favorite_scripts_tree_and_drag_and_drop() {
    use rustyrobo::settings::FavoriteScript;
    let conn = local_mongo8();
    let fav = |name: &str, db: &str| FavoriteScript { name: name.into(), connection: Some(conn.id), db: db.into(), script: format!("// {name}"), ..Default::default() };
    let s = AppSettings { connections: vec![conn.clone()], favorites: vec![fav("Alpha", "shop"), fav("Beta", "logs")], ..Default::default() };
    let env = env("favorites-dnd", &s);
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(2);
    h.get_by_label_contains("Favorite Scripts").click();
    h.run_steps(3);
    // Connection > database > scripts.
    for l in ["Local Mongo 8", "logs", "shop", "Alpha", "Beta"] {
        assert!(has_label(&h, l), "{l}");
    }

    // Drag Beta (logs) onto Alpha (shop): moved before it, into shop.
    let entry = |name: &'static str| move |n: &egui_kittest::kittest::AccessKitNode<'_>| n.role() == egui::accesskit::Role::Button && n.label().is_some_and(|l| l.contains(name));
    let from = h.query_by(entry("Beta")).unwrap().rect().center();
    let to = h.query_by(entry("Alpha")).unwrap().rect().center();
    h.hover_at(from);
    h.run_steps(2);
    h.drag_at(from);
    h.run_steps(2);
    for k in 1..=10 {
        h.hover_at(from + (to - from) * (k as f32 / 10.0));
        h.run_steps(1);
    }
    h.drop_at(to);
    h.run_steps(3);
    let saved = saved_settings(&env).favorites;
    let shape: Vec<(&str, &str)> = saved.iter().map(|f| (f.name.as_str(), f.db.as_str())).collect();
    assert_eq!(shape, vec![("Beta", "shop"), ("Alpha", "shop")]);
    assert!(!has_label(&h, "logs"), "empty database gone");
}

// -------------------------------------------------------- fonts and RTL

/// App with the system fonts scanned and the connection manager closed.
fn app_harness() -> Harness<'static, App> {
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(3);
    wait_until(&mut h, "system fonts", Duration::from_secs(20), |_| fonts::lists().is_some());
    h
}

fn open_preferences(h: &mut Harness<'_, App>) {
    h.get_by_label("Options").click();
    h.run_steps(2);
    h.get_by_label("Preferences…").click();
    // The dialog is centered once it knows its size.
    h.run_steps(5);
    assert!(has_label(h, "Interface font:"));
    assert!(has_label(h, "Monospace font:"));
}

/// 0: interface font, 1: monospace font, 2: theme.
fn combo_box<'n>(h: &'n Harness<'_, App>, index: usize) -> egui_kittest::Node<'n> {
    h.query_all_by_role(egui::accesskit::Role::ComboBox).nth(index).expect("combo box")
}

fn press(h: &mut Harness<'_, App>, key: Key) {
    h.key_press(key);
    h.run_steps(2);
}

fn first_ui_font(h: &Harness<'_, App>) -> String {
    h.ctx.fonts(|f| f.definitions().families[&egui::FontFamily::Proportional][0].clone())
}

fn body_size(h: &Harness<'_, App>) -> f32 {
    h.ctx.global_style().text_styles[&egui::TextStyle::Body].size
}

fn wait_for_bundled_font(h: &mut Harness<'_, App>) {
    wait_until(h, "bundled font", Duration::from_secs(20), |h| first_ui_font(h) == "Ubuntu-Light");
}

#[test]
fn preferences_show_fonts_at_once_and_save_them_on_save() {
    let env = env("fonts", &AppSettings::default());
    let mut h = app_harness();
    let Some(family) = fonts::lists().and_then(|l| l.all.first()).cloned() else { return };
    let pick_font = |h: &mut Harness<'_, App>| {
        open_preferences(h);
        combo_box(h, 0).click();
        h.run_steps(3);
        h.get_by_label(&family).click();
        h.run_steps(3);
        assert_eq!(h.state().settings.fonts.ui.as_ref(), Some(&family), "applied before Save");
        // The font is loaded in the background and comes first in its family.
        wait_until(h, "chosen font", Duration::from_secs(20), |h| first_ui_font(h) != "Ubuntu-Light");
        assert_eq!(saved_settings(&env).fonts, fonts::FontChoice::default(), "not saved yet");
    };

    pick_font(&mut h);
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert_eq!(h.state().settings.fonts, fonts::FontChoice::default(), "cancelled");
    wait_for_bundled_font(&mut h);

    pick_font(&mut h);
    press(&mut h, Key::Escape);
    assert!(!has_label(&h, "Interface font:"), "Escape closes the dialog");
    assert_eq!(h.state().settings.fonts, fonts::FontChoice::default(), "Escape cancels");
    wait_for_bundled_font(&mut h);

    pick_font(&mut h);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(h.state().settings.fonts.ui.as_ref(), Some(&family));
    assert_eq!(saved_settings(&env).fonts.ui.as_ref(), Some(&family));
    assert_ne!(first_ui_font(&h), "Ubuntu-Light", "the font stays");
}

#[test]
fn preferences_show_theme_and_font_size_at_once_and_cancel_restores_them() {
    let env = env("appearance", &AppSettings::default());
    let mut h = app_harness();
    open_preferences(&mut h);

    combo_box(&h, 2).click();
    h.run_steps(3);
    h.get_by_label("Nord").click();
    h.run_steps(3);
    assert_eq!(h.state().theme.name, "Nord");

    h.get_by_role(egui::accesskit::Role::Slider).focus();
    h.run_steps(2);
    press(&mut h, Key::ArrowRight);
    assert_eq!(h.state().settings.font_size, 14.0);
    assert_eq!(body_size(&h), 14.0);

    let saved = saved_settings(&env);
    assert_eq!((saved.theme.as_str(), saved.font_size), (theme::DEFAULT_THEME, 13.0), "not saved yet");

    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert_eq!(h.state().settings.theme, theme::DEFAULT_THEME);
    assert_ne!(h.state().theme.name, "Nord");
    assert_eq!(h.state().settings.font_size, 13.0);
    assert_eq!(body_size(&h), 13.0);
    let saved = saved_settings(&env);
    assert_eq!((saved.theme.as_str(), saved.font_size), (theme::DEFAULT_THEME, 13.0));
}

#[test]
fn closing_the_window_does_not_save_what_preferences_show() {
    let env = env("exit", &AppSettings::default());
    let mut h = app_harness();
    open_preferences(&mut h);
    h.get_by_role(egui::accesskit::Role::Slider).focus();
    h.run_steps(2);
    press(&mut h, Key::ArrowRight);
    assert_eq!(h.state().settings.font_size, 14.0);
    eframe::App::on_exit(h.state_mut());
    assert_eq!(saved_settings(&env).font_size, 13.0);
}

#[test]
fn up_and_down_walk_the_focused_combo_box() {
    let _env = env("arrows", &AppSettings::default());
    let mut h = app_harness();
    let Some(families) = fonts::lists().map(|l| &l.all).filter(|l| l.len() >= 3) else { return };
    let font = |h: &Harness<'_, App>| h.state().settings.fonts.ui.clone();
    open_preferences(&mut h);

    press(&mut h, Key::ArrowDown);
    assert_eq!(font(&h), None, "no combo box has the focus");

    // With the keyboard.
    combo_box(&h, 0).focus();
    h.run_steps(2);
    press(&mut h, Key::ArrowDown);
    assert_eq!(font(&h).as_ref(), Some(&families[0]), "after Default");
    press(&mut h, Key::ArrowDown);
    assert_eq!(font(&h).as_ref(), Some(&families[1]));
    assert_eq!(combo_box(&h, 0).value().as_ref(), Some(&families[1]), "the combo box shows it");
    press(&mut h, Key::ArrowUp);
    assert_eq!(font(&h).as_ref(), Some(&families[0]));
    press(&mut h, Key::ArrowUp);
    assert_eq!(font(&h), None);
    press(&mut h, Key::ArrowUp);
    assert_eq!(font(&h), None, "stops on the first item");

    // After choosing with the mouse, the focus stays on the combo box.
    combo_box(&h, 0).click();
    h.run_steps(3);
    h.get_by_label(&families[0]).click();
    h.run_steps(3);
    press(&mut h, Key::ArrowDown);
    assert_eq!(font(&h).as_ref(), Some(&families[1]));

    // The list stays open, on the new item.
    combo_box(&h, 0).click();
    h.run_steps(3);
    press(&mut h, Key::ArrowDown);
    assert_eq!(font(&h).as_ref(), Some(&families[2]));
    assert!(h.query_by_label("Default").is_some(), "the list is open");
    press(&mut h, Key::Enter);
    assert!(h.query_by_label("Default").is_none(), "Enter closes the list");
    assert_eq!(font(&h).as_ref(), Some(&families[2]));

    // Each combo box has its own focus, and stops on its last item.
    combo_box(&h, 2).focus();
    h.run_steps(2);
    let themes: Vec<_> = theme::names().collect();
    for _ in 0..themes.len() + 1 {
        press(&mut h, Key::ArrowDown);
    }
    assert_eq!(h.state().settings.theme, *themes.last().unwrap());
    assert_eq!(font(&h).as_ref(), Some(&families[2]), "the other combo box does not move");
}

#[test]
fn reset_to_default_shows_the_defaults_and_save_keeps_them() {
    let custom = AppSettings {
        batch_size: 200,
        font_size: 18.0,
        theme: "Nord".into(),
        fonts: fonts::FontChoice { ui: Some("Missing Font".into()), mono: Some("Missing Mono".into()) },
        connections: vec![ConnectionSettings::default()],
        show_logs: true,
        ..AppSettings::default()
    };
    let env = env("reset", &custom);
    let mut h = app_harness();
    let defaults = AppSettings::default();
    let reset_shown = |h: &Harness<'_, App>| {
        let s = &h.state().settings;
        (s.batch_size, s.font_size, s.theme.clone(), s.fonts.clone()) == (defaults.batch_size, defaults.font_size, defaults.theme.clone(), defaults.fonts.clone())
    };
    open_preferences(&mut h);
    assert!(!h.get_by_label("Reset to Default").accesskit_node().is_disabled());
    h.get_by_label("Reset to Default").click();
    h.run_steps(3);
    assert!(reset_shown(&h), "applied at once");
    assert_eq!(body_size(&h), defaults.font_size);
    assert_eq!(saved_settings(&env).batch_size, 200, "not saved yet");
    assert!(h.get_by_label("Reset to Default").accesskit_node().is_disabled(), "nothing left to reset");

    // The dialog follows the font size, then is centered again.
    h.run_steps(3);
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    let s = &h.state().settings;
    assert_eq!((s.batch_size, s.font_size, s.theme.as_str(), &s.fonts), (200, 18.0, "Nord", &custom.fonts), "cancelled");

    open_preferences(&mut h);
    h.get_by_label("Reset to Default").click();
    h.run_steps(6);
    h.get_by_label("Save").click();
    h.run_steps(3);
    let saved = saved_settings(&env);
    assert_eq!((saved.batch_size, saved.font_size, saved.theme.as_str(), &saved.fonts), (50, 13.0, theme::DEFAULT_THEME, &defaults.fonts));
    assert_eq!(saved.connections.len(), 1, "only the preferences of the dialog are reset");
    assert!(saved.show_logs, "only the preferences of the dialog are reset");
}

#[test]
fn font_size_slider_stays_under_the_pointer_during_a_drag() {
    let _env = env("slider", &AppSettings::default());
    let mut h = app_harness();
    open_preferences(&mut h);
    let slider = |h: &Harness<'_, App>| h.get_by_role(egui::accesskit::Role::Slider).rect();
    let rail = slider(&h);
    let at = |x: f32| egui::pos2(rail.left() + x * rail.width(), rail.center().y);

    h.hover_at(at(0.1));
    h.drag_at(at(0.1));
    h.run_steps(3);
    let start = h.state().settings.font_size;
    let left = slider(&h).left();

    // The pointer stays still: the size must not move anymore.
    h.hover_at(at(0.9));
    let mut sizes = Vec::new();
    for _ in 0..8 {
        h.step();
        sizes.push(h.state().settings.font_size);
        assert_eq!(slider(&h).left(), left, "the slider moved under the pointer: {sizes:?}");
    }
    let end = *sizes.last().unwrap();
    assert!(end > start + 5.0, "from {start} to {end}");
    assert!(sizes[1..].iter().all(|&s| s == end), "the size runs away: {sizes:?}");
    assert_eq!(body_size(&h), end, "the app follows during the drag");

    // At the end of the drag, the dialog follows too.
    h.drop_at(at(0.9));
    h.run_steps(4);
    assert_eq!(h.state().settings.font_size, end);
    assert_ne!(slider(&h).left(), left, "the dialog is laid out with the new size");
}

#[test]
fn preferences_dialog_shrinks_back_after_growing() {
    let _env = env("shrink", &AppSettings::default());
    let mut h = app_harness();
    open_preferences(&mut h);
    let width = |h: &Harness<'_, App>| h.get_by_label("Save").rect().right() - h.get_by_label("Reset to Default").rect().left();
    let start = width(&h);
    h.get_by_role(egui::accesskit::Role::Slider).focus();
    h.run_steps(2);
    for key in [Key::ArrowRight, Key::ArrowLeft] {
        for _ in 0..8 {
            h.key_press(key);
            h.step();
        }
        h.run_steps(5);
        if key == Key::ArrowRight {
            assert!(width(&h) > start + 50.0, "from {start} to {}", width(&h));
        }
    }
    assert_eq!(h.state().settings.font_size, 13.0);
    assert_eq!(width(&h), start);
}

fn docs_output(docs: Vec<Bson>) -> ShellOutput {
    ShellOutput { statement: "db.x.find({})".into(), line: 1, kind: OutputKind::Docs { docs, spec: None, ns: None, more: false }, elapsed: Duration::ZERO }
}

/// Results outside the App: they need no server.
fn pane_harness(docs: Vec<Bson>, mode: ViewMode) -> Harness<'static, OutputPane> {
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 400.0)).build_ui_state(
        |ui, pane: &mut OutputPane| {
            let mut actions = Vec::new();
            let mut cx = PaneCtx { tab: 1, server: 1, index: 0, theme: theme::resolve(theme::DEFAULT_THEME, true), fmt: FormatOptions { tz: Default::default(), uuid: Default::default() }, font_size: 13.0, actions: &mut actions };
            show_pane(ui, pane, &mut cx);
        },
        OutputPane::new(docs_output(docs), mode, 50, true),
    );
    h.run_steps(3);
    h
}

#[test]
fn copy_value_menu_shows_its_shortcut() {
    let mut h = pane_harness(vec![Bson::Document(doc! { "_id": 1, "a": "x", "b": "y" })], ViewMode::Tree);
    let menu_has = |h: &Harness<'_, OutputPane>, item: &str| h.query_all_by_label_contains(item).any(|n| n.accesskit_node().label().is_some_and(|l| l.contains("Ctrl+C")));
    let names_shortcut = |h: &Harness<'_, OutputPane>, item: &str| h.query_all_by_label_contains(item).any(|n| n.accesskit_node().label().is_some_and(|l| l.contains("Ctrl+Shift+C")));

    // One row.
    h.get_by_label("x").click_secondary();
    h.run_steps(3);
    assert!(menu_has(&h, "Copy Value"));
    assert!(!menu_has(&h, "Copy Name"), "this shortcut does not copy the name");
    assert!(names_shortcut(&h, "Copy Name and Value"));
    h.key_press(Key::Escape);
    h.run_steps(3);

    // Several rows.
    h.get_by_label("x").click();
    h.run_steps(2);
    h.get_by_label("y").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    h.get_by_label("y").click_secondary();
    h.run_steps(3);
    assert!(menu_has(&h, "Copy Values"));
    assert!(!menu_has(&h, "Copy JSON"), "this shortcut copies the values");
    assert!(names_shortcut(&h, "Copy Names and Values"));
}

// ------------------------------------------------------ find in the results

/// Opens the search of the pane and types a query.
fn find_in_pane(h: &mut Harness<'_, OutputPane>, query: &str) {
    h.state_mut().open_find();
    h.run_steps(2);
    h.event(egui::Event::Text(query.into()));
    h.run_steps(5);
}

fn pane_has_label(h: &Harness<'_, OutputPane>, text: &str) -> bool {
    h.query_all_by_label(text).next().is_some()
}

#[test]
fn find_goes_through_the_tree_and_opens_what_it_finds() {
    let docs = vec![
        Bson::Document(doc! { "_id": 1, "name": "Ada" }),
        Bson::Document(doc! { "_id": 2, "profile": { "address": { "city": "Paris" } } }),
        Bson::Document(doc! { "_id": 3, "city": "paris" }),
    ];
    let mut h = pane_harness(docs, ViewMode::Tree);
    assert!(!pane_has_label(&h, "Paris"), "collapsed");

    find_in_pane(&mut h, "pari");
    assert!(pane_has_label(&h, "1 of 2"));
    assert!(pane_has_label(&h, "Paris"), "its parents are open");

    h.key_press(Key::Enter);
    h.run_steps(5);
    assert!(pane_has_label(&h, "2 of 2"));
    assert!(pane_has_label(&h, "paris"), "the next one is open");
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(pane_has_label(&h, "1 of 2"), "back to the first");
    h.key_press_modifiers(Modifiers::SHIFT, Key::Enter);
    h.run_steps(3);
    assert!(pane_has_label(&h, "2 of 2"), "backwards");

    // The focus stays in the bar: typing goes on.
    h.event(egui::Event::Text("s".into()));
    h.run_steps(3);
    assert!(pane_has_label(&h, "1 of 2"), "the query changed: back to the first");

    h.event(egui::Event::Text("zzz".into()));
    h.run_steps(3);
    assert!(pane_has_label(&h, "No results"));

    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(!h.state().find_is_open());
    assert!(!pane_has_label(&h, "No results"));
}

#[test]
fn find_scrolls_to_a_hit_far_down_the_tree() {
    let docs: Vec<Bson> = (0..300).map(|i| Bson::Document(doc! { "_id": i, "s": if i == 290 { "needle" } else { "hay" } })).collect();
    let mut h = pane_harness(docs, ViewMode::Tree);
    assert!(!pane_has_label(&h, "needle"));
    find_in_pane(&mut h, "needle");
    assert!(pane_has_label(&h, "1 of 1"));
    let row = h.get_by_label("needle").rect();
    assert!((100.0..300.0).contains(&row.center().y), "not in the middle of the view: {row:?}");
}

#[test]
fn find_scrolls_to_a_cell_far_away_in_the_table() {
    let mut far = doc! { "_id": 299 };
    for c in 0..40 {
        far.insert(format!("c{c}"), if c == 39 { "needle" } else { "hay" });
    }
    let mut docs: Vec<Bson> = (0..299).map(|i| Bson::Document(doc! { "_id": i, "c0": "hay" })).collect();
    docs.push(Bson::Document(far));
    let mut h = pane_harness(docs, ViewMode::Table);
    find_in_pane(&mut h, "needle");
    assert!(pane_has_label(&h, "1 of 1"));
    let cell = h.get_by_label("needle").rect();
    assert!(cell.left() >= 0.0 && cell.right() <= 1000.0 && cell.top() > 0.0 && cell.bottom() < 400.0, "not in view: {cell:?}");
}

#[test]
fn find_in_the_table_counts_cells_and_follows_the_view() {
    let docs = vec![Bson::Document(doc! { "a": { "x": "hit", "y": "hit" }, "b": "hit" })];
    let mut h = pane_harness(docs, ViewMode::Table);
    find_in_pane(&mut h, "hit");
    assert!(pane_has_label(&h, "1 of 2"), "two cells");
    // Other views count what they show.
    h.state_mut().mode = ViewMode::Tree;
    h.run_steps(3);
    assert!(pane_has_label(&h, "1 of 3"), "three rows");
    h.state_mut().mode = ViewMode::Text;
    h.run_steps(3);
    assert!(pane_has_label(&h, "1 of 3"), "three occurrences");
}

#[test]
fn find_in_the_text_views() {
    let mut h = pane_harness(vec![Bson::Document(doc! { "a": "needle", "b": "Needle" })], ViewMode::Text);
    find_in_pane(&mut h, "needle");
    assert!(pane_has_label(&h, "1 of 2"));
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(pane_has_label(&h, "2 of 2"));

    // The output of print().
    let output = ShellOutput { statement: "print()".into(), line: 1, kind: OutputKind::Text("one\ntwo\none".into()), elapsed: Duration::ZERO };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 400.0)).build_ui_state(
        |ui, pane: &mut OutputPane| {
            let mut actions = Vec::new();
            let mut cx = PaneCtx { tab: 1, server: 1, index: 0, theme: theme::resolve(theme::DEFAULT_THEME, true), fmt: FormatOptions::default(), font_size: 13.0, actions: &mut actions };
            show_pane(ui, pane, &mut cx);
        },
        OutputPane::new(output, ViewMode::Tree, 50, true),
    );
    h.run_steps(3);
    find_in_pane(&mut h, "one");
    assert!(pane_has_label(&h, "1 of 2"));
}

#[test]
fn find_follows_the_pages_and_errors_have_none() {
    let mut h = pane_harness(vec![Bson::Document(doc! { "a": "hit" })], ViewMode::Tree);
    find_in_pane(&mut h, "hit");
    assert!(pane_has_label(&h, "1 of 1"));
    h.state_mut().set_page(vec![Bson::Document(doc! { "a": "hit", "b": "hit" })], 50);
    h.run_steps(3);
    assert!(pane_has_label(&h, "1 of 2"), "the bar stays, on the new page");

    let error = ShellOutput { statement: "x".into(), line: 1, kind: OutputKind::Error("boom".into()), elapsed: Duration::ZERO };
    assert!(!OutputPane::new(error, ViewMode::Tree, 50, true).can_find());
}

#[test]
fn rtl_results_read_from_right_to_left() {
    // Labels hold the text in visual order: the last word first.
    let sentence = "שלום עולם";
    let drawn = "עולם שלום";
    let key = "שם מלא";
    let drawn_key = "מלא שם";
    for mode in [ViewMode::Tree, ViewMode::Table] {
        let docs = vec![Bson::Document(doc! { "_id": 1, key: sentence, "latin": "Plain text" })];
        let h = pane_harness(docs, mode);
        let has = |text: &str| h.query_all_by_label_contains(text).next().is_some();
        assert!(has(drawn), "{mode:?}: value");
        assert!(!has(sentence), "{mode:?}: value");
        assert!(has(drawn_key), "{mode:?}: key");
        assert!(!has(key), "{mode:?}: key");
        assert!(has("Plain text"), "{mode:?}: other text is untouched");
    }
}

#[test]
fn long_rtl_values_keep_their_start_visible() {
    // Wider than its column: it starts on the right, so it must end past the
    // left of the cell and not overflow on the right.
    let long = "שלום עולם ".repeat(60);
    let long = long.trim_end();
    let docs = vec![Bson::Document(doc! { "_id": 1, "note": long, "next": "after" })];
    let h = pane_harness(docs, ViewMode::Tree);
    let value = h.query_all_by_label_contains("עולם שלום").next().expect("value");
    let (value_right, value_left) = (value.rect().right(), value.rect().left());
    let header = h.get_by_label("Value").rect();
    let type_header = h.get_by_label("Type").rect();
    assert!(value_right <= type_header.left(), "{value_right} stays in its column");
    assert!(value_left < header.left(), "{value_left} goes past the left of the cell");
}

#[test]
fn rtl_text_view_reads_from_right_to_left_and_copies_the_real_text() {
    let text = "// note\n\"name\" : \"שלום עולם\",\n";
    let mut h = Harness::builder().with_size(egui::vec2(600.0, 200.0)).build_ui_state(
        |ui, text: &mut String| {
            code_view(ui, egui::Id::new("view"), text, theme::resolve(theme::DEFAULT_THEME, true), egui::FontId::monospace(13.0));
        },
        text.to_string(),
    );
    h.ctx.set_fonts(fonts::definitions(&Default::default()));
    h.run_steps(3);
    let view = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
    assert_eq!(view.value().as_deref(), Some(text), "the buffer is not reordered");
    view.click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(2);
    h.event(egui::Event::Copy);
    h.step();
    let copied = h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    assert_eq!(copied.as_deref(), Some(text));
}

#[test]
fn rtl_text_in_an_editor_does_not_break_it() {
    // egui counts too many glyphs in right-to-left runs, so that cursors are off
    // on these rows (emilk/egui#8576). It must stay harmless: no panic on cursors
    // past the end of the text, and no loss of text.
    let text = "שָׁלוֹם עוֹלָם لا إله abc\nسلام 123";
    let mut h = Harness::builder().with_size(egui::vec2(600.0, 200.0)).build_ui_state(
        |ui, text: &mut String| {
            ui.add(egui::TextEdit::multiline(text).desired_width(f32::INFINITY));
        },
        text.to_string(),
    );
    h.ctx.set_fonts(fonts::definitions(&Default::default()));
    h.run_steps(3);
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).click();
    h.run_steps(2);
    for key in [Key::ArrowLeft, Key::ArrowRight, Key::ArrowUp, Key::ArrowDown, Key::Home, Key::End] {
        for _ in 0..text.chars().count() {
            h.key_press(key);
            h.key_press_modifiers(Modifiers::SHIFT, key);
        }
        h.run_steps(1);
        assert_eq!(h.state(), text);
    }
    // Typing past the end of the text, and deleting there.
    h.key_press_modifiers(Modifiers::COMMAND, Key::End);
    h.run_steps(1);
    h.event(egui::Event::Text("x".into()));
    h.run_steps(1);
    assert_eq!(h.state().chars().count(), text.chars().count() + 1);
    h.key_press(Key::Backspace);
    h.key_press(Key::Delete);
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.key_press(Key::Backspace);
    h.run_steps(2);
    assert_eq!(h.state(), "");
}

// ---------------------------------------------------------------- live

fn live() -> bool {
    std::env::var("RR_LIVE").is_ok()
}

fn connect_main(h: &mut Harness<'_, App>) {
    h.run_steps(3);
    h.get_by_label("Local Mongo 8").click();
    h.run_steps(2);
    h.get_by_label("Connect").click();
    wait_until(h, "connection", Duration::from_secs(20), |h| {
        h.state().servers.first().is_some_and(|s| s.conn().is_some())
    });
}

#[test]
fn live_connect_query_and_views() {
    if !live() {
        return;
    }
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let _env = env("live-query", &s);
    let mut h = harness();
    connect_main(&mut h);
    assert!(has_label(&h, "Local Mongo 8 ("), "server node with database count");

    // Seed data through a shell tab, then query it.
    let sid = h.state().servers[0].id;
    h.state_mut().actions.push(rustyrobo::app::Action::OpenShell {
        server: sid,
        db: "rrui".into(),
        script: "db.people.drop(); db.people.insertMany([{ name: 'Ada', age: 36 }, { name: 'Linus', age: 28 }, { name: 'Grace', age: 85 }])".into(),
        execute: true,
        title: "seed".into(),
        cursor: None,
    });
    wait_until(&mut h, "seed", Duration::from_secs(15), |h| h.state().tabs.first().is_some_and(|t| !t.running && t.elapsed.is_some()));

    // Rename the tab from its context menu.
    h.get_by_label("seed").click_secondary();
    h.run_steps(2);
    h.get_by_label("Rename Shell…").click();
    h.run_steps(3);
    replace_text(&mut h, "seed", "people");
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().tabs[0].title, "people");
    assert!(h.state().tabs[0].renaming.is_none());

    // Type a query in the editor and run it with F5.
    let tab = h.state_mut().tabs.first_mut().unwrap();
    tab.script = "db.people.find({ age: { $gt: 30 } }).sort({ age: 1 })".into();
    h.run_steps(2);
    h.key_press(Key::F5);
    wait_until(&mut h, "query results", Duration::from_secs(15), |h| {
        h.state().tabs[0].outputs.first().is_some_and(|o| o.docs().len() == 2)
    });
    h.run_steps(3);
    assert!(has_label(&h, "people"), "result header shows the collection");
    assert!(has_label(&h, "(1) ObjectId("), "tree view root rows");
    assert!(has_label(&h, "Ada"), "first document auto-expanded");

    // Table and text views (F3 / F4).
    h.key_press(Key::F3);
    h.run_steps(3);
    assert!(has_label(&h, "name") && has_label(&h, "age"), "table columns");
    h.key_press(Key::F4);
    h.run_steps(3);
    let text_view = h.query_all_by(|n| n.value().is_some_and(|v| v.contains("\"name\" : \"Grace\""))).next().is_some();
    assert!(text_view, "text view shows shell JSON");

    // Errors are shown in the result pane and the logs.
    h.state_mut().tabs[0].script = "db.people.find({ $nope: 1 })".into();
    h.key_press(Key::F5);
    wait_until(&mut h, "error", Duration::from_secs(15), |h| !h.state().tabs[0].running && h.state().tabs[0].outputs.len() == 1);
    h.run_steps(3);
    assert!(has_label(&h, "unknown top level operator"));

    // Multiple selection in the tree: Ctrl+click, then only the actions
    // that apply to several documents.
    h.state_mut().tabs[0].script = "db.people.find({ age: { $gt: 30 } }).sort({ age: 1 })".into();
    h.key_press(Key::F5);
    wait_until(&mut h, "query results", Duration::from_secs(15), |h| h.state().tabs[0].outputs.first().is_some_and(|o| o.docs().len() == 2));
    h.key_press(Key::F2);
    h.run_steps(3);
    h.get_by_label_contains("(1) ").click();
    h.run_steps(2);
    h.get_by_label_contains("(2) ").click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    assert_eq!(h.state().tabs[0].outputs[0].selected.len(), 2);
    h.get_by_label_contains("(2) ").click_secondary();
    h.run_steps(3);
    assert!(has_label(&h, "Copy JSON (2 documents)"));
    assert!(!has_label(&h, "Edit Document…"), "single-document actions hidden");
    h.get_by_label_contains("Delete 2 Documents…").click();
    h.run_steps(5);
    h.get_by_label("Yes").click();
    wait_until(&mut h, "documents deleted", Duration::from_secs(15), |h| {
        h.state().tabs[0].outputs.first().is_some_and(|o| o.docs().is_empty()) && !h.state().tabs[0].running
    });
}

#[test]
fn live_explorer_tree_and_dialogs() {
    if !live() {
        return;
    }
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let _env = env("live-explorer", &s);
    let mut h = harness();
    connect_main(&mut h);
    let sid = h.state().servers[0].id;

    // Create a database through the dialog.
    h.state_mut().actions.push(rustyrobo::app::Action::Dialog(rustyrobo::ui::dialogs::Dialog::CreateDatabase {
        server: sid,
        name: "rrui_created".into(),
        collection: "first".into(),
    }));
    h.run_steps(3);
    assert!(has_label(&h, "Create Database"));
    h.get_by_label("Create").click();
    wait_until(&mut h, "database listed", Duration::from_secs(15), |h| {
        h.state().servers[0].dbs.iter().any(|d| d.name == "rrui_created")
    });

    // Expanding nodes loads collections lazily.
    let db_id = egui::Id::new(("db", sid, "rrui_created"));
    let srv_id = egui::Id::new(("srv", sid));
    for id in [srv_id, db_id, db_id.with("collections")] {
        let ctx = h.ctx.clone();
        let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(&ctx, id, false);
        st.set_open(true);
        st.store(&ctx);
    }
    wait_until(&mut h, "collections", Duration::from_secs(15), |h| has_label(h, "Collections (1)"));
    assert!(has_label(&h, "first"));

    // Drop it again through the confirmation dialog.
    h.state_mut().actions.push(rustyrobo::app::Action::Dialog(rustyrobo::ui::dialogs::Dialog::confirm(
        "Drop Database",
        "Drop database rrui_created?",
        rustyrobo::app::Action::Command {
            server: sid,
            db: "rrui_created".into(),
            cmd: bson::doc! { "dropDatabase": 1 },
            label: "Drop database rrui_created".into(),
            refresh: rustyrobo::app::Refresh::Databases,
        },
    )));
    h.run_steps(3);
    h.get_by_label("Yes").click();
    wait_until(&mut h, "database dropped", Duration::from_secs(15), |h| {
        !h.state().servers[0].dbs.iter().any(|d| d.name == "rrui_created")
    });
}

#[test]
fn live_bad_password_reports_error() {
    if !live() {
        return;
    }
    let mut c = local_mongo8();
    c.credentials.password = "wrong".into();
    let s = AppSettings { connections: vec![c], ..Default::default() };
    let _env = env("live-badpw", &s);
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Local Mongo 8").click();
    h.run_steps(2);
    h.get_by_label("Connect").click();
    wait_until(&mut h, "error dialog", Duration::from_secs(20), |h| has_label(h, "Cannot connect"));
    assert!(has_label(&h, "Authentication failed"));
}

#[test]
fn live_connection_test_in_editor() {
    if !live() {
        return;
    }
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let _env = env("live-test", &s);
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label(&format!("{} Edit", icon::PENCIL_SIMPLE)).click();
    h.run_steps(3);
    h.get_by_label_contains("Test").click();
    wait_until(&mut h, "test result", Duration::from_secs(20), |h| has_label(h, "Connected to MongoDB"));
    assert!(has_label(&h, "Connect to server"));
    assert!(has_label(&h, "Authentication"));
    assert!(has_label(&h, "List databases"));
}

/// What the last frame put in the clipboard.
fn copied(h: &Harness<'_, App>) -> Option<String> {
    h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    })
}

fn press_copy(h: &mut Harness<'_, App>) -> Option<String> {
    h.event(egui::Event::Copy);
    h.step();
    let text = copied(h);
    h.run_steps(2);
    text
}

/// Ctrl+Shift+C reaches egui as a copy event, with Shift held.
fn press_copy_with_names(h: &mut Harness<'_, App>) -> Option<String> {
    h.event(egui::Event::ModifiersChanged(Modifiers::COMMAND | Modifiers::SHIFT));
    let text = press_copy(h);
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
    text
}

#[test]
fn live_copy_and_find_with_the_keyboard_in_the_results() {
    if !live() {
        return;
    }
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let _env = env("live-keys", &s);
    let mut h = harness();
    connect_main(&mut h);
    let sid = h.state().servers[0].id;
    h.state_mut().actions.push(rustyrobo::app::Action::OpenShell {
        server: sid,
        db: "rrkeys".into(),
        script: "db.people.drop(); db.people.insertMany([{ name: 'Ada', age: 36, tags: ['math'] }, { name: 'Linus', age: 28 }])".into(),
        execute: true,
        title: "seed".into(),
        cursor: None,
    });
    wait_until(&mut h, "seed", Duration::from_secs(15), |h| h.state().tabs.first().is_some_and(|t| !t.running && t.elapsed.is_some()));
    let run = |h: &mut Harness<'_, App>, script: &str| {
        h.state_mut().tabs[0].script = script.into();
        h.run_steps(2);
        h.key_press(Key::F5);
        h.run_steps(2);
        wait_until(h, "results", Duration::from_secs(15), |h| h.state().tabs[0].outputs.first().is_some_and(|o| o.docs().len() == 2) && !h.state().tabs[0].running);
        h.run_steps(3);
    };
    run(&mut h, "db.people.find({}).sort({ age: -1 })");

    // The value under the pointer, without clicking.
    h.get_by_label("Ada").hover();
    h.run_steps(2);
    assert_eq!(press_copy(&mut h).as_deref(), Some("Ada"));
    assert!(has_label(&h, "Copied: Ada"), "the status bar tells what was copied");
    h.get_by_label("36").hover();
    h.run_steps(2);
    assert_eq!(press_copy(&mut h).as_deref(), Some("36"));
    assert_eq!(press_copy_with_names(&mut h).as_deref(), Some("\"age\" : 36"));
    // Elsewhere than on a value, nothing is copied.
    h.get_by_label("Options").hover();
    h.run_steps(2);
    assert_eq!(press_copy(&mut h), None);

    // Even with the focus in the editor, the pointer decides...
    let editor = h.query_all_by(|n| n.role() == egui::accesskit::Role::MultilineTextInput).next().expect("editor").rect();
    h.hover_at(editor.center());
    h.drag_at(editor.center());
    h.drop_at(editor.center());
    h.run_steps(2);
    h.get_by_label("Ada").hover();
    h.run_steps(2);
    assert_eq!(press_copy(&mut h).as_deref(), Some("Ada"), "pointer moved to the value");
    // ...unless the keyboard went to the editor since: it is just resting there.
    h.key_press(Key::End);
    h.run_steps(2);
    assert_eq!(press_copy(&mut h), None, "the value does not replace what the editor copies");

    // Ctrl+F: running the script hands the keyboard over to the results.
    run(&mut h, "db.people.find({}).sort({ age: -1 })");
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(3);
    assert!(h.state().tabs[0].outputs[0].find_is_open(), "opens on the results under the pointer");
    h.event(egui::Event::Text("linus".into()));
    h.run_steps(5);
    assert!(has_label(&h, "1 of 1"));
    assert!(h.query_all_by_label("Linus").next().is_some(), "the second document is open");
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(!h.state().tabs[0].outputs[0].find_is_open());

    // Over the editor, Ctrl+F stays the editor's.
    h.hover_at(editor.center());
    h.drag_at(editor.center());
    h.drop_at(editor.center());
    h.run_steps(2);
    h.hover_at(editor.center());
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(3);
    assert!(!h.state().tabs[0].outputs[0].find_is_open());
    h.key_press(Key::Escape);
    h.run_steps(3);

    // Away from the results: the selected row, once the focus has left the editor.
    h.get_by_label("Linus").click();
    h.run_steps(3);
    h.get_by_label("Options").hover();
    h.run_steps(2);
    assert_eq!(press_copy(&mut h).as_deref(), Some("Linus"));
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(3);
    assert!(h.state().tabs[0].outputs[0].find_is_open(), "the pane with the selection");
    h.key_press(Key::Escape);
    h.run_steps(3);

    // The table: the cell under the pointer.
    h.key_press(Key::F3);
    h.run_steps(3);
    h.get_by_label("Linus").hover();
    h.run_steps(2);
    assert_eq!(press_copy(&mut h).as_deref(), Some("Linus"));
    h.get_by_label("[ 1 element ]").hover();
    h.run_steps(2);
    assert!(press_copy(&mut h).is_some_and(|t| t.contains("math")), "a whole array");

    // Not behind a dialog.
    h.get_by_label("Linus").hover();
    h.run_steps(2);
    h.state_mut().actions.push(rustyrobo::app::Action::Dialog(rustyrobo::ui::dialogs::Dialog::About));
    h.run_steps(3);
    assert_eq!(press_copy(&mut h), None);
}

#[test]
fn live_favorite_scripts_and_delete_key() {
    if !live() {
        return;
    }
    // Documents not expanded: a Shift+click range then holds documents only.
    let s = AppSettings { connections: vec![local_mongo8()], auto_expand: false, ..Default::default() };
    let env = env("live-favorites", &s);
    let mut h = harness();
    connect_main(&mut h);
    let sid = h.state().servers[0].id;
    h.state_mut().actions.push(rustyrobo::app::Action::OpenShell {
        server: sid,
        db: "rrui".into(),
        script: "db.fav.drop(); db.fav.insertMany([{ n: 1 }, { n: 2 }, { n: 3 }])".into(),
        execute: true,
        title: "numbers".into(),
        cursor: None,
    });
    wait_until(&mut h, "seed", Duration::from_secs(15), |h| h.state().tabs.first().is_some_and(|t| !t.running && t.elapsed.is_some()));

    // The star asks for a name and files the script under its connection.
    h.get_by_label(rustyrobo::ui::ICON_FAVORITE).click();
    h.run_steps(3);
    assert!(has_label(&h, "Add to Favorite Scripts"));
    replace_text(&mut h, "numbers", "Numbers");
    h.key_press(Key::Enter);
    h.run_steps(3);
    let favs = saved_settings(&env).favorites;
    assert_eq!((favs.len(), favs[0].name.as_str(), favs[0].db.as_str()), (1, "Numbers", "rrui"));
    assert_eq!(favs[0].connection, Some(s.connections[0].id));
    h.state_mut().tabs[0].script = "db.fav.find({})".into();
    h.run_steps(3);
    h.get_by_label(&format!("{} •", rustyrobo::ui::ICON_FAVORITE)).click();
    h.run_steps(3);
    assert_eq!(saved_settings(&env).favorites[0].script, "db.fav.find({})");

    // Load it from the manager: select, then Enter.
    h.get_by_label_contains("Favorite Scripts").click();
    h.run_steps(3);
    let entry = |n: &egui_kittest::kittest::AccessKitNode<'_>| n.role() == egui::accesskit::Role::Button && n.label().is_some_and(|l| l.contains("Numbers"));
    h.query_by(entry).expect("favorite entry").click();
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().tabs.len(), 2);
    assert_eq!(h.state().tabs[1].favorite, Some(favs[0].id), "new shell linked to the favorite");
    assert_eq!(h.state().tabs[1].script, "db.fav.find({})");

    // Delete key on a Shift+click range of documents.
    h.key_press(Key::F5);
    wait_until(&mut h, "results", Duration::from_secs(15), |h| h.state().tabs[1].outputs.first().is_some_and(|o| o.docs().len() == 3));
    h.run_steps(3);
    h.get_by_label_contains("(1) ").click();
    h.run_steps(2);
    h.get_by_label_contains("(3) ").click_modifiers(Modifiers::SHIFT);
    h.run_steps(2);
    assert_eq!(h.state().tabs[1].outputs[0].selected.len(), 3);
    h.key_press(Key::Delete);
    h.run_steps(5);
    assert!(has_label(&h, "Delete 3 documents from \"fav\"?"));
    h.get_by_label("Yes").click();
    wait_until(&mut h, "documents deleted", Duration::from_secs(15), |h| {
        h.state().tabs[1].outputs.first().is_some_and(|o| o.docs().is_empty()) && !h.state().tabs[1].running
    });

    // Loading a favorite whose connection is closed connects first.
    h.state_mut().actions.push(rustyrobo::app::Action::Disconnect(sid));
    h.run_steps(3);
    assert!(h.state().servers.is_empty() && h.state().tabs.is_empty());
    h.state_mut().actions.push(rustyrobo::app::Action::OpenFavorite(favs[0].id));
    wait_until(&mut h, "reconnect and open", Duration::from_secs(20), |h| h.state().tabs.len() == 1);
    assert_eq!(h.state().tabs[0].title, "Numbers");
}

#[test]
fn live_cross_script_variables() {
    if !live() {
        return;
    }
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let _env = env("live-vars", &s);
    let mut h = harness();
    connect_main(&mut h);
    let sid = h.state().servers[0].id;
    h.state_mut().actions.push(rustyrobo::app::Action::OpenShell {
        server: sid,
        db: "rrui".into(),
        script: "const n = 2\nlet seen = new Set([1])\nn * seen.size".into(),
        execute: true,
        title: "vars".into(),
        cursor: None,
    });
    let done = |h: &Harness<'_, App>| h.state().tabs.first().is_some_and(|t| !t.running && t.elapsed.is_some());
    wait_until(&mut h, "first run", Duration::from_secs(15), done);
    let last_text = |h: &Harness<'_, App>| match &h.state().tabs[0].outputs.last().map(|o| &o.output.kind) {
        Some(OutputKind::Text(t)) => t.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(last_text(&h), "2");
    assert!(!h.state().tabs[0].keep_vars, "off by default");

    // By default every execution starts clean: running again works.
    let tab = h.state().tabs[0].id;
    let run = |h: &mut Harness<'_, App>| {
        h.state_mut().tabs[0].elapsed = None;
        h.state_mut().actions.push(rustyrobo::app::Action::Execute(tab));
        wait_until(h, "run", Duration::from_secs(15), done);
    };
    run(&mut h);
    assert_eq!(last_text(&h), "2");

    // Keep the variables, but drop `n`.
    h.get_by_label("Cross-script variables").click();
    h.run_steps(3);
    assert!(h.state().tabs[0].keep_vars);
    h.get_by_label(&format!("{} 2/2", icon::LIST_CHECKS)).click();
    h.run_steps(3);
    h.get_by_label("n").click();
    h.run_steps(3);
    assert!(has_label(&h, "Set(1)"), "summary of the kept variable");
    h.key_press(Key::Escape);
    h.state_mut().tabs[0].script = "seen.add(typeof n === 'undefined' ? 'dropped' : 'kept'); seen.size + typeof n".into();
    run(&mut h);
    // `typeof n` reads a dropped variable: undefined.
    assert_eq!(last_text(&h), "2undefined");
    run(&mut h);
    assert_eq!(last_text(&h), "2undefined", "a Set keeps one 'dropped' entry");
}

