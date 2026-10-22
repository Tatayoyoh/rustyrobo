//! Application state, background events and the main window.
//!
//! UI code never blocks: it pushes [`Action`]s, which spawn tasks on the
//! tokio runtime; results come back as [`AppEvent`]s through a channel and
//! are applied at the start of the next frame.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bson::{Bson, Document, doc};
use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{Key, KeyboardShortcut, Modifiers};
use uuid::Uuid;

use crate::bsonfmt::FormatOptions;
use crate::mongo::{self, ConnectSecrets, Connection, StepReport};
use crate::settings::{AppSettings, ConnectionSettings, Orientation, ViewMode};
use crate::shell::{OutputKind, ShellHandle, ShellOptions, ShellReply};
use crate::theme::{self, Theme};
use crate::ui::connections::{ConnectionEditor, ConnectionList};
use crate::ui::dialogs::Dialog;
use crate::ui::query::{OutputPane, QueryTab};

pub enum Load<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(t) => Some(t),
            _ => None,
        }
    }
}

pub struct CollNode {
    pub name: String,
    /// "collection", "view" or "timeseries".
    pub kind: String,
    pub indexes: Load<Vec<Document>>,
}

pub struct DbNode {
    pub name: String,
    pub collections: Load<Vec<CollNode>>,
    pub functions: Load<Vec<Document>>,
    pub users: Load<Vec<Document>>,
    /// Collection names for autocompletion, kept in sync with `collections`.
    pub coll_names: Vec<String>,
}

impl DbNode {
    fn new(name: String) -> Self {
        DbNode { name, collections: Load::Idle, functions: Load::Idle, users: Load::Idle, coll_names: Vec::new() }
    }
}

pub enum ServerStatus {
    Connecting,
    Connected(Arc<Connection>),
    Failed(String),
}

pub struct Server {
    pub id: u64,
    pub settings: ConnectionSettings,
    pub secrets: ConnectSecrets,
    pub status: ServerStatus,
    pub dbs: Vec<DbNode>,
    pub dbs_loading: bool,
}

impl Server {
    pub fn conn(&self) -> Option<&Arc<Connection>> {
        match &self.status {
            ServerStatus::Connected(c) => Some(c),
            _ => None,
        }
    }

    pub fn db_mut(&mut self, name: &str) -> Option<&mut DbNode> {
        self.dbs.iter_mut().find(|d| d.name == name)
    }

