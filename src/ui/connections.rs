//! Connection manager (list) and the connection editor.
//!
//! Compared with Robo 3T the editor adds: a section sidebar with live status
//! badges, inline validation, a two-way connection string field, a step by
//! step connection test (SSH → server → auth → databases), colour tags,
//! groups, a searchable list, keyboard navigation and unsaved-change guard.

use std::path::Path;

use egui::{Align, Color32, Id, Layout, RichText, Ui};
use uuid::Uuid;

use crate::app::{Action, App, AppEvent, LogLevel};
use crate::mongo::uri::{Secrets, apply_uri, build_uri};
use crate::mongo::{ConnectSecrets, Connection, StepReport};
use crate::settings::{
    AppSettings, AuthMechanism, ConnectionSettings, ConnectionType, HostPort, ReadPreference, SshAuthMethod,
    find_robo3t_configs, import_robo3t,
};
use crate::theme::Theme;
use crate::ui::color_dot;

pub const TAG_COLORS: [[u8; 3]; 9] = [
    [0xE5, 0x48, 0x4D],
    [0xF2, 0x8C, 0x28],
    [0xE8, 0xC5, 0x47],
    [0x46, 0xA7, 0x58],
    [0x2D, 0xB8, 0xA6],
    [0x3B, 0x82, 0xF6],
    [0x8B, 0x5C, 0xF6],
    [0xD9, 0x46, 0xEF],
    [0x8A, 0x8F, 0x98],
];

// ------------------------------------------------------------------ list

pub struct ConnectionList {
    pub filter: String,
    pub selected: Option<Uuid>,
    confirm_delete: Option<Uuid>,
    message: Option<(String, bool)>,
    focus_search: bool,
}

impl ConnectionList {
    pub fn new(settings: &AppSettings) -> Self {
        let selected = settings
            .connections
            .iter()
            .max_by_key(|c| c.last_connected)
            .map(|c| c.id);
        ConnectionList { filter: String::new(), selected, confirm_delete: None, message: None, focus_search: true }
    }
}

fn matches(c: &ConnectionSettings, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_lowercase();
    c.name.to_lowercase().contains(&f)
        || c.address_label().to_lowercase().contains(&f)
        || c.group.to_lowercase().contains(&f)
        || c.ssh.host.to_lowercase().contains(&f)
}

fn badge(ui: &mut Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, color))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(5, 0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().color(color));
        });
}

fn attributes(ui: &mut Ui, c: &ConnectionSettings, th: &Theme) {
    match c.kind {
        ConnectionType::ReplicaSet => badge(ui, "RS", th.syntax.keyword),
        ConnectionType::Srv => badge(ui, "SRV", th.syntax.keyword),
        ConnectionType::Direct => {}
    }
    if c.ssh.enabled {
        badge(ui, "SSH", th.syntax.function);
    }
    if c.tls.enabled || c.kind == ConnectionType::Srv {
        badge(ui, "TLS", th.success);
    }
    if c.credentials.enabled {
        badge(ui, "AUTH", th.syntax.bson_type);
    }
}

fn relative_time(t: Option<chrono::DateTime<chrono::Utc>>) -> String {
    let Some(t) = t else { return "never".into() };
    let d = chrono::Utc::now() - t;
    if d.num_minutes() < 1 {
        "just now".into()
    } else if d.num_hours() < 1 {
        format!("{} min ago", d.num_minutes())
    } else if d.num_days() < 1 {
        format!("{} h ago", d.num_hours())
    } else if d.num_days() < 30 {
        format!("{} d ago", d.num_days())
    } else {
        t.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string()
    }
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    // The editor is drawn above the list when both are open.
    if app.conn_list.is_some() {
        show_list(app, ctx);
    }
    if app.conn_editor.is_some() {
        show_editor(app, ctx);
    }
}

