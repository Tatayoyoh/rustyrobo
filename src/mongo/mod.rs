//! MongoDB connectivity.
//!
//! Everything the application does goes through `runCommand` (find, getMore,
//! insert, listCollections…), which keeps the two driver backends small:
//! - `Modern`: driver 3.x, MongoDB 4.4 → 8.x
//! - `Legacy`: driver 2.x, MongoDB 3.6 → 4.2 (selected automatically when the
//!   server reports a wire version the modern driver refuses).

pub mod ssh;
pub mod uri;

use std::time::Instant;

use anyhow::{Context as _, anyhow};
use bson::{Bson, Document, doc};

use crate::settings::{ConnectionSettings, ConnectionType};
use ssh::{HostKeyStatus, SshSecrets, SshTunnel};

/// Secrets typed by the user at connect time ("ask each time" options).
#[derive(Debug, Clone, Default)]
pub struct ConnectSecrets {
    pub mongo_password: Option<String>,
    pub ssh: SshSecrets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Ssh,
    Server,
    Auth,
    Databases,
}

impl StepKind {
    pub fn label(self) -> &'static str {
        match self {
            StepKind::Ssh => "SSH tunnel",
            StepKind::Server => "Connect to server",
            StepKind::Auth => "Authentication",
            StepKind::Databases => "List databases",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StepReport {
    pub kind: StepKind,
    pub ok: bool,
    pub message: String,
    pub millis: u128,
}

pub enum Backend {
    Modern(mongodb::Client),
    Legacy(mongodb_legacy::Client),
}

pub enum Session {
    Modern(mongodb::ClientSession),
    Legacy(mongodb_legacy::ClientSession),
}

pub struct Connection {
    pub backend: Backend,
    pub tunnel: Option<SshTunnel>,
    pub version: String,
    pub wire_version: i32,
    pub set_name: Option<String>,
    pub is_mongos: bool,
    pub databases: Vec<String>,
}

impl Connection {
    /// Connects and validates the connection, reporting each step.
    pub async fn open(
        conn: &ConnectionSettings,
        secrets: &ConnectSecrets,
        mut report: impl FnMut(StepReport),
    ) -> anyhow::Result<Connection> {
        let mut step = |kind: StepKind, started: Instant, res: Result<String, String>| {
            report(StepReport {
                kind,
                ok: res.is_ok(),
                message: match res {
                    Ok(m) | Err(m) => m,
                },
                millis: started.elapsed().as_millis(),
            })
        };

        let mut tunnel = None;
        if conn.ssh.enabled {
            let t0 = Instant::now();
            if conn.kind != ConnectionType::Direct && conn.hosts.len() > 1 {
                // Only the first member is reachable through a single tunnel.
                step(StepKind::Ssh, t0, Ok("Replica set via SSH: tunnelling to the first member only".into()));
            }
            let target = conn.hosts.first().cloned().unwrap_or_default();
            match SshTunnel::open(&conn.ssh, &secrets.ssh, &target).await {
                Ok(t) => {
                    let detail = match &t.host_key {
                        HostKeyStatus::Known => String::new(),
                        HostKeyStatus::Learned { fingerprint } => {
                            format!(" — new host key recorded ({fingerprint})")
                        }
                    };
                    step(
                        StepKind::Ssh,
                        t0,
                        Ok(format!(
                            "{}@{}:{} to {} via 127.0.0.1:{}{detail}",
                            conn.ssh.username,
                            conn.ssh.host,
                            conn.ssh.port,
                            target.display(),
                            t.local_port
                        )),
                    );
                    tunnel = Some(t);
                }
                Err(e) => {
                    step(StepKind::Ssh, t0, Err(format!("{e:#}")));
                    return Err(e);
                }
            }
        }

        let uri = uri::build_uri(
            conn,
            secrets.mongo_password.as_deref(),
            tunnel.as_ref().map(|t| t.local_port),
            uri::Secrets::Include,
        );

        let t0 = Instant::now();
        let (backend, hello) = match connect_modern(&uri, conn).await {
            Ok(ok) => ok,
            Err(e) if is_wire_version_error(&e) => connect_legacy(&uri).await.inspect_err(|e| {
                step(StepKind::Server, t0, Err(error_message(e)));
            })?,
            Err(e) => {
                let msg = error_message(&e);
                let kind = if is_auth_error(&msg) { StepKind::Auth } else { StepKind::Server };
                step(kind, t0, Err(msg));
                return Err(e);
            }
        };

        let wire_version = hello.get("maxWireVersion").and_then(bson_as_i64).unwrap_or(0) as i32;
        let set_name = hello.get_str("setName").ok().map(String::from);
        let is_mongos = hello.get_str("msg").ok() == Some("isdbgrid");

        let mut connection = Connection {
            backend,
            tunnel,
            version: String::new(),
            wire_version,
            set_name,
            is_mongos,
            databases: Vec::new(),
        };
        let build = connection.run_command("admin", doc! { "buildInfo": 1 }, None).await;
        connection.version = build
            .ok()
            .and_then(|b| b.get_str("version").ok().map(String::from))
            .unwrap_or_else(|| "?".into());
        let topo = match (&connection.set_name, connection.is_mongos) {
            (_, true) => " (mongos)".to_string(),
            (Some(rs), _) => format!(" (replica set {rs})"),
            _ => String::new(),
        };
        let backend_name = match connection.backend {
            Backend::Modern(_) => "",
            Backend::Legacy(_) => " — legacy driver",
        };
        step(
            StepKind::Server,
            t0,
            Ok(format!("MongoDB {}{topo}{backend_name}", connection.version)),
        );

        if conn.credentials.enabled {
            let t1 = Instant::now();
            // The driver authenticates when the first connection is made, so
            // reaching this point means it succeeded.
            match connection.run_command("admin", doc! { "connectionStatus": 1 }, None).await {
                Ok(st) => {
                    let users = st
                        .get_document("authInfo")
                        .ok()
                        .and_then(|a| a.get_array("authenticatedUsers").ok())
                        .map(|u| u.len())
                        .unwrap_or(0);
                    step(
                        StepKind::Auth,
                        t1,
                        Ok(format!(
                            "{} on '{}' ({} user{})",
                            conn.credentials.username,
                            conn.credentials.database,
                            users,
                            if users == 1 { "" } else { "s" }
                        )),
                    );
                }
                Err(e) => {
                    step(StepKind::Auth, t1, Err(error_message(&e)));
                    return Err(e);
                }
            }
        }

        let t2 = Instant::now();
        let dbs = connection.list_database_names(conn).await;
        match &dbs {
            Ok(list) => step(StepKind::Databases, t2, Ok(format!("{} database(s)", list.len()))),
            Err(e) => step(StepKind::Databases, t2, Err(error_message(e))),
        }
        connection.databases = dbs?;
        Ok(connection)
    }

    pub fn is_legacy(&self) -> bool {
        matches!(self.backend, Backend::Legacy(_))
    }

    pub async fn run_command(
        &self,
        db: &str,
        command: Document,
        session: Option<&mut Session>,
    ) -> anyhow::Result<Document> {
        match (&self.backend, session) {
            (Backend::Modern(c), Some(Session::Modern(s))) => {
                Ok(c.database(db).run_command(command).session(s).await?)
            }
            (Backend::Modern(c), _) => Ok(c.database(db).run_command(command).await?),
            (Backend::Legacy(c), Some(Session::Legacy(s))) => {
                Ok(c.database(db).run_command_with_session(command, None, s).await?)
            }
            (Backend::Legacy(c), _) => Ok(c.database(db).run_command(command, None).await?),
        }
    }

    pub async fn start_session(&self) -> anyhow::Result<Session> {
        Ok(match &self.backend {
            Backend::Modern(c) => Session::Modern(c.start_session().await?),
            Backend::Legacy(c) => Session::Legacy(c.start_session(None).await?),
        })
    }

    pub async fn list_database_names(&self, conn: &ConnectionSettings) -> anyhow::Result<Vec<String>> {
        if conn.use_visible_databases && !conn.visible_databases.is_empty() {
            return Ok(conn.visible_databases.clone());
        }
        let res = self
            .run_command(
                "admin",
                doc! { "listDatabases": 1, "nameOnly": true, "authorizedDatabases": true },
                None,
            )
            .await;
        match res {
            Ok(d) => {
                let mut names: Vec<String> = d
                    .get_array("databases")
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_document()?.get_str("name").ok().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                names.sort();
                Ok(names)
            }
            Err(e) if is_unauthorized(&e) => {
                // Same fallback as Robo 3T: show the db the user can see.
                let mut v = Vec::new();
                for name in [&conn.default_database, &conn.credentials.database] {
                    if !name.is_empty() && !v.contains(name) {
                        v.push(name.clone());
                    }
                }
                if v.is_empty() {
                    Err(e).context("listDatabases is not authorized: specify visible databases in the connection settings")
                } else {
                    Ok(v)
                }
            }
            Err(e) => Err(e),
        }
    }

    pub fn shutdown(&self) {
        match &self.backend {
            Backend::Modern(c) => {
                let c = c.clone();
                tokio::spawn(async move { c.shutdown().immediate(true).await });
            }
            Backend::Legacy(_) => {}
        }
        if let Some(t) = &self.tunnel {
            t.close();
        }
    }
}

async fn connect_modern(uri: &str, conn: &ConnectionSettings) -> anyhow::Result<(Backend, Document)> {
    let mut opts = mongodb::options::ClientOptions::parse(uri).await?;
    if let Some(mongodb::options::Tls::Enabled(t)) = opts.tls.as_mut()
        && !conn.tls.cert_key_password.is_empty()
    {
        t.tls_certificate_key_file_password = Some(conn.tls.cert_key_password.clone().into_bytes());
    }
    let client = mongodb::Client::with_options(opts)?;
    let admin = client.database("admin");
    let hello = match admin.run_command(doc! { "hello": 1 }).await {
        Ok(h) => h,
        Err(e) if is_wire_version_error(&anyhow!("{e}")) => return Err(e.into()),
        Err(_) => admin.run_command(doc! { "isMaster": 1 }).await?,
    };
    Ok((Backend::Modern(client), hello))
}

async fn connect_legacy(uri: &str) -> anyhow::Result<(Backend, Document)> {
    let opts = mongodb_legacy::options::ClientOptions::parse(uri).await?;
    let client = mongodb_legacy::Client::with_options(opts)?;
    let hello = client
        .database("admin")
        .run_command(doc! { "isMaster": 1 }, None)
        .await?;
    Ok((Backend::Legacy(client), hello))
}

fn is_wire_version_error(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}").to_ascii_lowercase();
    s.contains("wire version") || s.contains("wireversion")
}

fn is_auth_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("authentication failed") || m.contains("auth error") || m.contains("scram")
}

