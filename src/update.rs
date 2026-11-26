//! Self-update from the latest published GitHub release.
//!
//! `releases/latest` only ever returns a published, non-prerelease release.
//! CI creates every release as a draft, so a build nobody has reviewed is
//! never offered here. Every asset must carry a valid Ed25519 signature.

use std::ffi::{CString, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::{debug, error, info, warn};

const LATEST_RELEASE: &str =
    "https://api.github.com/repos/krakerz/Weylus-Linux-Wayland/releases/latest";
const DOWNLOAD_PREFIX: &str = "https://github.com/krakerz/Weylus-Linux-Wayland/releases/download/";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Ed25519 public half of the release signing key; the CI secret RELEASE_SIGNING_KEY holds the
/// private half.
const PUBLIC_KEY_B64: &str = "KkXTzMlmy7DzEfWOW7VYVXA2J3ewMOaRxGlSgq8KjMY=";
/// Relative to the install prefix; install.sh writes it.
const MARKER: &str = "share/weylus/installed-with";

const MAX_JSON: u64 = 1 << 20;
const MAX_SIG: u64 = 4096;
const MAX_ASSET: u64 = 256 << 20;
const SIGNATURE_LEN: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const SMALL_BODY_TIMEOUT: Duration = Duration::from_secs(30);
const ASSET_BODY_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How this copy was installed, which decides what an update replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// `install.sh` from the release archive; the binary is `prefix/bin/weylus`.
    Archive { prefix: PathBuf },
    /// An AppImage file.
    AppImage { path: PathBuf },
    /// A build tree or a package manager's copy; updating doesn't apply.
    Source,
}

impl InstallKind {
    fn detect() -> Self {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok());
        // $APPIMAGE is inherited by everything an AppImage starts; only trust it when this binary
        // runs from inside the mounted image, or the update would overwrite another program's file
        if let (Some(appimage), Some(appdir)) =
            (std::env::var_os("APPIMAGE"), std::env::var_os("APPDIR"))
            && let (Some(exe), Ok(appdir)) = (&exe, Path::new(&appdir).canonicalize())
            && exe.starts_with(appdir)
        {
            return InstallKind::AppImage {
                path: PathBuf::from(appimage),
            };
        }
        // The marker works for any --prefix; a distro's /usr/bin/weylus never has it.
        if let Some(exe) = exe
            && exe.file_name().is_some_and(|n| n == "weylus")
            && let Some(bin) = exe.parent()
            && bin.file_name().is_some_and(|n| n == "bin")
            && let Some(prefix) = bin.parent()
            && prefix
                .join(MARKER)
                .symlink_metadata()
                .is_ok_and(|m| m.is_file())
        {
            return InstallKind::Archive {
                prefix: prefix.to_path_buf(),
            };
        }
        InstallKind::Source
    }

    pub fn describe(&self) -> &'static str {
        match self {
            InstallKind::Archive { .. } => "installed with install.sh",
            InstallKind::AppImage { .. } => "AppImage",
            InstallKind::Source => "source build",
        }
    }

    /// Why updating is impossible for this copy, if it is.
    pub fn update_blocker(&self) -> Option<String> {
        let Some(target) = self.target() else {
            return Some(
                "Updates are for the AppImage and install.sh builds; this copy was built from \
                 source or installed by a package manager."
                    .to_string(),
            );
        };
        let dir = target.parent()?;
        let writable = CString::new(dir.as_os_str().as_bytes())
            // SAFETY: `c_dir` is a valid NUL-terminated string that outlives the call.
            .is_ok_and(|c_dir| unsafe { libc::access(c_dir.as_ptr(), libc::W_OK) } == 0);
        (!writable).then(|| {
            format!(
                "No write access to {}; download the new release instead.",
                dir.display()
            )
        })
    }

    /// The file an update replaces and a restart starts.
    pub(crate) fn target(&self) -> Option<PathBuf> {
        match self {
            InstallKind::Archive { prefix } => Some(prefix.join("bin/weylus")),
            InstallKind::AppImage { path } => Some(path.clone()),
            InstallKind::Source => None,
        }
    }
}