fn show_list(app: &mut App, ctx: &egui::Context) {
    let Some(mut st) = app.conn_list.take() else { return };
    let th = app.theme;
    let mut close = false;
    let mut actions = Vec::new();
    let editor_open = app.conn_editor.is_some();

    let visible: Vec<usize> = app
        .settings
        .connections
        .iter()
        .enumerate()
        .filter(|(_, c)| matches(c, &st.filter))
        .map(|(i, _)| i)
        .collect();
    if st.selected.is_none_or(|s| !visible.iter().any(|&i| app.settings.connections[i].id == s)) {
        st.selected = visible.first().map(|&i| app.settings.connections[i].id);
    }
    let sel_index = st.selected.and_then(|s| app.settings.connections.iter().position(|c| c.id == s));

    // Keyboard navigation (only when the editor is not on top).
    if !editor_open && st.confirm_delete.is_none() {
        ctx.input_mut(|i| {
            let pos = sel_index.and_then(|si| visible.iter().position(|&v| v == si));
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                && let Some(&n) = visible.get(pos.map_or(0, |p| (p + 1).min(visible.len() - 1)))
            {
                st.selected = Some(app.settings.connections[n].id);
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                && let Some(&n) = visible.get(pos.map_or(0, |p| p.saturating_sub(1)))
            {
                st.selected = Some(app.settings.connections[n].id);
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                && let Some(id) = st.selected
            {
                actions.push(Action::Connect(id));
                close = true;
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) && st.selected.is_some() {
                st.confirm_delete = st.selected;
            }
        });
    }

    let modal = egui::Modal::new(Id::new("conn-list")).show(ctx, |ui| {
        let screen = ui.ctx().content_rect();
        ui.set_width((screen.width() * 0.7).clamp(640.0, 980.0));
        ui.horizontal(|ui| {
            ui.heading("MongoDB Connections");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut st.filter).hint_text("🔍 Search name, host, group…").desired_width(240.0));
                if std::mem::take(&mut st.focus_search) {
                    r.request_focus();
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("➕ Create").on_hover_text("New connection").clicked() {
                actions.push(Action::NewConnection);
            }
            let has_sel = st.selected.is_some();
            if ui.add_enabled(has_sel, egui::Button::new("✏ Edit")).clicked()
                && let Some(id) = st.selected
            {
                actions.push(Action::EditConnection(id));
            }
            if ui.add_enabled(has_sel, egui::Button::new("Clone")).clicked()
                && let Some(i) = sel_index
            {
                let mut c = app.settings.connections[i].clone();
                c.id = Uuid::new_v4();
                c.name = format!("Copy of {}", c.name);
                c.last_connected = None;
                app.conn_editor = Some(ConnectionEditor::new(c, true));
            }
            if ui.add_enabled(has_sel, egui::Button::new("🗑 Remove")).clicked() {
                st.confirm_delete = st.selected;
            }
            ui.separator();
            let can_up = sel_index.is_some_and(|i| i > 0) && st.filter.is_empty();
            let can_down = sel_index.is_some_and(|i| i + 1 < app.settings.connections.len()) && st.filter.is_empty();
            if ui.add_enabled(can_up, egui::Button::new("⏶")).on_hover_text("Move up").clicked()
                && let Some(i) = sel_index
            {
                app.settings.connections.swap(i, i - 1);
                actions.push(Action::SaveSettings);
            }
            if ui.add_enabled(can_down, egui::Button::new("⏷")).on_hover_text("Move down").clicked()
                && let Some(i) = sel_index
            {
                app.settings.connections.swap(i, i + 1);
                actions.push(Action::SaveSettings);
            }
            ui.separator();
            ui.menu_button("Import ⏷", |ui| {
                if ui.button("From connection string…").clicked() {
                    let mut e = ConnectionEditor::new(ConnectionSettings::default(), true);
                    e.uri_editing = true;
                    e.uri_text.clear();
                    app.conn_editor = Some(e);
                    ui.close();
                }
                let configs = find_robo3t_configs();
                if configs.is_empty() {
                    ui.add_enabled(false, egui::Button::new("From Robo 3T (no configuration found)"));
                }
                for path in configs {
                    if ui.button(format!("From Robo 3T: {}", path.display())).clicked() {
                        st.message = Some(import_from(app, &path));
                        ui.close();
                    }
                }
                if ui.button("From Robo 3T file…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Robo 3T", &["json"]).pick_file() {
                        st.message = Some(import_from(app, &path));
                    }
                    ui.close();
                }
            });
        });
        ui.add_space(4.0);
        if let Some((m, ok)) = &st.message {
            ui.label(RichText::new(m).color(if *ok { th.success } else { th.error }));
        }

        let list_h = (screen.height() * 0.5).clamp(220.0, 520.0);
        egui::Frame::new().fill(th.base).stroke(egui::Stroke::new(1.0, th.border)).corner_radius(4).show(ui, |ui| {
            ui.set_min_height(list_h);
            if app.settings.connections.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(list_h * 0.3);
                    ui.label(RichText::new("No connections yet").strong());
                    ui.label(RichText::new("Create one, paste a connection string or import from Robo 3T.").color(th.text_weak));
                    ui.add_space(8.0);
                    if ui.button("➕ Create connection").clicked() {
                        actions.push(Action::NewConnection);
                    }
                });
                return;
            }
            let row_h = app.settings.font_size + 12.0;
            let mut groups: Vec<String> = Vec::new();
            for &i in &visible {
                let g = &app.settings.connections[i].group;
                if !groups.contains(g) {
                    groups.push(g.clone());
                }
            }
            groups.sort_by_key(|a| (a.is_empty(), a.to_lowercase()));
            let grouped = groups.len() > 1 || groups.first().is_some_and(|g| !g.is_empty());
            egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("conn-grid").num_columns(5).striped(true).min_row_height(row_h).spacing([10.0, 2.0]).show(ui, |ui| {
                    for h in ["", "Name", "Address", "Attributes", "Last used"] {
                        ui.label(RichText::new(h).strong().color(th.text_weak));
                    }
                    ui.end_row();
                    for g in &groups {
                        if grouped {
                            ui.label("");
                            ui.label(RichText::new(if g.is_empty() { "Ungrouped" } else { g }).strong().color(th.accent));
                            ui.end_row();
                        }
                        for &i in visible.iter().filter(|&&i| &app.settings.connections[i].group == g) {
                            let c = &app.settings.connections[i];
                            let selected = st.selected == Some(c.id);
                            color_dot(ui, c.color, 12.0);
                            let name = ui.add(egui::Button::selectable(selected, RichText::new(&c.name).strong()).frame_when_inactive(false));
                            let addr = ui.add(egui::Label::new(RichText::new(c.address_label()).monospace()).sense(egui::Sense::click()));
                            ui.horizontal(|ui| attributes(ui, c, &th));
                            ui.label(RichText::new(relative_time(c.last_connected)).color(th.text_weak));
                            ui.end_row();
                            let clicked = name.clicked() || addr.clicked();
                            if clicked || name.secondary_clicked() {
                                st.selected = Some(c.id);
                            }
                            if name.double_clicked() || addr.double_clicked() {
                                actions.push(Action::Connect(c.id));
                                close = true;
                            }
                            let id = c.id;
                            name.context_menu(|ui| {
                                if ui.button("Connect").clicked() {
                                    actions.push(Action::Connect(id));
                                    close = true;
                                    ui.close();
                                }
                                if ui.button("Edit…").clicked() {
                                    actions.push(Action::EditConnection(id));
                                    ui.close();
                                }
                                if ui.button("Copy connection string").clicked() {
                                    ui.ctx().copy_text(build_uri(c, None, None, Secrets::Omit));
                                    ui.close();
                                }
                                if ui.button("Remove…").clicked() {
                                    st.confirm_delete = Some(id);
                                    ui.close();
                                }
                            });
                        }
                    }
                });
            });
        });

        // Deletion confirmation, inline.
        if let Some(del) = st.confirm_delete {
            let name = app.settings.connection(del).map(|c| c.name.clone()).unwrap_or_default();
            ui.add_space(6.0);
            egui::Frame::new().fill(th.surface).corner_radius(4).inner_margin(8).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Remove connection \"{name}\"?")).color(th.warning));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(RichText::new("Remove").color(th.error)).clicked() {
                            app.settings.connections.retain(|c| c.id != del);
                            st.confirm_delete = None;
                            st.selected = None;
                            actions.push(Action::SaveSettings);
                        }
                        if ui.button("Keep").clicked() {
                            st.confirm_delete = None;
                        }
                    });
                });
            });
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Double-click or Enter to connect · Del to remove · arrows to navigate").small().color(th.text_weak));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(st.selected.is_some(), egui::Button::new(RichText::new("Connect").strong())).clicked()
                    && let Some(id) = st.selected
                {
                    actions.push(Action::Connect(id));
                    close = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
    });
    if modal.should_close() && !editor_open {
        close = true;
    }
    if !close {
        app.conn_list = Some(st);
    }
    app.actions.extend(actions);
}

