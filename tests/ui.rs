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
use rustyrobo::app::{Action, App, CollNode, DbNode, Load, Server, ServerStatus, Zone};
use rustyrobo::bsonfmt::FormatOptions;
use rustyrobo::shell::{OutputKind, ShellOutput};
use rustyrobo::settings::{AppSettings, ConnectionSettings, Credentials, HostPort, ViewMode};
use rustyrobo::ui::dialogs::{Dialog, DocMode};
use rustyrobo::ui::editor::code_view;
use rustyrobo::ui::icon;
use rustyrobo::ui::output::{PaneCtx, show_pane};
use rustyrobo::transfer::{Delimiter, Format};
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

fn has_label<S>(h: &Harness<'_, S>, text: &str) -> bool {
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

/// A click of the pointer at `pos`, where there may be no widget of its own.
fn click_at<State>(h: &mut Harness<'_, State>, pos: egui::Pos2, button: egui::PointerButton) {
    h.hover_at(pos);
    h.run_steps(1);
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE });
        h.run_steps(1);
    }
    h.run_steps(2);
}

#[test]
fn a_click_anywhere_on_a_connection_row_selects_it() {
    let mut other = local_mongo8();
    other.id = uuid::Uuid::new_v4();
    other.name = "Production".into();
    let s = AppSettings { connections: vec![local_mongo8(), other.clone()], ..Default::default() };
    let _env = env("row-click", &s);
    let mut h = harness();
    h.run_steps(4);
    h.get_by_label("Local Mongo 8").click();
    h.run_steps(2);
    assert_eq!(h.state().conn_list.as_ref().unwrap().selected, Some(s.connections[0].id));
    let row_y = h.get_by_label("Production").rect().center().y;
    // The "Last used" column, then the empty end of the "Attributes" one.
    let last_x = h.get_by_label("Last used").rect().center().x;
    let attrs = h.get_by_label("Attributes").rect();
    click_at(&mut h, egui::pos2(last_x, row_y), egui::PointerButton::Primary);
    assert_eq!(h.state().conn_list.as_ref().unwrap().selected, Some(other.id), "last used");
    h.get_by_label("Local Mongo 8").click();
    h.run_steps(2);
    assert_eq!(h.state().conn_list.as_ref().unwrap().selected, Some(s.connections[0].id), "name");
    click_at(&mut h, egui::pos2(attrs.right() - 2.0, row_y), egui::PointerButton::Primary);
    assert_eq!(h.state().conn_list.as_ref().unwrap().selected, Some(other.id), "attributes");
    // The menu of the row, from anywhere on it.
    h.get_by_label("Local Mongo 8").click();
    h.run_steps(2);
    click_at(&mut h, egui::pos2(last_x, row_y), egui::PointerButton::Secondary);
    assert_eq!(h.state().conn_list.as_ref().unwrap().selected, Some(other.id));
    assert!(h.query_by_label("Copy connection string").is_some(), "context menu");
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
    h.key_press_modifiers(Modifiers::COMMAND, Key::J);
    h.run_steps(3);
    assert!(h.state().favorites.is_some(), "Ctrl+J opens the favorite scripts");
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
    assert!(has_label(&h, "Open a collection"));
    assert!(has_label(&h, "View / edit the selected document"));
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
        h.get_by_label_contains("v9.9.9 available").click();
        h.run_steps(4);
        assert!(has_label(h, "New version 9.9.9"));
    };
    open(&mut h);
    assert!(text_field_shows(&h, "- Things"), "the changelog");
    assert!(has_label(&h, "Full changelog"));
    assert!(!text_field_shows(&h, "SHA256SUMS"), "the commands start folded");

    // The commands, by system.
    h.get_by_label("Update manually").click();
    // Unfolding is animated: until it ends, the tabs move under the clicks.
    h.run_steps(30);
    for (os, command) in [("Windows", "Expand-Archive"), ("Linux", "tar xzf"), ("macOS", "ditto")] {
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

    // The link: the file for this system, in the browser, and the dialog stays.
    open(&mut h);
    h.get_by_label("Download 9.9.9 binary").click();
    h.step();
    assert_eq!(opened_url(&h).as_deref(), Some("https://github.com/Tatayoyoh/rustyrobo/releases/download/v9.9.9/rustyrobo-v9.9.9-linux-x86_64.tar.gz"));
    h.run_steps(3);
    assert!(has_label(&h, "New version 9.9.9"));
}

fn update_dialog(h: &mut Harness<'_, App>, asset: Option<&str>) {
    let release = std::sync::Arc::new(rustyrobo::update::Release { version: "9.9.9".into(), notes: String::new(), asset: asset.map(String::from) });
    h.state_mut().update = Some(release.clone());
    h.state_mut().actions.push(rustyrobo::app::Action::Dialog(rustyrobo::ui::dialogs::Dialog::update(release)));
    h.run_steps(4);
}

fn install_button(h: &Harness<'_, App>) -> bool {
    !h.get_by_label_contains("Download update and restart").accesskit_node().is_disabled()
}

#[test]
fn update_installation_shows_its_progress_and_can_be_cancelled() {
    use rustyrobo::app::AppEvent;
    use rustyrobo::update::Install;
    use std::sync::atomic::{AtomicBool, Ordering};
    let _env = env("update-install", &AppSettings::default());
    let mut h = app_harness();
    update_dialog(&mut h, Some("file.tar.gz"));
    assert!(install_button(&h));

    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    h.state_mut().install = Install::Running { done: 0, total: None, cancel: cancel.clone() };
    h.step();
    assert!(!install_button(&h), "only one at a time");
    assert!(h.query_all_by_label_contains("Downloading…").next().is_some());
    h.state().tx.send(AppEvent::UpdateProgress(50, Some(200))).unwrap();
    h.run_steps(2);
    assert!(h.query_all_by_label_contains("25%").next().is_some());
    h.state().tx.send(AppEvent::UpdateProgress(200, Some(200))).unwrap();
    h.run_steps(2);
    assert!(h.query_all_by_label_contains("Installing…").next().is_some());

    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(cancel.load(Ordering::Relaxed), "the download stops");
    assert!(matches!(h.state().install, Install::Idle));
    assert!(!has_label(&h, "New version 9.9.9"));

    // A failure is shown, and can be retried. It goes away with the dialog.
    update_dialog(&mut h, Some("file.tar.gz"));
    h.state_mut().install = Install::Running { done: 0, total: None, cancel };
    h.state().tx.send(AppEvent::UpdateInstalled(Err("Wrong checksum".into()))).unwrap();
    h.run_steps(3);
    assert!(h.query_all_by_label_contains("Wrong checksum").next().is_some());
    assert!(install_button(&h));
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(matches!(h.state().install, Install::Idle));
}

#[test]
fn update_needs_a_file_for_this_system() {
    let _env = env("update-no-file", &AppSettings::default());
    let mut h = app_harness();
    update_dialog(&mut h, None);
    assert!(!install_button(&h));
    assert!(h.query_all_by_label_contains("cannot update by itself").next().is_some());
}

#[cfg(unix)]
#[test]
fn installed_update_restarts_the_program() {
    let _env = env("update-restart", &AppSettings::default());
    let mut h = app_harness();
    h.state_mut().update_target = Some("/usr/bin/env".into());
    h.state().tx.send(rustyrobo::app::AppEvent::UpdateInstalled(Ok(()))).unwrap();
    h.step();
    let closing = h.output().viewport_output.values().any(|v| v.commands.contains(&egui::ViewportCommand::Close));
    assert!(closing, "the new program is started, this one closes");

    // The program cannot start: the update is done all the same.
    let mut h = app_harness();
    h.state_mut().update_target = Some("/no/such/program".into());
    update_dialog(&mut h, Some("file.tar.gz"));
    h.state().tx.send(rustyrobo::app::AppEvent::UpdateInstalled(Ok(()))).unwrap();
    h.run_steps(3);
    assert!(h.query_all_by_label_contains("could not restart").next().is_some());
}

#[test]
fn live_failed_update_leaves_the_program_alone() {
    if !live() {
        return;
    }
    let env = env("update-404", &AppSettings::default());
    let mut h = app_harness();
    let target = env.dir.join("rustyrobo");
    std::fs::write(&target, "the program").unwrap();
    h.state_mut().update_target = Some(target.clone());
    update_dialog(&mut h, Some("no-such-file.tar.gz"));
    h.get_by_label_contains("Download update and restart").click();
    wait_until(&mut h, "failure", Duration::from_secs(30), |h| matches!(h.state().install, rustyrobo::update::Install::Failed(_)));
    h.run_steps(3);
    assert!(h.query_all_by_label_contains("Cannot download the update").next().is_some());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "the program");
    assert_eq!(std::fs::read_dir(&env.dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".rustyrobo-update")).count(), 0, "no leftovers");
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

/// Two clicks of the pointer (`Node::click` goes through AccessKit, which
/// egui does not count as a double-click).
fn double_click_at<State>(h: &mut Harness<'_, State>, pos: egui::Pos2) {
    h.hover_at(pos);
    h.run_steps(1);
    // All in one frame: the harness runs a frame per queued event, and a
    // frame lasts longer than the double-click delay.
    for pressed in [true, false, true, false] {
        h.input_mut().events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    }
    h.run_steps(3);
}

/// Replaces the content of the text field holding `value` by typing over it.
fn retype(h: &mut Harness<'_, App>, value: &str, new: &str) {
    let field = |h: &Harness<'_, App>, v: &str| h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some(v)).next().is_some();
    assert!(field(h, value), "no text field holds {value:?}");
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some(value)).next().unwrap().focus();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(1);
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some(value)).next().unwrap().type_text(new);
    h.run_steps(2);
}

fn saved_history(env: &Env) -> rustyrobo::history::History {
    serde_json::from_str(&std::fs::read_to_string(env.dir.join("history.json")).unwrap()).unwrap()
}

/// Is a list entry of the history window (a button holding `script`).
fn history_entry(script: &'static str) -> impl Fn(&egui_kittest::kittest::AccessKitNode<'_>) -> bool {
    move |n| n.role() == egui::accesskit::Role::Button && n.label().is_some_and(|l| l.contains(script))
}

