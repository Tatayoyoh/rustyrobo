//! Detection of new versions: the latest release on GitHub, against this build.
//! Drafts and pre-releases are never the "latest" one for GitHub.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO: &str = "https://github.com/Tatayoyoh/rustyrobo";
const LATEST_API: &str = "https://api.github.com/repos/Tatayoyoh/rustyrobo/releases/latest";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// `1.3.0`: only digits and dots, since it ends up in commands to copy.
    pub version: String,
    pub notes: String,
    /// The file for this system, if the release has one.
    pub asset: Option<String>,
}

/// Installation of a release from the update dialog.
#[derive(Clone, Debug, Default)]
pub enum Install {
    #[default]
    Idle,
    Running { done: u64, total: Option<u64>, cancel: Arc<AtomicBool> },
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
    MacOs,
}

impl Os {
    pub const ALL: [Os; 3] = [Os::Linux, Os::Windows, Os::MacOs];

    pub fn current() -> Os {
        match std::env::consts::OS {
            "windows" => Os::Windows,
            "macos" => Os::MacOs,
            _ => Os::Linux,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Os::Linux => "Linux",
            Os::Windows => "Windows",
            Os::MacOs => "macOS",
        }
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
}

/// `v1.2.3` or `1.2.3`, nothing else.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.strip_prefix('v').unwrap_or(s).split('.').map(|p| if p.bytes().all(|b| b.is_ascii_digit()) { p.parse().ok() } else { None });
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().https_only(true).timeout_connect(Some(Duration::from_secs(15))).timeout_global(Some(timeout)).build().into()
}

