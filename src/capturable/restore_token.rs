//! The portal's screen-cast restore token, kept across restarts.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("weylus/screen_restore_token"))
}

fn load_from(path: &Path) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(token) => Some(token.trim().to_string()).filter(|token| !token.is_empty()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            debug!("Failed to read {}: {err}", path.display());
            None
        }
    }
}

fn store_at(path: &Path, token: Option<&str>) -> io::Result<()> {
    match token {
        Some(token) => {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)?;
            file.write_all(token.as_bytes())
        }
        None => match fs::remove_file(path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        },
    }
}

pub fn load() -> Option<String> {
    load_from(&path()?)
}

/// Stores the token, or removes the stored one for `None`. Failures only warn: a capture must
/// never fail because of this file.
pub fn store(token: Option<&str>) {
    let Some(path) = path() else {
        return;
    };
    if let Err(err) = store_at(&path, token) {
        warn!("Failed to update {}: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "weylus-restore-token-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn token_round_trips_and_is_private() {
        let dir = temp_dir();
        let path = dir.join("weylus/screen_restore_token");
        assert_eq!(load_from(&path), None);
        store_at(&path, Some("abc-123")).unwrap();
        assert_eq!(load_from(&path).as_deref(), Some("abc-123"));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        store_at(&path, None).unwrap();
        assert_eq!(load_from(&path), None);
        store_at(&path, None).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