#[test]
fn query_history_window() {
    use rustyrobo::history::{History, HistoryEntry};
    let conn = local_mongo8();
    let entry = |script: &str, db: &str| HistoryEntry { connection: Some(conn.id), connection_name: "Old name".into(), db: db.into(), script: script.into(), ..Default::default() };
    let history = History { entries: vec![entry("db.users.find({})", "shop"), entry("db.orders.count()", "shop"), entry("db.logs.drop()", "logs")] };
    let env = env("history", &AppSettings { connections: vec![conn.clone()], ..Default::default() });
    std::fs::write(env.dir.join("history.json"), serde_json::to_string(&history).unwrap()).unwrap();
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    h.run_steps(3);
    assert!(has_label(&h, "Query History"));
    for script in ["db.users.find({})", "db.orders.count()", "db.logs.drop()"] {
        assert!(h.query_by(history_entry(script)).is_some(), "{script}");
    }
    assert!(has_label(&h, "Local Mongo 8 · shop"), "current name of the connection");
    assert_eq!(h.state().history_window.as_ref().unwrap().selected, Some(history.entries[2].id), "newest first, selected");

    // The search field has the focus: typing filters.
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some("")).next().unwrap().type_text("SHOP users");
    h.run_steps(3);
    assert!(h.query_by(history_entry("db.users.find({})")).is_some());
    assert!(h.query_by(history_entry("db.orders.count()")).is_none() && h.query_by(history_entry("db.logs.drop()")).is_none());
    replace_text(&mut h, "SHOP users", "nothing like this");
    assert!(has_label(&h, "No matching queries"));
    replace_text(&mut h, "nothing like this", "");

    // Arrows select, Del deletes, and the file follows.
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(h.state().history_window.as_ref().unwrap().selected, Some(history.entries[1].id));
    assert!(h.query_by_label(icon::TRASH).is_none(), "no delete button until a row is hovered");

    // The open and delete buttons of a row show when the row is hovered, far apart.
    let open = format!("{} Open", icon::ARROW_SQUARE_OUT);
    assert!(h.query_by_label(&open).is_none());
    h.query_by(history_entry("db.orders.count()")).unwrap().hover();
    h.run_steps(2);
    let gap = h.get_by_label(icon::TRASH).rect().min.x - h.get_by_label(&open).rect().max.x;
    assert!(gap >= 24.0, "{gap}");
    h.get_by_label(icon::TRASH).click();
    h.run_steps(3);
    assert!(h.query_by(history_entry("db.orders.count()")).is_none());
    assert_eq!(saved_history(&env).entries.len(), 2);

    // Clear All asks in a popup: Keep and Escape close it, not the window.
    h.get_by_label("Clear All…").click();
    h.run_steps(3);
    assert!(has_label(&h, "Clear History") && has_label(&h, "Clear the 2 queries of the history?"));
    h.get_by_label("Keep").click();
    h.run_steps(3);
    assert!(!has_label(&h, "Clear the 2 queries of the history?"));
    h.get_by_label("Clear All…").click();
    h.run_steps(3);
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(!has_label(&h, "Clear the 2 queries of the history?"));
    assert!(h.state().history_window.is_some());
    assert_eq!(h.state().history.entries.len(), 2);
    h.get_by_label("Clear All…").click();
    h.run_steps(3);
    h.get_by_label("Clear").click();
    h.run_steps(3);
    assert!(has_label(&h, "No queries yet"));
    assert!(saved_history(&env).entries.is_empty());

    // Recording can be turned off from the window.
    h.get_by_label("Record executed scripts").click();
    h.run_steps(2);
    assert!(!saved_settings(&env).query_history);
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(h.state().history_window.is_none());
}

#[test]
fn export_and_import_dialogs() {
    use rustyrobo::app::Action;
    use rustyrobo::ui::dialogs::Dialog;
    let env = env("transfer-dialogs", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(2);

    h.state_mut().actions.push(Action::Dialog(Dialog::export(1, "shop", "orders")));
    h.run_steps(3);
    assert!(has_label(&h, "Export — shop.orders"));
    // Canonical by default, the info button tells why.
    assert!(matches!(&h.state().dialog, Some(Dialog::Export(f)) if f.canonical));
    assert!(!has_label(&h, "Canonical, the default, also writes the type of each number"));
    h.get_by_label(icon::INFO).click();
    h.run_steps(3);
    assert!(has_label(&h, "Canonical or relaxed Extended JSON") && has_label(&h, "Canonical, the default, also writes the type of each number"));
    assert!(has_label(&h, "like NumberLong(5), comes back as a 32-bit one"));
    h.get_by_label(icon::INFO).click();
    h.run_steps(3);
    assert!(!has_label(&h, "Canonical, the default, also writes the type of each number"), "closed by a second click");
    assert!(matches!(&h.state().dialog, Some(Dialog::Export(_))), "the export dialog stays");
    let default = h.query_all_by(|n| is_text_input(n) && n.value().is_some_and(|v| v.ends_with("orders.json"))).next().unwrap().value().unwrap();
    let path = env.dir.join("orders.json").display().to_string();
    retype(&mut h, &default, &path);
    assert!(has_input(&h, &path));
    // CSV: fields and delimiter, and the extension follows.
    h.get_by_label("CSV").click();
    h.run_steps(3);
    assert!(h.query_by_label("Semicolon").is_some() && h.query_by_label("Canonical").is_none());
    assert!(has_input(&h, &env.dir.join("orders.csv").display().to_string()));
    // A bad query keeps the dialog open with the error.
    retype(&mut h, "{}", "{ broken");
    h.get_by_label("Export").click();
    h.run_steps(3);
    let error = match &h.state().dialog {
        Some(Dialog::Export(f)) => f.error.clone().unwrap_or_default(),
        _ => panic!("dialog closed"),
    };
    assert!(error.starts_with("Query:"), "{error}");
    assert!(has_label(&h, &error));
    // The folder and the format of an export are proposed for the next ones.
    retype(&mut h, "{ broken", "{}");
    h.get_by_label("Semicolon").click();
    h.run_steps(2);
    h.get_by_label("Export").click();
    h.run_steps(3);
    assert!(h.state().dialog.is_none());
    let saved = saved_settings(&env).export;
    assert_eq!((saved.dir, saved.format, saved.delimiter), (env.dir.display().to_string(), Format::Csv, Delimiter::Semicolon));
    h.state_mut().actions.push(Action::Dialog(Dialog::export(1, "shop", "users")));
    h.run_steps(3);
    assert!(has_input(&h, &env.dir.join("users.csv").display().to_string()));
    match &h.state().dialog {
        Some(Dialog::Export(f)) => assert_eq!((f.format, f.delimiter), (Format::Csv, Delimiter::Semicolon)),
        _ => panic!("no export dialog"),
    }
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().dialog.is_none());

    // Import: the format and the collection follow the file name.
    let file = env.dir.join("customers.csv");
    std::fs::write(&file, "name,age\nAda,36\n").unwrap();
    h.state_mut().actions.push(Action::Dialog(Dialog::import(1, "shop", None)));
    h.run_steps(3);
    assert!(has_label(&h, "Import — shop"));
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some("")).next().unwrap().focus();
    h.run_steps(2);
    h.query_all_by(|n| is_text_input(n) && n.value().as_deref() == Some("")).next().unwrap().type_text(&file.display().to_string());
    h.run_steps(3);
    assert!(has_input(&h, "customers"), "collection named after the file");
    assert!(has_label(&h, "Detect numbers, booleans, dates and ObjectIds"), "CSV options");
    h.get_by_label("Drop the collection first").click();
    h.run_steps(2);
    assert!(has_label(&h, "will be deleted"));
    h.get_by_label("Import").click();
    h.run_steps(3);
    // Not connected: nothing runs, nothing breaks.
    assert!(h.state().dialog.is_none() && h.state().transfers.is_empty());
}