/// Detected once at start-up: after an update replaced the binary, /proc/self/exe reads
/// "(deleted)" and detection would fall back to `Source`.
pub fn install_kind() -> &'static InstallKind {
    static KIND: OnceLock<InstallKind> = OnceLock::new();
    KIND.get_or_init(InstallKind::detect)
}

pub fn current_version() -> &'static str {
    CURRENT
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub version: String,
    asset: Asset,
    signature_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Asset {
    name: String,
    url: String,
    sha256: Option<[u8; 32]>,
}

#[derive(Debug, Deserialize)]
struct ReleaseJson {
    tag_name: String,
    #[serde(default)]
    assets: Vec<AssetJson>,
}

#[derive(Debug, Clone, Deserialize)]
struct AssetJson {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

/// Where releases are looked up and which download URLs are trusted.
struct Endpoint {
    api_url: String,
    download_prefix: String,
    https_only: bool,
}

impl Endpoint {
    /// `WEYLUS_UPDATE_URL` is for tests; the signature stays required, so it can't weaken
    /// verification.
    fn from_env() -> Self {
        Self::resolve(std::env::var("WEYLUS_UPDATE_URL").ok())
    }

    fn resolve(override_url: Option<String>) -> Self {
        match override_url {
            None => Endpoint {
                api_url: LATEST_RELEASE.to_string(),
                download_prefix: DOWNLOAD_PREFIX.to_string(),
                https_only: true,
            },
            Some(url) => {
                let dir_end = url.rfind('/').map_or(0, |i| i + 1);
                Endpoint {
                    download_prefix: url[..dir_end].to_string(),
                    api_url: url,
                    https_only: false,
                }
            }
        }
    }

    fn agent(&self, body_timeout: Duration) -> ureq::Agent {
        ureq::Agent::config_builder()
            .https_only(self.https_only)
            .max_redirects(5)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            .timeout_recv_body(Some(body_timeout))
            .user_agent(format!("weylus/{CURRENT}"))
            .build()
            .into()
    }
}

/// The newer release for this install, if any.
pub fn check() -> Result<Option<Update>> {
    check_for(install_kind(), &Endpoint::from_env())
}

/// Downloads, verifies and installs `update`. The running process keeps its (now unlinked)
/// binary; a restart picks up the new version.
pub fn apply(update: &Update, progress: &Progress) -> Result<()> {
    apply_for(install_kind(), &Endpoint::from_env(), update, progress)
}

fn check_for(kind: &InstallKind, endpoint: &Endpoint) -> Result<Option<Update>> {
    if *kind == InstallKind::Source {
        return Ok(None);
    }
    let response = endpoint
        .agent(SMALL_BODY_TIMEOUT)
        .get(&endpoint.api_url)
        .header("Accept", "application/vnd.github+json")
        .call();
    // GitHub answers 404 until the first release is published.
    if let Err(ureq::Error::StatusCode(404)) = response {
        return Ok(None);
    }
    let release: ReleaseJson = response
        .context("couldn't reach GitHub")?
        .body_mut()
        .with_config()
        .limit(MAX_JSON)
        .read_json()
        .context("unexpected release data")?;
    Ok(pick_update(
        &release,
        kind,
        CURRENT,
        &endpoint.download_prefix,
    ))
}

fn pick_update(
    release: &ReleaseJson,
    kind: &InstallKind,
    current: &str,
    download_prefix: &str,
) -> Option<Update> {
    if parse_version(&release.tag_name)? <= parse_version(current)? {
        return None;
    }
    let version = release.tag_name.trim_start_matches('v');
    // The version becomes part of file and folder names below.
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return None;
    }
    let name = match kind {
        InstallKind::Archive { .. } => format!("weylus-v{version}-linux-x86_64.tar.gz"),
        InstallKind::AppImage { .. } => format!("Weylus-{version}-x86_64.AppImage"),
        InstallKind::Source => return None,
    };
    let asset = release.assets.iter().find(|a| a.name == name)?;
    let sig_name = format!("{name}.sig");
    let Some(sig) = release.assets.iter().find(|a| a.name == sig_name) else {
        warn!("release v{version} has no signature; not offered");
        return None;
    };
    for url in [&asset.browser_download_url, &sig.browser_download_url] {
        if !url.starts_with(download_prefix) {
            warn!("release v{version} points outside {download_prefix}; not offered");
            return None;
        }
    }
    let sha256 = match asset.digest.as_deref() {
        None => None,
        Some(digest) => match parse_digest(digest) {
            Some(hash) => Some(hash),
            None => {
                warn!("release v{version} has an unusable digest {digest:?}; not offered");
                return None;
            }
        },
    };
    Some(Update {
        version: version.to_string(),
        asset: Asset {
            name,
            url: asset.browser_download_url.clone(),
            sha256,
        },
        signature_url: sig.browser_download_url.clone(),
    })
}