    pub fn label(&self) -> String {
        match &self.status {
            ServerStatus::Connected(_) => format!("{} ({})", self.settings.name, self.dbs.len()),
            _ => self.settings.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Refresh {
    None,
    Databases,
    Collections(String),
    Indexes(String, String),
    Users(String),
    Functions(String),
    Tab(u64),
}

#[derive(Clone)]
pub enum Action {
    ShowConnections,
    ShowFavorites,
    /// Opens a favorite script in a new shell, on its connection (connecting
    /// first if needed), or on the active one.
    OpenFavorite(Uuid),
    /// Saves the script of a shell as a favorite, under that name.
    AddFavorite { tab: u64, name: String },
    NewConnection,
    EditConnection(Uuid),
    Connect(Uuid),
    ConnectWith(Uuid, ConnectSecrets),
    Disconnect(u64),
    RefreshServer(u64),
    LoadCollections(u64, String),
    LoadIndexes(u64, String, String),
    LoadUsers(u64, String),
    LoadFunctions(u64, String),
    LoadFields(u64, String, String),
    OpenShell { server: u64, db: String, script: String, execute: bool, title: String, cursor: Option<usize> },
    Execute(u64),
    Stop(u64),
    CloseTab(u64),
    DuplicateTab(u64),
    Page { tab: u64, out: usize, skip: u64, limit: u64 },
    Command { server: u64, db: String, cmd: Document, label: String, refresh: Refresh },
    Commands { server: u64, db: String, cmds: Vec<Document>, label: String, refresh: Refresh },
    Dialog(Dialog),
    OpenFile,
    SaveFile { save_as: bool },
    SaveSettings,
    ApplyTheme,
}

pub enum AppEvent {
    Step(u64, StepReport),
    Connected(u64, Result<Arc<Connection>, String>),
    Databases(u64, Result<Vec<String>, String>),
    Collections(u64, String, Result<Vec<(String, String)>, String>),
    Indexes(u64, String, String, Result<Vec<Document>, String>),
    Users(u64, String, Result<Vec<Document>, String>),
    Functions(u64, String, Result<Vec<Document>, String>),
    Fields(u64, String, String, Vec<String>),
    Shell(u64, ShellReply),
    Page(u64, usize, u64, Result<Vec<Bson>, String>),
    CommandDone { server: u64, label: String, result: Result<Document, String>, refresh: Refresh },
    Test(u64, StepReport),
    TestDone(u64, Result<String, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

pub struct LogEntry {
    pub at: chrono::DateTime<chrono::Local>,
    pub level: LogLevel,
    pub text: String,
}

pub struct App {
    pub settings: AppSettings,
    pub rt: tokio::runtime::Runtime,
    pub tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
    pub ctx: egui::Context,
    pub servers: Vec<Server>,
    pub tabs: Vec<QueryTab>,
    pub active_tab: usize,
    pub logs: VecDeque<LogEntry>,
    pub actions: Vec<Action>,
    pub conn_list: Option<ConnectionList>,
    pub favorites: Option<crate::ui::favorites::FavoritesManager>,
    /// Favorite to open once its connection is established.
    pending_favorite: Option<Uuid>,
    pub conn_editor: Option<ConnectionEditor>,
    pub dialog: Option<Dialog>,
    pub toast: Option<(String, Instant, LogLevel)>,
    next_id: u64,
    theme_name: String,
    system_dark: bool,
    pub theme: Theme,
    fullscreen: bool,
    pub autotest: Option<crate::autotest::AutoTest>,
    pub logo: Option<egui::TextureHandle>,
    /// Sampled field names for query completion: (server, db) → collection → fields.
    pub fields: std::collections::HashMap<(u64, String), std::collections::HashMap<String, Vec<String>>>,
}

pub const FIND_LIMIT_STATUS: Duration = Duration::from_secs(4);

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings = AppSettings::load();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("rustyrobo-io")
            .build()
            .expect("tokio runtime");
        let (tx, rx) = unbounded();
        let system_dark = cc.egui_ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(true);
        let theme = *theme::resolve(&settings.theme, system_dark);
        theme::apply(&cc.egui_ctx, &theme, settings.font_size);
        let mut app = App {
            theme_name: settings.theme.clone(),
            settings,
            rt,
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
            servers: Vec::new(),
            tabs: Vec::new(),
            active_tab: 0,
            logs: VecDeque::new(),
            actions: Vec::new(),
            conn_list: None,
            favorites: None,
            pending_favorite: None,
            conn_editor: None,
            dialog: None,
            toast: None,
            next_id: 1,
            system_dark,
            theme,
            fullscreen: false,
            autotest: crate::autotest::AutoTest::from_env(),
            logo: crate::assets::logo_texture(&cc.egui_ctx),
            fields: Default::default(),
        };
        app.log(LogLevel::Info, format!("Rustyrobo {} started", env!("CARGO_PKG_VERSION")));
        // Like Robo 3T: open the connection manager at startup.
        app.conn_list = Some(ConnectionList::new(&app.settings));
        app
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn fmt(&self) -> FormatOptions {
        FormatOptions { tz: self.settings.time_zone, uuid: self.settings.uuid_encoding }
    }

    pub fn log(&mut self, level: LogLevel, text: impl Into<String>) {
        let text = text.into();
        if level != LogLevel::Info {
            self.toast = Some((text.lines().next().unwrap_or_default().to_string(), Instant::now(), level));
        }
        self.logs.push_back(LogEntry { at: chrono::Local::now(), level, text });
        while self.logs.len() > 2000 {
            self.logs.pop_front();
        }
    }

    pub fn server(&self, id: u64) -> Option<&Server> {
        self.servers.iter().find(|s| s.id == id)
    }

    fn server_mut(&mut self, id: u64) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.id == id)
    }

    pub fn tab_mut(&mut self, id: u64) -> Option<&mut QueryTab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = AppEvent> + Send + 'static,
    {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.rt.spawn(async move {
            let ev = fut.await;
            let _ = tx.send(ev);
            ctx.request_repaint();
        });
    }

    // ------------------------------------------------------------ events

    fn poll_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            self.handle_event(ev);
        }
    }