#[test]
fn dump_and_restore_dialogs() {
    use rustyrobo::app::Action;
    use rustyrobo::ui::dialogs::Dialog;
    let env = env("dump-dialogs", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(2);

    // Dump: a folder and the collections listed in the explorer.
    h.state_mut().actions.push(Action::Dialog(Dialog::dump(1, "shop", vec!["orders".into(), "users".into()])));
    h.run_steps(3);
    assert!(has_label(&h, "Dump — shop") && has_label(&h, "Collections (2 of 2)"));
    let home = h.query_all_by(is_text_input).next().unwrap().value().unwrap();
    retype(&mut h, &home, &env.dir.display().to_string());
    assert!(has_label(&h, &env.dir.join("shop").display().to_string()), "where the files go");
    h.get_by_label("None").click();
    h.run_steps(2);
    h.get_by_label("Dump").click();
    h.run_steps(3);
    assert!(has_label(&h, "Choose at least one collection"));
    h.get_by_label("users").click();
    h.get_by_label("Compress (gzip, like mongodump --gzip)").click();
    h.run_steps(2);
    h.get_by_label("Dump").click();
    h.run_steps(3);
    // Not connected: nothing runs, but the folder is kept for the next time.
    assert!(h.state().dialog.is_none() && h.state().transfers.is_empty());
    assert_eq!(saved_settings(&env).last_dump, env.dir.join("shop").display().to_string());
    h.state_mut().actions.push(Action::Dialog(Dialog::dump(1, "shop", Vec::new())));
    h.run_steps(3);
    assert!(has_input(&h, &env.dir.display().to_string()), "the folder of the last dump");
    h.get_by_label("Cancel").click();
    h.run_steps(2);

    // Restore: the dump of the database in that folder, if any.
    let folder = env.dir.join("shop");
    std::fs::create_dir_all(&folder).unwrap();
    for f in ["users.bson", "users.metadata.json", "a%2Fb.bson.gz", "adults.metadata.json"] {
        std::fs::write(folder.join(f), "").unwrap();
    }
    h.state_mut().actions.push(Action::Dialog(Dialog::restore(1, "shop")));
    h.run_steps(3);
    assert!(has_label(&h, "Restore — shop") && has_input(&h, &folder.display().to_string()));
    assert!(has_label(&h, "Collections (3 of 3)") && has_label(&h, "a/b") && has_label(&h, "adults (view)"));
    retype(&mut h, &folder.display().to_string(), &env.dir.display().to_string());
    assert!(has_label(&h, "No collection in this dump"));
    retype(&mut h, &env.dir.display().to_string(), &folder.display().to_string());
    retype(&mut h, "shop", "shop copy");
    h.get_by_label("Drop each collection first").click();
    h.run_steps(2);
    assert!(has_label(&h, "will be replaced by its copy once complete"));
    h.get_by_label("Restore").click();
    h.run_steps(3);
    assert!(has_label(&h, "Invalid database name: shop copy"));
    retype(&mut h, "shop copy", "shop_copy");
    h.get_by_label("Restore").click();
    h.run_steps(3);
    assert!(h.state().dialog.is_none() && h.state().transfers.is_empty());

    // A folder restored from is proposed again, also for another database.
    let old = env.dir.join("backups").join("old");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("users.bson"), "").unwrap();
    h.state_mut().actions.push(Action::Dialog(Dialog::restore(1, "other")));
    h.run_steps(3);
    assert!(has_input(&h, &folder.display().to_string()), "the last one, with no dump of \"other\" next to it");
    retype(&mut h, &folder.display().to_string(), &old.display().to_string());
    h.get_by_label("Restore").click();
    h.run_steps(3);
    assert_eq!(saved_settings(&env).last_dump, old.display().to_string());
    h.state_mut().actions.push(Action::Dialog(Dialog::restore(1, "other")));
    h.run_steps(3);
    assert!(has_input(&h, &old.display().to_string()));
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    h.state_mut().actions.push(Action::Dialog(Dialog::dump(1, "other", Vec::new())));
    h.run_steps(3);
    assert!(has_input(&h, &env.dir.join("backups").display().to_string()), "dumps go next to it");

    // An archive: one file, remembered with its format.
    h.get_by_label("Archive (one file)").click();
    h.get_by_label("Compress (gzip, like mongodump --gzip)").click();
    h.run_steps(2);
    let archive = env.dir.join("backups").join("other.archive.gz");
    assert!(has_label(&h, &archive.display().to_string()), "where the archive goes");
    h.get_by_label("Dump").click();
    h.run_steps(3);
    assert_eq!(saved_settings(&env).last_dump, archive.display().to_string());
    // The archive of mongodump: a prelude with the collections, then their documents.
    let mut file = vec![0x6d, 0xe2, 0x99, 0x81];
    bson::doc! { "version": "0.1" }.to_writer(&mut file).unwrap();
    for (name, kind) in [("orders", "collection"), ("recent", "view")] {
        bson::doc! { "db": "other", "collection": name, "metadata": format!(r#"{{"type":"{kind}"}}"#), "size": 0, "type": kind }.to_writer(&mut file).unwrap();
    }
    file.extend([0xff; 4]);
    let mut gz = flate2::write::GzEncoder::new(std::fs::File::create(&archive).unwrap(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &file).unwrap();
    gz.finish().unwrap();
    h.state_mut().actions.push(Action::Dialog(Dialog::dump(1, "other", Vec::new())));
    h.run_steps(3);
    match &h.state().dialog {
        Some(Dialog::Dump(f)) => assert!(f.archive && f.gzip),
        _ => panic!("no dump dialog"),
    }
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    h.state_mut().actions.push(Action::Dialog(Dialog::restore(1, "other")));
    h.run_steps(3);
    assert!(has_input(&h, &archive.display().to_string()));
    assert!(has_label(&h, "Collections (2 of 2)") && has_label(&h, "recent (view)"));
}

#[test]
fn transfers_tell_when_they_read_and_when_they_write() {
    use rustyrobo::transfer::Progress;
    let _env = env("transfer-progress", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(2);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    h.state_mut().transfers.push(rustyrobo::app::Transfer { id: 99, label: "Import into shop.orders".into(), done: Progress::Checked(1500), cancel: cancel.clone(), stop: "" });
    h.run_steps(2);
    assert!(has_label(&h, "Import into shop.orders: reading the file, 1500 documents"), "nothing is written while the file is read");
    h.state_mut().transfers[0].done = Progress::Written(1000);
    h.run_steps(2);
    assert!(has_label(&h, "Import into shop.orders: 1000 documents"));
    // The cancel button next to it.
    let row = h.get_by_label("Import into shop.orders: 1000 documents").rect().center().y;
    let distance = |n: &egui_kittest::Node<'_>| (n.rect().center().y - row).abs();
    h.query_all_by_label(icon::X).min_by(|a, b| distance(a).total_cmp(&distance(b))).unwrap().click();
    h.run_steps(2);
    assert!(cancel.load(std::sync::atomic::Ordering::Relaxed), "cancelled from the status bar");
}

/// Asks the window to close, as its close button, Exit or Ctrl+Q do.
fn request_close(h: &mut Harness<'_, App>) {
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    h.step();
}

fn sent(h: &Harness<'_, App>, command: &egui::ViewportCommand) -> bool {
    h.output().viewport_output.values().any(|v| v.commands.contains(command))
}

/// The app without its connection manager.
fn closed_manager_harness() -> Harness<'static, App> {
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(2);
    h
}

/// A transfer as the app runs it, and its cancel flag.
fn running(h: &mut Harness<'_, App>, id: u64, label: &str, done: rustyrobo::transfer::Progress, stop: &'static str) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    h.state_mut().transfers.push(rustyrobo::app::Transfer { id, label: label.into(), done, cancel: cancel.clone(), stop });
    cancel
}

fn transfer_over(h: &mut Harness<'_, App>, id: u64) {
    let done = rustyrobo::app::AppEvent::TransferDone { id, server: 0, result: Err("cancelled: nothing was imported".into()), warning: false, refresh: rustyrobo::app::Refresh::None };
    h.state().tx.send(done).unwrap();
}

#[test]
fn quitting_during_a_transfer_asks_first() {
    use rustyrobo::transfer::Progress;
    use std::sync::atomic::Ordering;
    let _env = env("quit-transfer", &AppSettings::default());
    let mut h = closed_manager_harness();
    let cancel_close = egui::ViewportCommand::CancelClose;

    // Nothing running: it closes.
    request_close(&mut h);
    assert!(!sent(&h, &cancel_close));
    assert!(h.state().quit.is_none());

    // Transfers running: asked first, with what stopping each one leaves.
    let import = running(&mut h, 101, "Import into shop.orders", Progress::Written(1000), "Stopped, it leaves the collection as it was.");
    let restore = running(&mut h, 102, "Restore shop", Progress::Checked(10), "Stopped, it keeps the documents already restored.");
    request_close(&mut h);
    assert!(sent(&h, &cancel_close), "the close is cancelled");
    h.run_steps(2);
    assert!(has_label(&h, "Quit Rustyrobo?") && has_label(&h, "2 transfers are not done:"));
    assert!(has_label(&h, "Import into shop.orders") && has_label(&h, "Stopped, it leaves the collection as it was."));
    assert!(has_label(&h, "Restore shop") && has_label(&h, "Nothing is written yet: it reads its file."), "still reading its dump");

    // Kept running: Escape, or Cancel.
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(h.state().quit.is_none() && !has_label(&h, "Quit Rustyrobo?"));
    request_close(&mut h);
    h.run_steps(2);
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().quit.is_none());
    assert!(!import.load(Ordering::Relaxed) && !restore.load(Ordering::Relaxed), "not stopped");
    assert_eq!(h.state().transfers.len(), 2);

    // Stop and Quit: stopped, then closed once they are over.
    request_close(&mut h);
    h.run_steps(2);
    h.get_by_label("Stop and Quit").click();
    h.run_steps(2);
    assert!(import.load(Ordering::Relaxed) && restore.load(Ordering::Relaxed), "both stopped");
    assert!(has_label(&h, "Stopping the transfers…"));
    assert!(!sent(&h, &egui::ViewportCommand::Close), "they put back what they changed first");
    transfer_over(&mut h, 101);
    h.run_steps(2);
    assert!(!sent(&h, &egui::ViewportCommand::Close), "one is still ending");
    transfer_over(&mut h, 102);
    h.run_steps(2);
    assert!(sent(&h, &egui::ViewportCommand::Close));
    assert!(h.state().dialog.is_none(), "what they say when they stop goes to the logs only");
    assert!(h.state().logs.iter().any(|l| l.text.starts_with("Restore shop: cancelled")));
    request_close(&mut h);
    assert!(!sent(&h, &cancel_close), "this time it closes");
}

#[test]
fn quitting_can_be_forced_while_transfers_stop() {
    use rustyrobo::transfer::Progress;
    let _env = env("quit-now", &AppSettings::default());
    let mut h = closed_manager_harness();
    // A dialog open under the question stays when the quit is cancelled.
    h.state_mut().actions.push(rustyrobo::app::Action::Dialog(rustyrobo::ui::dialogs::Dialog::export(1, "shop", "orders")));
    h.run_steps(2);
    let _cancel = running(&mut h, 7, "Export shop.orders", Progress::Written(5), "Stopped, it leaves an incomplete file.");
    request_close(&mut h);
    h.run_steps(2);
    assert!(has_label(&h, "A transfer is not done:") && has_label(&h, "Stopped, it leaves an incomplete file."));
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(h.state().quit.is_none());
    assert!(matches!(h.state().dialog, Some(rustyrobo::ui::dialogs::Dialog::Export(_))), "the export dialog is still open");

    // Quit Now, without waiting.
    request_close(&mut h);
    h.run_steps(2);
    h.get_by_label("Stop and Quit").click();
    h.run_steps(2);
    h.get_by_label("Quit Now").click();
    h.run_steps(1);
    assert!(sent(&h, &egui::ViewportCommand::Close));
    // Or after a while, when a transfer does not end.
    h.state_mut().quit = Some(rustyrobo::app::Quit { stopping: Instant::now().checked_sub(rustyrobo::app::QUIT_WAIT + Duration::from_secs(1)) });
    h.run_steps(1);
    assert!(sent(&h, &egui::ViewportCommand::Close));
    // A close asked again while they stop is not cancelled.
    request_close(&mut h);
    assert!(!sent(&h, &egui::ViewportCommand::CancelClose));
}

#[test]
fn an_update_waits_for_the_transfers_to_restart() {
    use rustyrobo::transfer::Progress;
    let _env = env("update-transfer", &AppSettings::default());
    let mut h = closed_manager_harness();
    h.state_mut().update_target = Some("/no/such/program".into());
    let _cancel = running(&mut h, 8, "Dump shop", Progress::Written(5), "Stopped, it leaves an incomplete dump.");
    h.state().tx.send(rustyrobo::app::AppEvent::UpdateInstalled(Ok(()))).unwrap();
    h.step();
    assert!(sent(&h, &egui::ViewportCommand::Close) && h.state().restart_on_quit, "started once the user quits");
    request_close(&mut h);
    h.run_steps(2);
    assert!(has_label(&h, "Dump shop"));
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(!h.state().restart_on_quit, "no restart later on");
    assert!(h.state().logs.iter().any(|l| l.text.contains("the new version starts with the next launch")));
}

// ------------------------------------------------------------ documents

fn editor_rect(h: &Harness<'_, App>) -> egui::Rect {
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).rect()
}

