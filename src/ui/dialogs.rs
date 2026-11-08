//! Modal dialogs other than the connection manager.

use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bson::{Bson, Document, doc};
use egui::{FontId, Id, RichText, Ui};
use uuid::Uuid;

use crate::app::{Action, App, Quit, Refresh};
use crate::bsonfmt;
use crate::mongo::ConnectSecrets;
use crate::settings::{AppSettings, AuthMechanism, ConnectionSettings, ExportDefaults, SshAuthMethod};
use crate::shell::{QuerySpec, parse_shell_value};
use crate::dump::{DumpEntry, DumpOptions, RestoreOptions};
use crate::transfer::{Delimiter, ExportOptions, Format, ImportOptions, Progress};
use crate::ui::icon;
use crate::update::{Install, Os, Release};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocMode {
    Insert,
    Edit,
    View,
}

/// Validation of the document editor, run in the background a moment after
/// the last edit (parsing takes milliseconds: not on every keystroke).
#[derive(Clone, Default)]
pub struct DocCheck {
    /// Last result, with the hash of the text it is for.
    checked: Option<CheckResult>,
    /// Hash of the edited text and time of the edit, not checked yet.
    pending: Option<(u64, Instant)>,
    /// Result of the background check, with the hash of the text checked.
    incoming: Arc<Mutex<Option<CheckResult>>>,
}

/// Hash of the text checked, and the result.
type CheckResult = (u64, Result<(), String>);

const CHECK_DELAY: Duration = Duration::from_millis(300);

impl DocCheck {
    /// Returns the result for `text`, or the previous one while `text` is
    /// being checked (`None` before the first check).
    fn poll(&mut self, ctx: &egui::Context, text: &str) -> Option<&Result<(), String>> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        let hash = h.finish();
        if let Some(done) = self.incoming.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.checked = Some(done);
        }
        if let Some((h, result)) = &self.checked
            && *h == hash
        {
            self.pending = None;
            return Some(result);
        }
        match self.pending {
            Some((h, at)) if h == hash => {
                if at.elapsed() >= CHECK_DELAY {
                    // Far in the future: wait for the result, do not start again.
                    self.pending = Some((hash, Instant::now() + Duration::from_secs(3600)));
                    let (text, incoming, ctx) = (text.to_string(), self.incoming.clone(), ctx.clone());
                    std::thread::spawn(move || {
                        let result = parse_doc(&text).map(|_| ());
                        *incoming.lock().unwrap_or_else(|e| e.into_inner()) = Some((hash, result));
                        ctx.request_repaint();
                    });
                } else {
                    ctx.request_repaint_after(CHECK_DELAY);
                }
            }
            _ => {
                self.pending = Some((hash, Instant::now()));
                ctx.request_repaint_after(CHECK_DELAY);
            }
        }
        self.checked.as_ref().map(|(_, r)| r)
    }

    /// Records a result computed in the foreground (save, format).
    fn set(&mut self, text: &str, result: Result<(), String>) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        self.checked = Some((h.finish(), result));
    }
}

/// What the Preferences dialog edits.
#[derive(Clone, PartialEq)]
pub struct Preferences {
    batch_size: u32,
    font_size: f32,
    fonts: crate::fonts::FontChoice,
    theme: String,
}

impl Preferences {
    fn of(settings: &AppSettings) -> Self {
        Self { batch_size: settings.batch_size, font_size: settings.font_size, fonts: settings.fonts.clone(), theme: settings.theme.clone() }
    }

    /// Puts these values in the settings and shows what changes, without saving.
    pub fn apply(&self, settings: &mut AppSettings, actions: &mut Vec<Action>) {
        settings.batch_size = self.batch_size;
        if self.font_size != settings.font_size || self.theme != settings.theme {
            settings.font_size = self.font_size;
            settings.theme = self.theme.clone();
            actions.push(Action::PreviewTheme);
        }
        if self.fonts != settings.fonts {
            settings.fonts = self.fonts.clone();
            actions.push(Action::ApplyFonts);
        }
    }
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
    Document { mode: DocMode, server: u64, db: String, coll: String, text: String, original_id: Option<Bson>, tab: Option<u64>, check: DocCheck },
    User { server: u64, db: String, edit: bool, name: String, password: String, roles: Vec<(String, bool)>, extra_roles: String, error: Option<String> },
    Index(Box<IndexForm>),
    Export(Box<ExportForm>),
    Import(Box<ImportForm>),
    Dump(Box<DumpForm>),
    Restore(Box<RestoreForm>),
    Function { server: u64, db: String, original: Option<String>, name: String, code: String, error: Option<String> },
    ShellTimeout { value: u32 },
    /// The appearance follows `edit`, and goes back to `original` unless saved.
    Preferences { edit: Preferences, original: Preferences, frozen_style: Option<Arc<egui::Style>>, width: f32 },
    /// Name of the script of a shell being added to the favorites.
    FavoriteName { tab: u64, name: String },
    About,
    Shortcuts,
    Update { release: Arc<Release>, os: Os, commands_open: bool },
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

/// What the export dialog edits.
#[derive(Clone)]
pub struct ExportForm {
    pub server: u64,
    pub db: String,
    pub coll: String,
    pub filter: String,
    pub projection: String,
    pub sort: String,
    pub skip: String,
    pub limit: String,
    pub format: Format,
    pub canonical: bool,
    pub fields: String,
    pub delimiter: Delimiter,
    pub path: String,
    pub error: Option<String>,
}

/// What the import dialog edits.
#[derive(Clone)]
pub struct ImportForm {
    pub server: u64,
    pub db: String,
    pub coll: String,
    /// The collection name follows the file name until it is typed.
    pub coll_from_file: bool,
    pub path: String,
    pub format: Format,
    pub delimiter: Delimiter,
    pub detect_types: bool,
    pub drop_first: bool,
    pub error: Option<String>,
}

/// `name.ext` in the home directory.
fn default_path(name: &str, ext: &str) -> String {
    let dir = directories::UserDirs::new().map(|d| d.home_dir().to_path_buf()).unwrap_or_default();
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
    dir.join(format!("{safe}.{ext}")).display().to_string()
}

/// `path` moved to `dir`, when it is an existing folder.
fn in_dir(path: &str, dir: &str) -> String {
    let dir = std::path::Path::new(dir);
    match std::path::Path::new(path).file_name() {
        Some(name) if !dir.as_os_str().is_empty() && dir.is_dir() => dir.join(name).display().to_string(),
        _ => path.to_string(),
    }
}

/// `path` with the extension of `format`, when it has the one of another format.
fn with_extension(path: &str, format: Format) -> String {
    let p = std::path::Path::new(path);
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("json" | "csv" | "tsv") => p.with_extension(format.extension()).display().to_string(),
        _ => path.to_string(),
    }
}