    fn handle_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Step(server, step) => {
                let name = self.server(server).map(|s| s.settings.name.clone()).unwrap_or_default();
                let level = if step.ok { LogLevel::Info } else { LogLevel::Error };
                self.log(level, format!("[{name}] {}: {} ({} ms)", step.kind.label(), step.message, step.millis));
            }
            AppEvent::Connected(id, res) => {
                let Some(s) = self.server_mut(id) else { return };
                match res {
                    Ok(conn) => {
                        s.dbs = conn.databases.iter().cloned().map(DbNode::new).collect();
                        s.status = ServerStatus::Connected(conn);
                        let conn_id = s.settings.id;
                        let default_db = s.settings.default_database.clone();
                        if let Some(c) = self.settings.connections.iter_mut().find(|c| c.id == conn_id) {
                            c.last_connected = Some(chrono::Utc::now());
                        }
                        self.actions.push(Action::SaveSettings);
                        if !default_db.is_empty() {
                            self.actions.push(Action::LoadCollections(id, default_db));
                        }
                        let waiting = self.pending_favorite.and_then(|f| self.settings.favorites.iter().find(|x| x.id == f));
                        if let Some(f) = waiting
                            && f.connection == Some(conn_id)
                        {
                            self.actions.push(Action::OpenFavorite(f.id));
                            self.pending_favorite = None;
                        }
                    }
                    Err(e) => {
                        s.status = ServerStatus::Failed(e.clone());
                        let name = s.settings.name.clone();
                        self.pending_favorite = None;
                        self.log(LogLevel::Error, format!("Cannot connect to {name}: {e}"));
                        self.dialog = Some(Dialog::message("Cannot connect", format!("Cannot connect to the MongoDB at {name}.\n\nError:\n{e}")));
                    }
                }
            }
            AppEvent::Databases(id, res) => {
                let Some(s) = self.server_mut(id) else { return };
                s.dbs_loading = false;
                match res {
                    Ok(names) => {
                        // Keep loaded subtrees for databases that still exist.
                        let mut old: Vec<DbNode> = std::mem::take(&mut s.dbs);
                        s.dbs = names
                            .into_iter()
                            .map(|n| match old.iter().position(|d| d.name == n) {
                                Some(i) => old.swap_remove(i),
                                None => DbNode::new(n),
                            })
                            .collect();
                    }
                    Err(e) => self.log(LogLevel::Error, format!("listDatabases: {e}")),
                }
            }
            AppEvent::Collections(id, db, res) => {
                let Some(node) = self.server_mut(id).and_then(|s| s.db_mut(&db)) else { return };
                node.collections = match res {
                    Ok(list) => {
                        let mut old = match std::mem::replace(&mut node.collections, Load::Loading) {
                            Load::Ready(v) => v,
                            _ => Vec::new(),
                        };
                        Load::Ready(
                            list.into_iter()
                                .map(|(name, kind)| match old.iter().position(|c| c.name == name) {
                                    Some(i) => {
                                        let mut c = old.swap_remove(i);
                                        c.kind = kind;
                                        c
                                    }
                                    None => CollNode { name, kind, indexes: Load::Idle },
                                })
                                .collect(),
                        )
                    }
                    Err(e) => Load::Failed(e),
                };
                node.coll_names = node.collections.ready().map(|c| c.iter().map(|c| c.name.clone()).collect()).unwrap_or_default();
            }
            AppEvent::Indexes(id, db, coll, res) => {
                if let Some(c) = self
                    .server_mut(id)
                    .and_then(|s| s.db_mut(&db))
                    .and_then(|d| match &mut d.collections {
                        Load::Ready(v) => v.iter_mut().find(|c| c.name == coll),
                        _ => None,
                    })
                {
                    c.indexes = match res {
                        Ok(v) => Load::Ready(v),
                        Err(e) => Load::Failed(e),
                    };
                }
            }
            AppEvent::Users(id, db, res) => {
                if let Some(d) = self.server_mut(id).and_then(|s| s.db_mut(&db)) {
                    d.users = match res {
                        Ok(v) => Load::Ready(v),
                        Err(e) => Load::Failed(e),
                    };
                }
            }
            AppEvent::Functions(id, db, res) => {
                if let Some(d) = self.server_mut(id).and_then(|s| s.db_mut(&db)) {
                    d.functions = match res {
                        Ok(v) => Load::Ready(v),
                        Err(e) => Load::Failed(e),
                    };
                }
            }
            AppEvent::Fields(server, db, coll, list) => {
                self.fields.entry((server, db)).or_default().insert(coll, list);
            }
            AppEvent::Shell(tab_id, reply) => {
                let mode = self.settings.view_mode;
                let batch = self.settings.batch_size as u64;
                let auto_expand = self.settings.auto_expand;
                let mut errors = Vec::new();
                if let Some(tab) = self.tab_mut(tab_id) {
                    if tab.pending != Some(reply.request) {
                        return;
                    }
                    tab.pending = None;
                    tab.running = false;
                    tab.elapsed = Some(reply.elapsed);
                    if !reply.current_db.is_empty() {
                        tab.db = reply.current_db.clone();
                    }
                    tab.outputs = reply
                        .outputs
                        .into_iter()
                        .map(|o| {
                            if let OutputKind::Error(e) = &o.kind {
                                errors.push(e.clone());
                            }
                            OutputPane::new(o, mode, batch, auto_expand)
                        })
                        .collect();
                    tab.result_tab = 0;
                    if tab.outputs.is_empty() {
                        tab.notice = Some("Script executed successfully, but there are no results to show.".into());
                    } else {
                        tab.notice = None;
                    }
                }
                for e in errors {
                    self.log(LogLevel::Error, e);
                }
            }
            AppEvent::Page(tab_id, out, skip, res) => {
                let mut err = None;
                if let Some(pane) = self.tab_mut(tab_id).and_then(|t| t.outputs.get_mut(out)) {
                    pane.loading = false;
                    match res {
                        Ok(docs) => pane.set_page(docs, skip),
                        Err(e) => err = Some(e),
                    }
                }
                if let Some(e) = err {
                    self.log(LogLevel::Error, e);
                }
            }
            AppEvent::CommandDone { server, label, result, refresh } => {
                match result {
                    Ok(_) => self.log(LogLevel::Info, format!("{label}: done")),
                    Err(e) => {
                        self.log(LogLevel::Error, format!("{label}: {e}"));
                        self.dialog = Some(Dialog::message(&label, format!("Operation failed:\n\n{e}")));
                    }
                }
                self.refresh(server, refresh);
            }
            AppEvent::Test(token, step) => {
                if let Some(ed) = &mut self.conn_editor
                    && ed.test_token == token
                {
                    ed.test_steps.push(step);
                }
            }
            AppEvent::TestDone(token, res) => {
                if let Some(ed) = &mut self.conn_editor
                    && ed.test_token == token
                {
                    ed.testing = false;
                    ed.test_result = Some(res);
                }
            }
        }
    }

    fn refresh(&mut self, server: u64, r: Refresh) {
        match r {
            Refresh::None => {}
            Refresh::Databases => self.actions.push(Action::RefreshServer(server)),
            Refresh::Collections(db) => self.actions.push(Action::LoadCollections(server, db)),
            Refresh::Indexes(db, coll) => self.actions.push(Action::LoadIndexes(server, db, coll)),
            Refresh::Users(db) => self.actions.push(Action::LoadUsers(server, db)),
            Refresh::Functions(db) => self.actions.push(Action::LoadFunctions(server, db)),
            Refresh::Tab(t) => self.actions.push(Action::Execute(t)),
        }
    }

    // ------------------------------------------------------------ actions

    fn process_actions(&mut self) {
        let mut guard = 0;
        while !self.actions.is_empty() && guard < 64 {
            guard += 1;
            let actions = std::mem::take(&mut self.actions);
            for a in actions {
                self.run_action(a);
            }
        }
    }

    fn run_action(&mut self, action: Action) {
        match action {
            Action::ShowConnections => self.conn_list = Some(ConnectionList::new(&self.settings)),
            Action::ShowFavorites => {
                let linked = self.tabs.get(self.active_tab).and_then(|t| t.favorite);
                self.favorites = Some(crate::ui::favorites::FavoritesManager::new(linked));
            }
            Action::OpenFavorite(id) => {
                let Some(fav) = self.settings.favorites.iter().find(|f| f.id == id).cloned() else { return };
                let target = fav.connection.filter(|&c| self.settings.connection(c).is_some());
                let server = match target {
                    Some(c) => {
                        let Some(s) = self.servers.iter().find(|s| s.settings.id == c) else {
                            // Not connected yet: open once connected.
                            self.pending_favorite = Some(id);
                            self.actions.push(Action::Connect(c));
                            return;
                        };
                        match s.status {
                            ServerStatus::Connected(_) => s.id,
                            ServerStatus::Connecting => {
                                self.pending_favorite = Some(id);
                                return;
                            }
                            ServerStatus::Failed(_) => {
                                self.pending_favorite = Some(id);
                                self.actions.push(Action::Connect(c));
                                return;
                            }
                        }
                    }
                    None => {
                        // The active shell's connection, else the first connected one.
                        let active = self.tabs.get(self.active_tab).map(|t| t.server).filter(|&s| self.server(s).is_some_and(|s| s.conn().is_some()));
                        let Some(server) = active.or_else(|| self.servers.iter().find(|s| s.conn().is_some()).map(|s| s.id)) else {
                            self.log(LogLevel::Warn, format!("Connect to a server to load \"{}\"", fav.name));
                            return;
                        };
                        server
                    }
                };
                let n = self.tabs.len();
                self.open_shell(server, fav.db, fav.script, false, fav.name, None);
                if self.tabs.len() > n
                    && let Some(t) = self.tabs.last_mut()
                {
                    t.favorite = Some(id);
                }
            }
            Action::NewConnection => self.conn_editor = Some(ConnectionEditor::new(ConnectionSettings::default(), true)),
            Action::EditConnection(id) => {
                if let Some(c) = self.settings.connection(id) {
                    self.conn_editor = Some(ConnectionEditor::new(c.clone(), false));
                }
            }
            Action::Connect(id) => {
                let Some(c) = self.settings.connection(id).cloned() else { return };
                if c.credentials.ask_password || c.ssh.ask_password {
                    self.dialog = Some(Dialog::password_prompt(&c));
                } else {
                    self.connect(c, ConnectSecrets::default());
                }
            }
            Action::ConnectWith(id, secrets) => {
                if let Some(c) = self.settings.connection(id).cloned() {
                    self.connect(c, secrets);
                }
            }
            Action::Disconnect(id) => {
                if let Some(pos) = self.servers.iter().position(|s| s.id == id) {
                    let s = self.servers.remove(pos);
                    if let Some(c) = s.conn() {
                        let _guard = self.rt.enter();
                        c.shutdown();
                    }
                    self.tabs.retain(|t| t.server != id);
                    self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));
                    self.log(LogLevel::Info, format!("Disconnected from {}", s.settings.name));
                }
            }
            Action::RefreshServer(id) => {
                let Some(s) = self.server_mut(id) else { return };
                let Some(conn) = s.conn().cloned() else {
                    // Retry a failed connection.
                    if matches!(s.status, ServerStatus::Failed(_)) {
                        let (settings, secrets) = (s.settings.clone(), s.secrets.clone());
                        self.servers.retain(|x| x.id != id);
                        self.connect(settings, secrets);
                    }
                    return;
                };
                s.dbs_loading = true;
                let settings = s.settings.clone();
                self.spawn(async move {
                    AppEvent::Databases(id, conn.list_database_names(&settings).await.map_err(|e| mongo::error_message(&e)))
                });
            }
            Action::LoadCollections(id, db) => {
                let Some(s) = self.server_mut(id) else { return };
                let Some(conn) = s.conn().cloned() else { return };
                if let Some(d) = s.db_mut(&db) {
                    if matches!(d.collections, Load::Idle | Load::Failed(_)) {
                        d.collections = Load::Loading;
                    }
                } else {
                    s.dbs.push(DbNode::new(db.clone()));
                }
                self.spawn(async move {
                    let res = mongo::collect_cursor(
                        &conn,
                        &db,
                        doc! { "listCollections": 1, "nameOnly": false, "authorizedCollections": true, "cursor": {} },
                        100_000,
                        None,
                    )
                    .await
                    .map(|docs| {
                        let mut v: Vec<(String, String)> = docs
                            .iter()
                            .filter_map(|d| {
                                Some((d.get_str("name").ok()?.to_string(), d.get_str("type").unwrap_or("collection").to_string()))
                            })
                            .collect();
                        v.sort_by_key(|a| a.0.to_lowercase());
                        v
                    })
                    .map_err(|e| mongo::error_message(&e));
                    AppEvent::Collections(id, db, res)
                });
            }
            Action::LoadIndexes(id, db, coll) => {
                let Some(conn) = self.server(id).and_then(|s| s.conn().cloned()) else { return };
                if let Some(c) = self.server_mut(id).and_then(|s| s.db_mut(&db)).and_then(|d| match &mut d.collections {
                    Load::Ready(v) => v.iter_mut().find(|c| c.name == coll),
                    _ => None,
                }) && !matches!(c.indexes, Load::Ready(_))
                {
                    c.indexes = Load::Loading;
                }
                self.spawn(async move {
                    let res = mongo::collect_cursor(&conn, &db, doc! { "listIndexes": &coll, "cursor": {} }, 10_000, None)
                        .await
                        .map_err(|e| mongo::error_message(&e));
                    AppEvent::Indexes(id, db, coll, res)
                });
            }
            Action::LoadUsers(id, db) => {
                let Some(conn) = self.server(id).and_then(|s| s.conn().cloned()) else { return };
                if let Some(d) = self.server_mut(id).and_then(|s| s.db_mut(&db))
                    && !matches!(d.users, Load::Ready(_))
                {
                    d.users = Load::Loading;
                }
                self.spawn(async move {
                    let res = conn
                        .run_command(&db, doc! { "usersInfo": 1 }, None)
                        .await
                        .map(|d| {
                            d.get_array("users")
                                .map(|a| a.iter().filter_map(|u| u.as_document().cloned()).collect())
                                .unwrap_or_default()
                        })
                        .map_err(|e| mongo::error_message(&e));
                    AppEvent::Users(id, db, res)
                });
            }
            Action::LoadFunctions(id, db) => {
                let Some(conn) = self.server(id).and_then(|s| s.conn().cloned()) else { return };
                if let Some(d) = self.server_mut(id).and_then(|s| s.db_mut(&db))
                    && !matches!(d.functions, Load::Ready(_))
                {
                    d.functions = Load::Loading;
                }
                self.spawn(async move {
                    let res = mongo::collect_cursor(
                        &conn,
                        &db,
                        doc! { "find": "system.js", "filter": {}, "sort": { "_id": 1 } },
                        10_000,
                        None,
                    )
                    .await
                    .map_err(|e| mongo::error_message(&e));
                    AppEvent::Functions(id, db, res)
                });
            }
            Action::LoadFields(server, db, coll) => {
                let Some(conn) = self.server(server).and_then(|s| s.conn().cloned()) else { return };
                let map = self.fields.entry((server, db.clone())).or_default();
                if map.contains_key(&coll) {
                    return;
                }
                // Placeholder so the request is sent only once.
                map.insert(coll.clone(), Vec::new());
                self.spawn(async move {
                    let docs = mongo::collect_cursor(&conn, &db, field_names_command(&coll), 100, None)
                        .await
                        .map(|keys| keys.iter().map(keys_to_document).collect::<Vec<_>>())
                        .unwrap_or_default();
                    AppEvent::Fields(server, db, coll, sample_fields(&docs))
                });
            }
            Action::OpenShell { server, db, script, execute, title, cursor } => {
                self.open_shell(server, db, script, execute, title, cursor);
            }
            Action::Execute(tab_id) => self.execute(tab_id),
            Action::Stop(tab_id) => {
                if let Some(tab) = self.tab_mut(tab_id)
                    && let Some(sh) = &tab.shell
                {
                    sh.interrupt();
                }
            }
            Action::CloseTab(tab_id) => {
                if let Some(pos) = self.tabs.iter().position(|t| t.id == tab_id) {
                    let t = self.tabs.remove(pos);
                    if let Some(sh) = &t.shell {
                        sh.interrupt();
                    }
                    if self.active_tab >= pos && self.active_tab > 0 {
                        self.active_tab -= 1;
                    }
                }
            }
            Action::AddFavorite { tab, name } => {
                let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) else { return };
                let connection = self.servers.iter().find(|s| s.id == t.server).map(|s| s.settings.id);
                let f = crate::settings::FavoriteScript { name, connection, db: t.db.clone(), script: t.script.clone(), ..Default::default() };
                t.favorite = Some(f.id);
                self.settings.favorites.push(f);
                self.actions.push(Action::SaveSettings);
            }
            Action::DuplicateTab(tab_id) => {
                if let Some(t) = self.tabs.iter().find(|t| t.id == tab_id) {
                    let (server, db, script, title) = (t.server, t.db.clone(), t.script.clone(), t.title.clone());
                    self.open_shell(server, db, script, false, title, None);
                }
            }
            Action::Page { tab, out, skip, limit } => {
                let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) else { return };
                let server = t.server;
                let Some(pane) = t.outputs.get_mut(out) else { return };
                let OutputKind::Docs { spec: Some(spec), .. } = &pane.output.kind else { return };
                let spec = spec.clone();
                pane.loading = true;
                let Some(conn) = self.server(server).and_then(|s| s.conn().cloned()) else { return };
                self.spawn(async move {
                    let res = crate::shell::fetch_page(&conn, &spec, skip, limit).await.map_err(|e| mongo::error_message(&e));
                    AppEvent::Page(tab, out, skip, res)
                });
            }
            Action::Command { server, db, cmd, label, refresh } => {
                self.actions.push(Action::Commands { server, db, cmds: vec![cmd], label, refresh });
            }
            Action::Commands { server, db, cmds, label, refresh } => {
                let Some(conn) = self.server(server).and_then(|s| s.conn().cloned()) else { return };
                self.log(LogLevel::Info, format!("{label}…"));
                self.spawn(async move {
                    let mut last = Ok(Document::new());
                    for cmd in cmds {
                        last = conn.run_command(&db, cmd, None).await.map_err(|e| mongo::error_message(&e)).and_then(check_write);
                        if last.is_err() {
                            break;
                        }
                    }
                    AppEvent::CommandDone { server, label, result: last, refresh }
                });
            }
            Action::Dialog(d) => self.dialog = Some(d),
            Action::OpenFile => self.open_file(),
            Action::SaveFile { save_as } => self.save_file(save_as),
            Action::SaveSettings => {
                if let Err(e) = self.settings.save() {
                    self.log(LogLevel::Error, format!("Cannot save settings: {e}"));
                }
            }
            Action::ApplyTheme => {
                self.theme_name = self.settings.theme.clone();
                self.theme = *theme::resolve(&self.settings.theme, self.system_dark);
                theme::apply(&self.ctx, &self.theme, self.settings.font_size);
                self.actions.push(Action::SaveSettings);
            }
        }
    }

    fn connect(&mut self, settings: ConnectionSettings, secrets: ConnectSecrets) {
        let id = self.next_id();
        self.log(LogLevel::Info, format!("Connecting to {} ({})…", settings.name, settings.address_label()));
        self.servers.push(Server {
            id,
            settings: settings.clone(),
            secrets: secrets.clone(),
            status: ServerStatus::Connecting,
            dbs: Vec::new(),
            dbs_loading: false,
        });
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        self.spawn(async move {
            let res = Connection::open(&settings, &secrets, |step| {
                let _ = tx.send(AppEvent::Step(id, step));
                ctx.request_repaint();
            })
            .await;
            AppEvent::Connected(id, res.map(Arc::new).map_err(|e| mongo::error_message(&e)))
        });
    }

    pub fn open_shell(&mut self, server: u64, db: String, script: String, execute: bool, title: String, cursor: Option<usize>) {
        let Some(s) = self.server(server) else { return };
        let Some(conn) = s.conn().cloned() else {
            self.log(LogLevel::Warn, "Server is not connected");
            return;
        };
        let db = if db.is_empty() {
            let d = &s.settings.default_database;
            if d.is_empty() { "test".to_string() } else { d.clone() }
        } else {
            db
        };
        let id = self.next_id();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let shell = ShellHandle::spawn(
            conn,
            self.rt.handle().clone(),
            db.clone(),
            ShellOptions {
                batch_size: self.settings.batch_size,
                timeout: Duration::from_secs(self.settings.shell_timeout_secs.max(1) as u64),
                load_mongorc: self.settings.load_mongorc,
            },
            move |reply| {
                let _ = tx.send(AppEvent::Shell(id, reply));
                ctx.request_repaint();
            },
        );
        let title = if title.is_empty() { tab_title(&script) } else { title };
        let mut tab = QueryTab::new(id, server, db, title, script, shell);
        tab.cursor_hint = cursor;
        tab.line_numbers = self.settings.line_numbers;
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        if let Some(coll) = crate::ui::editor::statement_collection(&self.tabs[self.tabs.len() - 1].script) {
            self.actions.push(Action::LoadFields(server, self.tabs[self.tabs.len() - 1].db.clone(), coll));
        }
        if execute && self.settings.auto_exec {
            self.execute(id);
        }
    }

    fn execute(&mut self, tab_id: u64) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else { return };
        let script = tab.selection.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| tab.script.clone());
        if script.trim().is_empty() {
            return;
        }
        let Some(shell) = &mut tab.shell else { return };
        tab.pending = Some(shell.execute(script.clone()));
        tab.running = true;
        tab.started = Some(Instant::now());
        tab.notice = None;
        let first = script.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().to_string();
        let server = tab.server;
        let name = self.server(server).map(|s| s.settings.name.clone()).unwrap_or_default();
        self.log(LogLevel::Info, format!("[{name}] {first}"));
    }

    fn open_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JavaScript", &["js"])
            .add_filter("All files", &["*"])
            .pick_file()
        else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let target = self
                    .tabs
                    .get(self.active_tab)
                    .map(|t| (t.server, t.db.clone()))
                    .or_else(|| self.servers.iter().find(|s| s.conn().is_some()).map(|s| (s.id, String::new())));
                match target {
                    Some((server, db)) => {
                        let title = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                        self.open_shell(server, db, text, false, title, None);
                        if let Some(t) = self.tabs.last_mut() {
                            t.file = Some(path);
                        }
                    }
                    None => self.log(LogLevel::Warn, "Connect to a server before opening a script"),
                }
            }
            Err(e) => self.log(LogLevel::Error, format!("Cannot open {}: {e}", path.display())),
        }
    }

    fn save_file(&mut self, save_as: bool) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else { return };
        let path: Option<PathBuf> = match (&tab.file, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new()
                .add_filter("JavaScript", &["js"])
                .set_file_name(format!("{}.js", sanitize(&tab.title)))
                .save_file(),
        };
        let Some(path) = path else { return };
        let res = std::fs::write(&path, &tab.script);
        match res {
            Ok(()) => {
                tab.title = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                tab.file = Some(path.clone());
                tab.dirty = false;
                self.log(LogLevel::Info, format!("Saved {}", path.display()));
            }
            Err(e) => self.log(LogLevel::Error, format!("Cannot save {}: {e}", path.display())),
        }
    }

    // ------------------------------------------------------------ shortcuts

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        let tab_id = self.tabs.get(self.active_tab).map(|t| t.id);
        let modal_open = self.conn_list.is_some() || self.conn_editor.is_some() || self.dialog.is_some() || self.favorites.is_some();

        if pressed(cmd, Key::N) {
            self.actions.push(Action::ShowConnections);
        }
        if modal_open {
            return;
        }
        if (pressed(Modifiers::NONE, Key::F5) || pressed(cmd, Key::Enter))
            && let Some(t) = tab_id {
                self.actions.push(Action::Execute(t));
            }
        if pressed(Modifiers::NONE, Key::F6)
            && let Some(t) = tab_id {
                self.actions.push(Action::Stop(t));
            }
        if pressed(cmd, Key::R)
            && let Some(t) = tab_id {
                self.actions.push(Action::Execute(t));
            }
        if pressed(cmd, Key::O) {
            self.actions.push(Action::OpenFile);
        }
        if pressed(shift_cmd, Key::S) {
            self.actions.push(Action::SaveFile { save_as: true });
        } else if pressed(cmd, Key::S) {
            self.actions.push(Action::SaveFile { save_as: false });
        }
        if pressed(cmd, Key::W)
            && let Some(t) = tab_id {
                self.actions.push(Action::CloseTab(t));
            }
        if pressed(shift_cmd, Key::T) {
            if let Some(t) = tab_id {
                self.actions.push(Action::DuplicateTab(t));
            }
        } else if pressed(cmd, Key::T)
            && let Some(t) = self.tabs.get(self.active_tab) {
                let (server, db) = (t.server, t.db.clone());
                self.actions.push(Action::OpenShell { server, db, script: String::new(), execute: false, title: "New Shell".into(), cursor: None });
            }
        if pressed(cmd, Key::E) {
            self.settings.show_explorer = !self.settings.show_explorer;
            self.actions.push(Action::SaveSettings);
        }
        if pressed(cmd, Key::L) {
            self.settings.show_logs = !self.settings.show_logs;
            self.actions.push(Action::SaveSettings);
        }
        if pressed(Modifiers::NONE, Key::F10) {
            self.settings.orientation = match self.settings.orientation {
                Orientation::Vertical => Orientation::Horizontal,
                Orientation::Horizontal => Orientation::Vertical,
            };
            self.actions.push(Action::SaveSettings);
        }
        if pressed(Modifiers::NONE, Key::F11) {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        }
        if pressed(cmd, Key::M) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        if (pressed(Modifiers::CTRL, Key::Tab) || pressed(cmd, Key::PageDown))
            && !self.tabs.is_empty() {
                self.active_tab = (self.active_tab + 1) % self.tabs.len();
            }
        if (pressed(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab) || pressed(cmd, Key::PageUp))
            && !self.tabs.is_empty() {
                self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
            }
        for (k, mode) in [(Key::F2, ViewMode::Tree), (Key::F3, ViewMode::Table), (Key::F4, ViewMode::Text)] {
            if pressed(Modifiers::NONE, k)
                && let Some(t) = self.tabs.get_mut(self.active_tab)
            {
                for o in &mut t.outputs {
                    o.mode = mode;
                }
            }
        }
        if !self.settings.disable_connection_shortcuts {
            let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
            for (i, k) in keys.iter().enumerate() {
                if pressed(Modifiers::ALT, *k)
                    && let Some(c) = self.settings.connections.get(i)
                {
                    self.actions.push(Action::Connect(c.id));
                }
            }
        }
        if pressed(Modifiers::CTRL | Modifiers::SHIFT, Key::Q) || pressed(cmd, Key::Q) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn sync_system_theme(&mut self, ctx: &egui::Context) {
        let dark = ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(self.system_dark);
        if dark != self.system_dark || self.theme_name != self.settings.theme {
            self.system_dark = dark;
            self.actions.push(Action::ApplyTheme);
        }
    }
}

