//! Persistent application settings and connection definitions.
//!
//! Stored as JSON in the platform config dir (`~/.config/rustyrobo/settings.json`
//! on Linux). The file is written with 0600 permissions since it may contain
//! passwords.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConnectionType {
    #[default]
    Direct,
    ReplicaSet,
    /// `mongodb+srv://` — hosts are resolved through DNS SRV records.
    Srv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AuthMechanism {
    /// Let the driver negotiate (SCRAM-SHA-256 then SCRAM-SHA-1).
    #[default]
    Default,
    ScramSha1,
    ScramSha256,
    X509,
    Plain,
}

impl AuthMechanism {
    pub const ALL: [AuthMechanism; 5] = [
        AuthMechanism::Default,
        AuthMechanism::ScramSha1,
        AuthMechanism::ScramSha256,
        AuthMechanism::X509,
        AuthMechanism::Plain,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AuthMechanism::Default => "Auto (SCRAM negotiated)",
            AuthMechanism::ScramSha1 => "SCRAM-SHA-1",
            AuthMechanism::ScramSha256 => "SCRAM-SHA-256",
            AuthMechanism::X509 => "MONGODB-X509",
            AuthMechanism::Plain => "PLAIN (LDAP)",
        }
    }

    pub fn uri_name(self) -> Option<&'static str> {
        match self {
            AuthMechanism::Default => None,
            AuthMechanism::ScramSha1 => Some("SCRAM-SHA-1"),
            AuthMechanism::ScramSha256 => Some("SCRAM-SHA-256"),
            AuthMechanism::X509 => Some("MONGODB-X509"),
            AuthMechanism::Plain => Some("PLAIN"),
        }
    }

    pub fn from_uri_name(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "SCRAM-SHA-1" | "MONGODB-CR" => AuthMechanism::ScramSha1,
            "SCRAM-SHA-256" => AuthMechanism::ScramSha256,
            "MONGODB-X509" => AuthMechanism::X509,
            "PLAIN" => AuthMechanism::Plain,
            _ => AuthMechanism::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SshAuthMethod {
    #[default]
    Password,
    PrivateKey,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReadPreference {
    #[default]
    Primary,
    PrimaryPreferred,
    Secondary,
    SecondaryPreferred,
    Nearest,
}

impl ReadPreference {
    pub const ALL: [ReadPreference; 5] = [
        ReadPreference::Primary,
        ReadPreference::PrimaryPreferred,
        ReadPreference::Secondary,
        ReadPreference::SecondaryPreferred,
        ReadPreference::Nearest,
    ];

    pub fn uri_name(self) -> &'static str {
        match self {
            ReadPreference::Primary => "primary",
            ReadPreference::PrimaryPreferred => "primaryPreferred",
            ReadPreference::Secondary => "secondary",
            ReadPreference::SecondaryPreferred => "secondaryPreferred",
            ReadPreference::Nearest => "nearest",
        }
    }

    pub fn from_uri_name(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|r| r.uri_name().eq_ignore_ascii_case(s))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Credentials {
    pub enabled: bool,
    pub database: String,
    pub username: String,
    pub password: String,
    /// Prompt for the password at connect time instead of storing it.
    pub ask_password: bool,
    pub mechanism: AuthMechanism,
}

impl Default for Credentials {
    fn default() -> Self {
        Self {
            enabled: false,
            database: "admin".into(),
            username: String::new(),
            password: String::new(),
            ask_password: false,
            mechanism: AuthMechanism::Default,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub method: SshAuthMethod,
    pub password: String,
    pub private_key_file: String,
    pub passphrase: String,
    pub ask_password: bool,
    /// Reject servers whose host key is not already in known_hosts.
    pub strict_host_key: bool,
    pub keepalive_secs: u32,
}

impl Default for SshSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 22,
            username: String::new(),
            method: SshAuthMethod::Password,
            password: String::new(),
            private_key_file: default_private_key(),
            passphrase: String::new(),
            ask_password: false,
            strict_host_key: false,
            keepalive_secs: 30,
        }
    }
}

fn default_private_key() -> String {
    directories::BaseDirs::new()
        .map(|d| {
            let ssh = d.home_dir().join(".ssh");
            ["id_ed25519", "id_ecdsa", "id_rsa"]
                .iter()
                .map(|k| ssh.join(k))
                .find(|p| p.exists())
                .unwrap_or_else(|| ssh.join("id_ed25519"))
                .display()
                .to_string()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TlsSettings {
    pub enabled: bool,
    pub ca_file: String,
    pub cert_key_file: String,
    pub cert_key_password: String,
    pub allow_invalid_certificates: bool,
    pub allow_invalid_hostnames: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

impl Default for HostPort {
    fn default() -> Self {
        Self { host: "localhost".into(), port: 27017 }
    }
}

impl HostPort {
    pub fn display(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionSettings {
    pub id: uuid::Uuid,
    pub name: String,
    /// Free-form group used to organise the connection list.
    pub group: String,
    /// Accent colour tag (RGB) shown in the list and in the explorer.
    pub color: Option<[u8; 3]>,
    pub kind: ConnectionType,
    /// Direct: a single entry. Replica set: the seed list. SRV: host only.
    pub hosts: Vec<HostPort>,
    pub replica_set_name: String,
    pub read_preference: ReadPreference,
    pub credentials: Credentials,
    pub ssh: SshSettings,
    pub tls: TlsSettings,
    pub default_database: String,
    pub use_visible_databases: bool,
    pub visible_databases: Vec<String>,
    pub connect_timeout_secs: u32,
    pub app_name: String,
    /// Extra `key=value&…` options appended to the generated URI.
    pub extra_options: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_connected: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            name: "New Connection".into(),
            group: String::new(),
            color: None,
            kind: ConnectionType::Direct,
            hosts: vec![HostPort::default()],
            replica_set_name: String::new(),
            read_preference: ReadPreference::Primary,
            credentials: Credentials::default(),
            ssh: SshSettings::default(),
            tls: TlsSettings::default(),
            default_database: String::new(),
            use_visible_databases: false,
            visible_databases: Vec::new(),
            connect_timeout_secs: 10,
            app_name: "rustyrobo".into(),
            extra_options: String::new(),
            last_connected: None,
        }
    }
}

impl ConnectionSettings {
    pub fn address_label(&self) -> String {
        match self.kind {
            ConnectionType::Direct => self.hosts.first().map(HostPort::display).unwrap_or_default(),
            ConnectionType::Srv => format!(
                "srv://{}",
                self.hosts.first().map(|h| h.host.as_str()).unwrap_or_default()
            ),
            ConnectionType::ReplicaSet => {
                let set = if self.replica_set_name.is_empty() { "?" } else { &self.replica_set_name };
                let first = self.hosts.first().map(HostPort::display).unwrap_or_default();
                if self.hosts.len() > 1 {
                    format!("{set} [{first}, +{}]", self.hosts.len() - 1)
                } else {
                    format!("{set} [{first}]")
                }
            }
        }
    }

    pub fn auth_label(&self) -> String {
        if self.credentials.enabled {
            format!("{} / {}", self.credentials.database, self.credentials.username)
        } else {
            String::new()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ViewMode {
    #[default]
    Tree,
    Table,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TimeZoneMode {
    #[default]
    Utc,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UuidEncoding {
    #[default]
    Default,
    JavaLegacy,
    CSharpLegacy,
    PythonLegacy,
}

impl UuidEncoding {
    pub const ALL: [UuidEncoding; 4] = [
        UuidEncoding::Default,
        UuidEncoding::JavaLegacy,
        UuidEncoding::CSharpLegacy,
        UuidEncoding::PythonLegacy,
    ];

    pub fn label(self) -> &'static str {
        match self {
            UuidEncoding::Default => "Do not decode (show as is)",
            UuidEncoding::JavaLegacy => "Use Java Encoding",
            UuidEncoding::CSharpLegacy => "Use .NET Encoding",
            UuidEncoding::PythonLegacy => "Use Python Encoding",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AutocompletionMode {
    #[default]
    All,
    NoCollectionNames,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Orientation {
    #[default]
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub version: u32,
    pub connections: Vec<ConnectionSettings>,
    pub theme: String,
    pub view_mode: ViewMode,
    pub time_zone: TimeZoneMode,
    pub uuid_encoding: UuidEncoding,
    pub autocompletion: AutocompletionMode,
    pub load_mongorc: bool,
    pub auto_expand: bool,
    pub line_numbers: bool,
    pub auto_exec: bool,
    pub disable_connection_shortcuts: bool,
    pub shell_timeout_secs: u32,
    pub batch_size: u32,
    pub font_size: f32,
    pub orientation: Orientation,
    pub show_explorer: bool,
    pub show_logs: bool,
    pub tabbed_results: bool,
    pub favorites: Vec<FavoriteScript>,
}

/// Script saved in the favorite scripts manager, filed under a connection
/// and a database. Scripts are listed in the order of the list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FavoriteScript {
    pub id: uuid::Uuid,
    pub name: String,
    /// `None`: loaded on the active connection.
    pub connection: Option<uuid::Uuid>,
    pub db: String,
    pub script: String,
}

impl Default for FavoriteScript {
    fn default() -> Self {
        Self { id: uuid::Uuid::new_v4(), name: String::new(), connection: None, db: String::new(), script: String::new() }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            version: 1,
            connections: Vec::new(),
            theme: crate::theme::DEFAULT_THEME.into(),
            view_mode: ViewMode::Tree,
            time_zone: TimeZoneMode::Utc,
            uuid_encoding: UuidEncoding::Default,
            autocompletion: AutocompletionMode::All,
            load_mongorc: true,
            auto_expand: true,
            line_numbers: false,
            auto_exec: true,
            disable_connection_shortcuts: false,
            shell_timeout_secs: 15,
            batch_size: 50,
            font_size: 13.0,
            orientation: Orientation::Vertical,
            show_explorer: true,
            show_logs: false,
            tabbed_results: false,
            favorites: Vec::new(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RUSTYROBO_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("org", "rustyrobo", "rustyrobo")
        .map(|p| p.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".rustyrobo"))
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

impl AppSettings {
    pub fn load() -> Self {
        let path = settings_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("rustyrobo: cannot parse {}: {e}", path.display());
                let _ = std::fs::copy(&path, path.with_extension("json.bak"));
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn connection(&self, id: uuid::Uuid) -> Option<&ConnectionSettings> {
        self.connections.iter().find(|c| c.id == id)
    }
}

/// Finds Robo 3T / Robomongo configuration files in the home directory,
/// newest version first.
pub fn find_robo3t_configs() -> Vec<PathBuf> {
    let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (dir, file) in [(".3T/robo-3t", "robo3t.json"), (".3T/robomongo", "robomongo.json")] {
        if let Ok(entries) = std::fs::read_dir(home.join(dir)) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path().join(file))
                .filter(|p| p.exists())
                .collect();
            versions.sort();
            versions.reverse();
            found.extend(versions);
        }
    }
    for legacy in [".config/robomongo/robomongo.json", ".config/robomongo/0.9/robomongo.json"] {
        let p = home.join(legacy);
        if p.exists() {
            found.push(p);
        }
    }
    found
}

/// Imports connections from a Robo 3T configuration file.
///
/// Robo 3T encrypts stored passwords with a per-install key, so encrypted
/// passwords cannot be recovered and are left empty.
pub fn import_robo3t(path: &std::path::Path) -> anyhow::Result<Vec<ConnectionSettings>> {
    use serde_json::Value;
    let root: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let b = |v: &Value, k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    let n = |v: &Value, k: &str, d: u64| v.get(k).and_then(Value::as_u64).unwrap_or(d);

    let mut out = Vec::new();
    for c in root.get("connections").and_then(Value::as_array).into_iter().flatten() {
        let mut conn = ConnectionSettings {
            name: s(c, "connectionName"),
            default_database: s(c, "defaultDatabase"),
            ..Default::default()
        };
        if conn.name.is_empty() {
            conn.name = "Imported connection".into();
        }
        conn.hosts = vec![HostPort {
            host: s(c, "serverHost"),
            port: n(c, "serverPort", 27017) as u16,
        }];
        if b(c, "isReplicaSet") {
            conn.kind = ConnectionType::ReplicaSet;
            if let Some(rs) = c.get("replicaSet") {
                conn.replica_set_name = s(rs, "setNameUserEntered");
                if conn.replica_set_name.is_empty() {
                    conn.replica_set_name = s(rs, "cachedSetName");
                }
                let members: Vec<HostPort> = rs
                    .get("members")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter_map(parse_host_port)
                    .collect();
                if !members.is_empty() {
                    conn.hosts = members;
                }
                conn.read_preference = match n(rs, "readPreference", 0) {
                    1 => ReadPreference::PrimaryPreferred,
                    _ => ReadPreference::Primary,
                };
            }
        }
        if let Some(cred) = c.get("credentials").and_then(Value::as_array).and_then(|a| a.first()) {
            conn.credentials = Credentials {
                enabled: b(cred, "enabled"),
                database: s(cred, "databaseName"),
                username: s(cred, "userName"),
                password: s(cred, "userPassword"),
                ask_password: false,
                mechanism: AuthMechanism::from_uri_name(&s(cred, "mechanism")),
            };
        }
        if let Some(ssh) = c.get("ssh") {
            conn.ssh = SshSettings {
                enabled: b(ssh, "enabled"),
                host: s(ssh, "host"),
                port: n(ssh, "port", 22) as u16,
                username: s(ssh, "userName"),
                method: if s(ssh, "method") == "publickey" {
                    SshAuthMethod::PrivateKey
                } else {
                    SshAuthMethod::Password
                },
                password: s(ssh, "userPassword"),
                private_key_file: s(ssh, "privateKeyFile"),
                passphrase: s(ssh, "passphrase"),
                ask_password: b(ssh, "askPassword"),
                ..Default::default()
            };
        }
        if let Some(ssl) = c.get("ssl") {
            conn.tls = TlsSettings {
                enabled: b(ssl, "sslEnabled") || b(ssl, "enabled"),
                ca_file: s(ssl, "caFile"),
                cert_key_file: s(ssl, "pemKeyFile"),
                cert_key_password: String::new(),
                allow_invalid_certificates: b(ssl, "allowInvalidCertificates"),
                allow_invalid_hostnames: b(ssl, "allowInvalidHostnames"),
            };
        }
        conn.use_visible_databases = b(c, "useManuallyVisibleDbs");
        if let Some(dbs) = c.get("manuallyVisibleDbs").and_then(Value::as_array) {
            conn.visible_databases = dbs.iter().filter_map(Value::as_str).map(String::from).collect();
        }
        out.push(conn);
    }
    Ok(out)
}

pub fn parse_host_port(s: &str) -> Option<HostPort> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(27017);
        return Some(HostPort { host: host.to_string(), port });
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => Some(HostPort {
            host: h.to_string(),
            port: p.parse().ok()?,
        }),
        _ => Some(HostPort { host: s.to_string(), port: 27017 }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_parsing() {
        assert_eq!(parse_host_port("db:27018").unwrap(), HostPort { host: "db".into(), port: 27018 });
        assert_eq!(parse_host_port("db").unwrap(), HostPort { host: "db".into(), port: 27017 });
        assert_eq!(parse_host_port("[::1]:1234").unwrap(), HostPort { host: "::1".into(), port: 1234 });
        assert_eq!(parse_host_port("[fe80::1]").unwrap().port, 27017);
        assert_eq!(parse_host_port("::1").unwrap().host, "::1");
        assert!(parse_host_port("   ").is_none());
        assert!(parse_host_port("db:notaport").is_none());
    }

    #[test]
    fn host_display_brackets_ipv6() {
        assert_eq!(HostPort { host: "::1".into(), port: 1 }.display(), "[::1]:1");
        assert_eq!(HostPort { host: "a".into(), port: 2 }.display(), "a:2");
    }

    #[test]
    fn labels() {
        let mut c = ConnectionSettings::default();
        assert_eq!(c.address_label(), "localhost:27017");
        assert_eq!(c.auth_label(), "");
        c.credentials.enabled = true;
        c.credentials.username = "bob".into();
        assert_eq!(c.auth_label(), "admin / bob");
        c.kind = ConnectionType::ReplicaSet;
        c.replica_set_name = "rs0".into();
        c.hosts.push(HostPort { host: "b".into(), port: 1 });
        assert_eq!(c.address_label(), "rs0 [localhost:27017, +1]");
        c.kind = ConnectionType::Srv;
        c.hosts = vec![HostPort { host: "cluster.example.net".into(), port: 27017 }];
        assert_eq!(c.address_label(), "srv://cluster.example.net");
    }

    #[test]
    fn enums_round_trip_through_uri_names() {
        for m in AuthMechanism::ALL {
            if let Some(n) = m.uri_name() {
                assert_eq!(AuthMechanism::from_uri_name(n), m);
            }
        }
        assert_eq!(AuthMechanism::from_uri_name("mongodb-cr"), AuthMechanism::ScramSha1);
        for r in ReadPreference::ALL {
            assert_eq!(ReadPreference::from_uri_name(r.uri_name()), r);
        }
        assert_eq!(ReadPreference::from_uri_name("bogus"), ReadPreference::Primary);
    }

    #[test]
    fn settings_tolerate_missing_fields() {
        let s: AppSettings = serde_json::from_str(r#"{"connections":[{"name":"x"}]}"#).unwrap();
        assert_eq!(s.connections[0].name, "x");
        assert_eq!(s.batch_size, 50);
        assert_eq!(s.connections[0].hosts[0].port, 27017);
        let text = serde_json::to_string(&s).unwrap();
        let back: AppSettings = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn imports_robo3t_config() {
        let json = r#"{
          "connections": [
            {"connectionName": "prod", "serverHost": "db1", "serverPort": 27018, "defaultDatabase": "app",
             "credentials": [{"enabled": true, "databaseName": "admin", "userName": "bob", "mechanism": "SCRAM-SHA-256"}],
             "ssh": {"enabled": true, "host": "bastion", "port": 2200, "userName": "me", "method": "publickey", "privateKeyFile": "/k"},
             "ssl": {"sslEnabled": true, "caFile": "/ca.pem", "allowInvalidHostnames": true},
             "useManuallyVisibleDbs": true, "manuallyVisibleDbs": ["app", "logs"]},
            {"connectionName": "", "isReplicaSet": true, "serverHost": "x", "serverPort": 1,
             "replicaSet": {"setNameUserEntered": "rs0", "members": ["h1:27017", "h2:27018"], "readPreference": 1}}
          ]}"#;
        let dir = std::env::temp_dir().join(format!("rr-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("robo3t.json");
        std::fs::write(&path, json).unwrap();
        let list = import_robo3t(&path).unwrap();
        assert_eq!(list.len(), 2);
        let a = &list[0];
        assert_eq!(a.name, "prod");
        assert_eq!(a.hosts[0], HostPort { host: "db1".into(), port: 27018 });
        assert!(a.credentials.enabled);
        assert_eq!(a.credentials.mechanism, AuthMechanism::ScramSha256);
        assert!(a.ssh.enabled);
        assert_eq!(a.ssh.method, SshAuthMethod::PrivateKey);
        assert_eq!(a.ssh.port, 2200);
        assert!(a.tls.enabled && a.tls.allow_invalid_hostnames);
        assert_eq!(a.visible_databases, vec!["app", "logs"]);
        let b = &list[1];
        assert_eq!(b.name, "Imported connection");
        assert_eq!(b.kind, ConnectionType::ReplicaSet);
        assert_eq!(b.replica_set_name, "rs0");
        assert_eq!(b.hosts.len(), 2);
        assert_eq!(b.read_preference, ReadPreference::PrimaryPreferred);
        let _ = std::fs::remove_dir_all(dir);
    }
}