impl ExportForm {
    /// Proposes the folder and the format of the last export.
    pub fn set_defaults(&mut self, d: &ExportDefaults) {
        self.format = d.format;
        self.canonical = d.canonical;
        self.delimiter = d.delimiter;
        self.path = with_extension(&in_dir(&self.path, &d.dir), d.format);
    }

    fn new(server: u64, db: &str, coll: &str) -> Self {
        ExportForm {
            server,
            db: db.to_string(),
            coll: coll.to_string(),
            filter: "{}".into(),
            projection: String::new(),
            sort: String::new(),
            skip: String::new(),
            limit: String::new(),
            format: Format::JsonLines,
            canonical: true,
            fields: String::new(),
            delimiter: Delimiter::Comma,
            path: default_path(coll, "json"),
            error: None,
        }
    }

    /// The export options, or what is wrong in the form.
    pub fn options(&self) -> Result<ExportOptions, String> {
        let filter = opt_doc(&self.filter).map_err(|e| format!("Query: {e}"))?.unwrap_or_default();
        let projection = opt_doc(&self.projection).map_err(|e| format!("Projection: {e}"))?;
        let sort = opt_doc(&self.sort).map_err(|e| format!("Sort: {e}"))?;
        let number = |t: &str, what: &str| -> Result<u64, String> {
            if t.trim().is_empty() { Ok(0) } else { t.trim().parse().map_err(|_| format!("{what} must be a positive number")) }
        };
        let skip = number(&self.skip, "Skip")?;
        let limit = number(&self.limit, "Limit")?;
        let path = self.path.trim();
        if path.is_empty() {
            return Err("Choose a file".into());
        }
        let path = std::path::PathBuf::from(path);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
            && !dir.is_dir()
        {
            return Err(format!("Folder not found: {}", dir.display()));
        }
        let fields = self.fields.split(',').map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect();
        Ok(ExportOptions {
            db: self.db.clone(),
            coll: self.coll.clone(),
            filter,
            projection,
            sort,
            skip,
            limit,
            format: self.format,
            canonical: self.canonical,
            fields,
            delimiter: self.delimiter,
            path,
        })
    }
}

impl ImportForm {
    fn new(server: u64, db: &str, coll: Option<&str>) -> Self {
        ImportForm {
            server,
            db: db.to_string(),
            coll: coll.unwrap_or_default().to_string(),
            coll_from_file: coll.is_none(),
            path: String::new(),
            format: Format::JsonLines,
            delimiter: Delimiter::Comma,
            detect_types: true,
            drop_first: false,
            error: None,
        }
    }

    /// Format, delimiter and collection name from the file name.
    pub fn set_path(&mut self, path: String) {
        let p = std::path::Path::new(&path);
        self.format = Format::of_path(p);
        self.delimiter = Delimiter::of_path(p);
        if self.coll_from_file {
            self.coll = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        }
        self.path = path;
    }

    pub fn options(&self) -> Result<ImportOptions, String> {
        let coll = self.coll.trim();
        if coll.is_empty() {
            return Err("A collection name is required".into());
        }
        if coll.contains('$') || coll.starts_with("system.") {
            return Err(format!("Invalid collection name: {coll}"));
        }
        let path = std::path::PathBuf::from(self.path.trim());
        if !path.is_file() {
            return Err(if self.path.trim().is_empty() { "Choose a file".into() } else { format!("File not found: {}", path.display()) });
        }
        Ok(ImportOptions {
            db: self.db.clone(),
            coll: coll.to_string(),
            format: self.format,
            delimiter: self.delimiter,
            detect_types: self.detect_types,
            drop_first: self.drop_first,
            path,
        })
    }
}

/// What the dump dialog edits.
#[derive(Clone)]
pub struct DumpForm {
    pub server: u64,
    pub db: String,
    /// Folder in which the folder or the archive of the database is written.
    pub dir: String,
    /// The collections of the explorer, with whether to dump them. Empty
    /// when not loaded: all are dumped.
    pub collections: Vec<(String, bool)>,
    pub gzip: bool,
    /// One file instead of a folder.
    pub archive: bool,
    pub error: Option<String>,
}

fn home_dir() -> String {
    directories::UserDirs::new().map(|d| d.home_dir().display().to_string()).unwrap_or_default()
}

impl DumpForm {
    fn new(server: u64, db: &str, collections: Vec<String>) -> Self {
        DumpForm {
            server,
            db: db.to_string(),
            dir: home_dir(),
            collections: collections.into_iter().map(|c| (c, true)).collect(),
            gzip: false,
            archive: false,
            error: None,
        }
    }

    /// Proposes the folder and the format of the last dump or restore,
    /// `last` being its folder or archive.
    pub fn set_last(&mut self, last: &str) {
        let path = std::path::Path::new(last);
        if let Some(dir) = path.parent().filter(|d| !last.is_empty() && d.is_dir()) {
            self.dir = dir.display().to_string();
            self.archive = path.is_file();
            self.gzip = self.archive && last.ends_with(".gz");
        }
    }

    pub fn options(&self) -> Result<DumpOptions, String> {
        let dir = std::path::PathBuf::from(self.dir.trim());
        if self.dir.trim().is_empty() {
            return Err("Choose a folder".into());
        }
        if !dir.is_dir() {
            return Err(format!("Folder not found: {}", dir.display()));
        }
        let collections: Vec<String> = self.collections.iter().filter(|(_, on)| *on).map(|(c, _)| c.clone()).collect();
        if !self.collections.is_empty() && collections.is_empty() {
            return Err("Choose at least one collection".into());
        }
        // All of them: also those created since the explorer listed them.
        let all = collections.len() == self.collections.len();
        Ok(DumpOptions { db: self.db.clone(), collections: if all { Vec::new() } else { collections }, dir, gzip: self.gzip, archive: self.archive })
    }
}

/// What the restore dialog edits.
#[derive(Clone)]
pub struct RestoreForm {
    pub server: u64,
    pub db: String,
    /// Folder or archive.
    pub from: String,
    /// The collections of the dump, with whether to restore them, or why it cannot be read.
    pub found: Result<Vec<(DumpEntry, bool)>, String>,
    pub drop_first: bool,
    pub indexes: bool,
    pub error: Option<String>,
}

impl RestoreForm {
    fn new(server: u64, db: &str) -> Self {
        RestoreForm { server, db: db.to_string(), from: String::new(), found: Ok(Vec::new()), drop_first: false, indexes: true, error: None }
    }

