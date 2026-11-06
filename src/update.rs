//! Detection of new versions: the latest release on GitHub, against this build.
//! Drafts and pre-releases are never the "latest" one for GitHub.

use std::time::Duration;

use serde::Deserialize;

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

/// The latest release, when it is newer than this build. Blocking.
pub fn check() -> Result<Option<Release>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(15))).build().into();
    let mut reply = agent
        .get(LATEST_API)
        .header("User-Agent", concat!("rustyrobo/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let json = reply.body_mut().read_to_string().map_err(|e| e.to_string())?;
    Release::newer_than(&json, env!("CARGO_PKG_VERSION"))
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
        let prefix = format!("rustyrobo-v{version}-{}-{}.", std::env::consts::OS, std::env::consts::ARCH);
        let asset = latest.assets.into_iter().map(|a| a.name).find(|n| n.starts_with(&prefix));
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
        let this_system = format!("rustyrobo-v1.3.0-{}-{}.tar.gz", std::env::consts::OS, std::env::consts::ARCH);
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
