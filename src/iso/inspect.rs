//! Reading the rootfs image: distro name, package manager, desktop.
//!
//! The TUI never unpacks the SquashFS image; `unsquashfs` reads it directly:
//! `-cat` for `etc/os-release` and a single `-l` listing for the binaries and
//! session files. All matching is done by pure functions so it can be tested
//! without a real image.

use std::io;
use std::process::{Command, Output};

use crate::config;
use crate::distro::registry::distros;

/// `KEY=value` pairs from `etc/os-release`, with quotes and surrounding
/// whitespace stripped.
fn os_release_fields(contents: &str) -> Vec<(String, String)> {
    contents
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let value = value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            Some((key.trim().to_string(), value))
        })
        .collect()
}

/// Distro name for the review screen. `PRETTY_NAME` wins; otherwise the
/// registry recognizes `ID` / `ID_LIKE` and, as a last resort, `NAME` is
/// used. An empty or unrecognized file yields "Unknown".
pub fn parse_distro_name(os_release: &str) -> String {
    let fields = os_release_fields(os_release);
    let value = |key: &str| {
        fields
            .iter()
            .find(|(field, _)| field == key)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    };

    let pretty = value("PRETTY_NAME");
    if !pretty.is_empty() {
        return pretty;
    }

    let id = value("ID");
    let id_like = value("ID_LIKE");
    for adapter in distros() {
        if adapter.matches_os_release(&id) || adapter.matches_os_release(&id_like) {
            // The embedded distro config can override the display name; a
            // user override under `$XDG_CONFIG_HOME` wins over it. The
            // compiled adapter name is the final fallback.
            return config::load_builtin(adapter.id())
                .map(|metadata| metadata.name)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| adapter.name().to_string());
        }
    }

    let name = value("NAME");
    if name.is_empty() {
        "Unknown".to_string()
    } else {
        name
    }
}

/// Distro name read from `etc/os-release` inside the SquashFS image. A
/// missing/corrupt image (or a largest Ubuntu layer without `os-release`)
/// degrades to "Unknown" so the user can continue.
pub fn distro_name_from(rootfs_image: &str) -> String {
    match read_file_from_squashfs(rootfs_image, "etc/os-release") {
        Ok(contents) => parse_distro_name(&contents),
        Err(_) => "Unknown".to_string(),
    }
}

/// Complete `unsquashfs -l` listing of the image. Callers run it once and
/// feed the same string to the package-manager and desktop parsers.
pub fn squashfs_listing(rootfs_image: &str) -> Result<String, String> {
    run_unsquashfs(&["-l", rootfs_image])
}

/// Contents of one file inside the image, via `unsquashfs -cat`.
fn read_file_from_squashfs(rootfs_image: &str, path: &str) -> Result<String, String> {
    run_unsquashfs(&["-cat", rootfs_image, path])
}

/// Runs a read-only `unsquashfs` command and captures its stdout.
fn run_unsquashfs(args: &[&str]) -> Result<String, String> {
    let output = Command::new("unsquashfs")
        .args(args)
        .output()
        .map_err(|err| unsquashfs_start_error(err))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(command_error("unsquashfs", &output))
    }
}

fn unsquashfs_start_error(err: io::Error) -> String {
    if err.kind() == io::ErrorKind::NotFound {
        "unsquashfs is not installed (install squashfs-tools)".to_string()
    } else {
        format!("could not run unsquashfs: {}", err)
    }
}

/// True when the listing contains `relative` (e.g. `usr/bin/pacman`) as a
/// complete path. `unsquashfs -l` prefixes every entry with
/// `squashfs-root/`, so a plain suffix match would also accept lookalikes
/// like `usr/bin/pacman-conf`.
fn listing_has_path(listing: &str, relative: &str) -> bool {
    let suffix = format!("/{}", relative);
    listing.lines().any(|line| {
        let path = line.trim().trim_end_matches('/');
        path == relative || path.ends_with(&suffix)
    })
}

/// Package manager whose binary appears in the `unsquashfs -l` output, looked
/// up through the distro registry so the adapters stay the single source of
/// truth. Pure: takes the listing, never touches the filesystem.
pub fn parse_pkg_mgr_from_listing(listing: &str) -> Option<String> {
    for adapter in distros() {
        let manager = adapter.package_manager();
        // `package_manager_binary` from the distro config (user override or
        // embedded default) is authoritative when present; the compiled
        // adapter value is the fallback.
        let binary = config::load_builtin(adapter.id())
            .map(|metadata| metadata.package_manager_binary)
            .filter(|binary| !binary.is_empty())
            .unwrap_or_else(|| manager.binary_name().to_string());
        if binary.is_empty() {
            continue;
        }
        for dir in ["usr/bin", "bin", "usr/sbin", "sbin"] {
            if listing_has_path(listing, &format!("{}/{}", dir, binary)) {
                return Some(manager.name().to_string());
            }
        }
    }
    None
}