    /// Proposes the dump of this database next to `last`, the folder or
    /// archive of the last dump or restore, else `last`.
    pub fn set_last(&mut self, last: &str) {
        if last.is_empty() {
            return;
        }
        let last = std::path::Path::new(last);
        let name = crate::dump::escape(&self.db);
        let siblings: Vec<std::path::PathBuf> = match last.parent() {
            Some(p) if last.is_file() => vec![p.join(format!("{name}.archive")), p.join(format!("{name}.archive.gz"))],
            Some(p) => vec![p.join(&name)],
            None => Vec::new(),
        };
        let found = siblings.into_iter().find(|s| s.exists()).or_else(|| last.exists().then(|| last.to_path_buf()));
        if let Some(path) = found {
            self.set_path(path.display().to_string());
        }
    }

    /// Lists the collections of the dump.
    pub fn set_path(&mut self, from: String) {
        self.from = from;
        self.rescan();
    }

    pub fn rescan(&mut self) {
        let from = self.from.trim();
        self.found = if from.is_empty() {
            Ok(Vec::new())
        } else {
            crate::dump::scan(std::path::Path::new(from), self.db.trim()).map(|v| v.into_iter().map(|e| (e, true)).collect()).map_err(|e| format!("{e:#}"))
        };
    }

    pub fn options(&self) -> Result<RestoreOptions, String> {
        let db = self.db.trim();
        if db.is_empty() || db.contains(['/', '\\', '.', ' ', '"', '$']) {
            return Err(format!("Invalid database name: {db}"));
        }
        let found = self.found.as_ref().map_err(|e| e.clone())?;
        if found.is_empty() {
            return Err(if self.from.trim().is_empty() { "Choose a folder or an archive".into() } else { "No collection in this dump".into() });
        }
        let collections: Vec<String> = found.iter().filter(|(_, on)| *on).map(|(e, _)| e.name.clone()).collect();
        if collections.is_empty() {
            return Err("Choose at least one collection".into());
        }
        Ok(RestoreOptions {
            db: db.to_string(),
            from: std::path::PathBuf::from(self.from.trim()),
            collections: if collections.len() == found.len() { Vec::new() } else { collections },
            drop_first: self.drop_first,
            indexes: self.indexes,
        })
    }
}

const DB_ROLES: &[&str] = &["read", "readWrite", "dbAdmin", "dbOwner", "userAdmin"];
const ADMIN_ROLES: &[&str] = &[
    "root", "readAnyDatabase", "readWriteAnyDatabase", "userAdminAnyDatabase", "dbAdminAnyDatabase",
    "clusterAdmin", "clusterManager", "clusterMonitor", "hostManager", "backup", "restore",
];

impl Dialog {
    pub fn preferences(settings: &AppSettings) -> Self {
        let edit = Preferences::of(settings);
        Dialog::Preferences { original: edit.clone(), edit, frozen_style: None, width: 0.0 }
    }

    pub fn message(title: &str, message: impl Into<String>) -> Self {
        Dialog::Message { title: title.to_string(), message: message.into() }
    }

    pub fn update(release: Arc<Release>) -> Self {
        Dialog::Update { release, os: Os::current(), commands_open: false }
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

    pub fn export(server: u64, db: &str, coll: &str) -> Self {
        Dialog::Export(Box::new(ExportForm::new(server, db, coll)))
    }

    /// Export of the documents of a query of the results.
    pub fn export_query(server: u64, spec: &QuerySpec) -> Self {
        let f = Default::default();
        let mut form = ExportForm::new(server, &spec.db, &spec.coll);
        form.filter = bsonfmt::doc_to_shell(&spec.filter, f);
        form.projection = spec.projection.as_ref().map(|p| one_line(&bsonfmt::doc_to_shell(p, f))).unwrap_or_default();
        form.sort = spec.sort.as_ref().map(|s| one_line(&bsonfmt::doc_to_shell(s, f))).unwrap_or_default();
        form.skip = if spec.skip > 0 { spec.skip.to_string() } else { String::new() };
        form.limit = if spec.limit > 0 { spec.limit.to_string() } else { String::new() };
        Dialog::Export(Box::new(form))
    }

    /// Dump of a database; `collections` are those listed in the explorer.
    pub fn dump(server: u64, db: &str, collections: Vec<String>) -> Self {
        Dialog::Dump(Box::new(DumpForm::new(server, db, collections)))
    }

    pub fn restore(server: u64, db: &str) -> Self {
        Dialog::Restore(Box::new(RestoreForm::new(server, db)))
    }

    /// Import into `coll`, or into a collection named after the file.
    pub fn import(server: u64, db: &str, coll: Option<&str>) -> Self {
        Dialog::Import(Box::new(ImportForm::new(server, db, coll)))
    }

    pub fn document(mode: DocMode, server: u64, db: &str, coll: &str, text: String, original_id: Option<Bson>, tab: Option<u64>) -> Self {
        Dialog::Document { mode, server, db: db.to_string(), coll: coll.to_string(), text, original_id, tab, check: DocCheck::default() }
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

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn buttons(ui: &mut Ui, ok_label: &str, ok_enabled: bool) -> (bool, bool) {
    let mut ok = false;
    let mut cancel = false;
    button_row(ui, |ui| {
        if ui.add_enabled(ok_enabled, egui::Button::new(RichText::new(ok_label).strong())).clicked() {
            ok = true;
        }
        if ui.button("Cancel").clicked() {
            cancel = true;
        }
    });
    (ok, cancel)
}

/// Single right-aligned button for informational dialogs.
/// Fixed height and scrolling, whatever the length of the text.
fn read_only_text(ui: &mut Ui, salt: &str, text: &str, font: egui::TextStyle, height: f32, theme: &crate::theme::Theme) {
    egui::Frame::new().fill(theme.base).stroke(egui::Stroke::new(1.0, theme.border)).corner_radius(4).inner_margin(4).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(salt).auto_shrink(false).max_height(height).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut &*text).id_salt(salt).font(font).frame(egui::Frame::NONE).desired_width(f32::INFINITY));
        });
    });
}

fn close_button(ui: &mut Ui, label: &str) -> bool {
    let mut clicked = false;
    button_row(ui, |ui| clicked = ui.button(RichText::new(label).strong()).clicked());
    clicked
}

/// Right-aligned row of buttons at the bottom of a dialog, as wide as the
/// content above it: a bare right-to-left layout would take the whole
/// available size and stretch the modal.
fn button_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(6.0);
    let size = egui::vec2(ui.min_rect().width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), add);
}

fn title(ui: &mut Ui, t: &str) {
    ui.heading(t);
    ui.separator();
}