/// Raises driver-level success responses that contain write errors.
fn check_write(d: Document) -> Result<Document, String> {
    if let Ok(errs) = d.get_array("writeErrors")
        && let Some(e) = errs.first().and_then(|e| e.as_document())
    {
        return Err(format!(
            "{} (code {})",
            e.get_str("errmsg").unwrap_or("write error"),
            e.get("code").and_then(mongo::bson_as_i64).unwrap_or(0)
        ));
    }
    if let Ok(wce) = d.get_document("writeConcernError") {
        return Err(wce.get_str("errmsg").unwrap_or("write concern error").to_string());
    }
    Ok(d)
}

/// Aggregation returning only the field names (two levels) of 100 sampled
/// documents: the server does the work, so large documents never travel.
pub fn field_names_command(coll: &str) -> Document {
    doc! {
        "aggregate": coll,
        "pipeline": [
            { "$sample": { "size": 100 } },
            { "$project": { "_id": 0, "f": { "$map": {
                "input": { "$objectToArray": "$$ROOT" },
                "as": "e",
                "in": {
                    "k": "$$e.k",
                    "s": { "$cond": [
                        { "$eq": [{ "$type": "$$e.v" }, "object"] },
                        { "$map": { "input": { "$objectToArray": "$$e.v" }, "as": "x", "in": "$$x.k" } },
                        []
                    ] }
                }
            } } } }
        ],
        "cursor": {}
    }
}