/// Desktop session files that map to a desktop environment. `plasma.desktop`
/// (Plasma 6) and `ubuntu.desktop` (Ubuntu's GNOME session) are not in the
/// task sketch's list but are what current releases actually ship.
const DE_SESSION_FILES: &[(&str, &str)] = &[
    ("hyprland.desktop", "Hyprland"),
    ("plasmawayland.desktop", "KDE Plasma"),
    ("plasma.desktop", "KDE Plasma"),
    ("gnome.desktop", "GNOME"),
    ("ubuntu.desktop", "GNOME"),
    ("xfce.desktop", "XFCE"),
    ("sway.desktop", "Sway"),
];

/// Desktop environment found in the `unsquashfs -l` output. Pure.
pub fn desktop_environment_in(listing: &str) -> Option<&'static str> {
    DE_SESSION_FILES
        .iter()
        .find(|(file, _)| {
            listing_has_path(listing, &format!("usr/share/wayland-sessions/{}", file))
                || listing_has_path(listing, &format!("usr/share/xsessions/{}", file))
        })
        .map(|(_, label)| *label)
}

/// Last non-empty stderr line of a failed tool, for compact error messages.
fn command_error(tool: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(200).collect::<String>())
        .unwrap_or_default();
    if line.is_empty() {
        format!("{} exited with status {}", tool, output.status)
    } else {
        format!("{}: {}", tool, line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_managers_are_found_in_unsquashfs_listings() {
        for (paths, expected) in [
            (vec!["squashfs-root/usr/bin/pacman"], Some("pacman")),
            (vec!["squashfs-root/usr/bin/apt-get"], Some("apt")),
            (vec!["squashfs-root/usr/bin/dnf"], Some("dnf")),
            (vec!["squashfs-root/usr/bin/zypper"], Some("zypper")),
            (vec!["squashfs-root/usr/bin/xbps-install"], Some("xbps")),
            (vec!["squashfs-root/usr/bin/emerge"], Some("emerge")),
            (vec!["squashfs-root/sbin/apk"], Some("apk")),
            (vec!["squashfs-root/bin/apk"], Some("apk")),
            (
                vec![
                    "squashfs-root/usr/bin/zypper",
                    "squashfs-root/usr/bin/pacman",
                ],
                Some("pacman"),
            ),
            (vec!["squashfs-root/usr/bin/pacman-conf"], None),
            (vec![], None),
        ] {
            let listing = paths.join("\n");
            assert_eq!(
                parse_pkg_mgr_from_listing(&listing),
                expected.map(str::to_string),
                "listing: {listing:?}"
            );
        }
    }

    #[test]
    fn distro_names_come_from_os_release_or_the_registry() {
        for (os_release, expected) in [
            (
                "PRETTY_NAME=\"CachyOS\"\nID=cachyos\nID_LIKE=arch\n",
                "CachyOS",
            ),
            ("ID=cachyos\nID_LIKE=arch\n", "CachyOS/Arch"),
            ("ID=opensuse\nID_LIKE=\"suse opensuse\"\n", "openSUSE/SUSE"),
            ("ID=void\n", "Void Linux"),
            ("ID=gentoo\n", "Gentoo"),
            ("ID=alpine\n", "Alpine Linux"),
            ("NAME=\"Mystery Linux\"\n", "Mystery Linux"),
            ("not a release file", "Unknown"),
            ("", "Unknown"),
        ] {
            assert_eq!(parse_distro_name(os_release), expected, "{os_release:?}");
        }
    }

    #[test]
    fn desktops_are_found_in_unsquashfs_listings() {
        assert_eq!(
            desktop_environment_in("squashfs-root/usr/share/wayland-sessions/hyprland.desktop"),
            Some("Hyprland")
        );
        assert_eq!(
            desktop_environment_in("squashfs-root/usr/share/xsessions/xfce.desktop"),
            Some("XFCE")
        );
        assert_eq!(
            desktop_environment_in("squashfs-root/usr/share/wayland-sessions/gnome.desktop"),
            Some("GNOME")
        );
        assert_eq!(desktop_environment_in("squashfs-root/usr/bin/pacman"), None);
    }

    #[test]
    fn unreadable_images_degrade_to_unknown() {
        let missing = std::env::temp_dir().join("iso-builder-missing-rootfs.sfs");
        let missing = missing.to_str().unwrap();
        let _ = std::fs::remove_file(missing);
        assert_eq!(distro_name_from(missing), "Unknown");
        assert!(squashfs_listing(missing).is_err());
        assert_eq!(parse_pkg_mgr_from_listing(""), None);
    }
}
