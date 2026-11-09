//! Self-update via GitHub Releases.
//!
//! When code is pushed, GitHub Actions builds the exe and publishes it as the
//! latest release: `goodbyedc-<platform>.exe` plus a `.sha256` file containing its
//! SHA-256 hash (a fingerprint of the file's bytes).
//! The app compares the hash of its own exe with the published one. If they
//! differ, an "Update" button appears. Clicking it downloads the new exe, checks
//! its hash, renames the running exe to ".old", puts the new one in its place and
//! restarts the app. The old file is deleted on the next start.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
// `StreamExt` adds `.next()` to async streams (used for the download).
use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
// `AsyncWriteExt` adds async `write_all`/`flush` to tokio files.
use tokio::io::AsyncWriteExt;

/// e.g. "windows-x86_64", "macos-aarch64".
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Bytes -> lowercase hex string. `{b:02x}` = two hex digits with a leading zero.
/// `.collect()` can build a String from an iterator of Strings.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// "goodbyedc.exe" + ".old" -> "goodbyedc.exe.old".
fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    // Paths aren't always valid UTF-8, so we work with `OsString` here.
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Hash of the running exe, computed once at startup (even if the file is
/// replaced later during an update).
///
/// The return type `&'static str` is a string that lives for the whole program.
/// That works because it's stored in a `static` `OnceLock`: a global value that is
/// set at most once and can then be read from anywhere.
fn own_sha256() -> Result<&'static str> {
    static SHA: OnceLock<String> = OnceLock::new();
    if let Some(s) = SHA.get() {
        return Ok(s);
    }
    let exe = std::env::current_exe()?;
    // `with_context` adds a message to the error (only built if there is an error).
    let data = std::fs::read(&exe).with_context(|| format!("{} okunamadı", exe.display()))?;
    let s = hex(&Sha256::digest(&data));
    Ok(SHA.get_or_init(|| s))
}

/// The GitHub repository that built this exe ("user/repo"). GitHub Actions sets
/// this environment variable during the build; `option_env!` reads it at *compile
/// time*. In manual builds it's `None`, which disables updating.
const REPO: Option<&str> = option_env!("GITHUB_REPOSITORY");

/// The GitHub build number (same as "Sürüm N"); absent in local builds.
const BUILD: Option<&str> = option_env!("GOODBYEDC_BUILD");

/// Version shown in the UI: "Sürüm 14", or "Geliştirme 0.2.0" for local builds.
pub fn version_label() -> String {
    match BUILD {
        Some(n) => format!("Sürüm {n}"),
        None => format!("Geliştirme {}", env!("CARGO_PKG_VERSION")),
    }
}

/// Only release builds made by GitHub Actions update themselves.
fn enabled() -> bool {
    !cfg!(debug_assertions) && REPO.is_some()
}

/// Download URL of the latest release's exe (`suffix` = "" or ".sha256").
fn asset_url(suffix: &str) -> String {
    format!(
        "https://github.com/{}/releases/latest/download/goodbyedc-{}{}{}",
        REPO.unwrap_or_default(),
        platform(),
        std::env::consts::EXE_SUFFIX,
        suffix
    )
}

/// Called at startup: cleans up files left over from the previous update.
pub fn startup() {
    if !enabled() {
        return;
    }
    // Compute (and cache) our own hash now, before anything can replace the file.
    let _ = own_sha256();
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(with_suffix(&exe, ".new"));
        let old = with_suffix(&exe, ".old");
        if old.exists() {
            // Windows may keep the file locked until the old process has fully
            // exited, so retry for up to 10 seconds in the background.
            std::thread::spawn(move || {
                for _ in 0..20 {
                    if std::fs::remove_file(&old).is_ok() || !old.exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            });
        }
    }
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .user_agent("goodbyedc-updater")
        .build()?)
}

/// Result of an update check.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// Can this build update itself at all? (false for manual builds)
    pub supported: bool,
    /// Is a different (newer) version published?
    pub available: bool,
}

/// Hash of the latest published release; `None` if nothing is published yet.
async fn latest_sha(http: &reqwest::Client) -> Result<Option<String>> {
    let r = http.get(asset_url(".sha256")).send().await?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !r.status().is_success() {
        bail!("güncelleme kontrolü başarısız: HTTP {}", r.status());
    }
    let text = r.text().await?;
    // Take the first 64 characters (a SHA-256 hex string is 64 chars long).
    let sha: String = text.trim().chars().take(64).collect::<String>().to_lowercase();
    if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("sürüm özeti okunamadı");
    }
    Ok(Some(sha))
}

/// Checks whether an update is available.
pub async fn check() -> Result<Check> {
    if !enabled() {
        return Ok(Check { supported: false, available: false });
    }
    let latest = latest_sha(&client()?).await?;
    // "available" = a release exists AND its hash differs from ours.
    let available = latest.is_some_and(|s| s != own_sha256().unwrap_or_default());
    Ok(Check { supported: true, available })
}

/// Downloads the new version and puts it in place of the running exe.
/// The app must be restarted afterwards (backend.rs does that).
pub async fn apply() -> Result<()> {
    if !enabled() {
        bail!("Bu sürüm kendini güncelleyemez (elle derlenmiş)");
    }
    let http = client()?;
    let expected = latest_sha(&http).await?.ok_or_else(|| anyhow!("yayınlanmış sürüm yok"))?;

    let exe = std::env::current_exe()?;
    let new_path = with_suffix(&exe, ".new");
    let old_path = with_suffix(&exe, ".old");

    // `error_for_status()` turns HTTP error codes (404, 500, ...) into errors.
    let resp = http.get(asset_url("")).send().await?.error_for_status()?;
    let mut file = tokio::fs::File::create(&new_path)
        .await
        .with_context(|| format!("{} oluşturulamadı (klasöre yazma izni var mı?)", new_path.display()))?;
    // Hash the file while downloading it, chunk by chunk, so we don't need to
    // keep the whole file in memory.
    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    // `while let Some(x) = ...` loops until the stream ends (returns None).
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("indirme kesildi")?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    // Close the file before renaming it (Windows can't rename an open file).
    drop(file);

    if hex(&hasher.finalize()) != expected {
        let _ = std::fs::remove_file(&new_path);
        bail!("İndirilen dosya bozuk, tekrar dene");
    }
    // On macOS/Linux the new file must be marked executable (rwxr-xr-x).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // A running exe can't be deleted on Windows, but it *can* be renamed.
    let _ = std::fs::remove_file(&old_path);
    std::fs::rename(&exe, &old_path).context("çalışan uygulama yeniden adlandırılamadı")?;
    if let Err(e) = std::fs::rename(&new_path, &exe) {
        // Roll back so the app keeps working.
        let _ = std::fs::rename(&old_path, &exe);
        return Err(e).context("yeni sürüm yerine konamadı");
    }
    log::info!("güncelleme kuruldu: {}", exe.display());
    Ok(())
}