/// The latest release, when it is newer than this build. Blocking.
pub fn check() -> Result<Option<Release>, String> {
    let mut reply = agent(Duration::from_secs(15))
        .get(LATEST_API)
        .header("User-Agent", concat!("rustyrobo/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let json = reply.body_mut().read_to_string().map_err(|e| e.to_string())?;
    Release::newer_than(&json, env!("CARGO_PKG_VERSION"))
}

/// Name of the file of a release for this system, see `release.yml`.
fn asset_name(version: &str) -> String {
    let extension = if cfg!(target_os = "linux") { "tar.gz" } else { "zip" };
    format!("rustyrobo-v{version}-{}-{}.{extension}", std::env::consts::OS, std::env::consts::ARCH)
}

impl Release {
    /// The release described by the JSON of GitHub, if newer than `current`.
    pub fn newer_than(json: &str, current: &str) -> Result<Option<Release>, String> {
        let latest: GithubRelease = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let found = parse_version(&latest.tag_name).ok_or_else(|| format!("unexpected tag {:?}", latest.tag_name))?;
        let current = parse_version(current).ok_or_else(|| format!("unexpected version {current:?}"))?;
        if found <= current {
            return Ok(None);
        }
        let version = format!("{}.{}.{}", found.0, found.1, found.2);
        let notes = latest.body.unwrap_or_default().replace("\r\n", "\n").trim().to_string();
        // The exact name: it also becomes a file name and a part of a URL.
        let expected = asset_name(&version);
        let asset = latest.assets.into_iter().map(|a| a.name).find(|n| *n == expected);
        Ok(Some(Release { version, notes, asset }))
    }

    pub fn changelog_url(&self) -> String {
        format!("{REPO}/blob/v{}/CHANGELOG.md", self.version)
    }

    /// The file for this system, or the page of the release without one.
    pub fn download_url(&self) -> String {
        let v = &self.version;
        match &self.asset {
            Some(asset) => format!("{REPO}/releases/download/v{v}/{asset}"),
            None => format!("{REPO}/releases/tag/v{v}"),
        }
    }

    /// Downloads the file of the release, checks it against SHA256SUMS, installs it.
    pub fn commands(&self, os: Os) -> String {
        let v = &self.version;
        let base = format!("{REPO}/releases/download/v{v}");
        match os {
            Os::Linux => format!(
                r#"cd "$(mktemp -d)"
base={base}
name=rustyrobo-v{v}-linux-$(uname -m)
curl -fLO "$base/$name.tar.gz"
curl -fLO "$base/SHA256SUMS"
grep -F "$name.tar.gz" SHA256SUMS | sha256sum -c
tar xzf "$name.tar.gz"
install -Dm755 "$name/rustyrobo" ~/.local/bin/rustyrobo"#
            ),
            Os::MacOs => format!(
                r#"cd "$(mktemp -d)"
base={base}
arch=$(uname -m); [ "$arch" = arm64 ] && arch=aarch64
name=rustyrobo-v{v}-macos-$arch
curl -fLO "$base/$name.zip"
curl -fLO "$base/SHA256SUMS"
grep -F "$name.zip" SHA256SUMS | shasum -a 256 -c
ditto -x -k "$name.zip" /Applications"#
            ),
            Os::Windows => format!(
                r#"# PowerShell. Close Rustyrobo first.
$base = "{base}"
$name = "rustyrobo-v{v}-windows-x86_64"
$tmp = New-Item -ItemType Directory (Join-Path $env:TEMP ([guid]::NewGuid()))
$dest = "$env:LOCALAPPDATA\Programs\Rustyrobo"
Invoke-WebRequest "$base/$name.zip" -OutFile "$tmp\$name.zip"
Invoke-WebRequest "$base/SHA256SUMS" -OutFile "$tmp\SHA256SUMS"
$sum = (Select-String -SimpleMatch "$name.zip" "$tmp\SHA256SUMS").Line.Split(" ")[0]
if ((Get-FileHash "$tmp\$name.zip").Hash -ne $sum) {{ throw "Wrong checksum" }}
Expand-Archive "$tmp\$name.zip" -DestinationPath $tmp
New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item "$tmp\$name\rustyrobo.exe" $dest"#
            ),
        }
    }
}

/// Removed whatever happens. In the directory of what gets replaced: a rename
/// needs the same file system, and a directory that cannot be written is
/// better known before downloading.
struct Staging(PathBuf);

impl Staging {
    fn new(parent: &Path) -> Result<Staging, String> {
        let dir = parent.join(format!(".rustyrobo-update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).map_err(|e| format!("Cannot write to {}: {e}", parent.display()))?;
        Ok(Staging(dir))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Replaces the program at `exe` with the file of the release, once checked
/// against SHA256SUMS. Blocking. `progress` gets the bytes downloaded and the
/// total, if known.
pub fn install(release: &Release, exe: &Path, cancel: &AtomicBool, mut progress: impl FnMut(u64, Option<u64>)) -> Result<(), String> {
    let asset = release.asset.as_deref().ok_or("This release has no file for this system")?;
    let bundle = app_bundle(exe);
    let parent = bundle.unwrap_or(exe).parent().ok_or("The program has no parent directory")?;
    let staging = Staging::new(parent)?;
    let agent = agent(Duration::from_secs(600));
    let base = format!("{REPO}/releases/download/v{}", release.version);

    let archive = staging.0.join(asset);
    let digest = download(&agent, &format!("{base}/{asset}"), &archive, cancel, &mut progress)?;
    let sums = agent.get(format!("{base}/SHA256SUMS")).call().and_then(|mut r| r.body_mut().read_to_string()).map_err(|e| format!("Cannot download the checksums: {e}"))?;
    match checksum_of(&sums, asset) {
        Some(expected) if expected.eq_ignore_ascii_case(&digest) => {}
        Some(_) => return Err(format!("Wrong checksum for {asset}")),
        None => return Err(format!("No checksum for {asset}")),
    }

    let content = staging.0.join("content");
    extract(&archive, &content).map_err(|e| format!("Cannot extract {asset}: {e}"))?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".into());
    }
    match bundle {
        Some(bundle) => replace_bundle(&content, bundle, exe, &staging.0),
        None => replace_binary(&content, exe),
    }
}

/// Saves `url` to `path`, and returns its SHA-256.
fn download(agent: &ureq::Agent, url: &str, path: &Path, cancel: &AtomicBool, progress: &mut impl FnMut(u64, Option<u64>)) -> Result<String, String> {
    const REPORT_EVERY: u64 = 256 * 1024;
    let fail = |e: &dyn std::fmt::Display| format!("Cannot download the update: {e}");
    let mut reply = agent.get(url).call().map_err(|e| fail(&e))?;
    let total = reply.body().content_length();
    let mut reader = reply.body_mut().as_reader();
    let mut file = fs::File::create(path).map_err(|e| fail(&e))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    let (mut done, mut reported) = (0, 0);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        let n = reader.read(&mut buf).map_err(|e| fail(&e))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| fail(&e))?;
        done += n as u64;
        if done - reported >= REPORT_EVERY {
            reported = done;
            progress(done, total);
        }
    }
    progress(done, total);
    Ok(hex::encode(hash.finalize()))
}

/// In the format of `sha256sum`: `<hash>  <name>`.
fn checksum_of<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim_start().trim_start_matches('*') == name).then_some(hash)
    })
}

