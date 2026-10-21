//! SSH tunnel: local port forwarding to the MongoDB host through an SSH server.
//!
//! A listener is bound on `127.0.0.1:<random port>`; every accepted TCP
//! connection is forwarded through a `direct-tcpip` channel to the target.
//! The driver is pointed at the local port with `directConnection=true`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use russh::client;
use russh::keys::{self, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::settings::{HostPort, SshAuthMethod, SshSettings};

#[derive(Debug, Clone, Default)]
pub struct SshSecrets {
    pub password: Option<String>,
    pub passphrase: Option<String>,
}

/// Host-key verification outcome, reported to the user.
#[derive(Debug, Clone)]
pub enum HostKeyStatus {
    Known,
    /// First connection: key recorded in rustyrobo's own known_hosts.
    Learned { fingerprint: String },
}

struct Handler {
    host: String,
    port: u16,
    strict: bool,
    status: Arc<parking_lot::Mutex<Option<Result<HostKeyStatus, String>>>>,
}

fn app_known_hosts() -> PathBuf {
    crate::settings::config_dir().join("known_hosts")
}

impl client::Handler for Handler {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(cert) => cert.public_key().clone().into(),
        };
        let fingerprint = key.fingerprint(keys::HashAlg::Sha256).to_string();
        let verdict = |r: Result<HostKeyStatus, String>| {
            let ok = r.is_ok();
            *self.status.lock() = Some(r);
            ok
        };

        let mut known = false;
        for path in [dirs_home_known_hosts(), Some(app_known_hosts())].into_iter().flatten() {
            if !path.exists() {
                continue;
            }
            match keys::check_known_hosts_path(&self.host, self.port, &key, &path) {
                Ok(true) => known = true,
                Ok(false) => {}
                Err(keys::Error::KeyChanged { line }) => {
                    return Ok(verdict(Err(format!(
                        "HOST KEY CHANGED for {}:{} ({}:{line}). Possible man-in-the-middle attack; \
                         server presented {fingerprint}",
                        self.host,
                        self.port,
                        path.display()
                    ))));
                }
                Err(_) => {}
            }
        }
        if known {
            return Ok(verdict(Ok(HostKeyStatus::Known)));
        }
        if self.strict {
            return Ok(verdict(Err(format!(
                "Host key for {}:{} is not in known_hosts (strict checking enabled). Fingerprint: {fingerprint}",
                self.host, self.port
            ))));
        }
        let path = app_known_hosts();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = keys::known_hosts::learn_known_hosts_path(&self.host, self.port, &key, &path);
        Ok(verdict(Ok(HostKeyStatus::Learned { fingerprint })))
    }
}

fn dirs_home_known_hosts() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().join(".ssh").join("known_hosts"))
}

