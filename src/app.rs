//! Application state, background events and the main window.
//!
//! UI code never blocks: it pushes [`Action`]s, which spawn tasks on the
//! tokio runtime; results come back as [`AppEvent`]s through a channel and
//! are applied at the start of the next frame.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bson::{Bson, Document, doc};
use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{FontDefinitions, Key, KeyboardShortcut, Modifiers};
use uuid::Uuid;

use crate::bsonfmt::FormatOptions;
use crate::fonts::FontChoice;
use crate::history::{History, HistoryEntry};
use crate::mongo::{self, ConnectSecrets, Connection, StepReport};
use crate::settings::{AppSettings, ConnectionSettings, ExportDefaults, Orientation, ViewMode};
use crate::shell::{DropVars, OutputKind, ShellHandle, ShellOptions, ShellReply};
use crate::theme::{self, Theme};
use crate::dump::{DumpOptions, RestoreOptions};
use crate::transfer::{ExportOptions, ImportOptions, Progress};
use crate::ui::connections::{ConnectionEditor, ConnectionList};
use crate::ui::dialogs::Dialog;
use crate::ui::query::{OutputPane, QueryTab, TabVar};

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
    /// The page shown by a result pane: `out` of tab `tab`.
    Page { tab: u64, out: usize },
}

#[derive(Clone)]
pub enum Action {
    CheckForUpdate,
    InstallUpdate(Arc<crate::update::Release>),
    ShowConnections,
    ShowFavorites,
    ShowHistory,
    /// Opens a script of the history in a new shell, like a favorite.
    OpenHistory(Uuid),
    DeleteHistory(Uuid),
    SaveHistory,
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
    Export { server: u64, opts: Box<ExportOptions> },
    Import { server: u64, opts: Box<ImportOptions> },
    Dump { server: u64, opts: Box<DumpOptions> },
    Restore { server: u64, opts: Box<RestoreOptions> },
    CancelTransfer(u64),
    OpenFile,
    SaveFile { save_as: bool },
    SaveSettings,
    ApplyTheme,
    /// Like `ApplyTheme`, without saving the settings.
    PreviewTheme,
    ApplyFonts,
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
    /// Documents exported, imported, or read before an import, so far.
    TransferProgress(u64, Progress),
    /// End of an export or import: what it did, or why it stopped.
    /// `warning`: done, but not completely (documents or collections refused).
    TransferDone { id: u64, server: u64, result: Result<String, String>, warning: bool, refresh: Refresh },
    Test(u64, StepReport),
    TestDone(u64, Result<String, String>),
    Fonts(FontChoice, FontDefinitions),
    Update(Result<Option<crate::update::Release>, String>),
    UpdateProgress(u64, Option<u64>),
    UpdateInstalled(Result<(), String>),
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

/// Script to open in a new shell, on its connection once connected.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptToOpen {
    /// `None`: the active connection.
    pub connection: Option<Uuid>,
    pub db: String,
    pub script: String,
    /// Empty: from the script.
    pub title: String,
    pub favorite: Option<Uuid>,
}

/// An export or import running in the background.
pub struct Transfer {
    pub id: u64,
    pub label: String,
    pub done: Progress,
    pub cancel: Arc<AtomicBool>,
    /// What stopping it once it writes leaves, told before quitting.
    pub stop: &'static str,
}

/// Quitting while transfers run, once confirmed.
pub struct Quit {
    /// Since when the transfers stopped to quit are ending.
    pub stopping: Option<Instant>,
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
    pub history: History,
    pub history_window: Option<crate::ui::history::HistoryWindow>,
    pub transfers: Vec<Transfer>,
    /// Script to open once its connection is established.
    pending_script: Option<ScriptToOpen>,
    pub conn_editor: Option<ConnectionEditor>,
    pub dialog: Option<Dialog>,
    /// Asked before quitting while transfers run, on top of any dialog.
    pub quit: Option<Quit>,
    /// An update waits for the transfers to stop: started when quitting.
    pub restart_on_quit: bool,
    pub toast: Option<(String, Instant, LogLevel)>,
    next_id: u64,
    theme_name: String,
    /// `None` until the system fonts are loaded.
    fonts_applied: Option<FontChoice>,
    system_dark: bool,
    pub theme: Theme,
    /// Theme shown instead of the one in the settings, while it is hovered in the menu.
    pub theme_preview: Option<&'static str>,
    fullscreen: bool,
    pub autotest: Option<crate::autotest::AutoTest>,
    /// Newer version found on GitHub.
    pub update: Option<Arc<crate::update::Release>>,
    pub install: crate::update::Install,
    /// The program an update replaces and restarts. Read at startup: once
    /// replaced, the system may no longer tell the path of the running one.
    pub update_target: Option<PathBuf>,
    pub logo: Option<egui::TextureHandle>,
    /// Sampled field names for query completion: (server, db) → collection → fields.
    pub fields: std::collections::HashMap<(u64, String), std::collections::HashMap<String, Vec<String>>>,
}

