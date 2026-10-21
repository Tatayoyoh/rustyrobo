//! Modal dialogs other than the connection manager.

use bson::{Bson, Document, doc};
use egui::{FontId, Id, RichText, Ui};
use uuid::Uuid;

use crate::app::{Action, App, Refresh};
use crate::bsonfmt;
use crate::mongo::ConnectSecrets;
use crate::settings::{AuthMechanism, ConnectionSettings, SshAuthMethod};
use crate::shell::parse_shell_value;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocMode {
    Insert,
    Edit,
    View,
}

#[derive(Clone)]
pub enum Dialog {
    Message { title: String, message: String },
    Confirm { title: String, message: String, action: Box<Action> },
    PasswordPrompt { conn: Uuid, name: String, mongo_user: Option<String>, mongo_pw: String, ssh_label: Option<(String, bool)>, ssh_pw: String },
    CreateDatabase { server: u64, name: String, collection: String },
    CreateCollection { server: u64, db: String, name: String, capped: bool, size: String, max: String, validator: String, error: Option<String> },
    RenameCollection { server: u64, db: String, coll: String, name: String },
    DuplicateCollection { server: u64, db: String, coll: String, name: String },
    CopyCollection { server: u64, db: String, coll: String, target_db: String, target_name: String },
    Document { mode: DocMode, server: u64, db: String, coll: String, text: String, original_id: Option<Bson>, tab: Option<u64>, error: Option<String>, valid: bool },
    User { server: u64, db: String, edit: bool, name: String, password: String, roles: Vec<(String, bool)>, extra_roles: String, error: Option<String> },
    Index(Box<IndexForm>),
    Function { server: u64, db: String, original: Option<String>, name: String, code: String, error: Option<String> },
    ShellTimeout { value: u32 },
    Preferences { batch_size: u32 },
    About,
    Shortcuts,
}

#[derive(Clone)]
pub struct IndexForm {
    pub server: u64,
    pub db: String,
    pub coll: String,
    pub original: Option<String>,
    pub name: String,
    pub keys: String,
    pub unique: bool,
    pub sparse: bool,
    pub background: bool,
    pub hidden: bool,
    pub ttl: String,
    pub partial: String,
    pub collation: String,
    pub text_lang: String,
    pub text_override: String,
    pub weights: String,
    pub tab: u8,
    pub error: Option<String>,
}

const DB_ROLES: &[&str] = &["read", "readWrite", "dbAdmin", "dbOwner", "userAdmin"];
const ADMIN_ROLES: &[&str] = &[
    "root", "readAnyDatabase", "readWriteAnyDatabase", "userAdminAnyDatabase", "dbAdminAnyDatabase",
    "clusterAdmin", "clusterManager", "clusterMonitor", "hostManager", "backup", "restore",
];

impl Dialog {
    pub fn message(title: &str, message: impl Into<String>) -> Self {
        Dialog::Message { title: title.to_string(), message: message.into() }
    }

    pub fn confirm(title: &str, message: impl Into<String>, action: Action) -> Self {
        Dialog::Confirm { title: title.to_string(), message: message.into(), action: Box::new(action) }
    }

    pub fn password_prompt(c: &ConnectionSettings) -> Self {
        let mongo_user = (c.credentials.enabled && c.credentials.ask_password && c.credentials.mechanism != AuthMechanism::X509)
            .then(|| format!("{}@{}", c.credentials.username, c.credentials.database));
        let ssh_label = (c.ssh.enabled && c.ssh.ask_password).then(|| match c.ssh.method {
            SshAuthMethod::PrivateKey => (format!("Passphrase for {}", c.ssh.private_key_file), true),
            _ => (format!("SSH password for {}@{}", c.ssh.username, c.ssh.host), false),
        });
        Dialog::PasswordPrompt { conn: c.id, name: c.name.clone(), mongo_user, mongo_pw: String::new(), ssh_label, ssh_pw: String::new() }
    }

    pub fn create_collection(server: u64, db: &str) -> Self {
        Dialog::CreateCollection { server, db: db.to_string(), name: String::new(), capped: false, size: String::new(), max: String::new(), validator: String::new(), error: None }
    }

    pub fn new_user(server: u64, db: &str) -> Self {
        let roles = roles_for(db).iter().map(|r| (r.to_string(), false)).collect();
        Dialog::User { server, db: db.to_string(), edit: false, name: String::new(), password: String::new(), roles, extra_roles: String::new(), error: None }
    }

