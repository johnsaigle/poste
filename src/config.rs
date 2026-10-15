use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    root: Option<String>,
}

/// Config file location. Override with `POSTE_CONFIG` (used by tests).
pub fn config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POSTE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    dirs::config_dir().map(|d| d.join("poste/config.toml"))
}

/// Expand a leading `~/` to the user's home directory.
pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(p)
}

pub fn default_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Documents/email-backup")
}

/// Resolve the archive root: `--root` flag > config file > default.
/// A malformed config is warned about and ignored, never fatal.
pub fn resolve_root(cli_root: Option<PathBuf>) -> PathBuf {
    if let Some(r) = cli_root {
        return r;
    }
    if let Some(path) = config_path()
        && let Ok(s) = std::fs::read_to_string(&path)
    {
        match toml::from_str::<ConfigFile>(&s) {
            Ok(f) => {
                if let Some(root) = f.root {
                    return expand_tilde(&root);
                }
            }
            Err(e) => {
                eprintln!("warning: ignoring {}: {e}", path.display());
            }
        }
    }
    default_root()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_tilde() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~/foo/bar"), home.join("foo/bar"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_tilde("relative"), PathBuf::from("relative"));
        assert_eq!(expand_tilde("~notilde"), PathBuf::from("~notilde"));
    }

    #[test]
    fn parses_config() {
        let f: ConfigFile = toml::from_str("root = \"~/mail\"\n").unwrap();
        assert_eq!(f.root, Some("~/mail".to_string()));
        let f: ConfigFile = toml::from_str("").unwrap();
        assert_eq!(f.root, None);
    }
}