pub fn is_unauthorized(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}");
    s.contains("Unauthorized") || s.contains("not authorized") || s.contains("code 13")
}

pub fn bson_as_i64(b: &Bson) -> Option<i64> {
    match b {
        Bson::Int32(i) => Some(*i as i64),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) => Some(*d as i64),
        _ => None,
    }
}

pub fn bson_as_f64(b: &Bson) -> Option<f64> {
    match b {
        Bson::Int32(i) => Some(*i as f64),
        Bson::Int64(i) => Some(*i as f64),
        Bson::Double(d) => Some(*d),
        Bson::Decimal128(d) => d.to_string().parse().ok(),
        _ => None,
    }
}

/// Runs a cursor-returning command (find/aggregate/listCollections…) and
/// drains up to `max` documents, killing the cursor afterwards.
pub async fn collect_cursor(
    c: &Connection,
    db: &str,
    cmd: Document,
    max: usize,
    mut session: Option<&mut Session>,
) -> anyhow::Result<Vec<Document>> {
    let res = c.run_command(db, cmd, session.as_deref_mut()).await?;
    let cursor = res.get_document("cursor").context("command did not return a cursor")?;
    let mut docs: Vec<Document> = cursor
        .get_array("firstBatch")
        .map(|a| a.iter().filter_map(|d| d.as_document().cloned()).collect())
        .unwrap_or_default();
    let ns = cursor.get_str("ns").unwrap_or_default().to_string();
    let coll = ns.split_once('.').map(|(_, c)| c.to_string()).unwrap_or_default();
    let mut id = cursor.get("id").and_then(bson_as_i64).unwrap_or(0);
    while id != 0 && docs.len() < max {
        let more = c
            .run_command(db, doc! { "getMore": id, "collection": &coll }, session.as_deref_mut())
            .await?;
        let cur = more.get_document("cursor")?;
        docs.extend(
            cur.get_array("nextBatch")
                .map(|a| a.iter().filter_map(|d| d.as_document().cloned()).collect::<Vec<_>>())
                .unwrap_or_default(),
        );
        id = cur.get("id").and_then(bson_as_i64).unwrap_or(0);
    }
    if id != 0 {
        let _ = c
            .run_command(db, doc! { "killCursors": &coll, "cursors": [id] }, session)
            .await;
    }
    docs.truncate(max);
    Ok(docs)
}

