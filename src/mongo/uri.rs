//! Conversion between [`ConnectionSettings`] and MongoDB connection strings.

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};

use crate::settings::{
    AuthMechanism, ConnectionSettings, ConnectionType, Credentials, HostPort, ReadPreference,
    parse_host_port,
};

/// Characters that must be escaped in the userinfo part of a URI.
const USERINFO: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b':')
    .add(b'/')
    .add(b'?')
    .add(b'#')
    .add(b'[')
    .add(b']')
    .add(b'@')
    .add(b'%')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'\\')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

fn enc(s: &str) -> String {
    utf8_percent_encode(s, USERINFO).to_string()
}

fn dec(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Secrets {
    Include,
    Mask,
    Omit,
}

/// Builds the URI for `conn`.
///
/// `tunnel` replaces the host list with the local end of an SSH tunnel; the
/// connection is then forced to `directConnection=true` because replica-set
/// members advertise addresses that are not reachable through the tunnel.
pub fn build_uri(
    conn: &ConnectionSettings,
    password: Option<&str>,
    tunnel: Option<u16>,
    secrets: Secrets,
) -> String {
    let mut uri = String::from(if conn.kind == ConnectionType::Srv && tunnel.is_none() {
        "mongodb+srv://"
    } else {
        "mongodb://"
    });

    let cred = &conn.credentials;
    if cred.enabled && !cred.username.is_empty() {
        uri.push_str(&enc(&cred.username));
        if cred.mechanism != AuthMechanism::X509 {
            let pw = password.unwrap_or(&cred.password);
            match secrets {
                Secrets::Include => {
                    uri.push(':');
                    uri.push_str(&enc(pw));
                }
                Secrets::Mask if !pw.is_empty() || cred.ask_password => uri.push_str(":*****"),
                _ => {}
            }
        }
        uri.push('@');
    }

    match tunnel {
        Some(port) => uri.push_str(&format!("127.0.0.1:{port}")),
        None => {
            let hosts: Vec<String> = match conn.kind {
                ConnectionType::Srv => conn.hosts.iter().take(1).map(|h| h.host.clone()).collect(),
                ConnectionType::Direct => conn.hosts.iter().take(1).map(HostPort::display).collect(),
                ConnectionType::ReplicaSet => conn.hosts.iter().map(HostPort::display).collect(),
            };
            uri.push_str(&hosts.join(","));
        }
    }

    uri.push('/');
    if cred.enabled && !cred.database.is_empty() {
        // Path = auth db; Robo 3T semantics where the default db is separate.
        uri.push_str(&enc(&cred.database));
    } else if !conn.default_database.is_empty() {
        uri.push_str(&enc(&conn.default_database));
    }

    let mut opts: Vec<String> = Vec::new();
    if cred.enabled {
        if !cred.database.is_empty() {
            opts.push(format!("authSource={}", enc(&cred.database)));
        }
        if let Some(m) = cred.mechanism.uri_name() {
            opts.push(format!("authMechanism={m}"));
        }
        if cred.mechanism == AuthMechanism::Plain && cred.database.is_empty() {
            opts.push("authSource=$external".into());
        }
    }
    match (conn.kind, tunnel) {
        (_, Some(_)) => opts.push("directConnection=true".into()),
        (ConnectionType::Direct, None) => opts.push("directConnection=true".into()),
        (ConnectionType::ReplicaSet, None) => {
            if !conn.replica_set_name.is_empty() {
                opts.push(format!("replicaSet={}", enc(&conn.replica_set_name)));
            }
        }
        _ => {}
    }
    if conn.read_preference != ReadPreference::Primary {
        opts.push(format!("readPreference={}", conn.read_preference.uri_name()));
    }
    let tls = &conn.tls;
    if tls.enabled {
        opts.push("tls=true".into());
        if !tls.ca_file.is_empty() {
            opts.push(format!("tlsCAFile={}", enc(&tls.ca_file)));
        }
        if !tls.cert_key_file.is_empty() {
            opts.push(format!("tlsCertificateKeyFile={}", enc(&tls.cert_key_file)));
        }
        if tls.allow_invalid_certificates {
            opts.push("tlsAllowInvalidCertificates=true".into());
        }
        // Through a tunnel the certificate never matches 127.0.0.1.
        if tls.allow_invalid_hostnames || tunnel.is_some() {
            opts.push("tlsAllowInvalidHostnames=true".into());
        }
    } else if conn.kind == ConnectionType::Srv && tunnel.is_some() {
        // SRV implies TLS by default; keep it when we rewrite to mongodb://.
        opts.push("tls=true".into());
        opts.push("tlsAllowInvalidHostnames=true".into());
    }
    if conn.connect_timeout_secs > 0 {
        let ms = conn.connect_timeout_secs * 1000;
        opts.push(format!("connectTimeoutMS={ms}"));
        opts.push(format!("serverSelectionTimeoutMS={ms}"));
    }
    if !conn.app_name.is_empty() {
        opts.push(format!("appName={}", enc(&conn.app_name)));
    }
    let extra = conn.extra_options.trim().trim_start_matches(['?', '&']);
    if !extra.is_empty() {
        opts.push(extra.to_string());
    }
    if !opts.is_empty() {
        uri.push('?');
        uri.push_str(&opts.join("&"));
    }
    uri
}

/// Parses a connection string into `conn`, keeping fields that the URI does
/// not express (name, SSH, colour…). Returns warnings for ignored options.
pub fn apply_uri(conn: &mut ConnectionSettings, uri: &str) -> Result<Vec<String>, String> {
    let uri = uri.trim();
    let (srv, rest) = if let Some(r) = uri.strip_prefix("mongodb+srv://") {
        (true, r)
    } else if let Some(r) = uri.strip_prefix("mongodb://") {
        (false, r)
    } else {
        return Err("URI must start with mongodb:// or mongodb+srv://".into());
    };
    let mut warnings = Vec::new();

    let (authority, path_query) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => match rest.find('?') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        },
    };
    let (userinfo, hosts) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    if hosts.is_empty() {
        return Err("URI has no host".into());
    }
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path_query.trim_start_matches('?'), ""),
    };
    let path = if path_query.starts_with('?') { "" } else { path };

    let host_list: Vec<HostPort> = hosts.split(',').filter_map(parse_host_port).collect();
    if host_list.is_empty() {
        return Err("URI has no valid host".into());
    }

    let mut cred = Credentials { enabled: userinfo.is_some(), ..Credentials::default() };
    if let Some(ui) = userinfo {
        let (u, p) = ui.split_once(':').unwrap_or((ui, ""));
        cred.username = dec(u);
        cred.password = dec(p);
        cred.ask_password = cred.password.is_empty() && conn.credentials.ask_password;
    }
    let db = dec(path);
    cred.database = if db.is_empty() { "admin".into() } else { db.clone() };

    let mut kind = if srv {
        ConnectionType::Srv
    } else if host_list.len() > 1 {
        ConnectionType::ReplicaSet
    } else {
        ConnectionType::Direct
    };
    let mut rs_name = String::new();
    let mut read_pref = ReadPreference::Primary;
    let mut tls = crate::settings::TlsSettings { enabled: srv, ..Default::default() };
    let mut extra = Vec::new();
    let mut timeout = conn.connect_timeout_secs;
    let mut app_name = conn.app_name.clone();

    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = dec(v);
        match k.to_ascii_lowercase().as_str() {
            "authsource" => {
                cred.database = v;
                cred.enabled = true;
            }
            "authmechanism" => cred.mechanism = AuthMechanism::from_uri_name(&v),
            "replicaset" => {
                rs_name = v;
                if !srv {
                    kind = ConnectionType::ReplicaSet;
                }
            }
            "directconnection" if v == "true" && !srv => kind = ConnectionType::Direct,
            "directconnection" => {}
            "readpreference" => read_pref = ReadPreference::from_uri_name(&v),
            "tls" | "ssl" => tls.enabled = v == "true",
            "tlscafile" | "sslcafile" => tls.ca_file = v,
            "tlscertificatekeyfile" | "sslpemkeyfile" => tls.cert_key_file = v,
            "tlscertificatekeyfilepassword" | "sslpemkeypassword" => tls.cert_key_password = v,
            "tlsallowinvalidcertificates" | "sslallowinvalidcertificates" => {
                tls.allow_invalid_certificates = v == "true"
            }
            "tlsallowinvalidhostnames" | "sslallowinvalidhostnames" => {
                tls.allow_invalid_hostnames = v == "true"
            }
            "tlsinsecure" if v == "true" => {
                tls.allow_invalid_certificates = true;
                tls.allow_invalid_hostnames = true;
            }
            "connecttimeoutms" | "serverselectiontimeoutms" => {
                if let Ok(ms) = v.parse::<u32>() {
                    timeout = (ms / 1000).max(1);
                }
            }
            "appname" => app_name = v,
            _ => {
                extra.push(pair.to_string());
                warnings.push(format!("Option '{k}' kept as an extra option"));
            }
        }
    }
    if cred.mechanism == AuthMechanism::X509 {
        cred.enabled = true;
        cred.database = "$external".into();
    }

    conn.kind = kind;
    conn.hosts = host_list;
    if kind == ConnectionType::Direct {
        conn.hosts.truncate(1);
    }
    conn.replica_set_name = rs_name;
    conn.read_preference = read_pref;
    conn.credentials = cred;
    conn.tls = tls;
    if !db.is_empty() && conn.default_database.is_empty() {
        conn.default_database = db;
    }
    conn.extra_options = extra.join("&");
    conn.connect_timeout_secs = timeout;
    conn.app_name = app_name;
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_replica_set() {
        let mut c = ConnectionSettings::default();
        apply_uri(
            &mut c,
            "mongodb://bob:p%40ss@h1:27017,h2:27018/admin?replicaSet=rs0&readPreference=secondary&tls=true",
        )
        .unwrap();
        assert_eq!(c.kind, ConnectionType::ReplicaSet);
        assert_eq!(c.hosts.len(), 2);
        assert_eq!(c.credentials.password, "p@ss");
        assert_eq!(c.read_preference, ReadPreference::Secondary);
        assert!(c.tls.enabled);
        let uri = build_uri(&c, None, None, Secrets::Include);
        assert!(uri.starts_with("mongodb://bob:p%40ss@h1:27017,h2:27018/admin?"));
        assert!(uri.contains("replicaSet=rs0"));
    }

    #[test]
    fn tunnel_forces_direct() {
        let mut c = ConnectionSettings::default();
        apply_uri(&mut c, "mongodb+srv://cluster0.example.net/").unwrap();
        let uri = build_uri(&c, None, Some(40000), Secrets::Include);
        assert!(uri.starts_with("mongodb://127.0.0.1:40000/"));
        assert!(uri.contains("directConnection=true"));
        assert!(uri.contains("tls=true"));
    }

    #[test]
    fn ipv6_host() {
        let mut c = ConnectionSettings::default();
        apply_uri(&mut c, "mongodb://[::1]:27019").unwrap();
        assert_eq!(c.hosts[0].host, "::1");
        assert_eq!(c.hosts[0].port, 27019);
        assert!(build_uri(&c, None, None, Secrets::Omit).starts_with("mongodb://[::1]:27019/"));
    }
}