fn error_line(ui: &mut Ui, app_theme: &crate::theme::Theme, e: &Option<String>) {
    if let Some(e) = e {
        ui.add(egui::Label::new(RichText::new(format!("{} {e}", icon::WARNING)).color(app_theme.error)).wrap());
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
                    search: true,
                    multi_cursor: true,
                    folding: false,
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
                if close_button(ui, "OK") || enter { Outcome::Close } else { Outcome::Keep }
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
            Dialog::Document { mode, server, db, coll, text, original_id, tab, check } => {
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
                            search: true,
                            multi_cursor: true,
                            folding: true,
                        },
                    );
                });
                let hint = RichText::new("Ctrl+F find · Ctrl+D select next occurrence · click the gutter to fold").small().color(theme.text_weak);
                if *mode == DocMode::View {
                    ui.label(hint);
                } else {
                    ui.horizontal(|ui| {
                        match check.poll(ui.ctx(), text) {
                            Some(Ok(())) => ui.label(RichText::new(format!("{} Valid document", icon::CHECK)).color(theme.success)),
                            Some(Err(e)) => ui.add(egui::Label::new(RichText::new(format!("{} {e}", icon::WARNING)).color(theme.error)).wrap()),
                            None => ui.label(RichText::new("Checking…").color(theme.text_weak)),
                        };
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(hint));
                    });
                }
                let mut out = Outcome::Keep;
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if *mode != DocMode::View && ui.button("Format").clicked() {
                        match parse_doc(text) {
                            Ok(d) => {
                                *text = bsonfmt::doc_to_shell(&d, Default::default());
                                check.set(text, Ok(()));
                            }
                            Err(e) => check.set(text, Err(e)),
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
                                Err(e) => check.set(text, Err(e)),
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
            Dialog::Export(f) => export_dialog(ui, f, &theme, font, &mut actions),
            Dialog::Import(f) => import_dialog(ui, f, &theme, &mut actions),
            Dialog::Dump(f) => dump_dialog(ui, f, &theme, &mut actions),
            Dialog::Restore(f) => restore_dialog(ui, f, &theme, &mut actions),
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
            Dialog::FavoriteName { tab, name } => {
                ui.set_width(380.0);
                title(ui, "Add to Favorite Scripts");
                ui.label("Name:");
                let r = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
                if !r.has_focus() && ui.memory(|m| m.focused().is_none()) {
                    r.request_focus();
                }
                let valid = !name.trim().is_empty();
                let (ok, cancel) = buttons(ui, "Add", valid);
                if valid && (ok || enter) {
                    actions.push(Action::AddFavorite { tab: *tab, name: name.trim().to_string() });
                }
                if (valid && (ok || enter)) || cancel { Outcome::Close } else { Outcome::Keep }
            }
            Dialog::Preferences { edit, original, frozen_style, width } => {
                // During a drag the font size changes everywhere but here: the
                // dialog would resize and move the slider under the pointer,
                // which would change the size again.
                if let Some(style) = frozen_style {
                    ui.set_style(Arc::clone(style));
                }
                // The separator of the title takes the width of the previous
                // frame: after a wide font or a large size, the dialog would
                // never shrink.
                if *width > 0.0 {
                    ui.set_max_width(*width);
                }
                title(ui, "Preferences");
                let grid = egui::Grid::new("prefs").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Batch size (documents per page):");
                    ui.add(egui::DragValue::new(&mut edit.batch_size).range(1..=10_000));
                    ui.end_row();
                    ui.label("Font size:");
                    ui.spacing_mut().slider_width = 150.0;
                    let size = ui.add(egui::Slider::new(&mut edit.font_size, 9.0..=24.0).integer().suffix(" pt"));
                    if size.dragged() {
                        frozen_style.get_or_insert_with(|| Arc::clone(ui.style()));
                    } else if frozen_style.take().is_some() {
                        ui.ctx().request_repaint();
                    }
                    ui.end_row();
                    let lists = crate::fonts::lists();
                    ui.label("Interface font:");
                    font_picker(ui, "pref-ui-font", &mut edit.fonts.ui, lists.map_or(&[], |l| &l.all));
                    ui.end_row();
                    ui.label("Monospace font:");
                    font_picker(ui, "pref-mono-font", &mut edit.fonts.mono, lists.map_or(&[], |l| &l.mono));
                    ui.end_row();
                    ui.label("Theme:");
                    if let Some(name) = combo(ui, "pref-theme", &edit.theme, crate::theme::names()) {
                        edit.theme = name.to_string();
                    }
                    ui.end_row();
                });
                *width = grid.response.rect.width();
                let defaults = Preferences::of(&AppSettings::default());
                let (mut ok, mut cancel) = (false, false);
                button_row(ui, |ui| {
                    ok = ui.button(RichText::new("Save").strong()).clicked();
                    cancel = ui.button("Cancel").clicked();
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if ui.add_enabled(*edit != defaults, egui::Button::new("Reset to Default")).clicked() {
                            *edit = defaults;
                        }
                    });
                });
                edit.apply(&mut app.settings, &mut actions);
                if ok {
                    *original = edit.clone();
                    actions.push(Action::SaveSettings);
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
                    ui.label(RichText::new("MongoDB 3.6 to 8.x · embedded QuickJS shell · SSH tunnels").small().color(theme.text_weak));
                });
                if close_button(ui, "OK") || enter { Outcome::Close } else { Outcome::Keep }
            }
            Dialog::Update { release, os, commands_open } => {
                ui.set_width(600.0);
                title(ui, &format!("New version {}", release.version));
                read_only_text(ui, "update-notes", &release.notes, egui::TextStyle::Body, 220.0, &theme);
                ui.hyperlink_to("Full changelog", release.changelog_url());
                ui.hyperlink_to(format!("Download {} binary", release.version), release.download_url());
                ui.add_space(4.0);
                let manual = egui::CollapsingHeader::new("Update manually").open(Some(*commands_open)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for choice in Os::ALL {
                            ui.selectable_value(os, choice, choice.label());
                        }
                    });
                    let commands = release.commands(*os);
                    read_only_text(ui, "update-commands", &commands, egui::TextStyle::Monospace, 160.0, &theme);
                    if ui.button(format!("{} Copy", icon::COPY)).on_hover_text("Copy the commands").clicked() {
                        ui.ctx().copy_text(commands);
                    }
                });
                if manual.header_response.clicked() {
                    *commands_open = !*commands_open;
                }
                ui.add_space(4.0);
                let mut running = false;
                match &app.install {
                    Install::Idle => {}
                    Install::Failed(e) => error_line(ui, &theme, &Some(e.clone())),
                    Install::Running { done, total, .. } => {
                        running = true;
                        let bar = match total {
                            Some(t) if done >= t => egui::ProgressBar::new(1.0).text("Installing…"),
                            Some(t) if *t > 0 => {
                                let part = *done as f32 / *t as f32;
                                egui::ProgressBar::new(part).text(format!("Downloading… {:.0}%", part * 100.0))
                            }
                            _ => egui::ProgressBar::new(0.0).text("Downloading…").animate(true),
                        };
                        ui.add(bar);
                    }
                }
                let possible = release.asset.is_some() && app.update_target.is_some();
                if !possible {
                    ui.label(RichText::new("This system cannot update by itself: download the update or use the commands.").color(theme.text_weak));
                } else if !app.tabs.is_empty() {
                    ui.label(RichText::new("Restarting closes the shells: unsaved scripts are lost, running ones are stopped.").color(theme.warning));
                }
                let (install, cancel) = buttons(ui, &format!("{} Download update and restart", icon::DOWNLOAD_SIMPLE), possible && !running);
                if install {
                    actions.push(Action::InstallUpdate(Arc::clone(release)));
                }
                if cancel { Outcome::Close } else { Outcome::Keep }
            }
            Dialog::Shortcuts => {
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
                        ("Ctrl+H", "Query history"),
                        ("Ctrl+E / Ctrl+L", "Toggle explorer / logs"),
                        ("Ctrl+Space", "Autocompletion"),
                        ("Ctrl+F", "Find in the editor or the results"),
                        ("Ctrl+C", "Copy the value under the pointer, or the selection"),
                        ("Ctrl+Shift+C", "Copy them with their names"),
                        ("Ctrl+D", "Select the next occurrence"),
                        ("Del", "Delete the selected documents"),
                        ("Alt+1…9", "Connect to the n-th connection"),
                    ] {
                        ui.label(RichText::new(k).monospace());
                        ui.label(d);
                        ui.end_row();
                    }
                });
                if close_button(ui, "Close") || enter { Outcome::Close } else { Outcome::Keep }
            }
        }
    });

    // egui repaints after the first sizing pass only: a dialog that settles
    // on its size a frame later would stay misplaced until the next input.
    let rect_id = Id::new("app-dialog-rect");
    if ctx.data(|d| d.get_temp::<egui::Rect>(rect_id)) != Some(resp.response.rect) {
        ctx.data_mut(|d| d.insert_temp(rect_id, resp.response.rect));
        ctx.request_repaint();
    }
    let close = matches!(resp.inner, Outcome::Close) || resp.should_close();
    if close && let Dialog::Preferences { original, .. } = &dlg {
        original.apply(&mut app.settings, &mut actions);
    }
    if close && matches!(dlg, Dialog::Update { .. }) {
        app.cancel_install();
    }
    if !close && app.dialog.is_none() {
        app.dialog = Some(dlg);
    }
    app.actions.extend(actions);
}