fn import_from(app: &mut App, path: &Path) -> (String, bool) {
    match import_robo3t(path) {
        Ok(list) => {
            let mut added = 0;
            for c in list {
                let dup = app.settings.connections.iter().any(|x| x.name == c.name && x.address_label() == c.address_label());
                if !dup {
                    app.settings.connections.push(c);
                    added += 1;
                }
            }
            app.actions.push(Action::SaveSettings);
            app.log(LogLevel::Info, format!("Imported {added} connection(s) from {}", path.display()));
            (
                format!("Imported {added} connection(s). Robo 3T stores passwords encrypted: re-enter them in each connection."),
                true,
            )
        }
        Err(e) => (format!("Import failed: {e}"), false),
    }
}

// ---------------------------------------------------------------- editor

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    General,
    Auth,
    Ssh,
    Tls,
    Advanced,
}

impl Section {
    const ALL: [Section; 5] = [Section::General, Section::Auth, Section::Ssh, Section::Tls, Section::Advanced];

    fn label(self) -> &'static str {
        match self {
            Section::General => "Connection",
            Section::Auth => "Authentication",
            Section::Ssh => "SSH Tunnel",
            Section::Tls => "TLS / SSL",
            Section::Advanced => "Advanced",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Section::General => "🖥",
            Section::Auth => "🔒",
            Section::Ssh => "🔀",
            Section::Tls => "🛡",
            Section::Advanced => "⚙",
        }
    }
}

pub struct Issue {
    pub section: Section,
    pub field: &'static str,
    pub message: String,
}

pub struct ConnectionEditor {
    pub conn: ConnectionSettings,
    original: ConnectionSettings,
    pub is_new: bool,
    pub section: Section,
    /// Starts a connection test on the next frame (used by the autotest).
    pub run_test: bool,
    show_password: bool,
    show_ssh_password: bool,
    pub uri_editing: bool,
    pub uri_text: String,
    uri_feedback: Option<(String, bool)>,
    visible_dbs_text: String,
    pub testing: bool,
    pub test_token: u64,
    pub test_steps: Vec<StepReport>,
    pub test_result: Option<Result<String, String>>,
    confirm_discard: bool,
    touched: bool,
}

impl ConnectionEditor {
    pub fn new(conn: ConnectionSettings, is_new: bool) -> Self {
        let visible_dbs_text = conn.visible_databases.join("\n");
        ConnectionEditor {
            original: conn.clone(),
            uri_text: build_uri(&conn, None, None, Secrets::Omit),
            conn,
            is_new,
            section: Section::General,
            run_test: false,
            show_password: false,
            show_ssh_password: false,
            uri_editing: false,
            uri_feedback: None,
            visible_dbs_text,
            testing: false,
            test_token: 0,
            test_steps: Vec::new(),
            test_result: None,
            confirm_discard: false,
            touched: false,
        }
    }

    fn dirty(&self) -> bool {
        self.is_new || self.conn != self.original
    }
}

pub fn validate(c: &ConnectionSettings) -> Vec<Issue> {
    let mut v = Vec::new();
    let mut push = |section, field, message: &str| v.push(Issue { section, field, message: message.to_string() });
    if c.name.trim().is_empty() {
        push(Section::General, "name", "A name is required");
    }
    match c.kind {
        ConnectionType::Direct => {
            if c.hosts.first().is_none_or(|h| h.host.trim().is_empty()) {
                push(Section::General, "host", "Host is required");
            }
            if c.hosts.first().is_some_and(|h| h.port == 0) {
                push(Section::General, "port", "Port must be between 1 and 65535");
            }
        }
        ConnectionType::ReplicaSet => {
            if c.hosts.iter().all(|h| h.host.trim().is_empty()) {
                push(Section::General, "members", "Add at least one member");
            }
            if c.hosts.iter().any(|h| h.port == 0) {
                push(Section::General, "members", "Every member needs a valid port");
            }
        }
        ConnectionType::Srv => {
            let h = c.hosts.first().map(|h| h.host.trim()).unwrap_or_default();
            if h.is_empty() {
                push(Section::General, "host", "Cluster hostname is required");
            } else if h.contains(':') || h.contains(',') {
                push(Section::General, "host", "SRV hostnames have no port and a single host");
            } else if h.split('.').count() < 3 {
                push(Section::General, "host", "SRV hostname needs at least 3 parts (e.g. cluster0.abcde.mongodb.net)");
            }
        }
    }
    let cr = &c.credentials;
    if cr.enabled {
        if cr.mechanism != AuthMechanism::X509 && cr.username.trim().is_empty() {
            push(Section::Auth, "username", "User name is required");
        }
        if cr.database.trim().is_empty() {
            push(Section::Auth, "database", "Authentication database is required (usually admin)");
        }
        if cr.mechanism == AuthMechanism::X509 && !c.tls.enabled {
            push(Section::Auth, "mechanism", "X.509 requires TLS with a client certificate");
        }
    }
    let s = &c.ssh;
    if s.enabled {
        if s.host.trim().is_empty() {
            push(Section::Ssh, "host", "SSH host is required");
        }
        if s.port == 0 {
            push(Section::Ssh, "port", "Invalid SSH port");
        }
        if s.username.trim().is_empty() {
            push(Section::Ssh, "username", "SSH user name is required");
        }
        if s.method == SshAuthMethod::PrivateKey {
            let p = crate::mongo::ssh::expand_tilde(&s.private_key_file);
            if s.private_key_file.trim().is_empty() {
                push(Section::Ssh, "key", "Select a private key file");
            } else if !p.exists() {
                push(Section::Ssh, "key", "Private key file not found");
            }
        }
        if s.method == SshAuthMethod::Agent && std::env::var_os("SSH_AUTH_SOCK").is_none() {
            push(Section::Ssh, "agent", "No SSH agent detected (SSH_AUTH_SOCK is not set)");
        }
    }
    let t = &c.tls;
    if t.enabled {
        if !t.ca_file.is_empty() && !Path::new(&t.ca_file).exists() {
            push(Section::Tls, "ca", "CA file not found");
        }
        if !t.cert_key_file.is_empty() && !Path::new(&t.cert_key_file).exists() {
            push(Section::Tls, "cert", "Certificate/key file not found");
        }
        if cr.enabled && cr.mechanism == AuthMechanism::X509 && t.cert_key_file.is_empty() {
            push(Section::Tls, "cert", "X.509 authentication needs a client certificate");
        }
    }
    if c.use_visible_databases && c.visible_databases.is_empty() {
        push(Section::Advanced, "visible", "List at least one database or disable this option");
    }
    for opt in c.extra_options.split('&').map(str::trim).filter(|o| !o.is_empty()) {
        if !opt.contains('=') {
            push(Section::Advanced, "extra", "Extra options must look like key=value&key2=value2");
            break;
        }
    }
    v
}