/// `1.2.3` / `v1.2.3` → (1, 2, 3); anything else is ignored.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().trim_start_matches('v').splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

/// `sha256:<64 hex digits>` → the raw hash.
fn parse_digest(digest: &str) -> Option<[u8; 32]> {
    let hex = digest.strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut hash = [0u8; 32];
    for (byte, pair) in hash.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(hash)
}

fn verify_signature(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<()> {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
        .verify(message, signature)
        .map_err(|_| anyhow::anyhow!("signature check failed"))
}

fn apply_for(
    kind: &InstallKind,
    endpoint: &Endpoint,
    update: &Update,
    progress: &Progress,
) -> Result<()> {
    if let Some(reason) = kind.update_blocker() {
        bail!(reason);
    }
    for url in [&update.asset.url, &update.signature_url] {
        if !url.starts_with(&endpoint.download_prefix) {
            bail!("refusing to download from {url}");
        }
    }
    let signature = endpoint
        .agent(SMALL_BODY_TIMEOUT)
        .get(&update.signature_url)
        .call()
        .context("couldn't download the signature")?
        .body_mut()
        .with_config()
        .limit(MAX_SIG)
        .read_to_vec()
        .context("couldn't download the signature")?;
    if signature.len() != SIGNATURE_LEN {
        bail!("signature check failed");
    }
    let (bytes, hash) = download(endpoint, &update.asset.url, progress)?;
    if update.asset.sha256.is_some_and(|expected| expected != hash) {
        bail!("checksum mismatch");
    }
    let public_key = base64::engine::general_purpose::STANDARD
        .decode(PUBLIC_KEY_B64)
        .context("invalid embedded public key")?;
    verify_signature(&public_key, &bytes, &signature)?;

    match kind {
        InstallKind::Archive { prefix } => install_archive(prefix, &update.version, &bytes),
        InstallKind::AppImage { path } => replace_file(path, &bytes),
        InstallKind::Source => bail!("updates apply to installed builds only"),
    }
}

/// The asset in memory plus its SHA-256, counting into `progress` as it goes.
fn download(endpoint: &Endpoint, url: &str, progress: &Progress) -> Result<(Vec<u8>, [u8; 32])> {
    let mut response = endpoint
        .agent(ASSET_BODY_TIMEOUT)
        .get(url)
        .call()
        .context("download failed")?;
    progress.total.store(
        response.body().content_length().unwrap_or(0),
        Ordering::Relaxed,
    );
    progress.received.store(0, Ordering::Relaxed);
    let mut reader = response.body_mut().with_config().limit(MAX_ASSET).reader();
    let mut bytes = Vec::new();
    let mut hasher = Sha256::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut chunk).context("download interrupted")?;
        if n == 0 {
            break;
        }
        hasher.update(&chunk[..n]);
        bytes.extend_from_slice(&chunk[..n]);
        progress.received.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok((bytes, hasher.finalize().into()))
}

