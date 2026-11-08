//! Kendini güncelleme (GitHub Releases).
//!
//! Koda push yapılınca GitHub Actions exe'yi derler ve en son sürüm (latest release)
//! olarak yayınlar: `goodbyedc-<platform>.exe` ve yanında `.sha256` özet dosyası.
//! Uygulama kendi exe'sinin SHA-256 özetini yayınlanan özetle karşılaştırır.
//! Farklıysa arayüzde "Güncelle" düğmesi çıkar. Tıklanınca yeni exe indirilir,
//! özeti doğrulanır, çalışan exe ".old" olarak yeniden adlandırılıp yerine yenisi
//! konur ve uygulama yeniden başlatılır. Eski dosya bir sonraki açılışta silinir.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// "windows-x86_64", "macos-aarch64" gibi.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Çalışan exe'nin özeti. Açılışta bir kez hesaplanır (dosya sonradan değişse bile).
fn own_sha256() -> Result<&'static str> {
    static SHA: OnceLock<String> = OnceLock::new();
    if let Some(s) = SHA.get() {
        return Ok(s);
    }
    let exe = std::env::current_exe()?;
    let data = std::fs::read(&exe).with_context(|| format!("{} okunamadı", exe.display()))?;
    let s = hex(&Sha256::digest(&data));
    Ok(SHA.get_or_init(|| s))
}

/// Derlemeyi yapan GitHub deposu ("kullanıcı/depo"). GitHub Actions bu değişkeni
/// kendisi verir; elle derlenen sürümlerde boş kalır ve güncelleme kapalı olur.
const REPO: Option<&str> = option_env!("GITHUB_REPOSITORY");

/// Sadece GitHub Actions'ta derlenmiş sürümler güncellenir.
fn enabled() -> bool {
    !cfg!(debug_assertions) && REPO.is_some()
}

fn asset_url(suffix: &str) -> String {
    format!(
        "https://github.com/{}/releases/latest/download/goodbyedc-{}{}{}",
        REPO.unwrap_or_default(),
        platform(),
        std::env::consts::EXE_SUFFIX,
        suffix
    )
}

/// Uygulama açılırken çağrılır: önceki güncellemeden kalan dosyaları temizler.
pub fn startup() {
    if !enabled() {
        return;
    }
    let _ = own_sha256();
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(with_suffix(&exe, ".new"));
        let old = with_suffix(&exe, ".old");
        if old.exists() {
            // Eski süreç tam kapanana kadar Windows dosyayı kilitli tutabilir.
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// Bu sürüm kendini güncelleyebilir mi? (elle derlenmiş sürümlerde false)
    pub supported: bool,
    pub available: bool,
}

/// Yayınlanan son sürümün özeti. Henüz sürüm yoksa None.
async fn latest_sha(http: &reqwest::Client) -> Result<Option<String>> {
    let r = http.get(asset_url(".sha256")).send().await?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !r.status().is_success() {
        bail!("güncelleme kontrolü başarısız: HTTP {}", r.status());
    }
    let text = r.text().await?;
    let sha: String = text.trim().chars().take(64).collect::<String>().to_lowercase();
    if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("sürüm özeti okunamadı");
    }
    Ok(Some(sha))
}

pub async fn check() -> Result<Check> {
    if !enabled() {
        return Ok(Check { supported: false, available: false });
    }
    let latest = latest_sha(&client()?).await?;
    let available = latest.is_some_and(|s| s != own_sha256().unwrap_or_default());
    Ok(Check { supported: true, available })
}

/// Yeni sürümü indirir ve çalışan exe'nin yerine koyar. Ardından uygulama yeniden başlatılmalı.
pub async fn apply() -> Result<()> {
    if !enabled() {
        bail!("Bu sürüm kendini güncelleyemez (elle derlenmiş)");
    }
    let http = client()?;
    let expected = latest_sha(&http).await?.ok_or_else(|| anyhow!("yayınlanmış sürüm yok"))?;

    let exe = std::env::current_exe()?;
    let new_path = with_suffix(&exe, ".new");
    let old_path = with_suffix(&exe, ".old");

    let resp = http.get(asset_url("")).send().await?.error_for_status()?;
    let mut file = tokio::fs::File::create(&new_path)
        .await
        .with_context(|| format!("{} oluşturulamadı (klasöre yazma izni var mı?)", new_path.display()))?;
    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("indirme kesildi")?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    if hex(&hasher.finalize()) != expected {
        let _ = std::fs::remove_file(&new_path);
        bail!("İndirilen dosya bozuk, tekrar dene");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // Çalışan exe silinemez ama yeniden adlandırılabilir.
    let _ = std::fs::remove_file(&old_path);
    std::fs::rename(&exe, &old_path).context("çalışan uygulama yeniden adlandırılamadı")?;
    if let Err(e) = std::fs::rename(&new_path, &exe) {
        let _ = std::fs::rename(&old_path, &exe);
        return Err(e).context("yeni sürüm yerine konamadı");
    }
    log::info!("güncelleme kuruldu: {}", exe.display());
    Ok(())
}