/// Tries every identity of the agent until one is accepted.
async fn auth_with_agent<S>(
    session: &mut client::Handle<Handler>,
    user: &str,
    mut agent: keys::agent::client::AgentClient<S>,
) -> anyhow::Result<bool>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let identities = agent.request_identities().await?;
    if identities.is_empty() {
        bail!("SSH agent has no identities");
    }
    let hash = session.best_supported_rsa_hash().await?.flatten();
    for id in identities {
        let keys::agent::AgentIdentity::PublicKey { key, .. } = id else { continue };
        let res = session
            .authenticate_publickey_with(user.to_string(), key, hash, &mut agent)
            .await
            .map_err(|e| anyhow!("SSH agent signing failed: {e}"))?;
        if res.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub struct SshTunnel {
    pub local_port: u16,
    pub host_key: HostKeyStatus,
    shutdown: watch::Sender<bool>,
    session: Arc<client::Handle<Handler>>,
}

impl SshTunnel {
    pub async fn open(ssh: &SshSettings, secrets: &SshSecrets, target: &HostPort) -> anyhow::Result<Self> {
        if ssh.host.trim().is_empty() {
            bail!("SSH host is empty");
        }
        let config = client::Config {
            keepalive_interval: (ssh.keepalive_secs > 0)
                .then(|| Duration::from_secs(ssh.keepalive_secs as u64)),
            keepalive_max: 3,
            nodelay: true,
            ..Default::default()
        };
        let status = Arc::new(parking_lot::Mutex::new(None));
        let handler = Handler {
            host: ssh.host.clone(),
            port: ssh.port,
            strict: ssh.strict_host_key,
            status: status.clone(),
        };

        let connect = client::connect(Arc::new(config), (ssh.host.as_str(), ssh.port), handler);
        let mut session = match tokio::time::timeout(Duration::from_secs(15), connect).await {
            Err(_) => bail!("SSH connection to {}:{} timed out", ssh.host, ssh.port),
            Ok(Err(e)) => {
                if let Some(Err(msg)) = status.lock().take() {
                    bail!(msg);
                }
                return Err(e).with_context(|| format!("SSH connection to {}:{} failed", ssh.host, ssh.port));
            }
            Ok(Ok(s)) => s,
        };
        let host_key = match status.lock().take() {
            Some(Ok(s)) => s,
            Some(Err(msg)) => bail!(msg),
            None => HostKeyStatus::Known,
        };

        let user = ssh.username.clone();
        let authenticated = match ssh.method {
            SshAuthMethod::Password => {
                let pw = secrets.password.clone().unwrap_or_else(|| ssh.password.clone());
                session.authenticate_password(user, pw).await?.success()
            }
            SshAuthMethod::PrivateKey => {
                let path = expand_tilde(&ssh.private_key_file);
                let pass = secrets
                    .passphrase
                    .clone()
                    .unwrap_or_else(|| ssh.passphrase.clone());
                let key = keys::load_secret_key(&path, (!pass.is_empty()).then_some(pass.as_str()))
                    .map_err(|e| match e {
                        keys::Error::KeyIsEncrypted => anyhow!("Private key is encrypted: a passphrase is required"),
                        other => anyhow!("Cannot load private key {}: {other}", path.display()),
                    })?;
                let hash = session.best_supported_rsa_hash().await?.flatten();
                session
                    .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                    .await?
                    .success()
            }
            SshAuthMethod::Agent => {
                #[cfg(unix)]
                let agent = keys::agent::client::AgentClient::connect_env()
                    .await
                    .context("Cannot reach SSH agent (is SSH_AUTH_SOCK set?)")?;
                #[cfg(windows)]
                let ok = {
                    // Windows OpenSSH agent first, then Pageant (PuTTY).
                    match keys::agent::client::AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
                        Ok(agent) => auth_with_agent(&mut session, &user, agent).await?,
                        Err(_) => {
                            let agent = keys::agent::client::AgentClient::connect_pageant()
                                .await
                                .context("No SSH agent found (Windows OpenSSH agent or Pageant)")?;
                            auth_with_agent(&mut session, &user, agent).await?
                        }
                    }
                };
                #[cfg(unix)]
                let ok = auth_with_agent(&mut session, &user, agent).await?;
                #[cfg(not(any(unix, windows)))]
                let ok = bail!("SSH agent is not supported on this platform");
                ok
            }
        };
        if !authenticated {
            bail!("SSH authentication failed for user '{}'", ssh.username);
        }

        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let local_port = listener.local_addr()?.port();
        let session = Arc::new(session);
        let (shutdown, mut stop) = watch::channel(false);

        let sess = session.clone();
        let target_host = target.host.clone();
        let target_port = target.port as u32;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.changed() => break,
                    accepted = listener.accept() => {
                        let Ok((mut socket, peer)) = accepted else { continue };
                        let sess = sess.clone();
                        let host = target_host.clone();
                        tokio::spawn(async move {
                            let channel = sess
                                .channel_open_direct_tcpip(host, target_port, peer.ip().to_string(), peer.port() as u32)
                                .await;
                            match channel {
                                Ok(ch) => {
                                    let mut stream = ch.into_stream();
                                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                                }
                                Err(e) => eprintln!("rustyrobo: SSH channel open failed: {e}"),
                            }
                        });
                    }
                }
            }
        });

        Ok(SshTunnel { local_port, host_key, shutdown, session })
    }

    pub fn close(&self) {
        let _ = self.shutdown.send(true);
        let session = self.session.clone();
        tokio::spawn(async move {
            let _ = session
                .disconnect(russh::Disconnect::ByApplication, "bye", "en")
                .await;
        });
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = directories::BaseDirs::new()
    {
        return home.home_dir().join(rest);
    }
    PathBuf::from(p)
}

/// What we know about a private key file, for the connection editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyFileState {
    Missing,
    /// Readable without a passphrase.
    Plain,
    /// Protected by a passphrase.
    Encrypted,
    Invalid(String),
}

/// Inspects a private key file. Results are cached by path and
/// modification time, so calling it every frame only costs a `stat`.
pub fn probe_private_key(path: &str) -> KeyFileState {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::SystemTime;
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (SystemTime, KeyFileState)>>> = OnceLock::new();

    let p = expand_tilde(path.trim());
    let Ok(meta) = std::fs::metadata(&p) else {
        return KeyFileState::Missing;
    };
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let cache = CACHE.get_or_init(Default::default);
    if let Some((t, state)) = cache.lock().ok().and_then(|c| c.get(&p).cloned())
        && t == mtime
    {
        return state;
    }
    let state = if meta.len() > 1 << 20 {
        KeyFileState::Invalid("file too large for a private key".into())
    } else {
        match keys::load_secret_key(&p, None) {
            Ok(_) => KeyFileState::Plain,
            Err(keys::Error::KeyIsEncrypted) => KeyFileState::Encrypted,
            Err(e) => {
                // Legacy PEM keys announce encryption in their headers.
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                if text.contains("ENCRYPTED") {
                    KeyFileState::Encrypted
                } else {
                    KeyFileState::Invalid(e.to_string())
                }
            }
        }
    };
    if let Ok(mut c) = cache.lock() {
        c.insert(p, (mtime, state.clone()));
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keygen(dir: &std::path::Path, name: &str, pass: &str) -> Option<String> {
        let path = dir.join(name);
        let ok = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", pass, "-f"])
            .arg(&path)
            .status()
            .ok()?
            .success();
        ok.then(|| path.display().to_string())
    }

    #[test]
    fn detects_encrypted_and_plain_keys() {
        let dir = std::env::temp_dir().join(format!("rr-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(probe_private_key(&dir.join("nope").display().to_string()), KeyFileState::Missing);
        let junk = dir.join("junk");
        std::fs::write(&junk, "not a key").unwrap();
        assert!(matches!(probe_private_key(&junk.display().to_string()), KeyFileState::Invalid(_)));
        // ssh-keygen may be missing on some CI images: only test what we can.
        if let Some(plain) = keygen(&dir, "plain", "") {
            assert_eq!(probe_private_key(&plain), KeyFileState::Plain);
        }
        if let Some(enc) = keygen(&dir, "enc", "secret") {
            assert_eq!(probe_private_key(&enc), KeyFileState::Encrypted);
            assert!(keys::load_secret_key(&enc, Some("secret")).is_ok());
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