/// Rebuilds a key-only document (`{ name: null, address: { city: null } }`)
/// from one result of [`field_names_command`].
pub fn keys_to_document(d: &Document) -> Document {
    let mut out = Document::new();
    for e in d.get_array("f").into_iter().flatten().filter_map(|e| e.as_document()) {
        let Ok(k) = e.get_str("k") else { continue };
        let subs: Document = e
            .get_array("s")
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str())
            .map(|s| (s.to_string(), Bson::Null))
            .collect();
        out.insert(k, if subs.is_empty() { Bson::Null } else { Bson::Document(subs) });
    }
    out
}

/// Field paths (dotted, up to 3 levels) seen in sample documents, most
/// frequent first.
pub fn sample_fields(docs: &[Document]) -> Vec<String> {
    fn walk(d: &Document, prefix: &str, depth: usize, counts: &mut std::collections::HashMap<String, (usize, usize)>, order: &mut usize) {
        for (k, v) in d {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            let e = counts.entry(path.clone()).or_insert_with(|| {
                *order += 1;
                (0, *order)
            });
            e.0 += 1;
            if depth < 3
                && let Bson::Document(sub) = v
            {
                walk(sub, &path, depth + 1, counts, order);
            }
        }
    }
    let mut counts = std::collections::HashMap::new();
    let mut order = 0;
    for d in docs {
        walk(d, "", 1, &mut counts, &mut order);
    }
    let mut v: Vec<(String, (usize, usize))> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)));
    v.into_iter().take(500).map(|(k, _)| k).collect()
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