/// The close button of the document dialog, next to the maximize one.
fn close_document_dialog(h: &mut Harness<'_, App>) {
    let row = h.query_all_by(|n| n.label().is_some_and(|l| l == icon::CORNERS_OUT || l == icon::CORNERS_IN)).next().expect("maximize button").rect();
    h.query_all_by_label(icon::X).find(|n| (n.rect().center().y - row.center().y).abs() < 2.0).expect("close button").click();
    h.run_steps(3);
}

#[test]
fn document_dialog_maximizes_and_closes_like_a_window() {
    let _env = env("doc-dialog", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(3);
    let text = "{\n    \"_id\" : 1,\n    \"name\" : \"Ada\"\n}";
    h.state_mut().dialog = Some(Dialog::document(DocMode::View, 1, "shop", "orders", text.into(), Some(Bson::Int32(1)), Some(1)));
    h.run_steps(4);
    let normal = editor_rect(&h);
    // The title as in the menus: the eye of View, the key word of Edit in the colour of its kind.
    let title_parts = |h: &Harness<'_, App>, start: &str| {
        h.output().shapes.iter().find_map(|c| match &c.shape {
            egui::Shape::Text(t) if t.galley.text().starts_with(start) => {
                Some(t.galley.job.sections.iter().map(|s| (t.galley.text()[s.byte_range.start.0..s.byte_range.end.0].to_string(), s.format.color)).collect::<Vec<_>>())
            }
            _ => None,
        })
    };
    assert!(title_parts(&h, &format!("{} View Document", icon::EYE)).is_some());
    assert!(has_label(&h, "View Document"), "read without the icon");

    h.get_by_label(icon::CORNERS_OUT).click();
    h.run_steps(4);
    let max = editor_rect(&h);
    assert!(max.width() > 1300.0 && max.height() > 700.0, "{normal:?} → {max:?}");
    assert!(max.bottom() < 900.0, "the buttons stay in the window: {max:?}");
    h.key_press_modifiers(Modifiers::COMMAND, Key::M);
    h.run_steps(4);
    assert_eq!(editor_rect(&h).width(), normal.width(), "Ctrl+M restores it");
    h.key_press_modifiers(Modifiers::COMMAND, Key::M);
    h.run_steps(4);

    // Edit: the same document in the editor, still maximized.
    h.get_by_label("Edit").click();
    h.run_steps(4);
    assert!(matches!(&h.state().dialog, Some(Dialog::Document { mode: DocMode::Edit, original_id: Some(Bson::Int32(1)), tab: Some(1), text: t, .. }) if t == text));
    assert!(has_label(&h, "Edit Document — shop.orders"));
    let edit = title_parts(&h, "Edit Document").expect("title");
    assert_eq!(edit[0], ("Edit".to_string(), h.state().theme.secondary));
    assert_eq!(edit[1].0, " Document — shop.orders");
    assert!(editor_rect(&h).width() > 1300.0, "maximized for the next documents too");
    // A double-click on the title bar restores it, and maximizes it again.
    let title = h.get_by_label("Edit Document — shop.orders").rect().center();
    double_click_at(&mut h, title);
    assert_eq!(editor_rect(&h).width(), normal.width());
    let bar = h.get_by_label(icon::CORNERS_OUT).rect().left_center() - egui::vec2(60.0, 0.0);
    double_click_at(&mut h, bar);
    assert!(editor_rect(&h).width() > 1300.0, "also between the title and the buttons");
    close_document_dialog(&mut h);
    assert!(h.state().dialog.is_none(), "closed by its window button");

    // Not from a collection: nothing to edit.
    h.state_mut().dialog = Some(Dialog::document(DocMode::View, 1, "", "", "{ \"a\" : 1 }".into(), None, Some(1)));
    h.run_steps(4);
    assert!(h.get_by_label("Edit").accesskit_node().is_disabled());
}

#[test]
fn document_dialog_opens_on_a_value_and_has_its_keys() {
    let _env = env("doc-dialog-keys", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(3);
    // On an object far down: the cursor at its key, in the editor, in view.
    let mut text = String::from("{\n    \"_id\" : 1,\n");
    for i in 0..80 {
        text.push_str(&format!("    \"f{i}\" : {i},\n"));
    }
    text.push_str("    \"a\" : {\n        \"b\" : 2\n    }\n}");
    let dialog = Dialog::document(DocMode::Edit, 1, "shop", "orders", text.clone(), Some(Bson::Int32(1)), Some(1)).focus_on(vec!["a".into()]);
    h.state_mut().dialog = Some(dialog);
    h.run_steps(4);
    let cursor = egui::TextEdit::load_state(&h.ctx, egui::Id::new("doc-editor").with("text")).and_then(|s| s.cursor.char_range()).map(|r| r.primary.index.0);
    assert_eq!(cursor, text.find("\"a\""));
    assert!(h.ctx.text_edit_focused());
    let line = text[..text.find("\"a\"").unwrap()].matches('\n').count();
    let shown = h.output().shapes.iter().find_map(|c| match &c.shape {
        egui::Shape::Text(t) if t.galley.job.text.contains("\"f79\"") => Some((t.pos.y + t.galley.rows[line].pos.y, c.clip_rect)),
        _ => None,
    });
    let (y, clip) = shown.expect("the text is painted");
    assert!(clip.y_range().contains(y), "the block is in view: {y} in {clip:?}");

    // Ctrl+S saves, not with a mistake.
    if let Some(Dialog::Document { text, .. }) = &mut h.state_mut().dialog {
        text.push_str(" oops");
    }
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    h.run_steps(3);
    assert!(h.state().dialog.is_some(), "an invalid document stays");
    if let Some(Dialog::Document { text, .. }) = &mut h.state_mut().dialog {
        text.truncate(text.len() - " oops".len());
    }
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    h.run_steps(3);
    assert!(h.state().dialog.is_none(), "saved");

    // Ctrl+E: from the view to the editor, when the document can be edited.
    h.state_mut().dialog = Some(Dialog::document(DocMode::View, 1, "shop", "orders", "{ \"_id\" : 1 }".into(), Some(Bson::Int32(1)), Some(1)));
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    h.run_steps(3);
    assert!(matches!(&h.state().dialog, Some(Dialog::Document { mode: DocMode::Edit, .. })));
    h.state_mut().dialog = Some(Dialog::document(DocMode::View, 1, "", "", "[ 1 ]".into(), None, Some(1)));
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::COMMAND, Key::E);
    h.run_steps(3);
    assert!(matches!(&h.state().dialog, Some(Dialog::Document { mode: DocMode::View, .. })), "nothing to edit");
}

#[test]
fn alt_f4_closes_a_document_not_the_app() {
    let _env = env("alt-f4", &AppSettings::default());
    let mut h = closed_manager_harness();
    h.state_mut().dialog = Some(Dialog::document(DocMode::View, 1, "shop", "orders", "{ \"_id\" : 1 }".into(), Some(Bson::Int32(1)), Some(1)));
    h.run_steps(3);
    h.input_mut().events.push(egui::Event::ModifiersChanged(Modifiers::ALT));
    request_close(&mut h);
    assert!(sent(&h, &egui::ViewportCommand::CancelClose));
    assert!(h.state().dialog.is_none());
    // Without a document, or with the close button of the window: the app closes.
    h.input_mut().events.push(egui::Event::ModifiersChanged(Modifiers::ALT));
    request_close(&mut h);
    assert!(!sent(&h, &egui::ViewportCommand::CancelClose));
    h.state_mut().dialog = Some(Dialog::document(DocMode::View, 1, "shop", "orders", "{ \"_id\" : 1 }".into(), Some(Bson::Int32(1)), Some(1)));
    h.run_steps(3);
    h.input_mut().events.push(egui::Event::ModifiersChanged(Modifiers::NONE));
    request_close(&mut h);
    assert!(!sent(&h, &egui::ViewportCommand::CancelClose));
}

/// [`pane_harness`] keeping the actions of the pane.
fn pane_actions_harness(docs: Vec<Bson>, mode: ViewMode) -> Harness<'static, (OutputPane, Vec<Action>)> {
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 400.0)).build_ui_state(
        |ui, (pane, actions): &mut (OutputPane, Vec<Action>)| {
            let mut cx = PaneCtx { tab: 1, server: 1, index: 0, theme: theme::resolve(theme::DEFAULT_THEME, true), fmt: FormatOptions { tz: Default::default(), uuid: Default::default() }, font_size: 13.0, actions };
            show_pane(ui, pane, &mut cx);
        },
        (OutputPane::new(docs_output(docs), mode, 50, true), Vec::new()),
    );
    h.run_steps(3);
    h
}

#[test]
fn a_double_click_on_the_id_views_the_document() {
    for mode in [ViewMode::Tree, ViewMode::Table] {
        let mut h = pane_actions_harness(vec![Bson::Document(doc! { "_id": "ada-id", "name": "Ada" })], mode);
        let at = h.get_by_label("ada-id").rect().center();
        double_click_at(&mut h, at);
        let viewed = |h: &Harness<'_, (OutputPane, Vec<Action>)>| h.state().1.iter().filter(|a| matches!(a, Action::Dialog(Dialog::Document { mode: DocMode::View, text, .. }) if text.contains("Ada"))).count();
        assert_eq!(viewed(&h), 1, "{mode:?}");
        assert!(h.state().0.cell_edit.is_none(), "{mode:?}: an _id is not edited in place");
        // Not the other values (edited in place, see the live tests).
        let at = h.get_by_label("Ada").rect().center();
        double_click_at(&mut h, at);
        assert_eq!(viewed(&h), 1, "{mode:?}");
    }
}