/// Asked before quitting while transfers run (see `App::confirm_quit`):
/// what stopping each one leaves; then waits for them to end.
pub fn quit(app: &mut App, ctx: &egui::Context) {
    let Some(q) = &app.quit else { return };
    let stopping = q.stopping.is_some();
    let theme = app.theme;
    // Quit, or keep the transfers running.
    let mut choice: Option<bool> = None;
    let mut now = false;
    let resp = egui::Modal::new(Id::new("quit-dialog")).show(ctx, |ui| {
        ui.set_max_width(520.0);
        title(ui, "Quit Rustyrobo?");
        if stopping {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Stopping the transfers…");
            });
            ui.label(RichText::new("Rustyrobo quits once they have ended their batch and put back what they were replacing.").small().color(theme.text_weak));
            now = close_button(ui, "Quit Now");
            return;
        }
        match app.transfers.len() {
            0 => ui.label("The transfers are over."),
            1 => ui.label("A transfer is not done:"),
            n => ui.label(format!("{n} transfers are not done:")),
        };
        for t in &app.transfers {
            ui.add_space(4.0);
            ui.label(RichText::new(&t.label).strong());
            let what = match t.done {
                Progress::Checked(_) => "Nothing is written yet: it reads its file.",
                Progress::Written(_) => t.stop,
            };
            ui.label(RichText::new(what).color(theme.text_weak));
        }
        let (ok, cancel) = buttons(ui, "Stop and Quit", true);
        if ok {
            choice = Some(true);
        } else if cancel {
            choice = Some(false);
        }
    });
    if now {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    // Escape or a click outside keeps the transfers running, unless they are stopping already.
    if !stopping && resp.should_close() {
        choice = Some(false);
    }
    match choice {
        Some(true) => {
            for t in &app.transfers {
                t.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            app.quit = Some(Quit { stopping: Some(Instant::now()) });
        }
        Some(false) => {
            app.quit = None;
            if std::mem::take(&mut app.restart_on_quit) {
                app.log(crate::app::LogLevel::Info, "Updated: the new version starts with the next launch of Rustyrobo");
            }
        }
        None => {}
    }
}

fn font_picker(ui: &mut Ui, id: &str, font: &mut Option<String>, families: &[String]) {
    const DEFAULT: &str = "Default";
    let items = std::iter::once(DEFAULT).chain(families.iter().map(String::as_str));
    if let Some(picked) = combo(ui, id, font.as_deref().unwrap_or(DEFAULT), items) {
        *font = (picked != DEFAULT).then(|| picked.to_string());
    }
}

/// A combo box that takes the focus when clicked, and whose selection follows Up
/// and Down when it has it. Returns the item to select.
fn combo<'a>(ui: &mut Ui, id: &str, current: &str, items: impl Iterator<Item = &'a str> + Clone) -> Option<&'a str> {
    // The id of the button, known before it is shown: so that it shows the new item.
    let button = ui.make_persistent_id(egui::IdSalt::new(id));
    // Otherwise the arrows move the focus to the next widget.
    ui.memory_mut(|m| m.set_focus_lock_filter(button, egui::EventFilter { vertical_arrows: true, ..Default::default() }));
    let mut picked = None;
    if ui.memory(|m| m.has_focus(button)) {
        let count = |key| ui.input_mut(|i| i.count_and_consume_key(egui::Modifiers::NONE, key) as isize);
        let step = count(egui::Key::ArrowDown) - count(egui::Key::ArrowUp);
        if step != 0 {
            let at = items.clone().position(|item| item == current);
            picked = items.clone().nth(at.map_or(0, |at| at.saturating_add_signed(step)));
        }
    }
    let stepped = picked.is_some();
    let shown = picked.unwrap_or(current);
    let response = egui::ComboBox::from_id_salt(id).selected_text(shown).width(220.0).height(320.0).show_ui(ui, |ui| {
        for item in items {
            let row = ui.selectable_label(item == shown, item);
            if stepped && item == shown {
                row.scroll_to_me(None);
            }
            if row.clicked() {
                picked = Some(item);
            }
        }
    });
    // A click in the list takes the focus away from the button.
    if response.response.clicked() || picked.is_some() {
        response.response.request_focus();
    }
    picked
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

