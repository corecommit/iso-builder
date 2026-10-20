//! Desktop discovery from the installer's own configuration.
//!
//! Session files (`usr/share/xsessions`) only show what is installed in the
//! live image, usually one desktop. The installer's config shows what the
//! distro officially *offers*. For Calamares (CachyOS, EndeavourOS, Manjaro,
//! Garuda, ...) that is two files that join on the group name:
//!
//! * `etc/calamares/settings.conf` lists the `packagechooser` instances and
//!   the config file each one reads;
//! * the chooser config with `method: netinstall-select` lists the choices
//!   (`items[].id` / `items[].name`);
//! * the netinstall YAML has one group per id, holding the package list.
//!
//! Everything here is pure (strings in, structs out) except
//! [`installer_desktops`], which reads single files out of the SquashFS image
//! with the same `unsquashfs -cat` the rest of detection uses.

use std::collections::HashMap;

use serde_yaml_ng::Value;

use super::inspect::{listing_has_path, read_file_from_squashfs};

const CALAMARES_SETTINGS: &str = "etc/calamares/settings.conf";
const CALAMARES_MODULES_DIR: &str = "etc/calamares/modules/";

/// One desktop the distro's installer offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopChoice {
    /// Installer id, also the netinstall group name (e.g. "Hyprland").
    pub id: String,
    /// English display name (e.g. "Plasma Desktop").
    pub name: String,
    /// Packages the installer installs by default for this choice,
    /// including subgroups that are pre-selected.
    pub packages: Vec<String>,
    /// Packages from optional (not pre-selected) subgroups.
    pub extras: Vec<String>,
}

/// Everything the script needs to edit the installer's desktop list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallerDesktops {
    /// Rootfs-relative path of the desktop chooser config, empty when the
    /// image has no supported installer.
    pub chooser_path: String,
    /// Rootfs-relative netinstall group files that contain the choices.
    pub netinstall_paths: Vec<String>,
    pub choices: Vec<DesktopChoice>,
}

/// Paths and ids come out of the ISO and end up in a root shell script, so
/// only plain characters are accepted; anything else is ignored.
fn is_safe_rel_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains("..")
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/@+".contains(c))
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

/// Config file names of every `instances:` entry in `settings.conf` whose
/// module is `module` (e.g. "packagechooser_desktop.conf").
fn instance_configs(settings_conf: &str, module: &str) -> Vec<String> {
    let Ok(doc) = serde_yaml_ng::from_str::<Value>(settings_conf) else {
        return Vec::new();
    };
    let Some(instances) = doc.get("instances").and_then(Value::as_sequence) else {
        return Vec::new();
    };
    instances
        .iter()
        .filter(|inst| text(inst, "module").as_deref() == Some(module))
        .filter_map(|inst| text(inst, "config"))
        .collect()
}

/// Chooser config file names from `settings.conf`.
pub fn chooser_config_names(settings_conf: &str) -> Vec<String> {
    instance_configs(settings_conf, "packagechooser")
}

/// Rootfs-relative netinstall group files named by a `netinstall.conf`
/// `groupsUrl` (a string or a list). Remote URLs are ignored; they cannot be
/// edited in the image.
pub fn groups_paths_from_conf(netinstall_conf: &str) -> Vec<String> {
    let Ok(doc) = serde_yaml_ng::from_str::<Value>(netinstall_conf) else {
        return Vec::new();
    };
    let urls: Vec<String> = match doc.get("groupsUrl") {
        Some(Value::String(url)) => vec![url.clone()],
        Some(Value::Sequence(list)) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    urls.iter()
        .filter_map(|url| {
            let path = url.strip_prefix("file://").unwrap_or(url);
            if path.contains("://") {
                return None;
            }
            let rel = path.trim_start_matches('/');
            (path.starts_with('/') || url.starts_with("file://")).then(|| rel.to_string())
        })
        .collect()
}

/// `packagechooser*.conf` files present under `etc/calamares/modules/`,
/// covering ISOs that skip `instances:` and rely on the default file name.
fn chooser_configs_in_listing(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let name = line.trim().split(CALAMARES_MODULES_DIR).nth(1)?;
            (name.starts_with("packagechooser") && name.ends_with(".conf") && !name.contains('/'))
                .then(|| name.to_string())
        })
        .collect()
}