#[test]
fn ctrl_p_lists_the_collections_matching_what_is_typed() {
    let _env = env("quick-open", &AppSettings::default());
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(3);
    let coll = |name: &str, kind: &str| CollNode { name: name.into(), kind: kind.into(), indexes: Load::Idle };
    let db = |name: &str, colls: Vec<CollNode>| DbNode { name: name.into(), collections: Load::Ready(colls), functions: Load::Idle, users: Load::Idle, coll_names: Vec::new() };
    h.state_mut().servers.push(Server {
        id: 7,
        settings: ConnectionSettings { name: "Prod".into(), ..Default::default() },
        secrets: Default::default(),
        status: ServerStatus::Connecting,
        dbs: vec![db("shop", vec![coll("orders", "collection"), coll("customers", "view"), coll("order_lines", "collection")]), db("blog", vec![coll("posts", "collection")])],
        dbs_loading: false,
    });
    h.key_press_modifiers(Modifiers::COMMAND, Key::P);
    h.run_steps(3);
    let listed = |h: &Harness<'_, App>, name: &str| h.query_all_by(|n| n.role() == egui::accesskit::Role::Button && n.label().is_some_and(|l| l.contains(name))).next().is_some();
    for name in ["orders", "customers", "order_lines", "posts"] {
        assert!(listed(&h, name), "{name}");
    }
    // Rows as wide as the bar.
    let field = h.get_by_role(egui::accesskit::Role::TextInput).rect();
    let row = h.query_all_by(|n| n.role() == egui::accesskit::Role::Button && n.label().is_some_and(|l| l.contains("posts"))).next().unwrap().rect();
    assert!((row.width() - field.width()).abs() < 20.0, "{row:?} {field:?}");
    h.event(egui::Event::Text("ord".into()));
    h.run_steps(3);
    assert!(listed(&h, "orders") && listed(&h, "order_lines"));
    assert!(!listed(&h, "customers") && !listed(&h, "posts"));
    h.event(egui::Event::Text("x".into()));
    h.run_steps(3);
    assert!(has_label(&h, "No collection matches"));
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(h.state().quick_open.is_none());

    // From the File menu too; Ctrl+P closes it.
    h.get_by_label("File").click();
    h.run_steps(2);
    h.get_by_label_contains("Open Collection…").click();
    h.run_steps(3);
    assert!(h.state().quick_open.is_some() && listed(&h, "posts"));
    h.key_press_modifiers(Modifiers::COMMAND, Key::P);
    h.run_steps(3);
    assert!(h.state().quick_open.is_none());
}

#[test]
fn the_explorer_goes_with_the_arrows_and_enter_opens_a_collection() {
    let _env = env("explorer-keys", &AppSettings::default());
    let mut h = closed_manager_harness();
    let coll = |name: &str| CollNode { name: name.into(), kind: "collection".into(), indexes: Load::Idle };
    let db = |name: &str, colls: Vec<CollNode>| DbNode { name: name.into(), collections: Load::Ready(colls), functions: Load::Idle, users: Load::Idle, coll_names: Vec::new() };
    h.state_mut().servers.push(Server {
        id: 7,
        settings: ConnectionSettings { name: "Prod".into(), ..Default::default() },
        secrets: Default::default(),
        status: ServerStatus::Connecting,
        dbs: vec![db("shop", vec![coll("orders"), coll("users")])],
        dbs_loading: false,
    });
    h.run_steps(2);
    let selected = |h: &Harness<'_, App>| h.ctx.data(|d| d.get_temp::<egui::Id>(egui::Id::new("explorer_selected")));
    let at = h.get_by_label_contains("Prod").rect().center();
    click_at(&mut h, at, egui::PointerButton::Primary);
    assert_eq!(h.state().zone, Some(Zone::Explorer));
    let press = |h: &mut Harness<'_, App>, key: Key| {
        h.key_press(key);
        h.run_steps(2);
    };
    // Right opens a node, then goes into it.
    for key in [Key::ArrowRight, Key::ArrowRight] {
        press(&mut h, key);
    }
    assert_eq!(selected(&h), Some(egui::Id::new(("db", 7u64, "shop"))));
    for key in [Key::ArrowRight, Key::ArrowRight, Key::ArrowRight, Key::ArrowRight] {
        press(&mut h, key);
    }
    assert_eq!(selected(&h), Some(egui::Id::new(("coll", 7u64, "shop", "orders"))));
    press(&mut h, Key::ArrowDown);
    assert_eq!(selected(&h), Some(egui::Id::new(("coll", 7u64, "shop", "users"))));
    // Shown, the pointer being on another row.
    let row = h.get_by_label_contains("users").rect().center();
    let fill = h.ctx.global_style().visuals.selection.bg_fill;
    let shown = h.output().shapes.iter().any(|c| matches!(&c.shape, egui::Shape::Rect(r) if r.fill == fill && r.rect.contains(row)));
    assert!(shown, "the selected row has the colour of the selection");
    // Enter opens it (here the server is not connected).
    press(&mut h, Key::Enter);
    assert!(h.state().logs.iter().any(|l| l.text.contains("Server is not connected")));
    // Left goes to the parent, then closes it.
    press(&mut h, Key::ArrowLeft);
    assert_eq!(selected(&h), Some(egui::Id::new(("db", 7u64, "shop")).with("collections")));
    press(&mut h, Key::ArrowLeft);
    assert!(!has_label(&h, "orders"), "closed");
    // Space opens it again, and closes it.
    press(&mut h, Key::Space);
    assert!(has_label(&h, "orders"), "open");
    press(&mut h, Key::Space);
    assert!(!has_label(&h, "orders"), "closed again");
    press(&mut h, Key::ArrowUp);
    assert_eq!(selected(&h), Some(egui::Id::new(("db", 7u64, "shop"))));
}

#[test]
fn a_new_connection_takes_the_keyboard_to_the_explorer() {
    // Nothing listens there: the connection fails, after the checks.
    let nowhere = ConnectionSettings { name: "Nowhere".into(), hosts: vec![HostPort { host: "127.0.0.1".into(), port: 1 }], last_connected: Some(chrono::Utc::now()), ..Default::default() };
    let s = AppSettings { connections: vec![nowhere], ..Default::default() };
    let _env = env("connect-focus", &s);
    let check = |h: &Harness<'_, App>| {
        let sid = h.state().servers.last().expect("server").id;
        assert_eq!(h.state().zone, Some(Zone::Explorer));
        assert_eq!(h.ctx.data(|d| d.get_temp::<egui::Id>(egui::Id::new("explorer_selected"))), Some(egui::Id::new(("srv", sid))));
        assert!(h.ctx.memory(|m| m.focused()).is_some(), "its row has the keyboard");
    };
    // Enter in the connection manager, at startup.
    let mut h = harness();
    h.run_steps(3);
    assert!(has_label(&h, "MongoDB Connections"));
    h.key_press(Key::Enter);
    h.run_steps(3);
    check(&h);
    // A recent connection of the welcome page.
    let mut h = closed_manager_harness();
    h.get_by_label("Nowhere").click();
    h.run_steps(3);
    check(&h);
}

#[test]
fn alt_arrows_go_between_the_areas_of_the_window() {
    let s = AppSettings { show_logs: true, ..Default::default() };
    let _env = env("zones", &s);
    let mut h = harness();
    h.run_steps(3);
    h.get_by_label("Close").click();
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(h.state().zone, Some(Zone::Logs), "nowhere yet: the area at the bottom");
    assert!(h.state().zone_flash.is_some(), "outlined");
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(h.state().zone, Some(Zone::Explorer));
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(h.state().zone, Some(Zone::Explorer), "nothing on the left");
    // A click elsewhere moves the start.
    let logs = h.state().zones.iter().find(|(z, _)| *z == Zone::Logs).unwrap().1;
    click_at(&mut h, logs.center(), egui::PointerButton::Primary);
    assert_eq!(h.state().zone, Some(Zone::Logs));
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
        // The buttons move with the new font: clicks need their new place.
        h.run_steps(3);
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
fn text_view_shows_the_next_documents_at_its_end() {
    // 2.4 MB: shown by parts.
    let big = "x".repeat(6000);
    let docs: Vec<Bson> = (0..400).map(|i| Bson::Document(doc! { "_id": i, "s": big.clone() })).collect();
    let mut h = pane_harness(docs, ViewMode::Text);
    let text = |h: &Harness<'_, OutputPane>| h.get_by_role(egui::accesskit::Role::MultilineTextInput).value().unwrap_or_default();
    assert!(text(&h).contains("more document(s): scroll down to show them"));
    assert!(!text(&h).contains("/* 400 */"));
    // Scrolled down to the end with the wheel, part after part.
    h.hover_at(egui::pos2(500.0, 200.0));
    let mut parts = 1;
    while !text(&h).contains("/* 400 */") && parts < 40 {
        h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Page, delta: egui::vec2(0.0, -1000.0), phase: egui::TouchPhase::Move, modifiers: Modifiers::NONE });
        h.run_steps(3);
        parts += 1;
    }
    assert!(text(&h).contains("/* 400 */"), "loaded at the end");
    assert!(parts > 2, "by parts: {parts}");
    assert!(!text(&h).contains("more document(s)"));
}