fn extract(archive: &Path, into: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let file = fs::File::open(archive)?;
    if archive.extension().is_some_and(|e| e == "zip") {
        zip::ZipArchive::new(file)?.extract(into)?;
    } else {
        tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(into)?;
    }
    Ok(())
}

/// `Foo.app` for `Foo.app/Contents/MacOS/rustyrobo`.
fn app_bundle(exe: &Path) -> Option<&Path> {
    let macos = exe.parent().filter(|p| p.ends_with("Contents/MacOS"))?;
    macos.parent()?.parent().filter(|b| b.extension().is_some_and(|e| e == "app"))
}

fn find(dir: &Path, wanted: &impl Fn(&Path) -> bool) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if wanted(&path) {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find(&path, wanted)
        {
            return Some(found);
        }
    }
    None
}

/// The whole bundle: its signature covers `Info.plist` and the resources too,
/// a new binary alone would not match them.
fn replace_bundle(content: &Path, bundle: &Path, exe: &Path, staging: &Path) -> Result<(), String> {
    let new = find(content, &|p| p.is_dir() && p.extension().is_some_and(|e| e == "app")).ok_or("No application in the update")?;
    let name = exe.file_name().ok_or("The program has no name")?;
    if !new.join("Contents/MacOS").join(name).is_file() {
        return Err(format!("No {} in the update", name.display()));
    }
    let previous = staging.join("previous.app");
    fs::rename(bundle, &previous).map_err(|e| format!("Cannot replace {}: {e}", bundle.display()))?;
    fs::rename(&new, bundle).map_err(|e| {
        let _ = fs::rename(&previous, bundle);
        format!("Cannot replace {}: {e}", bundle.display())
    })
}