fn file_row(ui: &mut Ui, label: &str, path: &mut String, browse: impl FnOnce() -> Option<std::path::PathBuf>) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let browse_w = 80.0;
        changed |= ui.add(egui::TextEdit::singleline(path).desired_width(ui.available_width() - browse_w)).changed();
        if ui.button("Browse…").clicked()
            && let Some(p) = browse()
        {
            *path = p.display().to_string();
            changed = true;
        }
    });
    changed
}

/// What the info button next to "Canonical Extended JSON" tells.
const EXTENDED_JSON: &[&str] = &[
    "JSON has no type for dates, ObjectIds, 64-bit integers, decimals or binary data. Extended JSON writes them as small objects, like {\"$oid\": \"…\"} or {\"$date\": …}, so that they can be read back.",
    "Canonical, the default, also writes the type of each number: {\"$numberInt\": \"1\"}, {\"$numberLong\": \"5\"}, {\"$numberDouble\": \"1.5\"}. Every value comes back exactly as it was, imported here, with mongoimport or by a MongoDB driver: the choice for a backup, or a copy to another server.",
    "Relaxed writes numbers as plain JSON numbers, and dates as text (2024-01-02T03:04:05.006Z): a smaller file, easier to read or to give to other programs. But a number then does not tell its type: a 64-bit integer small enough for 32 bits, like NumberLong(5), comes back as a 32-bit one (NumberInt), as with mongoexport and mongoimport. Doubles keep their decimal point (2.0) and stay doubles.",
];

fn export_dialog(ui: &mut Ui, f: &mut ExportForm, theme: &crate::theme::Theme, font: f32, actions: &mut Vec<Action>) -> Outcome {
    ui.set_width(560.0);
    title(ui, &format!("Export — {}.{}", f.db, f.coll));
    ui.label("Query:");
    // The editor takes the height it is given: in a modal, that would grow at each frame.
    let height = 4.0 * (font + 4.0) + 8.0;
    ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
        ui.set_max_height(height);
        code_box(ui, Id::new("export-filter"), &mut f.filter, theme, font, 3, false);
    });
    egui::Grid::new("export-query").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        let mono = |ui: &mut Ui, text: &mut String, hint: &str| {
            ui.add(egui::TextEdit::singleline(text).font(egui::TextStyle::Monospace).hint_text(hint).desired_width(f32::INFINITY));
        };
        ui.label("Projection:");
        mono(ui, &mut f.projection, "{ name: 1 }");
        ui.end_row();
        ui.label("Sort:");
        mono(ui, &mut f.sort, "{ _id: 1 }");
        ui.end_row();
        ui.label("Skip / limit:");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut f.skip).hint_text("0").desired_width(80.0));
            ui.add(egui::TextEdit::singleline(&mut f.limit).hint_text("all").desired_width(80.0));
        });
        ui.end_row();
        ui.label("Format:");
        ui.horizontal(|ui| {
            for format in Format::ALL {
                if ui.radio_value(&mut f.format, format, format.label()).changed() {
                    f.path = with_extension(&f.path, format);
                }
            }
        });
        ui.end_row();
        if f.format == Format::Csv {
            ui.label("Fields:");
            ui.add(egui::TextEdit::singleline(&mut f.fields).hint_text("name, address.city — empty: those of the first documents").desired_width(f32::INFINITY));
            ui.end_row();
            ui.label("Delimiter:");
            ui.horizontal(|ui| {
                for d in Delimiter::ALL {
                    ui.radio_value(&mut f.delimiter, d, d.label());
                }
            });
            ui.end_row();
        } else {
            ui.label("");
            ui.horizontal(|ui| {
                ui.checkbox(&mut f.canonical, "Canonical Extended JSON (keeps every type)");
                let info = ui.small_button(icon::INFO).on_hover_text("Canonical or relaxed: what changes");
                egui::Popup::from_toggle_button_response(&info).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(440.0).show(|ui| {
                    ui.set_max_width(440.0);
                    ui.label(RichText::new("Canonical or relaxed Extended JSON").strong());
                    ui.add_space(4.0);
                    for paragraph in EXTENDED_JSON {
                        ui.add(egui::Label::new(*paragraph).wrap());
                        ui.add_space(4.0);
                    }
                });
            });
            ui.end_row();
        }
    });
    let format = f.format;
    let current = std::path::Path::new(&f.path);
    let name = current.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let dir = current.parent().filter(|d| d.is_dir()).map(|d| d.to_path_buf());
    file_row(ui, "File:", &mut f.path, || {
        let picker = rfd::FileDialog::new().add_filter(format.label(), &[format.extension()]).add_filter("All files", &["*"]).set_file_name(name);
        match dir {
            Some(d) => picker.set_directory(d),
            None => picker,
        }
        .save_file()
    });
    if std::path::Path::new(f.path.trim()).is_file() {
        ui.label(RichText::new(format!("{} The file exists and will be replaced.", icon::INFO)).small().color(theme.text_weak));
    }
    error_line(ui, theme, &f.error);
    let (ok, cancel) = buttons(ui, "Export", !f.path.trim().is_empty());
    if cancel {
        return Outcome::Close;
    }
    if !ok {
        return Outcome::Keep;
    }
    match f.options() {
        Ok(opts) => {
            actions.push(Action::Export { server: f.server, opts: Box::new(opts) });
            Outcome::Close
        }
        Err(e) => {
            f.error = Some(e);
            Outcome::Keep
        }
    }
}