#[test]
fn text_view_scrolls_and_its_cursor_goes_by_pages() {
    let docs: Vec<Bson> = (0..60).map(|i| Bson::Document(doc! { "_id": i, "a": 1, "b": 2 })).collect();
    let mut h = pane_harness(docs, ViewMode::Text);
    let fmt = FormatOptions { tz: Default::default(), uuid: Default::default() };
    // Where the first line of the text is drawn, and the top of the view.
    let top = |h: &Harness<'_, OutputPane>| {
        h.output().shapes.iter().find_map(|c| match &c.shape {
            egui::Shape::Text(t) if t.galley.job.text.starts_with("/* 1 */") => Some((t.pos.y, c.clip_rect.top())),
            _ => None,
        })
    };
    let (start, view_top) = top(&h).expect("text");
    // Without a cursor in it: the keys scroll it.
    for (key, scrolled) in [(Key::End, true), (Key::Home, false), (Key::PageDown, true), (Key::ArrowUp, true), (Key::Home, false), (Key::ArrowDown, true)] {
        let _ = rustyrobo::ui::output::navigate(h.state_mut(), key, 1, 1, fmt);
        h.run_steps(10);
        let (y, _) = top(&h).expect("text");
        assert_eq!(y < start, scrolled, "{key:?}: {y} from {start}");
    }
    let _ = rustyrobo::ui::output::navigate(h.state_mut(), Key::Home, 1, 1, fmt);
    h.run_steps(10);

    // With the cursor in it: PageDown moves it by a page, End and Home to the ends.
    click_at(&mut h, egui::pos2(100.0, view_top + 10.0), egui::PointerButton::Primary);
    let cursor = |h: &Harness<'_, OutputPane>| egui::TextEdit::load_state(&h.ctx, egui::Id::new(("out-json", 1u64, 0usize)).with("t")).and_then(|s| s.cursor.char_range()).map(|r| r.primary.index.0).unwrap_or_default();
    let text = h.get_by_role(egui::accesskit::Role::MultilineTextInput).value().unwrap_or_default();
    let line = |at: usize| text.chars().take(at).filter(|&c| c == '\n').count();
    let before = line(cursor(&h));
    h.key_press(Key::PageDown);
    h.run_steps(3);
    assert!(line(cursor(&h)) > before + 5, "a page down");
    h.key_press(Key::End);
    h.run_steps(3);
    assert_eq!(cursor(&h), text.chars().count());
    h.key_press(Key::Home);
    h.run_steps(3);
    assert_eq!(cursor(&h), 0);
}

#[test]
fn keys_hide_the_highlight_of_the_row_under_the_pointer() {
    let fmt = FormatOptions { tz: Default::default(), uuid: Default::default() };
    for mode in [ViewMode::Tree, ViewMode::Table] {
        let docs = (0..5).map(|i| Bson::Document(doc! { "_id": i, "name": format!("name {i}") })).collect();
        let mut h = pane_harness(docs, mode);
        // The row of the fourth document: its root in the tree, a cell in the table.
        let label = if mode == ViewMode::Tree { "(4) 3" } else { "name 3" };
        let row = h.get_by_label(label).rect().center();
        let fill = h.ctx.global_style().visuals.widgets.hovered.bg_fill;
        let highlighted = |h: &Harness<'_, OutputPane>| h.output().shapes.iter().any(|c| matches!(&c.shape, egui::Shape::Rect(r) if r.fill == fill && r.rect.contains(row)));
        h.hover_at(row);
        h.run_steps(2);
        assert!(highlighted(&h), "{mode:?}: under the pointer");
        let _ = rustyrobo::ui::output::navigate(h.state_mut(), Key::ArrowDown, 1, 1, fmt);
        h.run_steps(2);
        assert!(!highlighted(&h), "{mode:?}: the keys went through the rows");
        // A moment later: the keys are timed by the clock, the pointer by egui's,
        // which the harness moves on by large steps.
        std::thread::sleep(Duration::from_millis(600));
        h.hover_at(row + egui::vec2(3.0, 0.0));
        h.run_steps(2);
        assert!(highlighted(&h), "{mode:?}: the pointer moved");
    }
}

#[test]
fn a_button_opens_the_documents_of_the_tree_or_closes_them() {
    let docs = (0..3).map(|i| Bson::Document(doc! { "_id": i, "name": format!("name {i}"), "address": { "city": "Paris" } })).collect();
    let mut h = pane_harness(docs, ViewMode::Tree);
    assert!(has_label(&h, "name 0") && !has_label(&h, "name 2"), "the first document open");
    h.get_by_label(icon::ARROWS_OUT_SIMPLE).click();
    h.run_steps(3);
    assert!(has_label(&h, "name 1") && has_label(&h, "name 2"), "all the documents");
    assert!(!has_label(&h, "Paris"), "their first level only");
    h.get_by_label(icon::ARROWS_IN_SIMPLE).click();
    h.run_steps(3);
    assert!(!has_label(&h, "name 0"), "all closed");
    // Not in the table.
    h.state_mut().mode = ViewMode::Table;
    h.run_steps(3);
    assert!(h.query_by_label(icon::ARROWS_OUT_SIMPLE).is_none());
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
    assert_eq!(copied(&h).as_deref(), Some(text));
}

#[test]
fn rtl_text_view_copies_what_is_selected() {
    // egui padded right-to-left runs with glyphs (emilk/egui#8576): the next
    // lines were selected by offsets past them.
    let text = "\"name\" : \"שלום world\",\n\"author\" : \"John Smith\"\n";
    let mut h = Harness::builder().with_size(egui::vec2(600.0, 200.0)).build_ui_state(
        |ui, text: &mut String| {
            code_view(ui, egui::Id::new("view"), text, theme::resolve(theme::DEFAULT_THEME, true), egui::FontId::monospace(13.0));
        },
        text.to_string(),
    );
    h.ctx.set_fonts(fonts::definitions(&Default::default()));
    h.run_steps(3);
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).click();
    h.run_steps(2);
    let select = |h: &mut Harness<'_, String>, keys: &[Key]| {
        h.key_press_modifiers(Modifiers::CTRL, Key::Home);
        for &key in keys {
            h.key_press(key);
        }
        h.run_steps(1);
    };
    select(&mut h, &[Key::ArrowDown]);
    h.key_press_modifiers(Modifiers::SHIFT, Key::End);
    h.run_steps(1);
    h.event(egui::Event::Copy);
    h.step();
    assert_eq!(copied(&h).as_deref(), Some("\"author\" : \"John Smith\""));

    // The string reads from right to left: `"name" : "world שלום",` on screen.
    select(&mut h, &[Key::ArrowRight; 10]);
    for _ in 0..5 {
        h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowRight);
    }
    h.run_steps(1);
    h.event(egui::Event::Copy);
    h.step();
    assert_eq!(copied(&h).as_deref(), Some("world"));
}

#[test]
fn rtl_text_in_the_code_editor_keeps_the_cursor_in_place() {
    use rustyrobo::ui::editor::{CompletionMode, EditorOptions, code_editor};
    let text = "{ \"name\" : \"שלום עולם\" }\nabc";
    let mut h = Harness::builder().with_size(egui::vec2(600.0, 200.0)).build_ui_state(
        |ui, text: &mut String| {
            let fields = Default::default();
            let opts = EditorOptions {
                line_numbers: false,
                font: egui::FontId::monospace(13.0),
                collections: &[],
                fields: &fields,
                completion: CompletionMode::Off,
                focus: false,
                read_only: false,
                search: false,
                multi_cursor: false,
                folding: false,
                highlight: None,
            };
            code_editor(ui, egui::Id::new("editor"), text, theme::resolve(theme::DEFAULT_THEME, true), opts);
        },
        text.to_string(),
    );
    h.ctx.set_fonts(fonts::definitions(&Default::default()));
    h.run_steps(3);
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::CTRL, Key::Home);
    h.key_press(Key::ArrowDown);
    h.run_steps(1);
    h.event(egui::Event::Text("x".into()));
    h.run_steps(1);
    assert_eq!(h.state(), "{ \"name\" : \"שלום עולם\" }\nxabc");
    h.key_press_modifiers(Modifiers::SHIFT, Key::End);
    h.run_steps(1);
    h.event(egui::Event::Copy);
    h.step();
    assert_eq!(copied(&h).as_deref(), Some("abc"));
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
    // The documents of the query, counted after the first page.
    wait_until(&mut h, "count", Duration::from_secs(15), |h| has_label(h, "2 documents"));
    h.state_mut().tabs[0].script = "db.people.find({ name: 'Ada' })".into();
    h.key_press(Key::F5);
    wait_until(&mut h, "one document", Duration::from_secs(15), |h| has_label(h, "1 document"));
    h.state_mut().tabs[0].script = "db.people.find({ age: { $gt: 30 } }).sort({ age: 1 })".into();
    h.key_press(Key::F5);
    wait_until(&mut h, "query results", Duration::from_secs(15), |h| h.state().tabs[0].outputs.first().is_some_and(|o| o.docs().len() == 2));

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
fn copied<State>(h: &Harness<'_, State>) -> Option<String> {
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


#[test]
fn live_history_and_export_of_the_results() {
    if !live() {
        return;
    }
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let env = env("live-history", &s);
    let mut h = harness();
    connect_main(&mut h);
    let sid = h.state().servers[0].id;
    h.state_mut().actions.push(rustyrobo::app::Action::OpenShell {
        server: sid,
        db: "rrui".into(),
        script: "db.exp.drop(); db.exp.insertMany([{ _id: 1, n: 'a' }, { _id: 2, n: 'b' }, { _id: 3, n: 'c' }])".into(),
        execute: true,
        title: "seed".into(),
        cursor: None,
    });
    wait_until(&mut h, "seed", Duration::from_secs(15), |h| h.state().tabs.first().is_some_and(|t| !t.running && t.elapsed.is_some()));
    h.state_mut().tabs[0].script = "db.exp.find({ _id: { $gt: 1 } })".into();
    h.key_press(Key::F5);
    wait_until(&mut h, "results", Duration::from_secs(15), |h| h.state().tabs[0].outputs.first().is_some_and(|o| o.docs().len() == 2));
    h.run_steps(3);

    // Both executions are in the history, with their connection and database.
    let saved = saved_history(&env).entries;
    assert_eq!(saved.len(), 2);
    assert_eq!((saved[1].script.as_str(), saved[1].db.as_str(), saved[1].connection), ("db.exp.find({ _id: { $gt: 1 } })", "rrui", Some(s.connections[0].id)));

    // The export button of the results exports the query.
    h.get_by_label(&format!("{} Export", rustyrobo::ui::ICON_EXPORT)).click();
    h.run_steps(3);
    assert!(has_label(&h, "Export — rrui.exp"));
    assert!(h.query_all_by(|n| n.value().is_some_and(|v| v.contains("$gt"))).next().is_some(), "filter of the query");
    let path = env.dir.join("exp.json");
    let default = h.query_all_by(|n| is_text_input(n) && n.value().is_some_and(|v| v.ends_with("exp.json"))).next().unwrap().value().unwrap();
    replace_text(&mut h, &default, &path.display().to_string());
    h.get_by_label("Export").click();
    wait_until(&mut h, "export", Duration::from_secs(15), |h| h.state().transfers.is_empty() && path.exists());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"_id\":{\"$numberInt\":\"2\"},\"n\":\"b\"}\n{\"_id\":{\"$numberInt\":\"3\"},\"n\":\"c\"}\n", "canonical by default");

    // Imported back into another collection, which the explorer lists.
    h.state_mut().actions.push(rustyrobo::app::Action::Import {
        server: sid,
        opts: Box::new(rustyrobo::transfer::ImportOptions {
            db: "rrui".into(),
            coll: "exp_back".into(),
            format: rustyrobo::transfer::Format::JsonLines,
            delimiter: Default::default(),
            detect_types: true,
            drop_first: true,
            path: path.clone(),
        }),
    });
    wait_until(&mut h, "import", Duration::from_secs(15), |h| h.state().transfers.is_empty());
    h.run_steps(3);
    assert!(h.state().logs.iter().any(|l| l.text.contains("Import into rrui.exp_back: 2 documents imported")));

    // Opened again from the history, in a new shell on the same connection.
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    h.run_steps(3);
    h.query_by(history_entry("db.exp.drop()")).expect("seed entry").click();
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(h.state().history_window.is_none());
    assert_eq!(h.state().tabs.len(), 2);
    assert_eq!((h.state().tabs[1].server, h.state().tabs[1].db.as_str()), (sid, "rrui"));
    assert!(h.state().tabs[1].script.starts_with("db.exp.drop();"));
}