fn section_state(c: &ConnectionSettings, s: Section) -> Option<bool> {
    // Some(true) = in use, Some(false) = configured but disabled, None = n/a
    match s {
        Section::General => None,
        Section::Auth => Some(c.credentials.enabled),
        Section::Ssh => Some(c.ssh.enabled),
        Section::Tls => Some(c.tls.enabled || c.kind == ConnectionType::Srv),
        Section::Advanced => Some(c.use_visible_databases || !c.extra_options.is_empty() || !c.default_database.is_empty()),
    }
}

fn field_error(ui: &mut Ui, issues: &[Issue], field: &str, th: &Theme, show: bool) {
    if !show {
        return;
    }
    for i in issues.iter().filter(|i| i.field == field) {
        ui.label(RichText::new(format!("⚠ {}", i.message)).small().color(th.error));
    }
}

fn file_picker(ui: &mut Ui, value: &mut String, hint: &str, filters: &[(&str, &[&str])]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let w = ui.available_width() - 90.0;
        changed |= ui.add(egui::TextEdit::singleline(value).hint_text(hint).desired_width(w.max(120.0))).changed();
        if ui.button("Browse…").clicked() {
            let mut d = rfd::FileDialog::new();
            for (name, ext) in filters {
                d = d.add_filter(*name, ext);
            }
            if let Some(dir) = Path::new(value.as_str()).parent().filter(|p| p.exists()) {
                d = d.set_directory(dir);
            }
            if let Some(p) = d.pick_file() {
                *value = p.display().to_string();
                changed = true;
            }
        }
    });
    changed
}

fn password_field(ui: &mut Ui, value: &mut String, show: &mut bool, enabled: bool, hint: &str) {
    ui.horizontal(|ui| {
        let w = ui.available_width() - 40.0;
        ui.add_enabled(enabled, egui::TextEdit::singleline(value).password(!*show).hint_text(hint).desired_width(w.max(120.0)));
        if ui.add_enabled(enabled, egui::Button::new(if *show { "🙈" } else { "👁" })).on_hover_text(if *show { "Hide" } else { "Show" }).clicked() {
            *show = !*show;
        }
    });
}

fn grid(ui: &mut Ui, id: &str, f: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([12.0, 8.0]).min_col_width(150.0).show(ui, f);
}

