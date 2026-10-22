//! End-to-end UI tests, run headless with egui_kittest.
//!
//! Offline tests always run. Tests marked `live_` need the docker
//! environment (`dev/docker-compose.yml`) and `RR_LIVE=1`.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use rustyrobo::app::App;
use rustyrobo::settings::{AppSettings, ConnectionSettings, Credentials, HostPort};

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
    h.get_by_label("➕ Create").click();
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
    h.get_by_label("➕ Create").click();
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
    h.get_by_label("➕ Create").click();
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
    h.get_by_label("➕ Create").click();
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
    h.get_by_label("✏ Edit").click();
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
    wait_until(&mut h, "error", Duration::from_secs(5), |h| has_label(h, "⚠"));
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
    h.get_by_label("Delete 2 Documents…").click();
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
    h.get_by_label("✏ Edit").click();
    h.run_steps(3);
    h.get_by_label_contains("Test").click();
    wait_until(&mut h, "test result", Duration::from_secs(20), |h| has_label(h, "Connected to MongoDB"));
    assert!(has_label(&h, "Connect to server"));
    assert!(has_label(&h, "Authentication"));
    assert!(has_label(&h, "List databases"));
}