/// Unpacks the release archive and runs its own install.sh, so the binary, desktop entry and
/// icon are updated exactly as a manual reinstall would.
fn install_archive(prefix: &Path, version: &str, archive: &[u8]) -> Result<()> {
    let staging = dirs::cache_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .context("no cache directory")?
        .join("weylus/update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("couldn't create {}", staging.display()))?;
    let result = install_from_staging(prefix, version, archive, &staging);
    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn install_from_staging(
    prefix: &Path,
    version: &str,
    archive: &[u8],
    staging: &Path,
) -> Result<()> {
    let archive_path = staging.join("update.tar.gz");
    std::fs::write(&archive_path, archive)
        .with_context(|| format!("couldn't write {}", archive_path.display()))?;
    let status = Command::new("tar")
        .args(["--no-same-owner", "--no-same-permissions", "-xzf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(staging)
        .status()
        .context("couldn't run tar")?;
    if !status.success() {
        bail!("couldn't unpack the update archive");
    }
    let bundle = find_bundle(staging, &format!("weylus-v{version}-linux-x86_64"))
        .context("the update archive has no install.sh")?;
    let out = Command::new("sh")
        .arg(bundle.join("install.sh"))
        .arg("--prefix")
        .arg(prefix)
        .output()
        .context("couldn't run install.sh")?;
    if !out.status.success() {
        bail!(
            "install.sh failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// The versioned folder in the unpacked archive, if it is a real directory holding install.sh and
/// the binary as regular files (nothing is followed through symlinks).
fn find_bundle(staging: &Path, name: &str) -> Option<PathBuf> {
    let is_file = |p: PathBuf| p.symlink_metadata().is_ok_and(|m| m.is_file());
    let dir = staging.join(name);
    let is_dir = dir.symlink_metadata().is_ok_and(|m| m.is_dir());
    (is_dir && is_file(dir.join("install.sh")) && is_file(dir.join("weylus"))).then_some(dir)
}

/// Writes `<path>.part` and renames it over `path`, so a failure leaves the old file.
fn replace_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut partial = OsString::from(path);
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let result = std::fs::write(&partial, bytes)
        .and_then(|()| std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755)))
        .and_then(|()| std::fs::rename(&partial, path))
        .with_context(|| format!("couldn't replace {}", path.display()));
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

/// How far a download has got, shared with the UI (which polls it).
#[derive(Debug, Default)]
pub struct Progress {
    received: AtomicU64,
    /// 0 until known (the server may not say).
    total: AtomicU64,
}

impl Progress {
    pub fn received(&self) -> u64 {
        self.received.load(Ordering::Relaxed)
    }

    pub fn total(&self) -> Option<u64> {
        Some(self.total.load(Ordering::Relaxed)).filter(|&t| t > 0)
    }

    /// 0..=1, when the size is known.
    pub fn fraction(&self) -> Option<f32> {
        self.total()
            .map(|total| (self.received() as f32 / total as f32).clamp(0.0, 1.0))
    }

    /// "34 % · 3.5 / 10.1 MB", or "3.5 MB" when the size is unknown.
    pub fn describe(&self) -> String {
        let mb = |bytes: u64| bytes as f64 / 1_000_000.0;
        match (self.fraction(), self.total()) {
            (Some(fraction), Some(total)) => format!(
                "{:.0} % · {:.1} / {:.1} MB",
                fraction * 100.0,
                mb(self.received()),
                mb(total)
            ),
            _ => format!("{:.1} MB", mb(self.received())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Available(Update),
    Downloading { version: String },
    Installed { version: String },
    CheckFailed(String),
    UpdateFailed { update: Update, error: String },
}

/// `Quiet` is the start-up check: failures and "up to date" stay invisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    Quiet,
    Manual,
}

/// Status machine shared between the UI (which polls [`Updater::status`]) and the worker threads.
#[derive(Debug)]
pub struct Updater {
    status: Mutex<Status>,
    progress: Progress,
}

impl Updater {
    pub fn new() -> Arc<Self> {
        Arc::new(Updater {
            status: Mutex::new(Status::Idle),
            progress: Progress::default(),
        })
    }

    pub fn status(&self) -> Status {
        self.status.lock().expect("lock poisoned").clone()
    }

    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    fn set(&self, status: Status) {
        *self.status.lock().expect("lock poisoned") = status;
    }

    pub fn check_in_background(self: &Arc<Self>, mode: CheckMode) {
        if install_kind().update_blocker().is_some() {
            return;
        }
        {
            let mut status = self.status.lock().expect("lock poisoned");
            if matches!(*status, Status::Checking | Status::Downloading { .. }) {
                return;
            }
            *status = Status::Checking;
        }
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            let status = match (check(), mode) {
                (Ok(Some(update)), _) => Status::Available(update),
                (Ok(None), CheckMode::Quiet) => Status::Idle,
                (Ok(None), CheckMode::Manual) => Status::UpToDate,
                (Err(err), CheckMode::Quiet) => {
                    debug!("Update check failed: {err:#}");
                    Status::Idle
                }
                (Err(err), CheckMode::Manual) => Status::CheckFailed(format!("{err:#}")),
            };
            this.set(status);
        });
    }

    /// Starts the download from `Available` or retries after `UpdateFailed`; otherwise a no-op.
    pub fn apply_in_background(self: &Arc<Self>) {
        let update = {
            let mut status = self.status.lock().expect("lock poisoned");
            let update = match &*status {
                Status::Available(update) | Status::UpdateFailed { update, .. } => update.clone(),
                _ => return,
            };
            *status = Status::Downloading {
                version: update.version.clone(),
            };
            update
        };
        let this = Arc::clone(self);
        std::thread::spawn(move || {
            let status = match apply(&update, &this.progress) {
                Ok(()) => {
                    info!("Updated to Weylus {}", update.version);
                    Status::Installed {
                        version: update.version,
                    }
                }
                Err(err) => {
                    error!("Update to Weylus {} failed: {err:#}", update.version);
                    Status::UpdateFailed {
                        update,
                        error: format!("{err:#}"),
                    }
                }
            };
            this.set(status);
        });
    }
}

static RELAUNCH: AtomicBool = AtomicBool::new(false);

/// Asks `main` to start the updated copy once the GUI has returned.
pub fn request_relaunch() {
    RELAUNCH.store(true, Ordering::SeqCst);
}

/// Starts the updated copy if [`request_relaunch`] was called. Must run after the web server
/// stopped, or the new process can't bind the port.
pub fn spawn_pending_relaunch() {
    if !RELAUNCH.load(Ordering::SeqCst) {
        return;
    }
    // Same args: the config file carries the GUI settings, and e.g. --web-port must survive.
    match relaunch_command(install_kind(), std::env::args_os().skip(1)) {
        Some(mut command) => {
            if let Err(err) = command.spawn() {
                error!("Couldn't restart Weylus: {err}");
            }
        }
        None => error!("Couldn't restart Weylus: not an installed build"),
    }
}

fn relaunch_command(
    kind: &InstallKind,
    args: impl IntoIterator<Item = OsString>,
) -> Option<Command> {
    let mut command = Command::new(kind.target()?);
    command.args(args);
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("weylus-update-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const PREFIX: &str = "https://github.com/krakerz/Weylus-Linux-Wayland/releases/download/";

    fn archive_kind() -> InstallKind {
        InstallKind::Archive {
            prefix: PathBuf::from("/home/u/.local"),
        }
    }

    fn appimage_kind() -> InstallKind {
        InstallKind::AppImage {
            path: PathBuf::from("/home/u/Weylus.AppImage"),
        }
    }

    fn release(tag: &str, names: &[&str], digest: Option<&str>) -> ReleaseJson {
        let assets = names
            .iter()
            .map(|name| AssetJson {
                name: name.to_string(),
                browser_download_url: format!("{PREFIX}{tag}/{name}"),
                digest: digest.map(str::to_string),
            })
            .collect();
        ReleaseJson {
            tag_name: tag.to_string(),
            assets,
        }
    }

    const TGZ: &str = "weylus-v0.12.1-linux-x86_64.tar.gz";
    const TGZ_SIG: &str = "weylus-v0.12.1-linux-x86_64.tar.gz.sig";
    const APP: &str = "Weylus-0.12.1-x86_64.AppImage";
    const APP_SIG: &str = "Weylus-0.12.1-x86_64.AppImage.sig";

    #[test]
    fn versions_compare_numerically() {
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("1.2"), None);
    }

    #[test]
    fn picks_the_exact_assets() {
        let rel = release("v0.12.1", &[TGZ, TGZ_SIG, APP, APP_SIG], None);
        let archive = pick_update(&rel, &archive_kind(), "0.12.0", PREFIX).unwrap();
        assert_eq!(archive.version, "0.12.1");
        assert_eq!(archive.asset.name, TGZ);
        assert!(archive.signature_url.ends_with(TGZ_SIG));
        let appimage = pick_update(&rel, &appimage_kind(), "0.12.0", PREFIX).unwrap();
        assert_eq!(appimage.asset.name, APP);
        assert!(appimage.signature_url.ends_with(APP_SIG));
    }

    #[test]
    fn rejects_near_miss_names() {
        let rel = release(
            "v0.12.1",
            &[
                "weylus-v0.12.1-linux-x86_64.tar.gz.zsync",
                TGZ_SIG,
                "weylus-v0.12.1-linux-aarch64.tar.gz",
            ],
            None,
        );
        assert_eq!(pick_update(&rel, &archive_kind(), "0.12.0", PREFIX), None);
    }

    #[test]
    fn same_older_and_source_give_nothing() {
        let rel = release("v0.12.1", &[TGZ, TGZ_SIG, APP, APP_SIG], None);
        assert_eq!(pick_update(&rel, &archive_kind(), "0.12.1", PREFIX), None);
        assert_eq!(pick_update(&rel, &archive_kind(), "0.13.0", PREFIX), None);
        assert_eq!(
            pick_update(&rel, &InstallKind::Source, "0.12.0", PREFIX),
            None
        );
    }

    #[test]
    fn missing_signature_gives_nothing() {
        let rel = release("v0.12.1", &[TGZ, APP], None);
        assert_eq!(pick_update(&rel, &archive_kind(), "0.12.0", PREFIX), None);
    }

    #[test]
    fn urls_outside_the_prefix_give_nothing() {
        let rel = release("v0.12.1", &[TGZ, TGZ_SIG], None);
        assert_eq!(
            pick_update(&rel, &archive_kind(), "0.12.0", "https://example.org/"),
            None
        );
        let rel = release("v0.12.1", &[TGZ, TGZ_SIG], None);
        // the same host over http is not the pinned prefix either
        let http = PREFIX.replacen("https", "http", 1);
        assert_eq!(pick_update(&rel, &archive_kind(), "0.12.0", &http), None);
    }

    #[test]
    fn digest_is_carried_and_bad_digest_refuses() {
        let good = format!("sha256:{}", "ab".repeat(32));
        let rel = release("v0.12.1", &[TGZ, TGZ_SIG], Some(&good));
        let update = pick_update(&rel, &archive_kind(), "0.12.0", PREFIX).unwrap();
        assert_eq!(update.asset.sha256, Some([0xab; 32]));

        let rel = release("v0.12.1", &[TGZ, TGZ_SIG], None);
        let update = pick_update(&rel, &archive_kind(), "0.12.0", PREFIX).unwrap();
        assert_eq!(update.asset.sha256, None);

        for bad in ["md5:00112233445566778899aabbccddeeff", "sha256:abcd"] {
            let rel = release("v0.12.1", &[TGZ, TGZ_SIG], Some(bad));
            assert_eq!(
                pick_update(&rel, &archive_kind(), "0.12.0", PREFIX),
                None,
                "{bad}"
            );
        }
    }

    #[test]
    fn digest_parses_from_release_json() {
        let json = format!(
            r#"{{"tag_name":"v1.0.0","assets":[
                {{"name":"a","browser_download_url":"u","digest":"sha256:{}"}},
                {{"name":"b","browser_download_url":"u"}}]}}"#,
            "01".repeat(32)
        );
        let rel: ReleaseJson = serde_json::from_str(&json).unwrap();
        let digest = |i: usize| rel.assets[i].digest.as_deref().and_then(parse_digest);
        assert_eq!(digest(0), Some([1; 32]));
        assert_eq!(digest(1), None);
        assert_eq!(parse_digest("md5:00"), None);
        assert_eq!(parse_digest(&format!("sha256:{}", "zz".repeat(32))), None);
    }

    #[test]
    fn endpoint_resolves_default_and_override() {
        let real = Endpoint::resolve(None);
        assert_eq!(real.api_url, LATEST_RELEASE);
        assert_eq!(real.download_prefix, DOWNLOAD_PREFIX);
        assert!(real.https_only);
        let fake = Endpoint::resolve(Some("http://127.0.0.1:1756/latest.json".to_string()));
        assert_eq!(fake.api_url, "http://127.0.0.1:1756/latest.json");
        assert_eq!(fake.download_prefix, "http://127.0.0.1:1756/");
        assert!(!fake.https_only);
    }

    #[test]
    fn progress_describes_known_and_unknown_size() {
        let progress = Progress::default();
        progress.received.store(3_500_000, Ordering::Relaxed);
        assert_eq!(progress.describe(), "3.5 MB");
        assert_eq!(progress.fraction(), None);
        progress.total.store(10_000_000, Ordering::Relaxed);
        assert_eq!(progress.describe(), "35 % · 3.5 / 10.0 MB");
        assert_eq!(progress.fraction(), Some(0.35));
    }

    fn public_key() -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(PUBLIC_KEY_B64)
            .unwrap()
    }

    #[test]
    fn public_key_is_32_bytes() {
        assert_eq!(public_key().len(), 32);
    }

    #[test]
    fn known_answer_signature() {
        let message = b"weylus release signing test";
        let signature = base64::engine::general_purpose::STANDARD
            .decode("aPKcuZqtpzBzsn4s6tnhPdP3Xb8GAMpumOcQueIx7VSkhykd/J83PEP27NdqS+pVJoHGzOC54p9HpcJGYjbXBw==")
            .unwrap();
        verify_signature(&public_key(), message, &signature).unwrap();
        assert!(verify_signature(&public_key(), b"another message", &signature).is_err());
        assert!(verify_signature(&public_key(), message, &signature[..63]).is_err());
    }

    #[test]
    fn bundle_is_found_by_versioned_name() {
        let root = temp_dir("bundle");
        let bundle = root.join("weylus-v1.0.0-linux-x86_64");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("install.sh"), "").unwrap();
        std::fs::write(bundle.join("weylus"), "").unwrap();
        assert_eq!(
            find_bundle(&root, "weylus-v1.0.0-linux-x86_64"),
            Some(bundle.clone())
        );
        assert_eq!(find_bundle(&root, "weylus-v2.0.0-linux-x86_64"), None);

        // a symlinked binary is not a regular file
        std::fs::remove_file(bundle.join("weylus")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", bundle.join("weylus")).unwrap();
        assert_eq!(find_bundle(&root, "weylus-v1.0.0-linux-x86_64"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn symlinked_bundle_dir_is_ignored() {
        let root = temp_dir("bundle-link");
        let real = root.join("elsewhere");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("install.sh"), "").unwrap();
        std::fs::write(real.join("weylus"), "").unwrap();
        std::os::unix::fs::symlink(&real, root.join("weylus-v1.0.0-linux-x86_64")).unwrap();
        assert_eq!(find_bundle(&root, "weylus-v1.0.0-linux-x86_64"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn replace_file_swaps_content_and_mode() {
        let root = temp_dir("replace");
        let target = root.join("Weylus.AppImage");
        std::fs::write(&target, "old").unwrap();
        replace_file(&target, b"new").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(!root.join("Weylus.AppImage.part").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn replace_file_failure_leaves_no_part_file() {
        let root = temp_dir("replace-fail");
        let target = root.join("missing-dir/Weylus.AppImage");
        assert!(replace_file(&target, b"new").is_err());
        assert!(!root.join("missing-dir").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn replace_file_rename_failure_removes_part_file() {
        let root = temp_dir("replace-rename");
        // renaming a file over a non-empty directory fails after the .part file was written
        let target = root.join("Weylus.AppImage");
        std::fs::create_dir_all(target.join("inner")).unwrap();
        assert!(replace_file(&target, b"new").is_err());
        assert!(!root.join("Weylus.AppImage.part").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn relaunch_uses_the_install_target_and_same_args() {
        let command =
            relaunch_command(&archive_kind(), ["--web-port", "1755"].map(OsString::from)).unwrap();
        assert_eq!(command.get_program(), "/home/u/.local/bin/weylus");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--web-port", "1755"]
        );
        let command = relaunch_command(&appimage_kind(), []).unwrap();
        assert_eq!(command.get_program(), "/home/u/Weylus.AppImage");
        assert!(relaunch_command(&InstallKind::Source, []).is_none());
    }

    #[test]
    fn source_has_a_blocker_and_nothing_to_check() {
        assert!(InstallKind::Source.update_blocker().is_some());
        let endpoint = Endpoint::resolve(Some("http://127.0.0.1:9/x.json".to_string()));
        assert_eq!(check_for(&InstallKind::Source, &endpoint).unwrap(), None);
    }

    /// Needs a fake release server: see WEYLUS_UPDATE_URL, WEYLUS_E2E_PREFIX.
    #[test]
    #[ignore]
    fn update_archive_from_fake_release() {
        let prefix = std::env::var("WEYLUS_E2E_PREFIX").expect("WEYLUS_E2E_PREFIX is not set");
        let endpoint = Endpoint::from_env();
        assert!(
            !endpoint.https_only,
            "WEYLUS_UPDATE_URL is not set, this would hit GitHub"
        );
        let kind = InstallKind::Archive {
            prefix: PathBuf::from(prefix),
        };
        let update = check_for(&kind, &endpoint)
            .expect("check failed")
            .expect("the fake release was not offered");
        apply_for(&kind, &endpoint, &update, &Progress::default()).expect("update failed");
    }

    /// Needs a fake release server: see WEYLUS_UPDATE_URL, WEYLUS_E2E_APPIMAGE.
    #[test]
    #[ignore]
    fn update_appimage_from_fake_release() {
        let path = std::env::var("WEYLUS_E2E_APPIMAGE").expect("WEYLUS_E2E_APPIMAGE is not set");
        let endpoint = Endpoint::from_env();
        assert!(
            !endpoint.https_only,
            "WEYLUS_UPDATE_URL is not set, this would hit GitHub"
        );
        let kind = InstallKind::AppImage {
            path: PathBuf::from(path),
        };
        let update = check_for(&kind, &endpoint)
            .expect("check failed")
            .expect("the fake release was not offered");
        apply_for(&kind, &endpoint, &update, &Progress::default()).expect("update failed");
    }
}