fn import_dialog(ui: &mut Ui, f: &mut ImportForm, theme: &crate::theme::Theme, actions: &mut Vec<Action>) -> Outcome {
    ui.set_width(520.0);
    title(ui, &format!("Import — {}", f.db));
    let mut path = f.path.clone();
    if file_row(ui, "File:", &mut path, || {
        rfd::FileDialog::new().add_filter("JSON or CSV", &["json", "jsonl", "ndjson", "csv", "tsv"]).add_filter("All files", &["*"]).pick_file()
    }) {
        f.set_path(path);
    }
    egui::Grid::new("import").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label("Collection:");
        if ui.add(egui::TextEdit::singleline(&mut f.coll).desired_width(f32::INFINITY)).changed() {
            f.coll_from_file = false;
        }
        ui.end_row();
        ui.label("Format:");
        ui.horizontal(|ui| {
            ui.radio_value(&mut f.format, Format::JsonLines, "JSON (array or one document per line)");
            ui.radio_value(&mut f.format, Format::Csv, "CSV");
        });
        ui.end_row();
        if f.format == Format::Csv {
            ui.label("Delimiter:");
            ui.horizontal(|ui| {
                for d in Delimiter::ALL {
                    ui.radio_value(&mut f.delimiter, d, d.label());
                }
            });
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut f.detect_types, "Detect numbers, booleans, dates and ObjectIds");
            ui.end_row();
        }
        ui.label("");
        ui.checkbox(&mut f.drop_first, "Drop the collection first");
        ui.end_row();
    });
    if f.format == Format::Csv {
        ui.label(RichText::new("The first line holds the field names; a.b makes a sub-document.").small().color(theme.text_weak));
    }
    if f.drop_first && !f.coll.trim().is_empty() {
        let warning = format!("{} \"{}\" will be replaced by the documents of the file once they are all in: its documents and indexes will be deleted.", icon::WARNING, f.coll.trim());
        ui.label(RichText::new(warning).color(theme.warning));
    }
    error_line(ui, theme, &f.error);
    let (ok, cancel) = buttons(ui, "Import", !f.path.trim().is_empty() && !f.coll.trim().is_empty());
    if cancel {
        return Outcome::Close;
    }
    if !ok {
        return Outcome::Keep;
    }
    match f.options() {
        Ok(opts) => {
            actions.push(Action::Import { server: f.server, opts: Box::new(opts) });
            Outcome::Close
        }
        Err(e) => {
            f.error = Some(e);
            Outcome::Keep
        }
    }
}

/// Folder picker, opened in `current` when it exists.
fn pick_folder(current: &str) -> Option<std::path::PathBuf> {
    let picker = rfd::FileDialog::new();
    let current = std::path::Path::new(current.trim());
    if current.is_dir() { picker.set_directory(current) } else { picker }.pick_folder()
}

/// Checkboxes of a list of collections, with All / None.
fn collection_checks<T>(ui: &mut Ui, id: &str, items: &mut [(T, bool)], label: impl Fn(&T) -> String) {
    ui.horizontal(|ui| {
        ui.label(format!("Collections ({} of {}):", items.iter().filter(|(_, on)| *on).count(), items.len()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("None").clicked() {
                items.iter_mut().for_each(|(_, on)| *on = false);
            }
            if ui.small_button("All").clicked() {
                items.iter_mut().for_each(|(_, on)| *on = true);
            }
        });
    });
    egui::Frame::new().stroke(ui.visuals().widgets.noninteractive.bg_stroke).corner_radius(4).inner_margin(4).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(id).max_height(180.0).auto_shrink([false, true]).show(ui, |ui| {
            for (item, on) in items.iter_mut() {
                ui.checkbox(on, label(item));
            }
        });
    });
}

fn dump_dialog(ui: &mut Ui, f: &mut DumpForm, theme: &crate::theme::Theme, actions: &mut Vec<Action>) -> Outcome {
    ui.set_width(520.0);
    title(ui, &format!("Dump — {}", f.db));
    let current = f.dir.clone();
    file_row(ui, "Folder:", &mut f.dir, || pick_folder(&current));
    ui.horizontal(|ui| {
        ui.label("Format:");
        ui.radio_value(&mut f.archive, false, "Folder (a file per collection)");
        ui.radio_value(&mut f.archive, true, "Archive (one file)");
    });
    let target = DumpOptions { db: f.db.clone(), collections: Vec::new(), dir: f.dir.trim().into(), gzip: f.gzip, archive: f.archive }.target();
    let what = if f.archive {
        "an archive, like mongodump --archive"
    } else {
        "a folder in the format of mongodump: a .bson and a .metadata.json file per collection"
    };
    ui.label(RichText::new(format!("Writes {}, {what}.", target.display())).small().color(theme.text_weak));
    ui.add_space(4.0);
    if f.collections.is_empty() {
        ui.label("All the collections and views of the database.");
    } else {
        collection_checks(ui, "dump-collections", &mut f.collections, |c| c.clone());
    }
    ui.checkbox(&mut f.gzip, "Compress (gzip, like mongodump --gzip)");
    if target.is_dir() {
        ui.label(RichText::new(format!("{} The folder exists: the files of the dumped collections will be replaced.", icon::INFO)).small().color(theme.text_weak));
    } else if target.is_file() {
        ui.label(RichText::new(format!("{} The file exists and will be replaced.", icon::INFO)).small().color(theme.text_weak));
    }
    error_line(ui, theme, &f.error);
    let (ok, cancel) = buttons(ui, "Dump", !f.dir.trim().is_empty());
    if cancel {
        return Outcome::Close;
    }
    if !ok {
        return Outcome::Keep;
    }
    match f.options() {
        Ok(opts) => {
            actions.push(Action::Dump { server: f.server, opts: Box::new(opts) });
            Outcome::Close
        }
        Err(e) => {
            f.error = Some(e);
            Outcome::Keep
        }
    }
}

