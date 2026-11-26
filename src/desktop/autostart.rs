//! Session autostart entry ($XDG_CONFIG_HOME/autostart/weylus.desktop).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context;
use tracing::{info, warn};

use crate::update;

/// Characters that force quoting of an `Exec` argument (Desktop Entry spec).
const EXEC_RESERVED: &[char] = &[
    ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')',
    '`',
];

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("autostart/weylus.desktop"))
}

/// The file self-update replaces, so the entry survives updates; else the running binary.
fn launcher() -> anyhow::Result<PathBuf> {
    match update::install_kind().target() {
        Some(target) => Ok(target),
        None => std::env::current_exe().context("locating the Weylus binary"),
    }
}

fn exec_quote(arg: &str) -> String {
    let quoted = if arg.contains(EXEC_RESERVED) {
        let mut out = String::from("\"");
        for c in arg.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        // the string-value level of the .desktop format escapes backslashes once more
        out.replace('\\', "\\\\")
    } else {
        arg.to_string()
    };
    quoted.replace('%', "%%")
}

fn entry(program: &Path) -> anyhow::Result<String> {
    let program = program.to_str().context("binary path is not UTF-8")?;
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Weylus\n\
         Comment=Start Weylus in the system tray\n\
         Exec={} --hidden\n\
         Icon=weylus\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_quote(program)
    ))
}

pub(super) fn is_enabled() -> bool {
    path().is_some_and(|path| path.is_file())
}

pub(super) fn enable() -> anyhow::Result<()> {
    let path = path().context("no config directory")?;
    let content = entry(&launcher()?)?;
    let dir = path.parent().context("autostart path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join("weylus.desktop.tmp");
    fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    info!("Wrote autostart entry {}", path.display());
    Ok(())
}

pub(super) fn disable() -> anyhow::Result<()> {
    let Some(path) = path() else {
        return Ok(());
    };
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {}", path.display())),
    }
}

/// Rewrites a stale entry, e.g. after the AppImage was moved or the install method changed.
pub(super) fn refresh() {
    let Some(path) = path().filter(|path| path.is_file()) else {
        return;
    };
    let result = launcher()
        .and_then(|launcher| entry(&launcher))
        .and_then(|wanted| {
            if fs::read_to_string(&path).ok().as_deref() == Some(wanted.as_str()) {
                Ok(())
            } else {
                enable()
            }
        });
    if let Err(err) = result {
        warn!("Failed to refresh the autostart entry: {err:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path_is_unchanged() {
        assert_eq!(exec_quote("/usr/bin/weylus"), "/usr/bin/weylus");
    }

    #[test]
    fn path_with_space_is_quoted() {
        assert_eq!(exec_quote("/a b/weylus"), "\"/a b/weylus\"");
    }

    #[test]
    fn dollar_and_quote_are_escaped_then_doubled() {
        assert_eq!(exec_quote("/a$b"), "\"/a\\\\$b\"");
        assert_eq!(exec_quote("/a\"b"), "\"/a\\\\\"b\"");
    }

    #[test]
    fn percent_is_doubled() {
        assert_eq!(exec_quote("/a%b"), "/a%%b");
    }

    #[test]
    fn entry_runs_the_binary_hidden() {
        let content = entry(Path::new("/home/u/Apps/Weylus.AppImage")).unwrap();
        assert!(content.starts_with("[Desktop Entry]"));
        assert!(content.contains("Exec=/home/u/Apps/Weylus.AppImage --hidden\n"));
    }
}