#[test]
fn live_restore_that_is_not_complete_says_so() {
    if !live() {
        return;
    }
    use rustyrobo::ui::dialogs::Dialog;
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let env = env("live-restore-warning", &s);
    let mut h = harness();
    connect_main(&mut h);
    let sid = h.state().servers[0].id;
    // A dump with a collection whose options the server refuses.
    let dump = env.dir.join("dump");
    std::fs::create_dir_all(&dump).unwrap();
    for (name, options) in [("good", "{}"), ("bad", r#"{"capped":true}"#)] {
        std::fs::write(dump.join(format!("{name}.metadata.json")), format!(r#"{{"options":{options},"indexes":[]}}"#)).unwrap();
        let mut file = Vec::new();
        doc! { "_id": 1 }.to_writer(&mut file).unwrap();
        std::fs::write(dump.join(format!("{name}.bson")), file).unwrap();
    }
    let opts = rustyrobo::dump::RestoreOptions { db: "rrui_restore".into(), from: dump, collections: Vec::new(), drop_first: true, indexes: true };
    h.state_mut().actions.push(rustyrobo::app::Action::Restore { server: sid, opts: Box::new(opts) });
    wait_until(&mut h, "restore", Duration::from_secs(20), |h| h.state().transfers.is_empty());
    h.run_steps(3);
    let message = match &h.state().dialog {
        Some(Dialog::Message { message, .. }) => message.clone(),
        _ => panic!("no dialog"),
    };
    assert!(message.starts_with("Done, but not completely:") && message.contains("1 collection restored") && message.contains("bad: ") && message.ends_with("left as it was"), "{message}");
    assert!(has_label(&h, "Done, but not completely"));
}

#[test]
fn live_quitting_during_an_import_leaves_the_collection_as_it_was() {
    if !live() {
        return;
    }
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let env = env("live-quit-import", &s);
    let mut h = harness();
    connect_main(&mut h);
    let sid = h.state().servers[0].id;
    let conn = h.state().servers[0].conn().cloned().unwrap();
    let run = |h: &Harness<'_, App>, cmd: bson::Document| h.state().rt.block_on(conn.run_command("rrui_quit", cmd, None)).unwrap();
    run(&h, doc! { "dropDatabase": 1 });
    run(&h, doc! { "insert": "precious", "documents": [{ "_id": 1 }, { "_id": 2 }, { "_id": 3 }] });
    let file = env.dir.join("big.json");
    std::fs::write(&file, (0..300_000).map(|i| format!("{{\"_id\":{i},\"s\":\"some text\"}}\n")).collect::<String>()).unwrap();
    let opts = rustyrobo::transfer::ImportOptions {
        db: "rrui_quit".into(),
        coll: "precious".into(),
        format: rustyrobo::transfer::Format::JsonLines,
        delimiter: Default::default(),
        detect_types: true,
        drop_first: true,
        path: file,
    };
    h.state_mut().actions.push(rustyrobo::app::Action::Import { server: sid, opts: Box::new(opts) });
    wait_until(&mut h, "writing", Duration::from_secs(30), |h| h.state().transfers.first().is_some_and(|t| matches!(t.done, rustyrobo::transfer::Progress::Written(n) if n > 0)));
    request_close(&mut h);
    h.run_steps(2);
    assert!(has_label(&h, "Stopped, it leaves the collection as it was."));
    h.get_by_label("Stop and Quit").click();
    wait_until(&mut h, "quit", Duration::from_secs(15), |h| sent(h, &egui::ViewportCommand::Close));
    let collections = run(&h, doc! { "listCollections": 1, "nameOnly": true });
    let names: Vec<String> = collections.get_document("cursor").unwrap().get_array("firstBatch").unwrap().iter().map(|c| c.as_document().unwrap().get_str("name").unwrap().to_string()).collect();
    assert_eq!(names, vec!["precious"], "no temporary collection left");
    let count = run(&h, doc! { "count": "precious" });
    assert_eq!(count.get_i32("n").ok().map(i64::from).or(count.get_i64("n").ok()), Some(3), "as it was");
    run(&h, doc! { "dropDatabase": 1 });
}

#[test]
fn live_tree_values_are_edited_in_place() {
    if !live() {
        return;
    }
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let _env = env("live-tree-edit", &s);
    let mut h = harness();
    connect_main(&mut h);
    let sid = h.state().servers[0].id;
    h.state_mut().actions.push(rustyrobo::app::Action::OpenShell {
        server: sid,
        db: "rrui".into(),
        script: "db.tree.drop(); db.tree.insertOne({ _id: 1, address: { city: 'Paris', zip: '75001' }, tags: ['a', 'b'] })".into(),
        execute: true,
        title: "seed".into(),
        cursor: None,
    });
    wait_until(&mut h, "seed", Duration::from_secs(15), |h| h.state().tabs.first().is_some_and(|t| !t.running && t.elapsed.is_some()));
    h.state_mut().tabs[0].script = "db.tree.find({})".into();
    h.key_press(Key::F5);
    wait_until(&mut h, "results", Duration::from_secs(15), |h| h.state().tabs[0].outputs.first().is_some_and(|o| o.docs().len() == 1));
    h.key_press(Key::F2);
    h.run_steps(3);
    // The first document is open; a double-click opens its object and its array, without editing them.
    for key in ["address", "tags"] {
        let at = h.get_by_label(key).rect().center();
        double_click_at(&mut h, at);
    }
    assert!(h.state().tabs[0].outputs[0].cell_edit.is_none());
    let doc = |h: &Harness<'_, App>| h.state().tabs[0].outputs[0].docs()[0].as_document().unwrap().clone();

    // Double-click a nested value: edited in place, saved by its dotted path.
    let at = h.get_by_label("Paris").rect().center();
    double_click_at(&mut h, at);
    assert!(has_input(&h, "\"Paris\""), "editor with the value in shell syntax");
    retype(&mut h, "\"Paris\"", "\"Lyon\"");
    h.key_press(Key::Enter);
    wait_until(&mut h, "page reloaded", Duration::from_secs(15), |h| doc(h).get_document("address").unwrap().get_str("city") == Ok("Lyon"));
    assert!(h.state().tabs[0].outputs[0].cell_edit.is_none());

    // An array element, from the menu.
    h.get_by_label("b").click_secondary();
    h.run_steps(3);
    h.get_by_label_contains("Edit Value of \"tags.1\"").click();
    h.run_steps(3);
    retype(&mut h, "\"b\"", "NumberLong(2)");
    h.key_press(Key::Enter);
    wait_until(&mut h, "page reloaded", Duration::from_secs(15), |h| doc(h).get_array("tags").unwrap().get(1) == Some(&Bson::Int64(2)));

    // A nested field can be removed.
    h.get_by_label("75001").click_secondary();
    h.run_steps(3);
    h.get_by_label_contains("Remove Field \"address.zip\"").click();
    h.run_steps(5);
    assert!(has_label(&h, "Remove the field \"address.zip\" from this document?"));
    h.get_by_label("Yes").click();
    wait_until(&mut h, "field removed", Duration::from_secs(15), |h| !doc(h).get_document("address").unwrap().contains_key("zip"));

    // Enter on an object edits its document; saved with Ctrl+S, the page comes
    // back with the same rows open and selected, the keys going on from there.
    let current = |h: &Harness<'_, App>| h.state().tabs[0].outputs[0].selected.current().map(str::to_string);
    let at = h.get_by_label("address").rect().center();
    click_at(&mut h, at, egui::PointerButton::Primary);
    assert_eq!(current(&h).as_deref(), Some("0/address"));
    h.key_press(Key::Enter);
    h.run_steps(4);
    let Some(Dialog::Document { text, .. }) = &mut h.state_mut().dialog else { panic!("Edit Document") };
    *text = text.replace("\"Lyon\"", "\"Nice\"");
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::S);
    wait_until(&mut h, "page reloaded", Duration::from_secs(15), |h| doc(h).get_document("address").unwrap().get_str("city") == Ok("Nice"));
    assert!(h.state().dialog.is_none());
    assert_eq!(current(&h).as_deref(), Some("0/address"));
    assert!(h.state().tabs[0].outputs[0].expanded.contains("0/tags"), "still open: the page only was loaded again");
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(current(&h).as_deref(), Some("0/address/city"));
}

#[test]
fn live_table_cells_are_edited_in_place() {
    if !live() {
        return;
    }
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let _env = env("live-cells", &s);
    let mut h = harness();
    connect_main(&mut h);
    let sid = h.state().servers[0].id;
    h.state_mut().actions.push(rustyrobo::app::Action::OpenShell {
        server: sid,
        db: "rrui".into(),
        script: "db.cells.drop(); db.cells.insertMany([{ _id: 1, name: 'Ada', age: 36, tags: ['a'] }, { _id: 2, name: 'Linus', age: 28 }])".into(),
        execute: true,
        title: "seed".into(),
        cursor: None,
    });
    wait_until(&mut h, "seed", Duration::from_secs(15), |h| h.state().tabs.first().is_some_and(|t| !t.running && t.elapsed.is_some()));
    h.state_mut().tabs[0].script = "db.cells.find({}).sort({ _id: 1 })".into();
    h.key_press(Key::F5);
    wait_until(&mut h, "results", Duration::from_secs(15), |h| h.state().tabs[0].outputs.first().is_some_and(|o| o.docs().len() == 2));
    h.key_press(Key::F3);
    h.run_steps(3);
    let value = |h: &Harness<'_, App>, doc: usize, field: &str| h.state().tabs[0].outputs[0].docs()[doc].as_document().unwrap().get(field).cloned();

    // Double-click: the value in shell syntax, typed over, saved with Enter.
    let at = h.get_by_label("Ada").rect().center();
    double_click_at(&mut h, at);
    assert!(has_input(&h, "\"Ada\""), "editor with the value in shell syntax");
    retype(&mut h, "\"Ada\"", "\"Ada Lovelace\"");
    h.key_press(Key::Enter);
    wait_until(&mut h, "page reloaded", Duration::from_secs(15), |h| value(h, 0, "name") == Some(Bson::String("Ada Lovelace".into())));
    assert!(h.state().tabs[0].outputs[0].cell_edit.is_none());

    // From the menu: a number becomes a NumberLong; Escape cancels.
    h.get_by_label("28").click_secondary();
    h.run_steps(3);
    h.get_by_label_contains("Edit Value of \"age\"").click();
    h.run_steps(3);
    retype(&mut h, "28", "NumberLong(29)");
    h.key_press(Key::Enter);
    wait_until(&mut h, "page reloaded", Duration::from_secs(15), |h| value(h, 1, "age") == Some(Bson::Int64(29)));
    h.get_by_label("36").click_secondary();
    h.run_steps(3);
    h.get_by_label_contains("Edit Value of \"age\"").click();
    h.run_steps(3);
    retype(&mut h, "36", "99");
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(h.state().tabs[0].outputs[0].cell_edit.is_none());
    assert_eq!(value(&h, 0, "age"), Some(Bson::Int32(36)), "cancelled");

    // Bad syntax keeps the editor open.
    h.get_by_label("36").click_secondary();
    h.run_steps(3);
    h.get_by_label_contains("Edit Value of \"age\"").click();
    h.run_steps(3);
    retype(&mut h, "36", "{ oops");
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(h.state().tabs[0].outputs[0].cell_edit.as_ref().is_some_and(|e| e.error.is_some()));
    // Still focused: corrected and saved.
    retype(&mut h, "{ oops", "37");
    h.key_press(Key::Enter);
    wait_until(&mut h, "page reloaded", Duration::from_secs(15), |h| value(h, 0, "age") == Some(Bson::Int32(37)));
    h.key_press(Key::Escape);
    h.run_steps(3);

    // Arrays cannot be edited in place, but their field can be removed.
    h.get_by_label("[ 1 element ]").click_secondary();
    h.run_steps(3);
    h.get_by_label_contains("Remove Field \"tags\"").click();
    h.run_steps(5);
    assert!(has_label(&h, "Remove the field \"tags\" from this document?"));
    h.get_by_label("Yes").click();
    wait_until(&mut h, "field removed", Duration::from_secs(15), |h| h.state().tabs[0].outputs[0].docs().len() == 2 && value(h, 0, "tags").is_none());
}

#[test]
fn live_documents_open_from_ctrl_p_and_the_keys() {
    if !live() {
        return;
    }
    let s = AppSettings { connections: vec![local_mongo8()], ..Default::default() };
    let _env = env("live-open-docs", &s);
    let mut h = harness();
    connect_main(&mut h);
    let sid = h.state().servers[0].id;
    // Connected: the keyboard is in the explorer, the databases shown.
    assert_eq!(h.state().zone, Some(Zone::Explorer));
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    let selected = h.ctx.data(|d| d.get_temp::<egui::Id>(egui::Id::new("explorer_selected")));
    assert_eq!(selected, Some(egui::Id::new(("srv", sid)).with("system")), "the first row under the server");
    h.state_mut().actions.push(Action::OpenShell {
        server: sid,
        db: "rrui".into(),
        script: "db.quickdocs.drop(); db.quickdocs.insertMany([{ _id: 'ada-id', name: 'Ada' }, { _id: 'linus-id', name: 'Linus' }])".into(),
        execute: true,
        title: "seed".into(),
        cursor: None,
    });
    wait_until(&mut h, "seed", Duration::from_secs(15), |h| h.state().tabs.first().is_some_and(|t| !t.running && t.elapsed.is_some()));
    h.state_mut().actions.push(Action::RefreshServer(sid));
    wait_until(&mut h, "databases", Duration::from_secs(15), |h| h.state().servers[0].dbs.iter().any(|d| d.name == "rrui"));

    // Ctrl+P loads the collections of the server, and opens the one typed.
    h.key_press_modifiers(Modifiers::COMMAND, Key::P);
    h.run_steps(2);
    h.event(egui::Event::Text("quickdocs".into()));
    let listed = |h: &Harness<'_, App>| h.query_all_by(|n| n.role() == egui::accesskit::Role::Button && n.label().is_some_and(|l| l.contains("quickdocs"))).next().is_some();
    wait_until(&mut h, "collections", Duration::from_secs(15), listed);
    h.key_press(Key::Enter);
    wait_until(&mut h, "results", Duration::from_secs(15), |h| h.state().tabs.iter().any(|t| t.title == "quickdocs" && t.outputs.first().is_some_and(|o| o.docs().len() == 2)));
    assert!(h.state().quick_open.is_none());
    let tab = h.state().active_tab;
    assert_eq!(h.state().tabs[tab].title, "quickdocs", "in the new tab");
    let shown = |h: &Harness<'_, App>, mode: DocMode, id: &str| matches!(&h.state().dialog, Some(Dialog::Document { mode: m, original_id: Some(Bson::String(i)), .. }) if *m == mode && i == id);

    // Table view: a click selects the row, R views it, E edits it.
    h.key_press(Key::F3);
    h.run_steps(3);
    let at = h.get_by_label("Linus").rect().center();
    click_at(&mut h, at, egui::PointerButton::Primary);
    h.key_press(Key::R);
    h.run_steps(3);
    assert!(shown(&h, DocMode::View, "linus-id"));
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(h.state().dialog.is_none());
    h.key_press(Key::E);
    h.run_steps(3);
    assert!(shown(&h, DocMode::Edit, "linus-id"));
    close_document_dialog(&mut h);

    // Ctrl+K: the next view, for all the results of the tab.
    h.key_press_modifiers(Modifiers::COMMAND, Key::K);
    h.run_steps(2);
    assert_eq!(h.state().tabs[tab].outputs[0].mode, ViewMode::Text);
    h.key_press_modifiers(Modifiers::COMMAND, Key::K);
    h.run_steps(2);
    assert_eq!(h.state().tabs[tab].outputs[0].mode, ViewMode::Tree);

    // Alt+arrows: the editor takes the keyboard, and gives it back to the
    // results, whose script stays as it is; then the explorer.
    let script = h.state().tabs[tab].script.clone();
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowUp);
    h.run_steps(3);
    assert_eq!(h.state().zone, Some(Zone::Editor));
    assert!(h.ctx.text_edit_focused(), "the editor has the focus");
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(h.state().zone, Some(Zone::Results));
    assert!(!h.ctx.text_edit_focused());
    assert_eq!(h.state().tabs[tab].script, script);
    assert!(!h.state().tabs[tab].outputs[0].selected.is_empty(), "a document to view or edit");
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(h.state().zone, Some(Zone::Explorer));
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(h.state().zone, Some(Zone::Results));

    // Tree view: a double-click on the _id views the document, Edit goes on with it.
    h.key_press(Key::F2);
    h.run_steps(3);
    let at = h.get_by_label("ada-id").rect().center();
    double_click_at(&mut h, at);
    assert!(shown(&h, DocMode::View, "ada-id"));
    h.get_by_label("Edit").click();
    h.run_steps(3);
    assert!(shown(&h, DocMode::Edit, "ada-id"));
    close_document_dialog(&mut h);

    // Keyboard in the tree: Down goes to the next row, Enter edits it in place.
    let current = |h: &Harness<'_, App>| h.state().tabs[tab].outputs[0].selected.current().map(str::to_string);
    let at = h.get_by_label("ada-id").rect().center();
    click_at(&mut h, at, egui::PointerButton::Primary);
    h.key_press(Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(current(&h).as_deref(), Some("0/name"));
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(has_input(&h, "\"Ada\""), "edited in place");
    h.key_press(Key::Escape);
    h.run_steps(3);
    // End and Home: the last row, the first.
    h.key_press(Key::End);
    h.run_steps(2);
    assert_eq!(current(&h).as_deref(), Some("1"));
    h.key_press(Key::Home);
    h.run_steps(2);
    assert_eq!(current(&h).as_deref(), Some("0"));
    // Shift+Space: all the documents open, then closed.
    let open = |h: &Harness<'_, App>| h.state().tabs[tab].outputs[0].expanded.clone();
    h.key_press_modifiers(Modifiers::SHIFT, Key::Space);
    h.run_steps(2);
    assert_eq!(open(&h), std::collections::HashSet::from(["0".to_string(), "1".to_string()]));
    h.key_press_modifiers(Modifiers::SHIFT, Key::Space);
    h.run_steps(2);
    assert!(open(&h).is_empty());
    assert_eq!(current(&h).as_deref(), Some("0"));

    // In the table: Left and Right go through the columns, Enter edits the cell.
    h.key_press(Key::F3);
    h.run_steps(3);
    let at = h.get_by_label("Ada").rect().center();
    click_at(&mut h, at, egui::PointerButton::Primary);
    assert_eq!(h.state().tabs[tab].outputs[0].column.as_deref(), Some("name"), "the clicked cell");
    for key in [Key::ArrowLeft, Key::ArrowDown, Key::ArrowRight] {
        h.key_press(key);
        h.run_steps(2);
    }
    assert_eq!((current(&h).as_deref(), h.state().tabs[tab].outputs[0].column.as_deref()), (Some("1"), Some("name")));
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(has_input(&h, "\"Linus\""), "edited in place");
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(h.state().tabs[tab].outputs[0].cell_edit.is_none());
}