fn show_editor(app: &mut App, ctx: &egui::Context) {
    let Some(mut ed) = app.conn_editor.take() else { return };
    let th = app.theme;
    let mut close = false;
    let mut save = false;
    let mut save_connect = false;
    let mut start_test = false;

    if std::mem::take(&mut ed.run_test) {
        start_test = true;
    }
    let issues = validate(&ed.conn);
    let show_errors = ed.touched || !ed.is_new;

    let modal = egui::Modal::new(Id::new("conn-editor")).show(ctx, |ui| {
        let screen = ui.ctx().content_rect();
        ui.set_width((screen.width() * 0.75).clamp(720.0, 1040.0));
        let test_visible = ed.testing || !ed.test_steps.is_empty() || ed.test_result.is_some();
        let reserved = if test_visible { 400.0 } else { 260.0 };
        let body_h = (screen.height() - reserved).clamp(240.0, 640.0);

        // Header: name, colour tag, group.
        ui.horizontal(|ui| {
            ui.heading(if ed.is_new { "New Connection" } else { "Edit Connection" });
            if ed.dirty() && !ed.is_new {
                ui.label(RichText::new("● modified").small().color(th.warning));
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Name");
            let r = ui.add(egui::TextEdit::singleline(&mut ed.conn.name).desired_width(260.0).hint_text("e.g. Production cluster"));
            if r.changed() {
                ed.touched = true;
            }
            ui.add_space(12.0);
            ui.label("Group");
            ui.add(egui::TextEdit::singleline(&mut ed.conn.group).desired_width(140.0).hint_text("optional"));
            ui.add_space(12.0);
            ui.label("Colour");
            let none = ed.conn.color.is_none();
            if ui.add(egui::Button::new("∅").selected(none).small()).on_hover_text("No colour").clicked() {
                ed.conn.color = None;
            }
            for c in TAG_COLORS {
                let selected = ed.conn.color == Some(c);
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                let col = Color32::from_rgb(c[0], c[1], c[2]);
                ui.painter().circle_filled(rect.center(), 7.0, col);
                if selected {
                    ui.painter().circle_stroke(rect.center(), 9.0, egui::Stroke::new(2.0, th.text));
                }
                if resp.clicked() {
                    ed.conn.color = Some(c);
                }
            }
        });
        field_error(ui, &issues, "name", &th, show_errors);
        ui.add_space(4.0);

        // Connection string, two-way.
        egui::Frame::new().fill(th.surface).corner_radius(4).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("URI").strong());
                if ed.uri_editing {
                    let w = ui.available_width() - 150.0;
                    let r = ui.add(egui::TextEdit::singleline(&mut ed.uri_text).hint_text("mongodb://user:pass@host:27017/?replicaSet=rs0").desired_width(w.max(200.0)).font(egui::TextStyle::Monospace));
                    if !r.has_focus() && ed.uri_text.is_empty() {
                        r.request_focus();
                    }
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(RichText::new("Apply").strong()).clicked() || enter {
                        let mut c = ed.conn.clone();
                        match apply_uri(&mut c, &ed.uri_text) {
                            Ok(w) => {
                                if ed.is_new && (c.name.is_empty() || c.name == "New Connection") {
                                    c.name = c.hosts.first().map(|h| h.host.clone()).unwrap_or_default();
                                }
                                ed.visible_dbs_text = c.visible_databases.join("\n");
                                ed.conn = c;
                                ed.uri_editing = false;
                                ed.touched = true;
                                ed.uri_feedback = Some((
                                    if w.is_empty() { "Connection string applied".into() } else { format!("Applied — {}", w.join("; ")) },
                                    true,
                                ));
                            }
                            Err(e) => ed.uri_feedback = Some((e, false)),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        ed.uri_editing = false;
                        ed.uri_feedback = None;
                    }
                } else {
                    let uri = build_uri(&ed.conn, None, None, Secrets::Mask);
                    let w = ui.available_width() - 150.0;
                    ui.add_sized([w.max(200.0), 18.0], egui::Label::new(RichText::new(&uri).monospace().color(th.text_weak)).truncate())
                        .on_hover_text(&uri);
                    if ui.button("📋").on_hover_text("Copy connection string (without password)").clicked() {
                        ui.ctx().copy_text(build_uri(&ed.conn, None, None, Secrets::Omit));
                        ed.uri_feedback = Some(("Copied to clipboard".into(), true));
                    }
                    if ui.button("Paste URI…").on_hover_text("Fill the form from a connection string").clicked() {
                        ed.uri_editing = true;
                        ed.uri_text = build_uri(&ed.conn, None, None, Secrets::Include);
                        ed.uri_feedback = None;
                    }
                }
            });
            if let Some((m, ok)) = &ed.uri_feedback {
                ui.label(RichText::new(m).small().color(if *ok { th.success } else { th.error }));
            }
        });
        ui.add_space(6.0);

        ui.horizontal_top(|ui| {
            // Sidebar.
            ui.allocate_ui_with_layout(egui::vec2(190.0, body_h), Layout::top_down(Align::Min), |ui| {
                ui.set_min_height(body_h);
                ui.set_width(190.0);
                for s in Section::ALL {
                    let errs = issues.iter().filter(|i| i.section == s).count();
                    let selected = ed.section == s;
                    let r = ui.add_sized(
                        [186.0, 30.0],
                        egui::Button::selectable(selected, RichText::new(format!("{}  {}", s.icon(), s.label())).strong())
                            .right_text(if errs > 0 && show_errors {
                                RichText::new(format!("⚠ {errs}")).color(th.error)
                            } else {
                                match section_state(&ed.conn, s) {
                                    Some(true) => RichText::new("✔ on").small().color(th.success),
                                    Some(false) => RichText::new("off").small().color(th.text_weak),
                                    None => RichText::new(""),
                                }
                            }),
                    );
                    if r.clicked() {
                        ed.section = s;
                    }
                }
            });
            ui.add_sized([8.0, body_h], egui::Separator::default().vertical());
            // Section body.
            ui.vertical(|ui| {
                ui.set_min_height(body_h);
                egui::ScrollArea::vertical().id_salt("conn-section").max_height(body_h).auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(2.0);
                    let changed = match ed.section {
                        Section::General => general_section(ui, &mut ed, &issues, &th, show_errors),
                        Section::Auth => auth_section(ui, &mut ed, &issues, &th, show_errors),
                        Section::Ssh => ssh_section(ui, &mut ed, &issues, &th, show_errors),
                        Section::Tls => tls_section(ui, &mut ed, &issues, &th, show_errors),
                        Section::Advanced => advanced_section(ui, &mut ed, &issues, &th, show_errors),
                    };
                    if changed {
                        ed.touched = true;
                    }
                });
            });
        });

        // Test results.
        if ed.testing || !ed.test_steps.is_empty() || ed.test_result.is_some() {
            ui.separator();
            egui::Frame::new().fill(th.base).stroke(egui::Stroke::new(1.0, th.border)).corner_radius(4).inner_margin(8).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Connection test");
                    if ed.testing {
                        ui.spinner();
                    }
                    match &ed.test_result {
                        Some(Ok(m)) => {
                            ui.label(RichText::new(format!("✔ {m}")).color(th.success));
                        }
                        Some(Err(_)) => {
                            ui.label(RichText::new("✖ Failed").color(th.error));
                        }
                        None => {}
                    }
                });
                for st in &ed.test_steps {
                    ui.horizontal_wrapped(|ui| {
                        let (icon, c) = if st.ok { ("✔", th.success) } else { ("✖", th.error) };
                        ui.label(RichText::new(icon).color(c));
                        ui.label(RichText::new(st.kind.label()).strong());
                        ui.label(RichText::new(format!("{} ms", st.millis)).color(th.text_weak));
                        ui.add(egui::Label::new(RichText::new(&st.message).color(if st.ok { th.text } else { th.error })).wrap());
                    });
                }
                if let Some(Err(e)) = &ed.test_result
                    && !ed.test_steps.iter().any(|s| !s.ok)
                {
                    ui.add(egui::Label::new(RichText::new(e).color(th.error)).wrap());
                }
            });
        }

        // Footer.
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let can_test = !ed.testing && issues.is_empty();
            if ui.add_enabled(can_test, egui::Button::new("⚡ Test")).on_hover_text("Connect with the current settings without saving").clicked() {
                start_test = true;
            }
            if !issues.is_empty() && show_errors {
                ui.label(RichText::new(format!("{} issue(s) to fix", issues.len())).color(th.error));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ok = issues.is_empty();
                if ui.add_enabled(ok, egui::Button::new(RichText::new("Save & Connect").strong())).clicked() {
                    save = true;
                    save_connect = true;
                }
                if ui.add_enabled(ok, egui::Button::new("Save")).clicked() {
                    save = true;
                }
                if ed.confirm_discard {
                    if ui.button(RichText::new("Discard changes").color(th.error)).clicked() {
                        close = true;
                    }
                    ui.label(RichText::new("Unsaved changes —").color(th.warning));
                } else if ui.button("Cancel").clicked() {
                    if ed.dirty() && ed.touched {
                        ed.confirm_discard = true;
                    } else {
                        close = true;
                    }
                }
            });
        });
        if (ed.touched || ed.is_new) && !issues.is_empty() {
            ed.touched = true;
        }
    });

    if modal.should_close() && !ed.testing {
        if ed.dirty() && ed.touched {
            ed.confirm_discard = true;
        } else {
            close = true;
        }
    }

    if start_test {
        ed.test_steps.clear();
        ed.test_result = None;
        ed.testing = true;
        ed.test_token = app.next_id();
        let token = ed.test_token;
        let conn = ed.conn.clone();
        let secrets = ConnectSecrets {
            mongo_password: Some(conn.credentials.password.clone()),
            ssh: crate::mongo::ssh::SshSecrets { password: Some(conn.ssh.password.clone()), passphrase: Some(conn.ssh.passphrase.clone()) },
        };
        let tx = app.tx.clone();
        let ctx2 = ctx.clone();
        app.rt.spawn(async move {
            let tx2 = tx.clone();
            let c2 = ctx2.clone();
            let res = Connection::open(&conn, &secrets, move |step| {
                let _ = tx2.send(AppEvent::Test(token, step));
                c2.request_repaint();
            })
            .await;
            let out = match res {
                Ok(c) => {
                    let msg = format!("Connected to MongoDB {}", c.version);
                    c.shutdown();
                    Ok(msg)
                }
                Err(e) => Err(crate::mongo::error_message(&e)),
            };
            let _ = tx.send(AppEvent::TestDone(token, out));
            ctx2.request_repaint();
        });
    }

    if save {
        let mut c = ed.conn.clone();
        if c.credentials.ask_password {
            c.credentials.password.clear();
        }
        if c.ssh.ask_password {
            c.ssh.password.clear();
            c.ssh.passphrase.clear();
        }
        if c.kind == ConnectionType::Direct || c.kind == ConnectionType::Srv {
            c.hosts.truncate(1);
        }
        c.hosts.retain(|h| !h.host.trim().is_empty());
        for h in &mut c.hosts {
            h.host = h.host.trim().to_string();
        }
        let id = c.id;
        match app.settings.connections.iter_mut().find(|x| x.id == id) {
            Some(existing) => *existing = c,
            None => app.settings.connections.push(c),
        }
        app.actions.push(Action::SaveSettings);
        if let Some(list) = &mut app.conn_list {
            list.selected = Some(id);
        }
        if save_connect {
            app.actions.push(Action::Connect(id));
            app.conn_list = None;
        }
        close = true;
    }

    if !close {
        app.conn_editor = Some(ed);
    }
}

