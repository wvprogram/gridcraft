//! The control channel's key: one rule for the app (which writes it) and its clients (the MCP
//! bridge and `gridcraft-cli send`, which read it). See `docs/control-protocol.md`, Keys.
//!
//! The channel runs any command, including opening and saving files, so it must not accept
//! requests from whatever can reach `127.0.0.1`: other programs, other users of the machine, or
//! a web page in the browser posting to the port. The app writes a random key to a file only the
//! user can read; every request carries it.

use std::path::{Path, PathBuf};

/// Environment variable a client may use to pass the key instead of reading the key file.
pub const KEY_ENV: &str = "GRIDCRAFT_CONTROL_KEY";

/// The app's settings folder (preferences, logs, key files): macOS
/// `~/Library/Application Support/GridCraft`, Windows `%APPDATA%\GridCraft`, otherwise
/// `$XDG_CONFIG_HOME/gridcraft` or `~/.config/gridcraft`.
pub fn settings_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/GridCraft"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("GridCraft"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|c| c.join("gridcraft"))
    }
}

/// The key file of the app listening on `port`: `<settings>/control-key.local-<port>`.
pub fn key_path(settings: &Path, port: u16) -> PathBuf {
    settings.join(format!("control-key.local-{port}"))
}

/// Compare a request's key with the app's in time that doesn't depend on where they differ.
/// An empty key never matches.
pub fn keys_match(given: &str, key: &str) -> bool {
    let (a, b) = (given.as_bytes(), key.as_bytes());
    !b.is_empty() && a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_file_per_port() {
        let dir = Path::new("/settings/gridcraft");
        assert_eq!(key_path(dir, 7979), dir.join("control-key.local-7979"));
        assert_eq!(key_path(dir, 7980), dir.join("control-key.local-7980"));
    }

    #[test]
    fn keys_match_exactly() {
        assert!(keys_match("abc123", "abc123"));
        assert!(!keys_match("abc124", "abc123"));
        assert!(!keys_match("abc12", "abc123"));
        assert!(!keys_match("", ""), "an empty key never matches");
        assert!(!keys_match("x", ""));
    }
}