/// Paths (relative to the rootfs) of netinstall group files in the listing.
fn netinstall_paths_in_listing(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let at = line.find("etc/calamares/").or_else(|| line.find("usr/share/calamares/"))?;
            let path = &line[at..];
            (path.ends_with(".yaml") && path.contains("netinstall")).then(|| path.to_string())
        })
        .collect()
}

/// `(id, name)` pairs of a desktop chooser: only configs with
/// `method: netinstall-select` qualify (the bootloader chooser does not).
/// Translated keys such as `name[de]` are different keys and are ignored.
/// The empty-id "No Desktop" entry is kept; callers decide what to do with it.
pub fn parse_desktop_chooser(chooser_conf: &str) -> Option<Vec<(String, String)>> {
    let doc: Value = serde_yaml_ng::from_str(chooser_conf).ok()?;
    if text(&doc, "method").as_deref() != Some("netinstall-select") {
        return None;
    }
    let items = doc.get("items")?.as_sequence()?;
    Some(
        items
            .iter()
            .filter_map(|item| Some((text(item, "id")?, text(item, "name")?)))
            .collect(),
    )
}

fn package_names(group: &Value) -> Vec<String> {
    group
        .get("packages")
        .and_then(Value::as_sequence)
        .map(|packages| {
            packages
                .iter()
                .filter_map(|pkg| match pkg {
                    Value::String(name) => Some(name.clone()),
                    Value::Mapping(_) => text(pkg, "name"),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `(installed by default, optional)` packages of a group. A subgroup counts
/// as installed only with an explicit `selected: true`, like Calamares.
fn group_packages(group: &Value) -> (Vec<String>, Vec<String>) {
    let mut core = package_names(group);
    let mut extras = Vec::new();
    if let Some(subgroups) = group.get("subgroups").and_then(Value::as_sequence) {
        for sub in subgroups {
            let (sub_core, sub_extras) = group_packages(sub);
            if sub.get("selected").and_then(Value::as_bool) == Some(true) {
                core.extend(sub_core);
            } else {
                extras.extend(sub_core);
            }
            extras.extend(sub_extras);
        }
    }
    (core, extras)
}

/// Top-level netinstall groups by name.
pub fn parse_netinstall(netinstall_yaml: &str) -> HashMap<String, (Vec<String>, Vec<String>)> {
    let Ok(Value::Sequence(groups)) = serde_yaml_ng::from_str::<Value>(netinstall_yaml) else {
        return HashMap::new();
    };
    groups
        .iter()
        .filter_map(|group| Some((text(group, "name")?, group_packages(group))))
        .collect()
}

/// Joins chooser items with netinstall groups. Pure; `read` returns the
/// contents of a rootfs-relative path.
pub fn desktops_from_calamares<F>(settings_conf: &str, listing: &str, read: F) -> InstallerDesktops
where
    F: Fn(&str) -> Option<String>,
{
    let mut names = chooser_config_names(settings_conf);
    for name in chooser_configs_in_listing(listing) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let found = names.iter().find_map(|name| {
        let path = format!("{}{}", CALAMARES_MODULES_DIR, name);
        if !is_safe_rel_path(&path) {
            return None;
        }
        Some((path.clone(), parse_desktop_chooser(&read(&path)?)?))
    });
    let Some((chooser_path, items)) = found else {
        return InstallerDesktops::default();
    };

    // groupsUrl from netinstall.conf is the reliable pointer; scanning the
    // listing covers ISOs that keep the file somewhere unexpected.
    let mut conf_names = instance_configs(settings_conf, "netinstall");
    if !conf_names.iter().any(|n| n == "netinstall.conf") {
        conf_names.push("netinstall.conf".to_string());
    }
    let mut candidates: Vec<String> = Vec::new();
    for name in conf_names {
        if let Some(conf) = read(&format!("{}{}", CALAMARES_MODULES_DIR, name)) {
            candidates.extend(groups_paths_from_conf(&conf));
        }
    }
    candidates.extend(netinstall_paths_in_listing(listing));

    let ids: Vec<&str> = items.iter().map(|(id, _)| id.as_str()).collect();
    let mut groups: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
    let mut netinstall_paths: Vec<String> = Vec::new();
    for path in candidates {
        if !is_safe_rel_path(&path) || netinstall_paths.contains(&path) {
            continue;
        }
        let Some(yaml) = read(&path) else { continue };
        let parsed = parse_netinstall(&yaml);
        if parsed.keys().any(|name| ids.contains(&name.as_str())) {
            netinstall_paths.push(path);
        }
        for (name, packages) in parsed {
            groups.entry(name).or_insert(packages);
        }
    }

    let choices = items
        .into_iter()
        .filter(|(id, _)| is_safe_id(id))
        .map(|(id, name)| {
            let (packages, extras) = groups.get(&id).cloned().unwrap_or_default();
            DesktopChoice { id, name, packages, extras }
        })
        .collect();
    InstallerDesktops { chooser_path, netinstall_paths, choices }
}

/// Desktops the installer inside `rootfs_image` offers; empty when the image
/// has no supported installer or its files cannot be read.
pub fn installer_desktops(rootfs_image: &str, listing: &str) -> InstallerDesktops {
    if !listing_has_path(listing, CALAMARES_SETTINGS) {
        return InstallerDesktops::default();
    }
    let read = |path: &str| read_file_from_squashfs(rootfs_image, path).ok();
    let Some(settings) = read(CALAMARES_SETTINGS) else {
        return InstallerDesktops::default();
    };
    desktops_from_calamares(&settings, listing, read)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: &str = "---\ninstances:\n- id: bootloader\n  module: packagechooser\n  config: packagechooser_bootloader.conf\n- id: desktop\n  module: packagechooser\n  config: packagechooser_desktop.conf\n- id: enable_ufw\n  module: shellprocess\n  config: shellprocess_enable_ufw.conf\n";

    const CHOOSER: &str = "---\nmode: required\nmethod: netinstall-select\nlabels:\n    step: \"Desktop\"\n    step[de]: \"Desktop\"\nitems:\n    - id: \"\"\n      name: \"No Desktop\"\n    - id: KDE-Desktop\n      name: \"Plasma Desktop\"\n      name[de]: \"Plasma\"\n    - id: Hyprland\n      name: \"Hyprland\"\n      platform: \"desktop\"\n    - id: GNOME-Desktop\n      name: \"GNOME\"\n";

    const BOOTLOADER_CHOOSER: &str =
        "---\nmethod: legacy\nitems:\n    - id: grub\n      name: \"GRUB\"\n";

    const NETINSTALL: &str = "- name: \"CachyOS required (hidden)\"\n  hidden: true\n  packages:\n    - cachyos-keyring\n- name: \"KDE-Desktop\"\n  description[de]: \"x\"\n  packages:\n    - plasma-desktop\n    - sddm\n- name: \"Hyprland\"\n  packages:\n    - hyprland\n    - kitty\n- name: \"GNOME-Desktop\"\n  packages:\n    - gnome-shell\n    - gdm\n  subgroups:\n      - name: \"GNOME Apps\"\n        critical: false\n        packages:\n          - loupe\n      - name: \"Pre-selected\"\n        selected: true\n        packages:\n          - ptyxis\n";

    const LISTING: &str = "squashfs-root/etc/calamares/settings.conf\nsquashfs-root/etc/calamares/netinstall.yaml\nsquashfs-root/etc/calamares/modules/packagechooser_desktop.conf\nsquashfs-root/etc/calamares/modules/packagechooser_bootloader.conf\nsquashfs-root/etc/calamares/modules/shellprocess_enable_ufw.conf";

    #[test]
    fn chooser_instances_come_from_settings() {
        assert_eq!(
            chooser_config_names(SETTINGS),
            vec!["packagechooser_bootloader.conf", "packagechooser_desktop.conf"]
        );
        assert!(chooser_config_names("not: [valid").is_empty());
    }

    #[test]
    fn only_netinstall_select_choosers_are_desktop_lists() {
        assert!(parse_desktop_chooser(BOOTLOADER_CHOOSER).is_none());
        let items = parse_desktop_chooser(CHOOSER).unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0], (String::new(), "No Desktop".to_string()));
        assert_eq!(items[1], ("KDE-Desktop".to_string(), "Plasma Desktop".to_string()));
    }

    #[test]
    fn subgroups_split_into_default_and_optional_packages() {
        let groups = parse_netinstall(NETINSTALL);
        let (core, extras) = &groups["GNOME-Desktop"];
        assert_eq!(core, &["gnome-shell", "gdm", "ptyxis"]);
        assert_eq!(extras, &["loupe"]);
    }

    #[test]
    fn calamares_choices_join_chooser_items_with_netinstall_groups() {
        let read = |path: &str| match path {
            "etc/calamares/modules/packagechooser_bootloader.conf" => {
                Some(BOOTLOADER_CHOOSER.to_string())
            }
            "etc/calamares/modules/packagechooser_desktop.conf" => Some(CHOOSER.to_string()),
            "etc/calamares/netinstall.yaml" => Some(NETINSTALL.to_string()),
            _ => None,
        };
        let found = desktops_from_calamares(SETTINGS, LISTING, read);
        let ids: Vec<_> = found.choices.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["KDE-Desktop", "Hyprland", "GNOME-Desktop"]);
        assert_eq!(found.choices[1].packages, ["hyprland", "kitty"]);
        assert_eq!(found.choices[0].name, "Plasma Desktop");
        assert_eq!(
            found.chooser_path,
            "etc/calamares/modules/packagechooser_desktop.conf"
        );
        assert_eq!(found.netinstall_paths, ["etc/calamares/netinstall.yaml"]);
    }

    #[test]
    fn groups_url_is_followed_and_remote_urls_are_ignored() {
        assert_eq!(
            groups_paths_from_conf("groupsUrl: file:///etc/calamares/modules/netinstall.yaml\n"),
            ["etc/calamares/modules/netinstall.yaml"]
        );
        assert_eq!(
            groups_paths_from_conf(
                "groupsUrl:\n  - /etc/calamares/a.yaml\n  - https://example.org/b.yaml\n"
            ),
            ["etc/calamares/a.yaml"]
        );
        assert!(groups_paths_from_conf("groupsUrl: local\n").is_empty());
    }

    #[test]
    fn groups_url_wins_over_a_file_the_listing_scan_would_not_find() {
        let read = |path: &str| match path {
            "etc/calamares/modules/packagechooser_desktop.conf" => Some(CHOOSER.to_string()),
            "etc/calamares/modules/netinstall.conf" => {
                Some("groupsUrl: file:///opt/groups.yaml\n".to_string())
            }
            "opt/groups.yaml" => Some(NETINSTALL.to_string()),
            _ => None,
        };
        let listing = "squashfs-root/etc/calamares/modules/packagechooser_desktop.conf";
        let found = desktops_from_calamares(SETTINGS, listing, read);
        assert_eq!(found.netinstall_paths, ["opt/groups.yaml"]);
        assert_eq!(found.choices.len(), 3);
    }

    #[test]
    fn unsafe_ids_and_paths_from_the_image_are_dropped() {
        let chooser = "method: netinstall-select\nitems:\n  - id: \"a b\"\n    name: x\n  - id: ok\n    name: Ok\n  - id: \"$(reboot)\"\n    name: y\n";
        let read = |path: &str| match path {
            "etc/calamares/modules/packagechooser_desktop.conf" => Some(chooser.to_string()),
            _ => None,
        };
        let found = desktops_from_calamares(SETTINGS, "", read);
        let ids: Vec<_> = found.choices.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["ok"]);
        assert!(!is_safe_rel_path("../etc/passwd"));
        assert!(!is_safe_rel_path("etc/a b.yaml"));
        assert!(is_safe_rel_path("etc/calamares/modules/netinstall.yaml"));
    }

    #[test]
    fn missing_files_degrade_to_an_empty_list() {
        assert!(desktops_from_calamares(SETTINGS, LISTING, |_| None).choices.is_empty());
        assert_eq!(desktops_from_calamares("", "", |_| None), InstallerDesktops::default());
    }
}