fn replace_binary(content: &Path, exe: &Path) -> Result<(), String> {
    let new = find(content, &|p| p.is_file() && p.file_name().is_some_and(|n| n == "rustyrobo" || n == "rustyrobo.exe")).ok_or("No program in the update")?;
    let fail = |e: std::io::Error| format!("Cannot replace {}: {e}", exe.display());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755)).map_err(fail)?;
    }
    // Windows refuses to overwrite a running program: this moves it away first.
    #[cfg(windows)]
    let replaced = self_replace::self_replace(&new);
    #[cfg(not(windows))]
    let replaced = fs::rename(&new, exe);
    replaced.map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(tag: &str, assets: &[&str]) -> String {
        let assets: Vec<_> = assets.iter().map(|n| serde_json::json!({ "name": n })).collect();
        serde_json::json!({ "tag_name": tag, "body": "\r\n### Added\r\n- Things\r\n", "assets": assets }).to_string()
    }

    #[test]
    fn versions_are_compared_as_numbers() {
        assert_eq!(parse_version("v1.10.0"), Some((1, 10, 0)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        for bad in ["", "v1", "1.2", "1.2.3.4", "1.2.x", "1.2.+3", "1..3", "v1.2.3-rc1", "1.2.3; rm -rf ~"] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
        assert!(Release::newer_than(&github("v1.10.0", &[]), "1.9.9").unwrap().is_some());
        assert!(Release::newer_than(&github("v2.0.0", &[]), "1.99.99").unwrap().is_some());
        assert!(Release::newer_than(&github("v1.2.0", &[]), "1.2.0").unwrap().is_none(), "same");
        assert!(Release::newer_than(&github("v1.1.9", &[]), "1.2.0").unwrap().is_none(), "older");
    }

    #[test]
    fn unexpected_replies_are_errors() {
        assert!(Release::newer_than("<html>", "1.2.0").is_err());
        assert!(Release::newer_than(r#"{"message":"API rate limit exceeded"}"#, "1.2.0").is_err());
        assert!(Release::newer_than(&github("nightly", &[]), "1.2.0").is_err());
    }

    #[test]
    fn release_has_its_notes_and_links() {
        let this_system = asset_name("1.3.0");
        let r = Release::newer_than(&github("v1.3.0", &["rustyrobo-v1.3.0-beos-m68k.zip", &this_system, "SHA256SUMS"]), "1.2.0").unwrap().unwrap();
        assert_eq!(r.version, "1.3.0");
        assert_eq!(r.notes, "### Added\n- Things");
        assert_eq!(r.changelog_url(), "https://github.com/Tatayoyoh/rustyrobo/blob/v1.3.0/CHANGELOG.md");
        assert_eq!(r.download_url(), format!("https://github.com/Tatayoyoh/rustyrobo/releases/download/v1.3.0/{this_system}"));

        // No file for this system: the page of the release.
        let r = Release::newer_than(&github("v1.3.0", &["rustyrobo-v1.3.0-beos-m68k.zip"]), "1.2.0").unwrap().unwrap();
        assert_eq!(r.download_url(), "https://github.com/Tatayoyoh/rustyrobo/releases/tag/v1.3.0");
    }

    #[test]
    fn asset_must_have_the_exact_name() {
        let tricky = format!("{}/../../evil", asset_name("1.3.0"));
        let r = Release::newer_than(&github("v1.3.0", &[&tricky]), "1.2.0").unwrap().unwrap();
        assert_eq!(r.asset, None);
    }

    #[test]
    fn checksum_is_found_by_exact_file_name() {
        let sums = "aaa  rustyrobo-v1-linux.tar.gz\nbbb *rustyrobo-v1-macos.zip\nccc  SHA256SUMS.old\n";
        assert_eq!(checksum_of(sums, "rustyrobo-v1-linux.tar.gz"), Some("aaa"));
        assert_eq!(checksum_of(sums, "rustyrobo-v1-macos.zip"), Some("bbb"), "binary mode");
        assert_eq!(checksum_of(sums, "SHA256SUMS"), None);
        assert_eq!(checksum_of(sums, "linux.tar.gz"), None);
    }

    #[test]
    fn bundle_is_recognised_from_its_binary() {
        assert_eq!(app_bundle(Path::new("/Applications/My.app/Contents/MacOS/rustyrobo")), Some(Path::new("/Applications/My.app")));
        assert_eq!(app_bundle(Path::new("/home/me/.local/bin/rustyrobo")), None);
        assert_eq!(app_bundle(Path::new("/opt/Contents/MacOS/rustyrobo")), None);
    }

    #[test]
    fn a_directory_that_cannot_be_written_fails_before_downloading() {
        let r = Release { version: "1.3.0".into(), notes: String::new(), asset: Some("x.zip".into()) };
        let missing = std::env::temp_dir().join("rustyrobo-no-such-directory").join("rustyrobo");
        let e = install(&r, &missing, &AtomicBool::new(false), |_, _| {}).unwrap_err();
        assert!(e.starts_with("Cannot write to"), "{e}");
    }

    #[test]
    fn commands_name_the_files_of_the_release() {
        let r = Release { version: "1.3.0".into(), notes: String::new(), asset: None };
        for (os, file) in [(Os::Linux, "$name.tar.gz"), (Os::MacOs, "$name.zip"), (Os::Windows, "$name.zip")] {
            let c = r.commands(os);
            assert!(c.contains("releases/download/v1.3.0"), "{c}");
            assert!(c.contains("rustyrobo-v1.3.0-"), "{c}");
            assert!(c.contains(file), "{c}");
            assert!(c.contains("SHA256SUMS"), "{c}");
        }
        assert!(r.commands(Os::Windows).contains("{ throw"), "one brace, not the escaped two");
    }
}