fn general_section(ui: &mut Ui, ed: &mut ConnectionEditor, issues: &[Issue], th: &Theme, show: bool) -> bool {
    let mut changed = false;
    let c = &mut ed.conn;
    ui.label(RichText::new("Topology").strong());
    ui.horizontal(|ui| {
        changed |= ui.radio_value(&mut c.kind, ConnectionType::Direct, "Direct connection").changed();
        changed |= ui.radio_value(&mut c.kind, ConnectionType::ReplicaSet, "Replica set / multiple hosts").changed();
        changed |= ui.radio_value(&mut c.kind, ConnectionType::Srv, "DNS seed list (mongodb+srv)").changed();
    });
    if c.hosts.is_empty() {
        c.hosts.push(HostPort::default());
    }
    ui.add_space(6.0);
    match c.kind {
        ConnectionType::Direct => {
            grid(ui, "g-direct", |ui| {
                ui.label("Address");
                ui.horizontal(|ui| {
                    let h = &mut c.hosts[0];
                    changed |= ui.add(egui::TextEdit::singleline(&mut h.host).hint_text("localhost").desired_width(280.0)).changed();
                    ui.label(":");
                    changed |= ui.add(egui::DragValue::new(&mut h.port).range(0..=65535)).changed();
                });
                ui.end_row();
            });
            field_error(ui, issues, "host", th, show);
            field_error(ui, issues, "port", th, show);
        }
        ConnectionType::ReplicaSet => {
            grid(ui, "g-rs", |ui| {
                ui.label("Replica set name");
                changed |= ui.add(egui::TextEdit::singleline(&mut c.replica_set_name).hint_text("optional, e.g. rs0").desired_width(220.0)).changed();
                ui.end_row();
                ui.label("Read preference");
                egui::ComboBox::from_id_salt("readpref").selected_text(c.read_preference.uri_name()).show_ui(ui, |ui| {
                    for r in ReadPreference::ALL {
                        changed |= ui.selectable_value(&mut c.read_preference, r, r.uri_name()).changed();
                    }
                });
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Members").strong());
            let mut remove = None;
            let n = c.hosts.len();
            for (i, h) in c.hosts.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}.", i + 1)).color(th.text_weak));
                    changed |= ui.add(egui::TextEdit::singleline(&mut h.host).hint_text("host").desired_width(260.0)).changed();
                    ui.label(":");
                    changed |= ui.add(egui::DragValue::new(&mut h.port).range(0..=65535)).changed();
                    if ui.add_enabled(n > 1, egui::Button::new("✖").small()).on_hover_text("Remove member").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                c.hosts.remove(i);
                changed = true;
            }
            if ui.button("➕ Add member").clicked() {
                let port = c.hosts.last().map(|h| h.port).unwrap_or(27017);
                c.hosts.push(HostPort { host: String::new(), port });
                changed = true;
            }
            field_error(ui, issues, "members", th, show);
            if c.ssh.enabled {
                ui.label(RichText::new("ℹ Through SSH only the first member is reachable; the connection is made directly to it.").small().color(th.warning));
            }
        }
        ConnectionType::Srv => {
            grid(ui, "g-srv", |ui| {
                ui.label("Cluster hostname");
                changed |= ui.add(egui::TextEdit::singleline(&mut c.hosts[0].host).hint_text("cluster0.abcde.mongodb.net").desired_width(320.0)).changed();
                ui.end_row();
                ui.label("Read preference");
                egui::ComboBox::from_id_salt("readpref-srv").selected_text(c.read_preference.uri_name()).show_ui(ui, |ui| {
                    for r in ReadPreference::ALL {
                        changed |= ui.selectable_value(&mut c.read_preference, r, r.uri_name()).changed();
                    }
                });
                ui.end_row();
            });
            field_error(ui, issues, "host", th, show);
            ui.label(RichText::new("Hosts and options are discovered through DNS SRV/TXT records. TLS is enabled by default.").small().color(th.text_weak));
        }
    }
    ui.add_space(8.0);
    grid(ui, "g-db", |ui| {
        ui.label("Default database");
        changed |= ui.add(egui::TextEdit::singleline(&mut c.default_database).hint_text("used for new shells (default: test)").desired_width(280.0)).changed();
        ui.end_row();
    });
    changed
}

