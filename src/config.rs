//! Folder configuration (~/.config/upnext/config.json).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// Where new episodes land (e.g. Telegram's download folder). Files here can be moved to `library`.
    pub downloads: PathBuf,
    /// Long-term storage (e.g. a folder on a second drive). Shows get their own subfolder here.
    pub library: PathBuf,
    /// Player binary. Only VLC's history is understood for automatic progress.
    #[serde(default = "default_player")]
    pub player: String,
}

fn default_player() -> String {
    "vlc".into()
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(fallback))
}

pub fn config_file() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("upnext/config.json")
}
pub fn state_file() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("upnext/state.json")
}
pub fn thumb_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("upnext/thumbs")
}
pub fn vlc_history() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("vlc/vlc-qt-interface.conf")
}

impl Default for Config {
    fn default() -> Self {
        let h = home();
        Config { downloads: h.join("Downloads"), library: h.join("Videos"), player: default_player() }
    }
}

impl Config {
    fn load() -> Config {
        let path = config_file();
        if let Ok(txt) = std::fs::read_to_string(&path) {
            match serde_json::from_str(&txt) {
                Ok(c) => return c,
                Err(e) => eprintln!("upnext: ignoring invalid {}: {e}", path.display()),
            }
            return Config::default();
        }
        let c = Config::default();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(&path, serde_json::to_string_pretty(&c).unwrap_or_default());
        }
        c
    }

    pub fn sources(&self) -> Vec<&Path> {
        let mut v: Vec<&Path> = vec![&self.downloads, &self.library];
        // If one folder is inside the other, only walk the outer one.
        if self.downloads.starts_with(&self.library) {
            v.remove(0);
        } else if self.library.starts_with(&self.downloads) {
            v.remove(1);
        }
        v
    }

    pub fn in_downloads(&self, p: &Path) -> bool {
        p.starts_with(&self.downloads)
    }
}

static CONFIG: LazyLock<Config> = LazyLock::new(Config::load);

pub fn cfg() -> &'static Config {
    &CONFIG
}
