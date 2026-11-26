use std::net::IpAddr;
use std::{fs, path::PathBuf};

use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Serialize, Deserialize, Parser, Debug, Clone)]
#[command(version, about, long_about = None)]
pub struct Config {
    #[arg(long, help = "Access code")]
    pub access_code: Option<String>,
    #[arg(
        long,
        value_delimiter = ',',
        help = "Only accept connections from these IP addresses or CIDR ranges, comma-separated (e.g. 192.168.1.5,192.168.1.0/24). Loopback is always allowed; empty allows everyone."
    )]
    #[serde(default)]
    pub allowed_clients: Vec<String>,
    #[arg(
        long,
        help = "Ask in the desktop window before accepting a device that isn't in the allowed clients."
    )]
    #[serde(default)]
    pub ask_new_devices: bool,
    #[arg(long, default_value = "0.0.0.0", help = "Bind address")]
    pub bind_address: IpAddr,
    #[arg(long, default_value = "1701", help = "Web port")]
    pub web_port: u16,
    #[arg(
        long = "no-vaapi",
        action = clap::ArgAction::SetFalse,
        help = "Don't use hardware encoding through the Video Acceleration API."
    )]
    #[serde(default = "default_true", rename = "vaapi")]
    pub try_vaapi: bool,
    #[arg(long, help = "Try to use Nvidia's NVENC to encode the video via GPU.")]
    #[serde(default)]
    pub try_nvenc: bool,
    #[arg(long, help = "Start Weylus server immediately on program start.")]
    #[serde(default)]
    pub auto_start: bool,
    #[arg(long, help = "Run Weylus without gui and start immediately.")]
    #[serde(default)]
    pub no_gui: bool,
    #[arg(
        long = "no-wayland",
        action = clap::ArgAction::SetFalse,
        help = "Don't capture the screen through xdg-desktop-portal/PipeWire (Wayland)."
    )]
    #[serde(default = "default_true", rename = "wayland")]
    pub wayland_support: bool,
    #[arg(
        long = "no-remember-screen",
        action = clap::ArgAction::SetFalse,
        help = "Don't remember the shared screen across page reloads; ask for it on every connection."
    )]
    #[serde(default = "default_true", rename = "remember_screen")]
    pub remember_screen: bool,
    #[arg(
        long = "no-close-to-tray",
        action = clap::ArgAction::SetFalse,
        help = "Quit when the window is closed instead of keeping Weylus in the system tray."
    )]
    #[serde(default = "default_true", rename = "close_to_tray")]
    pub close_to_tray: bool,

    #[arg(
        long = "no-check-updates",
        action = clap::ArgAction::SetFalse,
        help = "Don't check for a newer Weylus release at start-up."
    )]
    #[serde(default = "default_true")]
    pub check_updates: bool,

    #[arg(long, help = "Print template of index.html served by Weylus.")]
    #[serde(skip)]
    pub print_index_html: bool,
    #[arg(long, help = "Print access.html served by Weylus.")]
    #[serde(skip)]
    pub print_access_html: bool,
    #[arg(long, help = "Print style.css served by Weylus.")]
    #[serde(skip)]
    pub print_style_css: bool,
    #[arg(long, help = "Print lib.js served by Weylus.")]
    #[serde(skip)]
    pub print_lib_js: bool,

    #[arg(
        long,
        help = "Use custom template of index.html to be served by Weylus."
    )]
    #[serde(skip)]
    pub custom_index_html: Option<PathBuf>,
    #[arg(long, help = "Use custom access.html to be served by Weylus.")]
    #[serde(skip)]
    pub custom_access_html: Option<PathBuf>,
    #[arg(long, help = "Use custom style.css to be served by Weylus.")]
    #[serde(skip)]
    pub custom_style_css: Option<PathBuf>,
    #[arg(long, help = "Use custom lib.js to be served by Weylus.")]
    #[serde(skip)]
    pub custom_lib_js: Option<PathBuf>,

    #[arg(long, help = "Print shell completions for given shell.")]
    #[serde(skip)]
    pub completions: Option<clap_complete::Shell>,
}

fn default_true() -> bool {
    true
}