fn auth_section(ui: &mut Ui, ed: &mut ConnectionEditor, issues: &[Issue], th: &Theme, show: bool) -> bool {
    let mut changed = false;
    let cr = &mut ed.conn.credentials;
    changed |= ui.checkbox(&mut cr.enabled, RichText::new("Perform authentication").strong()).changed();
    ui.add_space(6.0);
    ui.add_enabled_ui(cr.enabled, |ui| {
        grid(ui, "g-auth", |ui| {
            ui.label("Mechanism");
            egui::ComboBox::from_id_salt("mech").selected_text(cr.mechanism.label()).width(240.0).show_ui(ui, |ui| {
                for m in AuthMechanism::ALL {
                    changed |= ui.selectable_value(&mut cr.mechanism, m, m.label()).changed();
                }
            });
            ui.end_row();
            ui.label("Database");
            ui.vertical(|ui| {
                changed |= ui.add(egui::TextEdit::singleline(&mut cr.database).hint_text("admin").desired_width(240.0)).changed();
                field_error(ui, issues, "database", th, show);
            });
            ui.end_row();
            ui.label("User name");
            ui.vertical(|ui| {
                changed |= ui.add(egui::TextEdit::singleline(&mut cr.username).desired_width(240.0)).changed();
                field_error(ui, issues, "username", th, show);
            });
            ui.end_row();
            if cr.mechanism != AuthMechanism::X509 {
                ui.label("Password");
                ui.vertical(|ui| {
                    password_field(ui, &mut cr.password, &mut ed.show_password, !cr.ask_password, if cr.ask_password { "asked at connect time" } else { "" });
                    changed |= ui.checkbox(&mut cr.ask_password, "Ask for password each time").changed();
                });
                ui.end_row();
            }
        });
        field_error(ui, issues, "mechanism", th, show);
        let hint = match cr.mechanism {
            AuthMechanism::Default => "The driver negotiates SCRAM-SHA-256 or SCRAM-SHA-1 with the server.",
            AuthMechanism::X509 => "Uses the client certificate configured in the TLS section. Database is $external.",
            AuthMechanism::Plain => "LDAP proxy authentication (MongoDB Enterprise). Database is usually $external.",
            _ => "",
        };
        if !hint.is_empty() {
            ui.label(RichText::new(hint).small().color(th.text_weak));
        }
        if cr.mechanism == AuthMechanism::X509 && cr.database != "$external" && ui.small_button("Use $external").clicked() {
            cr.database = "$external".into();
            changed = true;
        }
    });
    changed
}