    pub fn edit_user(server: u64, db: &str, user: &Document) -> Self {
        let mut assigned: Vec<(String, String)> = user
            .get_array("roles")
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        let r = r.as_document()?;
                        Some((r.get_str("role").ok()?.to_string(), r.get_str("db").unwrap_or(db).to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let roles = roles_for(db)
            .iter()
            .map(|r| {
                let on = assigned.iter().any(|(n, d)| n == r && d == db);
                (r.to_string(), on)
            })
            .collect();
        assigned.retain(|(n, d)| !(d == db && roles_for(db).contains(&n.as_str())));
        let extra = assigned.iter().map(|(n, d)| format!("{n}@{d}")).collect::<Vec<_>>().join(", ");
        Dialog::User {
            server,
            db: db.to_string(),
            edit: true,
            name: user.get_str("user").unwrap_or_default().to_string(),
            password: String::new(),
            roles,
            extra_roles: extra,
            error: None,
        }
    }

    pub fn new_index(server: u64, db: &str, coll: &str) -> Self {
        Dialog::Index(Box::new(IndexForm {
            server,
            db: db.to_string(),
            coll: coll.to_string(),
            original: None,
            name: String::new(),
            keys: "{\n    \"field\" : 1\n}".into(),
            unique: false,
            sparse: false,
            background: false,
            hidden: false,
            ttl: String::new(),
            partial: String::new(),
            collation: String::new(),
            text_lang: String::new(),
            text_override: String::new(),
            weights: String::new(),
            tab: 0,
            error: None,
        }))
    }

    pub fn edit_index(server: u64, db: &str, coll: &str, ix: &Document) -> Self {
        let f = Default::default();
        let get_doc = |k: &str| ix.get_document(k).map(|d| bsonfmt::doc_to_shell(d, f)).unwrap_or_default();
        Dialog::Index(Box::new(IndexForm {
            server,
            db: db.to_string(),
            coll: coll.to_string(),
            original: ix.get_str("name").ok().map(String::from),
            name: ix.get_str("name").unwrap_or_default().to_string(),
            keys: get_doc("key"),
            unique: ix.get_bool("unique").unwrap_or(false),
            sparse: ix.get_bool("sparse").unwrap_or(false),
            background: ix.get_bool("background").unwrap_or(false),
            hidden: ix.get_bool("hidden").unwrap_or(false),
            ttl: ix.get("expireAfterSeconds").and_then(crate::mongo::bson_as_i64).map(|v| v.to_string()).unwrap_or_default(),
            partial: get_doc("partialFilterExpression"),
            collation: get_doc("collation"),
            text_lang: ix.get_str("default_language").unwrap_or_default().to_string(),
            text_override: ix.get_str("language_override").unwrap_or_default().to_string(),
            weights: get_doc("weights"),
            tab: 0,
            error: None,
        }))
    }

    pub fn document(mode: DocMode, server: u64, db: &str, coll: &str, text: String, original_id: Option<Bson>, tab: Option<u64>) -> Self {
        Dialog::Document { mode, server, db: db.to_string(), coll: coll.to_string(), text, original_id, tab, error: None, valid: false }
    }
}

fn roles_for(db: &str) -> Vec<&'static str> {
    let mut v: Vec<&str> = DB_ROLES.to_vec();
    if db == "admin" {
        v.extend_from_slice(ADMIN_ROLES);
    }
    v
}

enum Outcome {
    Keep,
    Close,
}

fn parse_doc(text: &str) -> Result<Document, String> {
    match parse_shell_value(text)? {
        Bson::Document(d) => Ok(d),
        other => Err(format!("Expected a document, got {}", bsonfmt::type_name(&other))),
    }
}

fn opt_doc(text: &str) -> Result<Option<Document>, String> {
    if text.trim().is_empty() { Ok(None) } else { parse_doc(text).map(Some) }
}

fn buttons(ui: &mut Ui, ok_label: &str, ok_enabled: bool) -> (bool, bool) {
    let mut ok = false;
    let mut cancel = false;
    ui.add_space(6.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.add_enabled(ok_enabled, egui::Button::new(RichText::new(ok_label).strong())).clicked() {
            ok = true;
        }
        if ui.button("Cancel").clicked() {
            cancel = true;
        }
    });
    (ok, cancel)
}

fn title(ui: &mut Ui, t: &str) {
    ui.heading(t);
    ui.separator();
}

