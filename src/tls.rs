use anyhow::Context;
use rcgen::{CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::ring;
use tokio_rustls::rustls::pki_types::pem::PemObject;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tracing::info;

const CERT_FILE: &str = "tls-cert.pem";
const KEY_FILE: &str = "tls-key.pem";
const NAMES_FILE: &str = "tls-names.txt";

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// Apple's limit for trusted TLS certificates, which keeps a later trust profile possible.
const VALIDITY: Duration = Duration::from_secs(825 * 24 * 60 * 60);
/// A stored certificate that expires sooner than this is replaced.
const RENEW_WITHIN: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Names are only ever added so the browser exception keeps working; this caps the growth.
const MAX_NAMES: usize = 64;

/// Loads or creates the self-signed certificate and builds the TLS config for the web server.
pub fn server_config(bind: IpAddr) -> anyhow::Result<Arc<ServerConfig>> {
    let dir = dirs::config_dir()
        .context("no config directory")?
        .join("weylus");
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let (cert, key) = load_or_create(&dir, &wanted_names(bind), SystemTime::now())?;
    build(cert, key)
}

fn build(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
) -> anyhow::Result<Arc<ServerConfig>> {
    let mut config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .context("selecting TLS protocol versions")?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .context("building the TLS config")?;
    // the server only speaks http1
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn wanted_names(bind: IpAddr) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = ["localhost", "127.0.0.1", "::1"]
        .into_iter()
        .map(String::from)
        .collect();
    if let Ok(hostname) = fs::read_to_string("/proc/sys/kernel/hostname") {
        let hostname = hostname.trim();
        if !hostname.is_empty() {
            names.insert(hostname.to_string());
            names.insert(format!("{hostname}.local"));
        }
    }
    for iface in pnet_datalink::interfaces() {
        if !iface.is_up() || iface.is_loopback() {
            continue;
        }
        for ip in iface.ips {
            let ip = ip.ip();
            let link_local = matches!(ip, IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80);
            if !link_local {
                names.insert(ip.to_string());
            }
        }
    }
    if !bind.is_unspecified() {
        names.insert(bind.to_string());
    }
    names
}

struct Stored {
    expires: u64,
    names: BTreeSet<String>,
}

fn read_names(dir: &Path) -> Option<Stored> {
    let text = fs::read_to_string(dir.join(NAMES_FILE)).ok()?;
    let mut lines = text.lines();
    let expires = lines
        .next()?
        .strip_prefix("expires=")?
        .trim()
        .parse()
        .ok()?;
    let names = lines
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    Some(Stored { expires, names })
}

fn read_pair(dir: &Path) -> Option<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let cert = CertificateDer::from_pem_slice(&fs::read(dir.join(CERT_FILE)).ok()?).ok()?;
    let key = PrivateKeyDer::from_pem_slice(&fs::read(dir.join(KEY_FILE)).ok()?).ok()?;
    Some((cert, key))
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn load_or_create(
    dir: &Path,
    wanted: &BTreeSet<String>,
    now: SystemTime,
) -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let stored = read_names(dir);
    if let Some(stored) = &stored {
        let fresh = stored.expires > unix_secs(now + RENEW_WITHIN);
        if fresh
            && wanted.is_subset(&stored.names)
            && let Some(pair) = read_pair(dir)
        {
            return Ok(pair);
        }
    }

    let mut names = wanted.clone();
    if let Some(stored) = stored {
        names.extend(stored.names);
    }
    if names.len() > MAX_NAMES {
        names = wanted.clone();
    }

    let key = KeyPair::generate().context("generating the TLS key")?;
    let mut params = CertificateParams::new(names.iter().cloned().collect::<Vec<_>>())
        .context("building the certificate parameters")?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Weylus");
    params.distinguished_name = dn;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.not_before = (now - DAY).into();
    let not_after = now + VALIDITY;
    params.not_after = not_after.into();
    let cert = params
        .self_signed(&key)
        .context("signing the certificate")?;

    fs::write(dir.join(CERT_FILE), cert.pem()).context("writing the certificate")?;
    write_private(&dir.join(KEY_FILE), key.serialize_pem().as_bytes())
        .context("writing the key")?;
    // last, so a crash halfway forces a regeneration
    let mut text = format!("expires={}\n", unix_secs(not_after));
    for name in &names {
        text.push_str(name);
        text.push('\n');
    }
    fs::write(dir.join(NAMES_FILE), text).context("writing the certificate names")?;
    info!(
        "Generated a self-signed certificate in {}.",
        dir.join(CERT_FILE).display()
    );

    Ok((
        cert.der().clone(),
        PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key.serialize_der())),
    ))
}

fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // an existing file keeps its old mode
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("weylus-tls-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn creates_files_with_private_key() {
        let tmp = TempDir::new("create");
        load_or_create(
            &tmp.0,
            &names(&["localhost", "127.0.0.1"]),
            SystemTime::now(),
        )
        .unwrap();
        for file in [CERT_FILE, KEY_FILE, NAMES_FILE] {
            assert!(tmp.0.join(file).is_file(), "{file} missing");
        }
        let mode = fs::metadata(tmp.0.join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn same_names_keep_the_certificate() {
        let tmp = TempDir::new("same");
        let now = SystemTime::now();
        let wanted = names(&["localhost", "192.168.1.5"]);
        let (first, _) = load_or_create(&tmp.0, &wanted, now).unwrap();
        let (second, _) = load_or_create(&tmp.0, &wanted, now).unwrap();
        assert_eq!(first.as_ref(), second.as_ref());
        // a subset of the stored names needs no new certificate either
        let (third, _) = load_or_create(&tmp.0, &names(&["localhost"]), now).unwrap();
        assert_eq!(first.as_ref(), third.as_ref());
    }

    #[test]
    fn new_name_regenerates_and_keeps_old_names() {
        let tmp = TempDir::new("extra");
        let now = SystemTime::now();
        let (first, _) =
            load_or_create(&tmp.0, &names(&["localhost", "192.168.1.5"]), now).unwrap();
        let (second, _) = load_or_create(&tmp.0, &names(&["localhost", "10.9.8.7"]), now).unwrap();
        assert_ne!(first.as_ref(), second.as_ref());
        let stored = read_names(&tmp.0).unwrap();
        assert_eq!(
            stored.names,
            names(&["localhost", "192.168.1.5", "10.9.8.7"])
        );
    }

    #[test]
    fn near_expiry_regenerates() {
        let tmp = TempDir::new("expiry");
        let now = SystemTime::now();
        let wanted = names(&["localhost"]);
        let (first, _) = load_or_create(&tmp.0, &wanted, now).unwrap();
        // 10 days before the stored expiry
        let later = now + VALIDITY - 10 * DAY;
        let (second, _) = load_or_create(&tmp.0, &wanted, later).unwrap();
        assert_ne!(first.as_ref(), second.as_ref());
    }

    #[test]
    fn garbage_certificate_is_replaced() {
        let tmp = TempDir::new("garbage");
        let now = SystemTime::now();
        let wanted = names(&["localhost"]);
        load_or_create(&tmp.0, &wanted, now).unwrap();
        fs::write(tmp.0.join(CERT_FILE), "not a certificate").unwrap();
        let (cert, _) = load_or_create(&tmp.0, &wanted, now).unwrap();
        let stored =
            CertificateDer::from_pem_slice(&fs::read(tmp.0.join(CERT_FILE)).unwrap()).unwrap();
        assert_eq!(cert.as_ref(), stored.as_ref());
    }

    #[test]
    fn builds_a_server_config() {
        let tmp = TempDir::new("build");
        let (cert, key) =
            load_or_create(&tmp.0, &names(&["localhost"]), SystemTime::now()).unwrap();
        let config = build(cert, key).unwrap();
        assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }
}