pub fn read_config() -> Option<Config> {
    if let Some(mut config_path) = dirs::config_dir() {
        config_path.push("weylus");
        config_path.push("weylus.toml");
        match fs::read_to_string(&config_path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => Some(c),
                Err(e) => {
                    warn!("Failed to read configuration file: {}", e);
                    None
                }
            },
            Err(err) => {
                match err.kind() {
                    std::io::ErrorKind::NotFound => {
                        debug!("Failed to read configuration file: {}", err)
                    }
                    _ => warn!("Failed to read configuration file: {}", err),
                }
                None
            }
        }
    } else {
        None
    }
}

pub fn write_config(conf: &Config) {
    match dirs::config_dir() {
        Some(mut config_path) => {
            config_path.push("weylus");
            if !config_path.exists()
                && let Err(err) = fs::create_dir_all(&config_path)
            {
                warn!("Failed create directory for configuration: {}", err);
                return;
            }
            config_path.push("weylus.toml");
            if let Err(err) = fs::write(
                config_path,
                toml::to_string_pretty(&conf).expect("Failed to encode config to toml."),
            ) {
                warn!("Failed to write configuration file: {}", err);
            }
        }
        None => {
            warn!("Failed to find configuration directory!");
        }
    }
}

pub fn get_config() -> Config {
    let args = std::env::args();
    if let Some(mut config) = read_config() {
        if args.len() > 1 {
            config.update_from(args);
        }
        config
    } else {
        Config::parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vaapi_and_wayland_default_to_on() {
        let config = Config::try_parse_from(["weylus"]).unwrap();
        assert!(config.try_vaapi);
        assert!(config.wayland_support);
    }

    #[test]
    fn no_flags_turn_vaapi_and_wayland_off() {
        let config = Config::try_parse_from(["weylus", "--no-vaapi", "--no-wayland"]).unwrap();
        assert!(!config.try_vaapi);
        assert!(!config.wayland_support);
    }

    #[test]
    fn remember_screen_defaults_to_on() {
        let config = Config::try_parse_from(["weylus"]).unwrap();
        assert!(config.remember_screen);
    }

    #[test]
    fn no_remember_screen_flag_turns_it_off() {
        let config = Config::try_parse_from(["weylus", "--no-remember-screen"]).unwrap();
        assert!(!config.remember_screen);
    }

    #[test]
    fn remember_screen_is_read_from_config_file() {
        let toml = toml_without_vaapi_and_wayland();
        assert!(toml.contains("remember_screen = true"));
        let config: Config =
            toml::from_str(&toml.replace("remember_screen = true", "remember_screen = false"))
                .unwrap();
        assert!(!config.remember_screen);
        // a file written before the option existed keeps the default
        let config: Config = toml::from_str(&toml.replace("remember_screen = true", "")).unwrap();
        assert!(config.remember_screen);
    }

    #[test]
    fn close_to_tray_defaults_to_on_and_reads_from_config_file() {
        assert!(Config::try_parse_from(["weylus"]).unwrap().close_to_tray);
        assert!(
            !Config::try_parse_from(["weylus", "--no-close-to-tray"])
                .unwrap()
                .close_to_tray
        );
        let toml = toml_without_vaapi_and_wayland();
        assert!(toml.contains("close_to_tray = true"));
        let config: Config =
            toml::from_str(&toml.replace("close_to_tray = true", "close_to_tray = false")).unwrap();
        assert!(!config.close_to_tray);
        // a file written before the option existed keeps the default
        let config: Config = toml::from_str(&toml.replace("close_to_tray = true", "")).unwrap();
        assert!(config.close_to_tray);
    }

    #[test]
    fn check_updates_defaults_to_on() {
        let config = Config::try_parse_from(["weylus"]).unwrap();
        assert!(config.check_updates);
    }

    #[test]
    fn no_check_updates_flag_turns_it_off() {
        let config = Config::try_parse_from(["weylus", "--no-check-updates"]).unwrap();
        assert!(!config.check_updates);
    }

    #[test]
    fn check_updates_is_read_from_config_file() {
        let toml = toml_without_vaapi_and_wayland();
        assert!(toml.contains("check_updates = true"));
        let config: Config =
            toml::from_str(&toml.replace("check_updates = true", "check_updates = false")).unwrap();
        assert!(!config.check_updates);
        // a file written before the option existed keeps the default
        let config: Config = toml::from_str(&toml.replace("check_updates = true", "")).unwrap();
        assert!(config.check_updates);
    }

    #[test]
    fn removed_gui_theme_in_config_file_is_ignored() {
        let toml = format!("gui_theme = \"Dark\"\n{}", toml_without_vaapi_and_wayland());
        assert!(toml::from_str::<Config>(&toml).is_ok());
    }

    #[test]
    fn old_flags_are_gone() {
        assert!(Config::try_parse_from(["weylus", "--try-vaapi"]).is_err());
        assert!(Config::try_parse_from(["weylus", "--wayland-support"]).is_err());
    }

    fn toml_without_vaapi_and_wayland() -> String {
        let config = Config::try_parse_from(["weylus"]).unwrap();
        toml::to_string(&config)
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("vaapi") && !line.starts_with("wayland"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn missing_keys_in_config_file_default_to_on() {
        let config: Config = toml::from_str(&toml_without_vaapi_and_wayland()).unwrap();
        assert!(config.try_vaapi);
        assert!(config.wayland_support);
    }

    #[test]
    fn persisted_false_is_read_with_new_keys() {
        let toml = format!(
            "vaapi = false\nwayland = false\n{}",
            toml_without_vaapi_and_wayland()
        );
        let config: Config = toml::from_str(&toml).unwrap();
        assert!(!config.try_vaapi);
        assert!(!config.wayland_support);
    }

    #[test]
    fn update_from_resets_absent_flags_to_their_defaults() {
        let toml = format!(
            "vaapi = false\nwayland = false\n{}",
            toml_without_vaapi_and_wayland()
        );
        let mut config: Config = toml::from_str(&toml).unwrap();
        config.update_from(["weylus", "--no-gui"]);
        // NOTE: clap's update_from resets arguments that are absent from the command line to
        // their defaults, so a persisted `false` is overridden to `true` as soon as any other
        // argument is given. This is existing clap behaviour shared by every flag.
        assert!(config.no_gui);
        assert!(config.try_vaapi);
        assert!(config.wayland_support);
    }

    #[test]
    fn allowed_clients_default_to_empty_and_ask_to_off() {
        let config = Config::try_parse_from(["weylus"]).unwrap();
        assert!(config.allowed_clients.is_empty());
        assert!(!config.ask_new_devices);
    }

    #[test]
    fn allowed_clients_flag_is_comma_separated() {
        let config =
            Config::try_parse_from(["weylus", "--allowed-clients", "10.0.0.1,10.0.0.0/8"]).unwrap();
        assert_eq!(config.allowed_clients, ["10.0.0.1", "10.0.0.0/8"]);
    }

    fn toml_without_allowed_clients() -> String {
        toml_without_vaapi_and_wayland()
            .lines()
            .filter(|l| !l.starts_with("allowed_clients") && !l.starts_with("ask_new_devices"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn allowed_clients_are_read_from_config_file() {
        let base = toml_without_allowed_clients();
        let toml = format!("allowed_clients = [\"192.168.1.5\"]\nask_new_devices = true\n{base}");
        let config: Config = toml::from_str(&toml).unwrap();
        assert_eq!(config.allowed_clients, ["192.168.1.5"]);
        assert!(config.ask_new_devices);
        // a file written before the options existed keeps the defaults
        let config: Config = toml::from_str(&base).unwrap();
        assert!(config.allowed_clients.is_empty());
        assert!(!config.ask_new_devices);
    }

    #[test]
    fn persisted_allowed_clients_survive_update_from() {
        let toml = format!(
            "allowed_clients = [\"192.168.1.5\"]\n{}",
            toml_without_allowed_clients()
        );
        let mut config: Config = toml::from_str(&toml).unwrap();
        config.update_from(["weylus", "--no-gui"]);
        assert_eq!(config.allowed_clients, ["192.168.1.5"]);
    }
}
