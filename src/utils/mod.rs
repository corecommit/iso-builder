//! Small helpers shared by the TUI, the ISO layer and the script generator.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// A package name typed by the user. Deliberately strict: no shell
/// metacharacters and no leading dash (which would be read as an option).
pub fn is_safe_package_token(token: &str) -> bool {
    !token.is_empty()
        && !token.starts_with('-')
        && token.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '_' | ':' | '@' | '/' | '-' | '=')
        })
}

/// RFC 1123 style host name: dot-separated labels of letters, digits and
/// hyphens, none starting or ending with a hyphen.
pub fn is_valid_hostname(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// File name of the remastered ISO. Empty input means `<source>-custom.iso`.
pub fn output_name_for(raw: &str, iso_path: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        let stem = Path::new(iso_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("custom");
        return Ok(format!("{}-custom.iso", stem));
    }
    if raw.contains('/') {
        return Err("The output name must be a file name, not a path.".to_string());
    }
    if raw.to_ascii_lowercase().ends_with(".iso") {
        Ok(raw.to_string())
    } else {
        Ok(format!("{}.iso", raw))
    }
}

/// Cleans up a path typed or pasted into the manual field: surrounding
/// quotes (drag & drop in a terminal adds them) and a leading `~`.
pub fn normalize_user_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let unquoted = trimmed
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .or_else(|| {
            trimmed
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .unwrap_or(trimmed);

    if unquoted == "~" {
        if let Some(home) = home_dir() {
            return home.display().to_string();
        }
    }
    if let Some(rest) = unquoted.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    unquoted.to_string()
}

fn common_prefix(a: &str, b: &str) -> String {
    a.chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x)
        .collect()
}

/// [Tab] completion for the manual path field: extends the input to the
/// longest common prefix of the matching directory entries.
pub fn complete_path(input: &str) -> String {
    let expanded = normalize_user_path(input);
    let (dir_part, prefix) = match expanded.rfind('/') {
        Some(index) => (
            expanded[..=index].to_string(),
            expanded[index + 1..].to_string(),
        ),
        None => (String::new(), expanded.clone()),
    };
    let read_target = if dir_part.is_empty() {
        ".".to_string()
    } else {
        dir_part.clone()
    };
    let Ok(entries) = fs::read_dir(&read_target) else {
        return expanded;
    };

    let mut names: Vec<(String, bool)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) {
                return None;
            }
            if name.starts_with('.') && !prefix.starts_with('.') {
                return None;
            }
            let is_dir = entry.path().is_dir();
            Some((name, is_dir))
        })
        .collect();
    if names.is_empty() {
        return expanded;
    }
    names.sort();

    let common = names
        .iter()
        .skip(1)
        .fold(names[0].0.clone(), |acc, (name, _)| {
            common_prefix(&acc, name)
        });
    let mut result = format!("{}{}", dir_part, common);
    if names.len() == 1 && names[0].1 {
        result.push('/');
    }
    result
}

/// Formats a byte count for humans: "~3.0 GB", "350 MB", "12 KB", "480 B".
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let value = bytes as f64;
    if value >= GB {
        format!("~{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{} MB", (value / MB).round() as u64)
    } else if value >= KB {
        format!("{} KB", (value / KB).round() as u64)
    } else {
        format!("{} B", bytes)
    }
}

/// "1 ISO" / "3 ISOs" - the unit used in scan report lines.
pub fn iso_count_unit(count: usize) -> &'static str {
    if count == 1 {
        "ISO"
    } else {
        "ISOs"
    }
}

/// Best-effort home directory: `$HOME` first, `$USERPROFILE` on Windows, and
/// `/root` as a last resort when running as root with neither variable set.
pub fn home_dir() -> Option<PathBuf> {
    if let Some(home) = env::var_os("HOME").filter(|home| !home.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(home) = env::var_os("USERPROFILE").filter(|home| !home.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if env::var("USER").map_or(false, |user| user == "root") {
        return Some(PathBuf::from("/root"));
    }
    None
}

/// `<home>/<suffix>`; an empty suffix yields the home directory itself.
pub fn home_subdir(home: &Option<PathBuf>, suffix: &str) -> Option<PathBuf> {
    home.as_ref().map(|home| {
        if suffix.is_empty() {
            home.clone()
        } else {
            home.join(suffix)
        }
    })
}

/// Whether `name` is on `$PATH`.
fn tool_in_path(name: &str) -> bool {
    env::var_os("PATH").map_or(false, |paths| {
        env::split_paths(&paths).any(|dir| dir.join(name).is_file())
    })
}

/// Tools the generated script needs, with whether they were found.
pub fn detect_tools() -> Vec<(&'static str, bool)> {
    [
        "xorriso",
        "unsquashfs",
        "mksquashfs",
        "chroot",
        "mountpoint",
        "sha256sum",
    ]
    .iter()
    .map(|name| (*name, tool_in_path(name)))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames_are_validated() {
        assert!(is_valid_hostname("box"));
        assert!(is_valid_hostname("my-box.example.org"));
        assert!(!is_valid_hostname(""));
        assert!(!is_valid_hostname("-box"));
        assert!(!is_valid_hostname("box-"));
        assert!(!is_valid_hostname("bo x"));
        assert!(!is_valid_hostname("a;rm -rf /"));
    }

    #[test]
    fn package_tokens_reject_shell_syntax() {
        assert!(is_safe_package_token("firefox"));
        assert!(is_safe_package_token("lib32-mesa"));
        assert!(is_safe_package_token("www-client/firefox-bin"));
        assert!(is_safe_package_token("pkg=1.2.3"));
        assert!(!is_safe_package_token("--noconfirm"));
        assert!(!is_safe_package_token("a;b"));
        assert!(!is_safe_package_token("$(id)"));
        assert!(!is_safe_package_token("a b"));
    }

    #[test]
    fn output_name_defaults_and_validation() {
        assert_eq!(
            output_name_for("", "/isos/cachyos.iso").unwrap(),
            "cachyos-custom.iso"
        );
        assert_eq!(output_name_for("mine", "/x.iso").unwrap(), "mine.iso");
        assert_eq!(output_name_for("mine.ISO", "/x.iso").unwrap(), "mine.ISO");
        assert!(output_name_for("../evil.iso", "/x.iso").is_err());
    }

    #[test]
    fn user_paths_are_normalised() {
        assert_eq!(normalize_user_path("  '/tmp/a b.iso'  "), "/tmp/a b.iso");
        assert_eq!(normalize_user_path("\"/tmp/x.iso\""), "/tmp/x.iso");
        assert_eq!(normalize_user_path("/tmp/x.iso"), "/tmp/x.iso");
    }

    #[test]
    fn common_prefix_stops_at_first_difference() {
        assert_eq!(common_prefix("ubuntu-24", "ubuntu-22"), "ubuntu-2");
        assert_eq!(common_prefix("abc", "xyz"), "");
    }
}
