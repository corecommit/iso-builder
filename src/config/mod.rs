//! Distro metadata: embedded defaults plus optional user overrides.
//!
//! The shipped TOMLs are compiled into the binary with [`include_str!`], so an
//! installed binary no longer depends on its working directory. Users can
//! override any adapter's metadata with:
//!
//! ```text
//! $XDG_CONFIG_HOME/iso-builder/distros/<id>.toml
//! ```
//!
//! When `XDG_CONFIG_HOME` is unset, `~/.config` is used, per the XDG base
//! directory specification. A user override takes precedence over the embedded
//! default; a missing, malformed, or mismatched override is ignored.

use std::env;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::IsoBuilderError;

/// Shipped metadata, embedded at compile time and keyed by adapter id.
const EMBEDDED_CONFIGS: &[(&str, &str)] = &[
    ("arch", include_str!("../../config/distros/arch.toml")),
    ("debian", include_str!("../../config/distros/debian.toml")),
    ("fedora", include_str!("../../config/distros/fedora.toml")),
];

/// Contents of one `<id>.toml` distro config.
#[derive(Debug, Clone, Deserialize)]
pub struct DistroConfig {
    pub id: String,
    pub name: String,
    pub package_manager_binary: String,
}

/// Reads and parses a distro config file.
pub fn load_distro_config(path: impl AsRef<Path>) -> Result<DistroConfig, IsoBuilderError> {
    let text = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&text)?)
}

/// Loads metadata for an adapter id (`arch`, `debian`, ...).
///
/// A user override in `$XDG_CONFIG_HOME/iso-builder/distros/<id>.toml` wins;
/// otherwise the embedded shipped config is used. Returns `None` for unknown
/// ids when no override exists. Callers still fall back to the compiled
/// adapter defaults, so a missing config never breaks detection.
pub fn load_builtin(id: &str) -> Option<DistroConfig> {
    load_builtin_from(user_override_dir().as_deref(), id)
}

/// Directory for per-user overrides, following the XDG base directory spec.
fn user_override_dir() -> Option<PathBuf> {
    let xdg_config_home = env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    override_dir_for(xdg_config_home.as_deref(), home.as_deref())
}

/// Pure path logic, split from the environment so tests are deterministic.
fn override_dir_for(xdg_config_home: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(xdg) = xdg_config_home {
        return Some(xdg.join("iso-builder").join("distros"));
    }
    home.map(|home| home.join(".config").join("iso-builder").join("distros"))
}

/// Override-first lookup with an explicit override directory. Tests call this
/// directly so they do not depend on the developer's real `$XDG_CONFIG_HOME`.
fn load_builtin_from(override_dir: Option<&Path>, id: &str) -> Option<DistroConfig> {
    override_dir
        .and_then(|dir| load_override(dir, id))
        .or_else(|| embedded_config(id))
}

fn load_override(dir: &Path, id: &str) -> Option<DistroConfig> {
    let config = load_distro_config(dir.join(format!("{}.toml", id))).ok()?;
    // A config copied under the wrong filename should not silently retarget
    // another adapter's metadata.
    (config.id == id).then_some(config)
}

fn embedded_config(id: &str) -> Option<DistroConfig> {
    let text = EMBEDDED_CONFIGS
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, text)| *text)?;
    toml::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("iso-builder-config-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn embedded_configs_are_available_by_id() {
        for (id, name, binary) in [
            ("arch", "CachyOS/Arch", "pacman"),
            ("debian", "Debian/Ubuntu", "apt-get"),
            ("fedora", "Fedora", "dnf"),
        ] {
            let config = load_builtin_from(None, id).expect("embedded config must parse");
            assert_eq!(config.id, id);
            assert_eq!(config.name, name);
            assert_eq!(config.package_manager_binary, binary);
        }
        assert!(load_builtin_from(None, "not-a-distro").is_none());
    }

    #[test]
    fn user_override_wins_and_missing_ids_use_embedded_configs() {
        let dir = temp_dir("override");
        fs::write(
            dir.join("arch.toml"),
            "id = \"arch\"\nname = \"My Arch\"\npackage_manager_binary = \"pacman\"\n",
        )
        .unwrap();

        let arch = load_builtin_from(Some(&dir), "arch").unwrap();
        assert_eq!(arch.name, "My Arch");

        // No override file: the embedded Debian config still answers.
        let debian = load_builtin_from(Some(&dir), "debian").unwrap();
        assert_eq!(debian.name, "Debian/Ubuntu");

        // Unknown ids can be supplied entirely by a user override.
        fs::write(
            dir.join("custom.toml"),
            "id = \"custom\"\nname = \"Custom\"\npackage_manager_binary = \"custom-pm\"\n",
        )
        .unwrap();
        assert_eq!(
            load_builtin_from(Some(&dir), "custom").unwrap().name,
            "Custom"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_or_mismatched_overrides_fall_back_to_embedded() {
        let dir = temp_dir("bad-override");
        fs::write(dir.join("arch.toml"), "id = [\n").unwrap();
        assert_eq!(
            load_builtin_from(Some(&dir), "arch").unwrap().name,
            "CachyOS/Arch"
        );

        // Correct TOML, but the id does not match the filename.
        fs::write(
            dir.join("arch.toml"),
            "id = \"debian\"\nname = \"Wrong\"\npackage_manager_binary = \"apt-get\"\n",
        )
        .unwrap();
        assert_eq!(
            load_builtin_from(Some(&dir), "arch").unwrap().name,
            "CachyOS/Arch"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn override_directory_follows_xdg_rules() {
        assert_eq!(
            override_dir_for(Some(Path::new("/xdg")), Some(Path::new("/home/user"))),
            Some(PathBuf::from("/xdg/iso-builder/distros"))
        );
        assert_eq!(
            override_dir_for(None, Some(Path::new("/home/user"))),
            Some(PathBuf::from("/home/user/.config/iso-builder/distros"))
        );
        assert_eq!(override_dir_for(None, None), None);
    }

    #[test]
    fn malformed_config_reports_an_error() {
        let dir = temp_dir("malformed");
        let path = dir.join("broken.toml");
        fs::write(&path, "id = [\n").unwrap();
        assert!(load_distro_config(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