fn ssh_section(ui: &mut Ui, ed: &mut ConnectionEditor, issues: &[Issue], th: &Theme, show: bool) -> bool {
    let mut changed = false;
    let s = &mut ed.conn.ssh;
    changed |= ui.checkbox(&mut s.enabled, RichText::new("Use SSH tunnel").strong()).changed();
    ui.label(RichText::new("MongoDB is reached through an SSH server (local port forwarding).").small().color(th.text_weak));
    ui.add_space(6.0);
    ui.add_enabled_ui(s.enabled, |ui| {
        grid(ui, "g-ssh", |ui| {
            ui.label("SSH address");
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    changed |= ui.add(egui::TextEdit::singleline(&mut s.host).hint_text("bastion.example.com").desired_width(260.0)).changed();
                    ui.label(":");
                    changed |= ui.add(egui::DragValue::new(&mut s.port).range(0..=65535)).changed();
                });
                field_error(ui, issues, "host", th, show);
                field_error(ui, issues, "port", th, show);
            });
            ui.end_row();
            ui.label("SSH user name");
            ui.vertical(|ui| {
                changed |= ui.add(egui::TextEdit::singleline(&mut s.username).desired_width(260.0)).changed();
                field_error(ui, issues, "username", th, show);
            });
            ui.end_row();
            ui.label("Authentication");
            ui.horizontal(|ui| {
                changed |= ui.radio_value(&mut s.method, SshAuthMethod::Password, "Password").changed();
                changed |= ui.radio_value(&mut s.method, SshAuthMethod::PrivateKey, "Private key").changed();
                changed |= ui.radio_value(&mut s.method, SshAuthMethod::Agent, "SSH agent").changed();
            });
            ui.end_row();
            match s.method {
                SshAuthMethod::Password => {
                    ui.label("Password");
                    ui.vertical(|ui| {
                        password_field(ui, &mut s.password, &mut ed.show_ssh_password, !s.ask_password, if s.ask_password { "asked at connect time" } else { "" });
                        changed |= ui.checkbox(&mut s.ask_password, "Ask for password each time").changed();
                    });
                    ui.end_row();
                }
                SshAuthMethod::PrivateKey => {
                    ui.label("Private key");
                    ui.vertical(|ui| {
                        changed |= file_picker(ui, &mut s.private_key_file, "~/.ssh/id_ed25519", &[("All files", &["*"])]);
                        field_error(ui, issues, "key", th, show);
                    });
                    ui.end_row();
                    ui.label("Passphrase");
                    ui.vertical(|ui| {
                        password_field(ui, &mut s.passphrase, &mut ed.show_ssh_password, !s.ask_password, if s.ask_password { "asked at connect time" } else { "empty if the key is not encrypted" });
                        changed |= ui.checkbox(&mut s.ask_password, "Ask for passphrase each time").changed();
                    });
                    ui.end_row();
                }
                SshAuthMethod::Agent => {
                    ui.label("Agent");
                    ui.vertical(|ui| {
                        let sock = std::env::var("SSH_AUTH_SOCK").unwrap_or_default();
                        ui.label(RichText::new(if sock.is_empty() { "not detected".to_string() } else { sock }).monospace().color(th.text_weak));
                        field_error(ui, issues, "agent", th, show);
                    });
                    ui.end_row();
                }
            }
            ui.label("Host key");
            ui.vertical(|ui| {
                changed |= ui.checkbox(&mut s.strict_host_key, "Strict checking (reject unknown hosts)").changed();
                ui.label(RichText::new("Keys are checked against ~/.ssh/known_hosts; unknown keys are recorded on first use unless strict checking is on.").small().color(th.text_weak));
            });
            ui.end_row();
            ui.label("Keep-alive");
            ui.horizontal(|ui| {
                changed |= ui.add(egui::DragValue::new(&mut s.keepalive_secs).range(0..=600).suffix(" s")).changed();
                ui.label(RichText::new("0 = disabled").small().color(th.text_weak));
            });
            ui.end_row();
        });
        if ed.conn.kind == ConnectionType::Srv {
            ui.label(RichText::new("ℹ With SRV, the first host is resolved locally; connect directly to a member host instead if possible.").small().color(th.warning));
        }
    });
    changed
}

fn tls_section(ui: &mut Ui, ed: &mut ConnectionEditor, issues: &[Issue], th: &Theme, show: bool) -> bool {
    let mut changed = false;
    let t = &mut ed.conn.tls;
    changed |= ui.checkbox(&mut t.enabled, RichText::new("Use TLS protocol").strong()).changed();
    if ed.conn.kind == ConnectionType::Srv && !t.enabled {
        ui.label(RichText::new("ℹ mongodb+srv enables TLS by default.").small().color(th.text_weak));
    }
    ui.add_space(6.0);
    ui.add_enabled_ui(t.enabled, |ui| {
        grid(ui, "g-tls", |ui| {
            ui.label("CA certificate");
            ui.vertical(|ui| {
                changed |= file_picker(ui, &mut t.ca_file, "system trust store if empty", &[("Certificates", &["pem", "crt", "cer"]), ("All files", &["*"])]);
                field_error(ui, issues, "ca", th, show);
            });
            ui.end_row();
            ui.label("Client certificate + key");
            ui.vertical(|ui| {
                changed |= file_picker(ui, &mut t.cert_key_file, "PEM file with certificate and private key", &[("PEM", &["pem"]), ("All files", &["*"])]);
                field_error(ui, issues, "cert", th, show);
            });
            ui.end_row();
            ui.label("Key password");
            let mut show_pw = false;
            password_field(ui, &mut t.cert_key_password, &mut show_pw, !t.cert_key_file.is_empty(), "if the key is encrypted");
            ui.end_row();
            ui.label("Validation");
            ui.vertical(|ui| {
                changed |= ui.checkbox(&mut t.allow_invalid_certificates, "Allow invalid certificates (self-signed)").changed();
                changed |= ui.checkbox(&mut t.allow_invalid_hostnames, "Allow invalid hostnames").changed();
                if t.allow_invalid_certificates || t.allow_invalid_hostnames {
                    ui.label(RichText::new("⚠ Disabling validation exposes the connection to interception.").small().color(th.warning));
                }
            });
            ui.end_row();
        });
    });
    changed
}

fn advanced_section(ui: &mut Ui, ed: &mut ConnectionEditor, issues: &[Issue], th: &Theme, show: bool) -> bool {
    let mut changed = false;
    grid(ui, "g-adv", |ui| {
        ui.label("Connect timeout");
        changed |= ui.add(egui::DragValue::new(&mut ed.conn.connect_timeout_secs).range(1..=300).suffix(" s")).changed();
        ui.end_row();
        ui.label("Application name");
        changed |= ui.add(egui::TextEdit::singleline(&mut ed.conn.app_name).desired_width(240.0)).changed();
        ui.end_row();
        ui.label("Extra URI options");
        ui.vertical(|ui| {
            changed |= ui
                .add(egui::TextEdit::singleline(&mut ed.conn.extra_options).hint_text("compressors=zstd&retryWrites=false").desired_width(360.0).font(egui::TextStyle::Monospace))
                .changed();
            field_error(ui, issues, "extra", th, show);
        });
        ui.end_row();
    });
    ui.add_space(8.0);
    changed |= ui.checkbox(&mut ed.conn.use_visible_databases, RichText::new("Manually specify visible databases").strong()).changed();
    ui.label(RichText::new("For users without the listDatabases privilege: one database per line.").small().color(th.text_weak));
    ui.add_enabled_ui(ed.conn.use_visible_databases, |ui| {
        if ui.add(egui::TextEdit::multiline(&mut ed.visible_dbs_text).desired_rows(4).desired_width(360.0)).changed() {
            ed.conn.visible_databases = ed
                .visible_dbs_text
                .split(['\n', ','])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            changed = true;
        }
    });
    field_error(ui, issues, "visible", th, show);
    changed
}
