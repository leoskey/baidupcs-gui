//! 本机小文件持久化：胶囊位置、暂停记录。目录：
//! Windows `%LOCALAPPDATA%`，macOS `~/Library/Application Support`，Linux `$XDG_DATA_HOME` 或 `~/.local/share`，
//! 下的 `baidupcs-gui`。

use std::path::PathBuf;

use serde::{Serialize, de::DeserializeOwned};

pub fn dir() -> PathBuf {
    let env = |name| std::env::var_os(name).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library").join("Application Support"))
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local").join("share")))
    };
    base.unwrap_or_else(std::env::temp_dir)
        .join("baidupcs-gui")
}

pub fn load<T: DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_slice(&std::fs::read(dir().join(name)).ok()?).ok()
}

pub fn save<T: Serialize>(name: &str, value: &T) {
    let _ = std::fs::create_dir_all(dir());
    if let Ok(json) = serde_json::to_vec(value) {
        let _ = std::fs::write(dir().join(name), json);
    }
}