fn error_line(ui: &mut Ui, app_theme: &crate::theme::Theme, e: &Option<String>) {
    if let Some(e) = e {
        ui.add(egui::Label::new(RichText::new(format!("⚠ {e}")).color(app_theme.error)).wrap());
    }
}

fn code_box(ui: &mut Ui, id: Id, text: &mut String, theme: &crate::theme::Theme, font: f32, rows: usize, read_only: bool) {
    egui::Frame::new()
        .fill(theme.base)
        .stroke(egui::Stroke::new(1.0, theme.border))
        .corner_radius(4)
        .inner_margin(4)
        .show(ui, |ui| {
            ui.set_min_height(rows as f32 * (font + 4.0));
            crate::ui::editor::code_editor(
                ui,
                id,
                text,
                theme,
                crate::ui::editor::EditorOptions {
                    line_numbers: true,
                    font: FontId::monospace(font),
                    collections: &[],
                    fields: &std::collections::HashMap::new(),
                    completion: crate::ui::editor::CompletionMode::Off,
                    focus: false,
                    read_only,
                },
            );
        });
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut dlg) = app.dialog.take() else { return };
    let theme = app.theme;
    let font = app.settings.font_size;
    let mut actions: Vec<Action> = Vec::new();
    let db_names: Vec<String> = match &dlg {
        Dialog::CopyCollection { server, .. } => app.server(*server).map(|s| s.dbs.iter().map(|d| d.name.clone()).collect()).unwrap_or_default(),
        _ => Vec::new(),
    };

    let resp = egui::Modal::new(Id::new("app-dialog")).show(ctx, |ui| -> Outcome {
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        match &mut dlg {
            Dialog::Message { title: t, message } => {
                ui.set_max_width(560.0);
                title(ui, t);
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    ui.add(egui::Label::new(message.as_str()).selectable(true).wrap());
                });
                let (ok, _) = buttons(ui, "OK", true);
                if ok || enter { Outcome::Close } else { Outcome::Keep }
            }
            Dialog::Confirm { title: t, message, action } => {
                ui.set_max_width(480.0);
                title(ui, t);
                ui.add(egui::Label::new(message.as_str()).wrap());
                let (ok, cancel) = buttons(ui, "Yes", true);
                if ok {
                    actions.push((**action).clone());
                }
                if ok || cancel { Outcome::Close } else { Outcome::Keep }
            }
            Dialog::PasswordPrompt { conn, name, mongo_user, mongo_pw, ssh_label, ssh_pw } => {
                ui.set_width(380.0);
                title(ui, &format!("Connect to {name}"));
                let mut first = true;
                if let Some(u) = mongo_user {
                    ui.label(format!("Password for {u}"));
                    let r = ui.add(egui::TextEdit::singleline(mongo_pw).password(true).desired_width(f32::INFINITY));
                    if first && !r.has_focus() && ui.memory(|m| m.focused().is_none()) {
                        r.request_focus();
                    }
                    first = false;
                }
                if let Some((label, _)) = ssh_label {
                    ui.label(label.as_str());
                    let r = ui.add(egui::TextEdit::singleline(ssh_pw).password(true).desired_width(f32::INFINITY));
                    if first && ui.memory(|m| m.focused().is_none()) {
                        r.request_focus();
                    }
                }
                let (ok, cancel) = buttons(ui, "Connect", true);
                if ok || enter {
                    let mut s = ConnectSecrets::default();
                    if mongo_user.is_some() {
                        s.mongo_password = Some(mongo_pw.clone());
                    }
                    match ssh_label {
                        Some((_, true)) => s.ssh.passphrase = Some(ssh_pw.clone()),
                        Some((_, false)) => s.ssh.password = Some(ssh_pw.clone()),
                        None => {}
                    }
                    actions.push(Action::ConnectWith(*conn, s));
                    Outcome::Close
                } else if cancel {
                    Outcome::Close
                } else {
                    Outcome::Keep
                }
            }
            Dialog::CreateDatabase { server, name, collection } => {
                ui.set_width(380.0);
                title(ui, "Create Database");
                egui::Grid::new("cdb").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Database name:");
                    ui.add(egui::TextEdit::singleline(name).desired_width(220.0)).request_focus();
                    ui.end_row();
                    ui.label("First collection:");
                    ui.text_edit_singleline(collection);
                    ui.end_row();
                });
                ui.label(RichText::new("MongoDB creates a database together with its first collection.").small().color(theme.text_weak));
                let valid = !name.trim().is_empty() && !collection.trim().is_empty() && !name.contains(['/', '\\', '.', ' ', '"', '$']);
                let (ok, cancel) = buttons(ui, "Create", valid);
                if valid && (ok || enter) {
                    actions.push(Action::Command {
                        server: *server,
                        db: name.trim().to_string(),
                        cmd: doc! { "create": collection.trim() },
                        label: format!("Create database {}", name.trim()),
                        refresh: Refresh::Databases,
                    });
                    Outcome::Close
                } else if cancel {
                    Outcome::Close
                } else {
                    Outcome::Keep
                }
            }
            Dialog::CreateCollection { server, db, name, capped, size, max, validator, error } => {
                ui.set_width(460.0);
                title(ui, "Create Collection");
                egui::Grid::new("ccoll").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(name).desired_width(260.0)).request_focus();
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(capped, "Capped collection");
                    ui.end_row();
                    if *capped {
                        ui.label("Size (bytes):");
                        ui.text_edit_singleline(size);
                        ui.end_row();
                        ui.label("Max documents:");
                        ui.text_edit_singleline(max);
                        ui.end_row();
                    }
                });
                ui.label("Validator (optional, shell syntax):");
                code_box(ui, Id::new("ccoll-validator"), validator, &theme, font, 4, false);
                error_line(ui, &theme, error);
                let valid = !name.trim().is_empty();
                let (ok, cancel) = buttons(ui, "Create", valid);
                if ok {
                    let mut cmd = doc! { "create": name.trim() };
                    let mut err = None;
                    if *capped {
                        cmd.insert("capped", true);
                        match size.trim().parse::<i64>() {
                            Ok(s) => {
                                cmd.insert("size", s);
                            }
                            Err(_) => err = Some("Capped collections need a size in bytes".to_string()),
                        }
                        if let Ok(m) = max.trim().parse::<i64>() {
                            cmd.insert("max", m);
                        }
                    }
                    match opt_doc(validator) {
                        Ok(Some(v)) => {
                            cmd.insert("validator", v);
                        }
                        Ok(None) => {}
                        Err(e) => err = Some(format!("Validator: {e}")),
                    }
                    if let Some(e) = err {
                        *error = Some(e);
                        Outcome::Keep
                    } else {
                        actions.push(Action::Command { server: *server, db: db.clone(), cmd, label: format!("Create collection {}", name.trim()), refresh: Refresh::Collections(db.clone()) });
                        Outcome::Close
                    }
                } else if cancel {
                    Outcome::Close
                } else {
                    Outcome::Keep
                }
            }
            Dialog::RenameCollection { server, db, coll, name } => {
                ui.set_width(380.0);
                title(ui, "Rename Collection");
                ui.label(format!("New name for \"{coll}\":"));
                ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY)).request_focus();
                let valid = !name.trim().is_empty() && name.trim() != coll;
                let (ok, cancel) = buttons(ui, "Rename", valid);
                if (ok || enter) && valid {
                    actions.push(Action::Command {
                        server: *server,
                        db: "admin".into(),
                        cmd: doc! { "renameCollection": format!("{db}.{coll}"), "to": format!("{db}.{}", name.trim()) },
                        label: format!("Rename collection {coll} to {}", name.trim()),
                        refresh: Refresh::Collections(db.clone()),
                    });
                    Outcome::Close
                } else if cancel {
                    Outcome::Close
                } else {
                    Outcome::Keep
                }
            }
            Dialog::DuplicateCollection { server, db, coll, name } => {
                ui.set_width(380.0);
                title(ui, "Duplicate Collection");
                ui.label(format!("Copy all documents of \"{coll}\" into:"));
                ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY)).request_focus();
                ui.label(RichText::new("Indexes are not copied. An existing target collection is replaced.").small().color(theme.text_weak));
                let valid = !name.trim().is_empty() && name.trim() != coll;
                let (ok, cancel) = buttons(ui, "Duplicate", valid);
                if (ok || enter) && valid {
                    actions.push(Action::Command {
                        server: *server,
                        db: db.clone(),
                        cmd: doc! { "aggregate": coll.as_str(), "pipeline": [{ "$match": {} }, { "$out": name.trim() }], "cursor": {} },
                        label: format!("Duplicate collection {coll} to {}", name.trim()),
                        refresh: Refresh::Collections(db.clone()),
                    });
                    Outcome::Close
                } else if cancel {
                    Outcome::Close
                } else {
                    Outcome::Keep
                }
            }
            Dialog::CopyCollection { server, db, coll, target_db, target_name } => {
                ui.set_width(420.0);
                title(ui, "Copy Collection to Database");
                egui::Grid::new("copycoll").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Target database:");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(target_db).desired_width(180.0));
                        egui::ComboBox::from_id_salt("copy-db").selected_text("…").show_ui(ui, |ui| {
                            for n in db_names.iter().filter(|n| *n != db) {
                                if ui.selectable_label(n == target_db, n).clicked() {
                                    *target_db = n.clone();
                                }
                            }
                        });
                    });
                    ui.end_row();
                    ui.label("Target collection:");
                    ui.text_edit_singleline(target_name);
                    ui.end_row();
                });
                ui.label(RichText::new("Documents are merged into the target collection ($merge, MongoDB 4.2+).").small().color(theme.text_weak));
                let same = target_db.trim() == db && target_name.trim() == coll;
                let valid = !(target_db.trim().is_empty() || target_name.trim().is_empty() || same);
                let (ok, cancel) = buttons(ui, "Copy", valid);
                if ok && valid {
                    actions.push(Action::Command {
                        server: *server,
                        db: db.clone(),
                        cmd: doc! { "aggregate": coll.as_str(), "pipeline": [{ "$merge": { "into": { "db": target_db.trim(), "coll": target_name.trim() } } }], "cursor": {} },
                        label: format!("Copy {db}.{coll} to {}.{}", target_db.trim(), target_name.trim()),
                        refresh: Refresh::Databases,
                    });
                    Outcome::Close
                } else if cancel {
                    Outcome::Close
                } else {
                    Outcome::Keep
                }
            }
            Dialog::Document { mode, server, db, coll, text, original_id, tab, error, valid } => {
                let screen = ui.ctx().content_rect();
                ui.set_width((screen.width() * 0.6).clamp(420.0, 900.0));
                let t = match mode {
                    DocMode::Insert => format!("Insert Document — {db}.{coll}"),
                    DocMode::Edit => format!("Edit Document — {db}.{coll}"),
                    DocMode::View => "View Document".to_string(),
                };
                title(ui, &t);
                let h = (screen.height() * 0.6).clamp(200.0, 700.0);
                egui::Frame::new().fill(theme.base).stroke(egui::Stroke::new(1.0, theme.border)).corner_radius(4).inner_margin(4).show(ui, |ui| {
                    ui.set_height(h);
                    let before = text.clone();
                    crate::ui::editor::code_editor(
                        ui,
                        Id::new("doc-editor"),
                        text,
                        &theme,
                        crate::ui::editor::EditorOptions {
                            line_numbers: true,
                            font: FontId::monospace(font),
                            collections: &[],
                    fields: &std::collections::HashMap::new(),
                            completion: crate::ui::editor::CompletionMode::Off,
                            focus: false,
                            read_only: *mode == DocMode::View,
                        },
                    );
                    if *text != before {
                        *valid = false;
                        *error = None;
                    }
                });
                if *valid {
                    ui.label(RichText::new("✔ Valid document").color(theme.success));
                }
                error_line(ui, &theme, error);
                let mut out = Outcome::Keep;
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if *mode != DocMode::View {
                        if ui.button("Validate").clicked() {
                            match parse_doc(text) {
                                Ok(_) => *valid = true,
                                Err(e) => *error = Some(e),
                            }
                        }
                        if ui.button("Format").clicked() {
                            match parse_doc(text) {
                                Ok(d) => *text = bsonfmt::doc_to_shell(&d, Default::default()),
                                Err(e) => *error = Some(e),
                            }
                        }
                    }
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if *mode == DocMode::View {
                            if ui.button("Close").clicked() {
                                out = Outcome::Close;
                            }
                            return;
                        }
                        if ui.button(RichText::new("Save").strong()).clicked() {
                            match parse_doc(text) {
                                Ok(d) => {
                                    let refresh = tab.map(Refresh::Tab).unwrap_or(Refresh::None);
                                    let cmd = match (*mode, original_id) {
                                        (DocMode::Edit, Some(id)) => doc! { "update": coll.as_str(), "updates": [{ "q": { "_id": id.clone() }, "u": d, "upsert": false }] },
                                        _ => doc! { "insert": coll.as_str(), "documents": [d] },
                                    };
                                    let label = if *mode == DocMode::Edit { format!("Update document in {coll}") } else { format!("Insert document into {coll}") };
                                    actions.push(Action::Command { server: *server, db: db.clone(), cmd, label, refresh });
                                    out = Outcome::Close;
                                }
                                Err(e) => *error = Some(e),
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            out = Outcome::Close;
                        }
                    });
                });
                out
            }
            Dialog::User { server, db, edit, name, password, roles, extra_roles, error } => {
                ui.set_width(460.0);
                title(ui, if *edit { "Edit User" } else { "Add User" });
                egui::Grid::new("user").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Database:");
                    ui.label(RichText::new(db.as_str()).strong());
                    ui.end_row();
                    ui.label("Name:");
                    ui.add_enabled(!*edit, egui::TextEdit::singleline(name).desired_width(240.0));
                    ui.end_row();
                    ui.label("Password:");
                    ui.add(egui::TextEdit::singleline(password).password(true).desired_width(240.0).hint_text(if *edit { "unchanged" } else { "" }));
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.label(RichText::new("Roles").strong());
                egui::Grid::new("roles").num_columns(3).show(ui, |ui| {
                    for (i, (r, on)) in roles.iter_mut().enumerate() {
                        ui.checkbox(on, r.as_str());
                        if i % 3 == 2 {
                            ui.end_row();
                        }
                    }
                });
                ui.label("Other roles (role@db, comma separated):");
                ui.add(egui::TextEdit::singleline(extra_roles).desired_width(f32::INFINITY));
                error_line(ui, &theme, error);
                let valid = !name.trim().is_empty() && (*edit || !password.is_empty());
                let (ok, cancel) = buttons(ui, if *edit { "Save" } else { "Add" }, valid);
                if ok {
                    let mut list: Vec<Bson> = roles.iter().filter(|(_, on)| *on).map(|(r, _)| Bson::Document(doc! { "role": r, "db": db.as_str() })).collect();
                    for item in extra_roles.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        let (r, d) = item.split_once('@').unwrap_or((item, db.as_str()));
                        list.push(Bson::Document(doc! { "role": r.trim(), "db": d.trim() }));
                    }
                    let cmd = if *edit {
                        let mut c = doc! { "updateUser": name.trim(), "roles": list };
                        if !password.is_empty() {
                            c.insert("pwd", password.clone());
                        }
                        c
                    } else {
                        doc! { "createUser": name.trim(), "pwd": password.clone(), "roles": list }
                    };
                    actions.push(Action::Command {
                        server: *server,
                        db: db.clone(),
                        cmd,
                        label: format!("{} user {}", if *edit { "Update" } else { "Create" }, name.trim()),
                        refresh: Refresh::Users(db.clone()),
                    });
                    Outcome::Close
                } else if cancel {
                    Outcome::Close
                } else {
                    Outcome::Keep
                }
            }
            Dialog::Index(f) => index_dialog(ui, f, &theme, font, &mut actions),
            Dialog::Function { server, db, original, name, code, error } => {
                let screen = ui.ctx().content_rect();
                ui.set_width((screen.width() * 0.5).clamp(420.0, 800.0));
                title(ui, if original.is_some() { "Edit Function" } else { "Add Function" });
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
                });
                code_box(ui, Id::new("fn-editor"), code, &theme, font, 14, false);
                error_line(ui, &theme, error);
                let valid = !name.trim().is_empty();
                let (ok, cancel) = buttons(ui, "Save", valid);
                if ok {
                    let n = name.trim().to_string();
                    let mut cmds = Vec::new();
                    if let Some(o) = original.as_ref().filter(|o| **o != n) {
                        cmds.push(doc! { "delete": "system.js", "deletes": [{ "q": { "_id": o.as_str() }, "limit": 1 }] });
                    }
                    cmds.push(doc! {
                        "update": "system.js",
                        "updates": [{ "q": { "_id": &n }, "u": { "_id": &n, "value": Bson::JavaScriptCode(code.clone()) }, "upsert": true }]
                    });
                    actions.push(Action::Commands { server: *server, db: db.clone(), cmds, label: format!("Save function {n}"), refresh: Refresh::Functions(db.clone()) });
                    Outcome::Close
                } else if cancel {
                    Outcome::Close
                } else {
                    Outcome::Keep
                }
            }
            Dialog::ShellTimeout { value } => {
                ui.set_width(340.0);
                title(ui, "Change Shell Timeout");
                ui.horizontal(|ui| {
                    ui.label("Shell timeout (seconds):");
                    ui.add(egui::DragValue::new(value).range(1..=86_400));
                });
                ui.label(RichText::new("Applies to newly opened shells.").small().color(theme.text_weak));
                let (ok, cancel) = buttons(ui, "Save", true);
                if ok {
                    app.settings.shell_timeout_secs = *value;
                    actions.push(Action::SaveSettings);
                }
                if ok || cancel { Outcome::Close } else { Outcome::Keep }
            }
            Dialog::Preferences { batch_size } => {
                ui.set_width(360.0);
                title(ui, "Preferences");
                egui::Grid::new("prefs").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Batch size (documents per page):");
                    ui.add(egui::DragValue::new(batch_size).range(1..=10_000));
                    ui.end_row();
                    ui.label("Font size:");
                    ui.add(egui::DragValue::new(&mut app.settings.font_size).range(9.0..=24.0).speed(0.25));
                    ui.end_row();
                    ui.label("Theme:");
                    let mut theme_name = app.settings.theme.clone();
                    egui::ComboBox::from_id_salt("pref-theme").selected_text(&theme_name).show_ui(ui, |ui| {
                        for n in crate::theme::names() {
                            ui.selectable_value(&mut theme_name, n.to_string(), n);
                        }
                    });
                    if theme_name != app.settings.theme {
                        app.settings.theme = theme_name;
                        actions.push(Action::ApplyTheme);
                    }
                    ui.end_row();
                });
                let (ok, cancel) = buttons(ui, "Save", true);
                if ok {
                    app.settings.batch_size = *batch_size;
                    actions.push(Action::ApplyTheme);
                }
                if ok || cancel { Outcome::Close } else { Outcome::Keep }
            }
            Dialog::About => {
                ui.set_width(420.0);
                ui.vertical_centered(|ui| {
                    if let Some(logo) = &app.logo {
                        ui.add(egui::Image::new(logo).fit_to_exact_size(egui::vec2(96.0, 96.0)));
                    }
                    ui.label(RichText::new("Rustyrobo").size(28.0).strong().color(theme.accent));
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.add_space(8.0);
                    ui.label("Native MongoDB management tool written in Rust.");
                    ui.label("Inspired by Robomongo / Robo 3T. Apache-2.0.");
                    ui.add_space(6.0);
                    ui.label(RichText::new("MongoDB 3.6 → 8.x · embedded QuickJS shell · SSH tunnels").small().color(theme.text_weak));
                });
                let (ok, _) = buttons(ui, "OK", true);
                if ok || enter { Outcome::Close } else { Outcome::Keep }
            }
            Dialog::Shortcuts => {
                ui.set_width(440.0);
                title(ui, "Keyboard Shortcuts");
                egui::Grid::new("keys").num_columns(2).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
                    for (k, d) in [
                        ("F5, Ctrl+Enter", "Execute script or selection"),
                        ("F6", "Stop execution"),
                        ("F2 / F3 / F4", "Tree / table / text view"),
                        ("F10", "Rotate editor and results"),
                        ("F11", "Full screen"),
                        ("Ctrl+N", "Connections"),
                        ("Ctrl+T", "New shell"),
                        ("Ctrl+Shift+T", "Duplicate shell"),
                        ("Ctrl+W", "Close shell"),
                        ("Ctrl+R", "Re-execute"),
                        ("Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous shell"),
                        ("Ctrl+O / Ctrl+S", "Open / save script"),
                        ("Ctrl+E / Ctrl+L", "Toggle explorer / logs"),
                        ("Ctrl+Space", "Autocompletion"),
                        ("Alt+1…9", "Connect to the n-th connection"),
                    ] {
                        ui.label(RichText::new(k).monospace());
                        ui.label(d);
                        ui.end_row();
                    }
                });
                let (ok, _) = buttons(ui, "Close", true);
                if ok || enter { Outcome::Close } else { Outcome::Keep }
            }
        }
    });

    let close = matches!(resp.inner, Outcome::Close) || resp.should_close();
    if !close && app.dialog.is_none() {
        app.dialog = Some(dlg);
    }
    app.actions.extend(actions);
}