pub fn tab_title(script: &str) -> String {
    let first = script.lines().find(|l| !l.trim().is_empty()).unwrap_or("New Shell").trim();
    let t: String = first.chars().take(28).collect();
    if first.chars().count() > 28 { format!("{t}…") } else { t }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_events();
        self.sync_system_theme(&ctx);
        self.shortcuts(&ctx);
        crate::ui::main_window(self, ui);
        crate::ui::connections::show(self, &ctx);
        crate::ui::favorites::show(self, &ctx);
        crate::ui::dialogs::show(self, &ctx);
        crate::autotest::tick(self, &ctx);
        self.process_actions();
        if self.tabs.iter().any(|t| t.running) || self.servers.iter().any(|s| matches!(s.status, ServerStatus::Connecting)) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn on_exit(&mut self) {
        let _ = self.settings.save();
        let _guard = self.rt.enter();
        for s in &self.servers {
            if let Some(c) = s.conn() {
                c.shutdown();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_file_names() {
        assert_eq!(tab_title("\n\n  db.users.find()  \n"), "db.users.find()");
        assert_eq!(tab_title(""), "New Shell");
        assert_eq!(tab_title("db.getCollection('a_long_collection_name').find({})"), "db.getCollection('a_long_col…");
        assert_eq!(sanitize("a b/c:d"), "a_b_c_d");
    }

    #[test]
    fn write_errors_are_detected() {
        assert!(check_write(doc! { "ok": 1, "n": 1 }).is_ok());
        let e = check_write(doc! { "ok": 1, "writeErrors": [{ "index": 0, "code": 11000, "errmsg": "E11000 duplicate key" }] }).unwrap_err();
        assert_eq!(e, "E11000 duplicate key (code 11000)");
        let e = check_write(doc! { "ok": 1, "writeConcernError": { "errmsg": "waiting for replication timed out" } }).unwrap_err();
        assert!(e.contains("replication"));
    }

    #[test]
    fn key_only_documents() {
        let d = doc! { "f": [{ "k": "name", "s": [] }, { "k": "address", "s": ["city", "zip"] }] };
        assert_eq!(keys_to_document(&d), doc! { "name": null, "address": { "city": null, "zip": null } });
        let f = sample_fields(&[keys_to_document(&d)]);
        assert_eq!(f, vec!["name", "address", "address.city", "address.zip"]);
        assert_eq!(field_names_command("c").get_str("aggregate").unwrap(), "c");
    }

    #[test]
    fn field_sampling() {
        let docs = vec![
            doc! { "_id": 1, "name": "a", "address": { "city": "x", "geo": { "lat": 1, "deep": { "too": 1 } } } },
            doc! { "_id": 2, "name": "b", "tags": [1] },
        ];
        let f = sample_fields(&docs);
        assert_eq!(&f[..2], &["_id".to_string(), "name".to_string()]);
        assert!(f.contains(&"address.city".to_string()));
        assert!(f.contains(&"address.geo.lat".to_string()));
        assert!(f.contains(&"address.geo.deep".to_string()));
        assert!(!f.contains(&"address.geo.deep.too".to_string()), "limited to 3 levels");
        assert!(sample_fields(&[]).is_empty());
    }
}