pub const FIND_LIMIT_STATUS: Duration = Duration::from_secs(4);
/// Longest wait, when quitting, for the stopped transfers to end.
pub const QUIT_WAIT: Duration = Duration::from_secs(10);

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
        cc.egui_ctx.set_fonts(crate::fonts::bundled());
        theme::apply(&cc.egui_ctx, &theme, settings.font_size);
        let mut app = App {
            theme_name: settings.theme.clone(),
            fonts_applied: None,
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
            history: History::load(),
            history_window: None,
            transfers: Vec::new(),
            pending_script: None,
            conn_editor: None,
            dialog: None,
            quit: None,
            restart_on_quit: false,
            toast: None,
            next_id: 1,
            system_dark,
            theme,
            theme_preview: None,
            fullscreen: false,
            autotest: crate::autotest::AutoTest::from_env(),
            update: None,
            install: Default::default(),
            update_target: std::env::current_exe().and_then(std::fs::canonicalize).ok(),
            logo: crate::assets::logo_texture(&cc.egui_ctx),
            fields: Default::default(),
        };
        app.log(LogLevel::Info, format!("Rustyrobo {} started", env!("CARGO_PKG_VERSION")));
        // Like Robo 3T: open the connection manager at startup.
        app.conn_list = Some(ConnectionList::new(&app.settings));
        app.load_fonts();
        app
    }

    /// The system fonts are scanned once, in the background.
    fn load_fonts(&self) {
        let choice = self.settings.fonts.clone();
        self.spawn(async move {
            let for_task = choice.clone();
            let fonts = tokio::task::spawn_blocking(move || crate::fonts::definitions(&for_task)).await;
            AppEvent::Fonts(choice, fonts.unwrap_or_else(|_| crate::fonts::bundled()))
        });
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

    /// Looks for a new version in the background. Being offline is no error
    /// worth a message in the status bar: it only goes to the logs. In a thread
    /// of its own, since dropping the runtime waits for its blocking tasks.
    pub fn check_for_update(&self) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(AppEvent::Update(crate::update::check()));
            ctx.request_repaint();
        });
    }

    /// Nothing comes back from an installation once cancelled.
    fn install_update(&mut self, release: Arc<crate::update::Release>) {
        let Some(exe) = self.update_target.clone() else { return };
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.install = crate::update::Install::Running { done: 0, total: None, cancel: Arc::clone(&cancel) };
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let send = |ev| {
                if !cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = tx.send(ev);
                    ctx.request_repaint();
                }
            };
            let result = crate::update::install(&release, &exe, &cancel, |done, total| send(AppEvent::UpdateProgress(done, total)));
            send(AppEvent::UpdateInstalled(result));
        });
    }

    /// When the update dialog closes.
    pub fn cancel_install(&mut self) {
        if let crate::update::Install::Running { cancel, .. } = &self.install {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.install = crate::update::Install::Idle;
    }

    /// Starts the new program and closes this one. While transfers run, the
    /// new one starts only once the user has chosen to stop them and quit.
    fn restart(&mut self) {
        if !self.transfers.is_empty() {
            self.restart_on_quit = true;
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        match self.start_new_program() {
            Ok(_) => self.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => self.install = crate::update::Install::Failed(format!("Updated, but could not restart: {e}. Restart Rustyrobo to use the new version.")),
        }
    }

    fn start_new_program(&self) -> Result<(), String> {
        let exe = self.update_target.as_ref().ok_or_else(|| "unknown path".to_string())?;
        std::process::Command::new(exe).args(std::env::args_os().skip(1)).spawn().map(|_| ()).map_err(|e| e.to_string())
    }

    // ------------------------------------------------------------ events

    fn poll_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            self.handle_event(ev);
        }
    }

    fn handle_event(&mut self, ev: AppEvent) {
        match ev {
            // Unless the check has been turned off in the meantime.
            AppEvent::Update(Ok(found)) => {
                if self.settings.check_updates {
                    self.update = found.map(Arc::new);
                }
            }
            AppEvent::Update(Err(e)) => self.log(LogLevel::Info, format!("Could not check for updates: {e}")),
            AppEvent::UpdateProgress(new_done, new_total) => {
                if let crate::update::Install::Running { done, total, .. } = &mut self.install {
                    (*done, *total) = (new_done, new_total);
                }
            }
            AppEvent::UpdateInstalled(Ok(())) => self.restart(),
            AppEvent::UpdateInstalled(Err(e)) => {
                self.log(LogLevel::Info, format!("Could not install the update: {e}"));
                self.install = crate::update::Install::Failed(e);
            }
            // Unless the choice has changed again in the meantime.
            AppEvent::Fonts(choice, fonts) => {
                if choice == self.settings.fonts && self.fonts_applied.as_ref() != Some(&choice) {
                    self.ctx.set_fonts(fonts);
                    self.fonts_applied = Some(choice);
                }
            }
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
                        if self.pending_script.as_ref().is_some_and(|p| p.connection == Some(conn_id))
                            && let Some(p) = self.pending_script.take()
                        {
                            self.open_script(p);
                        }
                    }
                    Err(e) => {
                        s.status = ServerStatus::Failed(e.clone());
                        let name = s.settings.name.clone();
                        self.pending_script = None;
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
                    // New variables are kept, the others keep their choice.
                    let vars = reply.vars.into_iter().map(|v| {
                        let keep = tab.vars.iter().find(|o| o.name == v.name).is_none_or(|o| o.keep);
                        TabVar { name: v.name, summary: v.summary, keep }
                    });
                    tab.vars = vars.collect();
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
            AppEvent::TransferProgress(id, n) => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.done = n;
                }
            }
            AppEvent::TransferDone { id, server, result, warning, refresh } => {
                let label = match self.transfers.iter().position(|t| t.id == id) {
                    Some(i) => self.transfers.remove(i).label,
                    None => return,
                };
                // Stopped to quit: in the logs only.
                let quitting = self.quit.as_ref().is_some_and(|q| q.stopping.is_some());
                match result {
                    // Not all of it: shown until read.
                    Ok(msg) if warning => {
                        self.log(LogLevel::Warn, format!("{label}: {msg}"));
                        if !quitting {
                            self.dialog = Some(Dialog::message(&label, format!("Done, but not completely:\n\n{msg}")));
                        }
                    }
                    Ok(msg) => {
                        self.log(LogLevel::Info, format!("{label}: {msg}"));
                        self.toast = Some((format!("{label}: {msg}"), Instant::now(), LogLevel::Info));
                    }
                    Err(e) => {
                        self.log(LogLevel::Error, format!("{label}: {e}"));
                        if !quitting {
                            self.dialog = Some(Dialog::message(&label, format!("Operation failed:\n\n{e}")));
                        }
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
            Refresh::Page { tab, out } => {
                if let Some(p) = self.tabs.iter().find(|t| t.id == tab).and_then(|t| t.outputs.get(out)) {
                    self.actions.push(Action::Page { tab, out, skip: p.skip, limit: p.page_size });
                }
            }
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
            Action::ShowHistory => self.history_window = Some(Default::default()),
            Action::DeleteHistory(id) => {
                self.history.remove(id);
                self.actions.push(Action::SaveHistory);
            }
            Action::SaveHistory => {
                if let Err(e) = self.history.save() {
                    self.log(LogLevel::Error, format!("Cannot save the query history: {e}"));
                }
            }
            Action::OpenFavorite(id) => {
                let Some(fav) = self.settings.favorites.iter().find(|f| f.id == id).cloned() else { return };
                self.open_script(ScriptToOpen { connection: fav.connection, db: fav.db, script: fav.script, title: fav.name, favorite: Some(id) });
            }
            Action::OpenHistory(id) => {
                let Some(e) = self.history.get(id).cloned() else { return };
                self.open_script(ScriptToOpen { connection: e.connection, db: e.db, script: e.script, title: String::new(), favorite: None });
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
            Action::Dialog(mut d) => {
                match &mut d {
                    Dialog::Export(f) => f.set_defaults(&self.settings.export),
                    Dialog::Dump(f) => f.set_last(&self.settings.last_dump),
                    Dialog::Restore(f) => f.set_last(&self.settings.last_dump),
                    _ => {}
                }
                self.dialog = Some(d);
            }
            Action::Export { server, opts } => self.export(server, *opts),
            Action::Import { server, opts } => self.import(server, *opts),
            Action::Dump { server, opts } => self.dump(server, *opts),
            Action::Restore { server, opts } => self.restore(server, *opts),
            Action::CancelTransfer(id) => {
                if let Some(t) = self.transfers.iter().find(|t| t.id == id) {
                    t.cancel.store(true, Ordering::Relaxed);
                }
            }
            Action::OpenFile => self.open_file(),
            Action::SaveFile { save_as } => self.save_file(save_as),
            Action::CheckForUpdate => self.check_for_update(),
            Action::InstallUpdate(release) => self.install_update(release),
            Action::SaveSettings => {
                if let Err(e) = self.settings.save() {
                    self.log(LogLevel::Error, format!("Cannot save settings: {e}"));
                }
            }
            Action::ApplyTheme => {
                self.update_theme();
                self.actions.push(Action::SaveSettings);
            }
            Action::PreviewTheme => self.update_theme(),
            Action::ApplyFonts => self.load_fonts(),
        }
    }

    /// Opens a script in a new shell: on its connection, connecting first if
    /// needed, or on the active one.
    fn open_script(&mut self, s: ScriptToOpen) {
        let target = s.connection.filter(|&c| self.settings.connection(c).is_some());
        let server = match target {
            Some(c) => {
                let Some(srv) = self.servers.iter().find(|x| x.settings.id == c) else {
                    // Not connected yet: open once connected.
                    self.pending_script = Some(s);
                    self.actions.push(Action::Connect(c));
                    return;
                };
                match srv.status {
                    ServerStatus::Connected(_) => srv.id,
                    ServerStatus::Connecting => {
                        self.pending_script = Some(s);
                        return;
                    }
                    ServerStatus::Failed(_) => {
                        self.pending_script = Some(s);
                        self.actions.push(Action::Connect(c));
                        return;
                    }
                }
            }
            None => {
                // The active shell's connection, else the first connected one.
                let active = self.tabs.get(self.active_tab).map(|t| t.server).filter(|&s| self.server(s).is_some_and(|s| s.conn().is_some()));
                let Some(server) = active.or_else(|| self.servers.iter().find(|s| s.conn().is_some()).map(|s| s.id)) else {
                    let name = if s.title.is_empty() { tab_title(&s.script) } else { s.title };
                    self.log(LogLevel::Warn, format!("Connect to a server to load \"{name}\""));
                    return;
                };
                server
            }
        };
        let n = self.tabs.len();
        self.open_shell(server, s.db, s.script, false, s.title, None);
        if self.tabs.len() > n
            && let Some(t) = self.tabs.last_mut()
        {
            t.favorite = s.favorite;
        }
    }

    fn start_transfer(&mut self, label: String, stop: &'static str) -> (u64, Arc<AtomicBool>, impl Fn(Progress) + Send + 'static) {
        let id = self.next_id();
        let cancel = Arc::new(AtomicBool::new(false));
        self.transfers.push(Transfer { id, label, done: Progress::Written(0), cancel: Arc::clone(&cancel), stop });
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let progress = move |n| {
            let _ = tx.send(AppEvent::TransferProgress(id, n));
            ctx.request_repaint();
        };
        (id, cancel, progress)
    }

    fn export(&mut self, server: u64, opts: ExportOptions) {
        // The next export proposes the same folder and format.
        let defaults = ExportDefaults {
            dir: opts.path.parent().map(|d| d.display().to_string()).filter(|d| !d.is_empty()).unwrap_or_else(|| self.settings.export.dir.clone()),
            format: opts.format,
            canonical: opts.canonical,
            delimiter: opts.delimiter,
        };
        if defaults != self.settings.export {
            self.settings.export = defaults;
            self.run_action(Action::SaveSettings);
        }
        let Some(conn) = self.server(server).and_then(|s| s.conn().cloned()) else { return };
        let label = format!("Export {}.{}", opts.db, opts.coll);
        self.log(LogLevel::Info, format!("{label} to {}…", opts.path.display()));
        let (id, cancel, progress) = self.start_transfer(label, "Stopped, it leaves an incomplete file.");
        self.spawn(async move {
            let result = crate::transfer::export(&conn, &opts, &cancel, |n| progress(Progress::Written(n)))
                .await
                .map(|n| format!("{n} document{} exported to {}", if n == 1 { "" } else { "s" }, opts.path.display()))
                .map_err(|e| mongo::error_message(&e));
            AppEvent::TransferDone { id, server, result, warning: false, refresh: Refresh::None }
        });
    }

    fn import(&mut self, server: u64, opts: ImportOptions) {
        let Some(conn) = self.server(server).and_then(|s| s.conn().cloned()) else { return };
        let label = format!("Import into {}.{}", opts.db, opts.coll);
        self.log(LogLevel::Info, format!("{label} from {}…", opts.path.display()));
        let stop = if opts.drop_first { "Stopped, it leaves the collection as it was." } else { "Stopped, it keeps the documents already imported." };
        let (id, cancel, progress) = self.start_transfer(label, stop);
        let refresh = Refresh::Collections(opts.db.clone());
        self.spawn(async move {
            let result = crate::transfer::import(&conn, &opts, cancel, progress).await;
            let warning = result.as_ref().is_ok_and(|i| i.failed > 0);
            let result = result.map(|i| i.summary()).map_err(|e| mongo::error_message(&e));
            AppEvent::TransferDone { id, server, result, warning, refresh }
        });
    }

    /// Remembers the folder of a dump or restore, proposed by the next ones.
    fn remember_dump(&mut self, folder: &std::path::Path) {
        let folder = folder.display().to_string();
        if folder != self.settings.last_dump {
            self.settings.last_dump = folder;
            self.run_action(Action::SaveSettings);
        }
    }

    fn dump(&mut self, server: u64, opts: DumpOptions) {
        self.remember_dump(&opts.target());
        let Some(conn) = self.server(server).and_then(|s| s.conn().cloned()) else { return };
        let label = format!("Dump {}", opts.db);
        self.log(LogLevel::Info, format!("{label} to {}…", opts.target().display()));
        let (id, cancel, progress) = self.start_transfer(label, "Stopped, it leaves an incomplete dump.");
        self.spawn(async move {
            let result = crate::dump::dump(&conn, &opts, &cancel, |n| progress(Progress::Written(n)))
                .await
                .map(|d| d.summary(&opts.target()))
                .map_err(|e| mongo::error_message(&e));
            AppEvent::TransferDone { id, server, result, warning: false, refresh: Refresh::None }
        });
    }

    fn restore(&mut self, server: u64, opts: RestoreOptions) {
        self.remember_dump(&opts.from);
        let Some(conn) = self.server(server).and_then(|s| s.conn().cloned()) else { return };
        let label = format!("Restore {}", opts.db);
        self.log(LogLevel::Info, format!("{label} from {}…", opts.from.display()));
        let stop = if opts.drop_first {
            "Stopped, it leaves the collections not restored yet as they were."
        } else {
            "Stopped, it keeps the documents already restored."
        };
        let (id, cancel, progress) = self.start_transfer(label, stop);
        let refresh = Refresh::Collections(opts.db.clone());
        self.spawn(async move {
            let result = crate::dump::restore(&conn, &opts, cancel, progress).await;
            let warning = result.as_ref().is_ok_and(|r| r.incomplete());
            let result = result.map(|r| r.summary()).map_err(|e| mongo::error_message(&e));
            AppEvent::TransferDone { id, server, result, warning, refresh }
        });
    }

    fn update_theme(&mut self) {
        self.theme_name = self.settings.theme.clone();
        self.theme = *theme::resolve(self.theme_preview.unwrap_or(&self.settings.theme), self.system_dark);
        theme::apply(&self.ctx, &self.theme, self.settings.font_size);
        // It shows on the next frame, which nothing else may ask for.
        self.ctx.request_repaint();
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
        // The results are next to look at.
        tab.typing = false;
        let script = tab.selection.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| tab.script.clone());
        if script.trim().is_empty() {
            return;
        }
        let drop = if tab.keep_vars {
            DropVars::Only(tab.vars.iter().filter(|v| !v.keep).map(|v| v.name.clone()).collect())
        } else {
            DropVars::All
        };
        let Some(shell) = &mut tab.shell else { return };
        tab.pending = Some(shell.execute(script.clone(), drop));
        tab.running = true;
        tab.started = Some(Instant::now());
        tab.notice = None;
        let first = script.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().to_string();
        let (server, db) = (tab.server, tab.db.clone());
        let settings = self.server(server).map(|s| (s.settings.id, s.settings.name.clone()));
        let name = settings.as_ref().map(|s| s.1.clone()).unwrap_or_default();
        self.log(LogLevel::Info, format!("[{name}] {first}"));
        if self.settings.query_history {
            let (connection, connection_name) = settings.map_or((None, String::new()), |(id, name)| (Some(id), name));
            self.history.push(HistoryEntry { connection, connection_name, db, script, ..Default::default() });
            if let Err(e) = self.history.save() {
                self.log(LogLevel::Error, format!("Cannot save the query history: {e}"));
            }
        }
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
        let modal_open = self.conn_list.is_some() || self.conn_editor.is_some() || self.dialog.is_some() || self.favorites.is_some() || self.history_window.is_some();

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
        if pressed(cmd, Key::H) {
            self.actions.push(Action::ShowHistory);
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

impl App {
    /// Asks before quitting while transfers run, and once asked to stop
    /// them, quits when they are over: a transfer stopped halfway puts back
    /// what it changed (the new collection of a replacement) first.
    fn confirm_quit(&mut self, ctx: &egui::Context) {
        let requested = ctx.input(|i| i.viewport().close_requested());
        match self.quit.as_ref().and_then(|q| q.stopping) {
            // A request now is this one, or the user insisting: it closes.
            Some(since) => {
                if !requested && (self.transfers.is_empty() || since.elapsed() > QUIT_WAIT) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            None if requested && !self.transfers.is_empty() => {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.quit = Some(Quit { stopping: None });
                // Seen also when closed from the taskbar.
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            None => {}
        }
    }
}

impl eframe::App for App {
    /// Also called while the window is hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.confirm_quit(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_events();
        self.sync_system_theme(&ctx);
        self.shortcuts(&ctx);
        crate::ui::main_window(self, ui);
        crate::ui::connections::show(self, &ctx);
        crate::ui::favorites::show(self, &ctx);
        crate::ui::history::show(self, &ctx);
        crate::ui::dialogs::show(self, &ctx);
        crate::ui::dialogs::quit(self, &ctx);
        crate::autotest::tick(self, &ctx);
        self.process_actions();
        if self.tabs.iter().any(|t| t.running) || !self.transfers.is_empty() || self.servers.iter().any(|s| matches!(s.status, ServerStatus::Connecting)) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn on_exit(&mut self) {
        // What the Preferences dialog shows is not saved yet.
        if let Some(Dialog::Preferences { original, .. }) = &self.dialog {
            original.apply(&mut self.settings, &mut Vec::new());
        }
        let _ = self.settings.save();
        self.cancel_install();
        for t in &self.transfers {
            t.cancel.store(true, Ordering::Relaxed);
        }
        let _guard = self.rt.enter();
        for s in &self.servers {
            if let Some(c) = s.conn() {
                c.shutdown();
            }
        }
        if self.restart_on_quit {
            let _ = self.start_new_program();
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