fn restore_dialog(ui: &mut Ui, f: &mut RestoreForm, theme: &crate::theme::Theme, actions: &mut Vec<Action>) -> Outcome {
    ui.set_width(560.0);
    title(ui, &format!("Restore — {}", f.db));
    let mut from = f.from.clone();
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label("From:");
        let buttons_w = 150.0;
        changed |= ui.add(egui::TextEdit::singleline(&mut from).desired_width(ui.available_width() - buttons_w)).changed();
        // The pickers open where the current dump is.
        let current = std::path::Path::new(f.from.trim());
        let start = if current.is_dir() { Some(current) } else { current.parent().filter(|p| p.is_dir()) };
        let picker = || match start {
            Some(s) => rfd::FileDialog::new().set_directory(s),
            None => rfd::FileDialog::new(),
        };
        let (folder, archive) = (ui.button("Folder…").clicked(), ui.button("Archive…").clicked());
        let picked = if folder {
            picker().pick_folder()
        } else if archive {
            picker().add_filter("mongodump archive", &["archive", "gz", "agz"]).add_filter("All files", &["*"]).pick_file()
        } else {
            None
        };
        if let Some(p) = picked {
            from = p.display().to_string();
            changed = true;
        }
    });
    if changed {
        f.set_path(from);
    }
    ui.label(
        RichText::new("A dump of rustyrobo or mongodump: the folder of a database, with its .bson and .metadata.json files, or an archive.")
            .small()
            .color(theme.text_weak),
    );
    ui.add_space(4.0);
    egui::Grid::new("restore").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.label("Into database:");
        // An archive of several databases is read for the one of this name.
        if ui.add(egui::TextEdit::singleline(&mut f.db).desired_width(f32::INFINITY)).changed() && std::path::Path::new(f.from.trim()).is_file() {
            f.rescan();
        }
        ui.end_row();
    });
    match &mut f.found {
        Err(e) => {
            ui.label(RichText::new(format!("{} {e}", icon::WARNING)).color(theme.error));
        }
        Ok(found) if found.is_empty() => {
            if !f.from.trim().is_empty() {
                ui.label(RichText::new(format!("{} No collection in this dump.", icon::WARNING)).color(theme.warning));
            }
        }
        Ok(found) => collection_checks(ui, "restore-collections", found, |e| if e.view { format!("{} (view)", e.name) } else { e.name.clone() }),
    }
    ui.checkbox(&mut f.drop_first, "Drop each collection first");
    ui.checkbox(&mut f.indexes, "Create the indexes");
    if f.drop_first {
        let warning = format!("{} Each restored collection of \"{}\" will be replaced by its copy once complete: its documents and indexes will be deleted.", icon::WARNING, f.db.trim());
        ui.label(RichText::new(warning).color(theme.warning));
    }
    error_line(ui, theme, &f.error);
    let ready = f.found.as_ref().is_ok_and(|v| v.iter().any(|(_, on)| *on));
    let (ok, cancel) = buttons(ui, "Restore", ready);
    if cancel {
        return Outcome::Close;
    }
    if !ok {
        return Outcome::Keep;
    }
    match f.options() {
        Ok(opts) => {
            actions.push(Action::Restore { server: f.server, opts: Box::new(opts) });
            Outcome::Close
        }
        Err(e) => {
            f.error = Some(e);
            Outcome::Keep
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_form_builds_the_options() {
        let dir = std::env::temp_dir();
        let mut f = ExportForm::new(1, "shop", "orders");
        assert!(f.path.ends_with("orders.json"));
        f.filter = "{ n: { $gt: NumberLong(5) } }".into();
        f.projection = "{ n: 1 }".into();
        f.limit = " 10 ".into();
        f.fields = "a, b.c,,".into();
        f.path = dir.join("out.csv").display().to_string();
        let o = f.options().unwrap();
        assert_eq!(o.filter, doc! { "n": { "$gt": 5i64 } });
        assert_eq!((o.projection, o.sort, o.skip, o.limit), (Some(doc! { "n": 1 }), None, 0, 10));
        assert_eq!(o.fields, vec!["a", "b.c"]);
        f.filter = String::new();
        assert_eq!(f.options().unwrap().filter, Document::new(), "empty query: all the documents");
        f.sort = "{ oops".into();
        assert!(f.options().unwrap_err().starts_with("Sort:"));
        f.sort.clear();
        f.skip = "-1".into();
        assert_eq!(f.options().unwrap_err(), "Skip must be a positive number");
        f.skip.clear();
        f.path = dir.join("no-such-dir-rr").join("x.json").display().to_string();
        assert!(f.options().unwrap_err().starts_with("Folder not found"));
    }

    #[test]
    fn export_in_the_last_folder_and_format() {
        let dir = std::env::temp_dir();
        let mut f = ExportForm::new(1, "shop", "orders");
        let default = f.path.clone();
        f.set_defaults(&ExportDefaults::default());
        assert_eq!((f.path.as_str(), f.format), (default.as_str(), Format::JsonLines), "no export yet");
        assert!(f.canonical, "canonical by default: it keeps every type");
        // Settings saved before the export remembered its options.
        assert!(serde_json::from_str::<ExportDefaults>(r#"{"dir":"/tmp"}"#).unwrap().canonical);
        assert!(!serde_json::from_str::<ExportDefaults>(r#"{"canonical":false}"#).unwrap().canonical, "the choice of the last export");
        f.set_defaults(&ExportDefaults { dir: dir.join("no-such-dir-rr").display().to_string(), ..Default::default() });
        assert_eq!(f.path, default, "removed folder");
        let last = ExportDefaults { dir: dir.display().to_string(), format: Format::Csv, canonical: false, delimiter: Delimiter::Semicolon };
        f.set_defaults(&last);
        assert_eq!(f.path, dir.join("orders.csv").display().to_string());
        assert_eq!((f.format, f.delimiter), (Format::Csv, Delimiter::Semicolon));
        f.set_defaults(&ExportDefaults { format: Format::JsonArray, canonical: true, ..last });
        assert!(f.canonical && f.path.ends_with("orders.json"));
    }

    #[test]
    fn export_of_a_query_keeps_its_parts() {
        let spec = QuerySpec {
            db: "shop".into(),
            coll: "orders".into(),
            filter: doc! { "status": "paid" },
            projection: Some(doc! { "total": 1 }),
            sort: Some(doc! { "total": -1 }),
            skip: 20,
            limit: 5,
            hint: None,
            collation: None,
        };
        let Dialog::Export(f) = Dialog::export_query(3, &spec) else { panic!() };
        let o = f.options().unwrap();
        assert_eq!((o.db.as_str(), o.coll.as_str(), o.skip, o.limit), ("shop", "orders", 20, 5));
        assert_eq!((o.filter, o.projection, o.sort), (spec.filter, spec.projection, spec.sort));
        assert_eq!(f.projection, "{ \"total\" : 1 }", "one line");
    }

    #[test]
    fn file_names_follow_the_format() {
        assert_eq!(with_extension("/tmp/a.json", Format::Csv), "/tmp/a.csv");
        assert_eq!(with_extension("/tmp/a.CSV", Format::JsonArray), "/tmp/a.json");
        assert_eq!(with_extension("/tmp/a.txt", Format::Csv), "/tmp/a.txt", "a chosen extension is kept");
        assert!(default_path("a/b c", "csv").ends_with("a_b_c.csv"));
        let mut f = ImportForm::new(1, "shop", None);
        f.set_path("/data/customers.tsv".into());
        assert_eq!((f.format, f.delimiter, f.coll.as_str()), (Format::Csv, Delimiter::Tab, "customers"));
        f.coll_from_file = false;
        f.set_path("/data/other.json".into());
        assert_eq!((f.format, f.coll.as_str()), (Format::JsonLines, "customers"), "typed name kept");
        assert_eq!(f.options().unwrap_err(), "File not found: /data/other.json");
        let mut f = ImportForm::new(1, "shop", Some("orders"));
        f.set_path("/data/x.json".into());
        assert_eq!(f.coll, "orders", "import into a chosen collection");
        f.coll = "a$b".into();
        assert!(f.options().unwrap_err().starts_with("Invalid collection name"));
    }
}