fn index_dialog(ui: &mut Ui, f: &mut IndexForm, theme: &crate::theme::Theme, font: f32, actions: &mut Vec<Action>) -> Outcome {
    ui.set_width(520.0);
    title(ui, &format!("{} — {}.{}", if f.original.is_some() { "Edit Index" } else { "Add Index" }, f.db, f.coll));
    ui.horizontal(|ui| {
        for (i, t) in ["Basic", "Advanced", "Text Search"].iter().enumerate() {
            if ui.selectable_label(f.tab == i as u8, *t).clicked() {
                f.tab = i as u8;
            }
        }
    });
    ui.separator();
    match f.tab {
        0 => {
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(f32::INFINITY).hint_text("generated from the keys"));
            });
            ui.label("Keys:");
            code_box(ui, Id::new("ix-keys"), &mut f.keys, theme, font, 6, false);
            ui.horizontal(|ui| {
                ui.checkbox(&mut f.unique, "Unique");
                ui.checkbox(&mut f.sparse, "Sparse");
                ui.checkbox(&mut f.background, "Create in background");
                ui.checkbox(&mut f.hidden, "Hidden (4.4+)");
            });
        }
        1 => {
            ui.horizontal(|ui| {
                ui.label("Expire after (seconds, TTL):");
                ui.add(egui::TextEdit::singleline(&mut f.ttl).desired_width(100.0));
            });
            ui.label("Partial filter expression:");
            code_box(ui, Id::new("ix-partial"), &mut f.partial, theme, font, 4, false);
            ui.label("Collation:");
            code_box(ui, Id::new("ix-collation"), &mut f.collation, theme, font, 3, false);
        }
        _ => {
            egui::Grid::new("ix-text").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Default language:");
                ui.text_edit_singleline(&mut f.text_lang);
                ui.end_row();
                ui.label("Language override:");
                ui.text_edit_singleline(&mut f.text_override);
                ui.end_row();
            });
            ui.label("Text weights:");
            code_box(ui, Id::new("ix-weights"), &mut f.weights, theme, font, 4, false);
        }
    }
    error_line(ui, theme, &f.error);
    let (ok, cancel) = buttons(ui, "Save", true);
    if cancel {
        return Outcome::Close;
    }
    if !ok {
        return Outcome::Keep;
    }
    let build = || -> Result<Document, String> {
        let keys = parse_doc(&f.keys).map_err(|e| format!("Keys: {e}"))?;
        if keys.is_empty() {
            return Err("Keys must not be empty".into());
        }
        let name = if f.name.trim().is_empty() {
            keys.iter().map(|(k, v)| format!("{k}_{}", bsonfmt::short_value(v, Default::default()).trim_matches('"'))).collect::<Vec<_>>().join("_")
        } else {
            f.name.trim().to_string()
        };
        let mut spec = doc! { "key": keys, "name": name };
        if f.unique {
            spec.insert("unique", true);
        }
        if f.sparse {
            spec.insert("sparse", true);
        }
        if f.background {
            spec.insert("background", true);
        }
        if f.hidden {
            spec.insert("hidden", true);
        }
        if !f.ttl.trim().is_empty() {
            let v: i64 = f.ttl.trim().parse().map_err(|_| "TTL must be a number of seconds".to_string())?;
            spec.insert("expireAfterSeconds", v);
        }
        if let Some(p) = opt_doc(&f.partial).map_err(|e| format!("Partial filter: {e}"))? {
            spec.insert("partialFilterExpression", p);
        }
        if let Some(c) = opt_doc(&f.collation).map_err(|e| format!("Collation: {e}"))? {
            spec.insert("collation", c);
        }
        if !f.text_lang.trim().is_empty() {
            spec.insert("default_language", f.text_lang.trim());
        }
        if !f.text_override.trim().is_empty() {
            spec.insert("language_override", f.text_override.trim());
        }
        if let Some(w) = opt_doc(&f.weights).map_err(|e| format!("Weights: {e}"))? {
            spec.insert("weights", w);
        }
        Ok(spec)
    };
    match build() {
        Ok(spec) => {
            let mut cmds = Vec::new();
            if let Some(o) = &f.original {
                cmds.push(doc! { "dropIndexes": f.coll.as_str(), "index": o.as_str() });
            }
            let name = spec.get_str("name").unwrap_or_default().to_string();
            cmds.push(doc! { "createIndexes": f.coll.as_str(), "indexes": [spec] });
            actions.push(Action::Commands {
                server: f.server,
                db: f.db.clone(),
                cmds,
                label: format!("{} index {name}", if f.original.is_some() { "Rebuild" } else { "Create" }),
                refresh: Refresh::Indexes(f.db.clone(), f.coll.clone()),
            });
            Outcome::Close
        }
        Err(e) => {
            f.error = Some(e);
            Outcome::Keep
        }
    }
}