/// Short, user-facing message for a driver error: `BadValue (2): unknown
/// top level operator…` instead of the driver's debug dump.
pub fn error_message(e: &anyhow::Error) -> String {
    if let Some(me) = e.downcast_ref::<mongodb::error::Error>() {
        use mongodb::error::ErrorKind as K;
        return match me.kind.as_ref() {
            K::Command(c) => format!("{} ({}): {}", c.code_name, c.code, c.message),
            K::Authentication { message, .. } => format!("Authentication failed: {message}"),
            K::ServerSelection { message, .. } => format!("Server selection timeout: {}", first_line(message)),
            K::Io(io) => format!("I/O error: {io}"),
            _ => first_line(&me.to_string()),
        };
    }
    if let Some(me) = e.downcast_ref::<mongodb_legacy::error::Error>() {
        use mongodb_legacy::error::ErrorKind as K;
        return match me.kind.as_ref() {
            K::Command(c) => format!("{} ({}): {}", c.code_name, c.code, c.message),
            K::Authentication { message, .. } => format!("Authentication failed: {message}"),
            K::ServerSelection { message, .. } => format!("Server selection timeout: {}", first_line(message)),
            K::Io(io) => format!("I/O error: {io}"),
            _ => first_line(&me.to_string()),
        };
    }
    format!("{e:#}")
}

fn first_line(s: &str) -> String {
    let s = s.split(", labels:").next().unwrap_or(s);
    s.chars().take(600).collect()
}
