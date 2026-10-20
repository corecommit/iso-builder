//! Package and tweak catalog shown by the wizard.

/// Package managers the generated script knows how to drive. These are the
/// keys used in every package table below.
pub const PKG_MANAGERS: [&str; 7] = ["pacman", "apt", "dnf", "zypper", "xbps", "emerge", "apk"];

/// `(package manager, package name)` pairs. A manager that is missing from
/// the table has no (known) package for that entry; the script then prints a
/// `[SKIP]` line instead of guessing a name that does not exist.
pub type PkgIds = &'static [(&'static str, &'static str)];

/// A desktop environment that can be installed into the target root filesystem.
/// Package names are intentionally explicit per package manager; unsupported
/// combinations are reported rather than guessed.
pub struct DesktopOption {
    pub name: &'static str,
    pub key: &'static str,
    pub desc: &'static str,
    pub ids: PkgIds,
    pub selected: bool,
}

impl DesktopOption {
    pub fn id_for(&self, pkg_mgr: &str) -> Option<&'static str> {
        self.ids.iter().find(|(mgr, _)| *mgr == pkg_mgr).map(|(_, id)| *id)
    }
}

pub fn default_desktops() -> Vec<DesktopOption> {
    vec![
        DesktopOption { name: "Keep current default", key: "keep", desc: "Do not install or change the default session", ids: &[], selected: true },
        DesktopOption { name: "GNOME", key: "gnome", desc: "Modern, streamlined desktop", ids: &[("pacman", "gnome"), ("apt", "gnome-shell gnome-session"), ("dnf", "@gnome-desktop"), ("zypper", "patterns-gnome-gnome"), ("xbps", "gnome")], selected: false },
        DesktopOption { name: "KDE Plasma", key: "plasma", desc: "Feature-rich and highly customizable", ids: &[("pacman", "plasma-meta"), ("apt", "kde-standard"), ("dnf", "@kde-desktop-environment"), ("zypper", "patterns-kde-kde_plasma"), ("xbps", "kde5")], selected: false },
        DesktopOption { name: "Xfce", key: "xfce", desc: "Lightweight and familiar", ids: &[("pacman", "xfce4 xfce4-goodies"), ("apt", "xfce4"), ("dnf", "@xfce-desktop"), ("zypper", "patterns-xfce-xfce"), ("xbps", "xfce4")], selected: false },
        DesktopOption { name: "Cinnamon", key: "cinnamon", desc: "Traditional desktop layout", ids: &[("pacman", "cinnamon"), ("apt", "cinnamon-desktop-environment"), ("dnf", "@cinnamon-desktop"), ("zypper", "patterns-cinnamon-cinnamon"), ("xbps", "cinnamon")], selected: false },
        DesktopOption { name: "MATE", key: "mate", desc: "Classic, lightweight desktop", ids: &[("pacman", "mate mate-extra"), ("apt", "mate-desktop-environment"), ("dnf", "@mate-desktop"), ("zypper", "patterns-mate-mate"), ("xbps", "mate")], selected: false },
        DesktopOption { name: "LXQt", key: "lxqt", desc: "Lightweight Qt-based desktop", ids: &[("pacman", "lxqt"), ("apt", "lxqt"), ("dnf", "@lxqt-desktop"), ("zypper", "patterns-lxqt-lxqt"), ("xbps", "lxqt")], selected: false },
    ]
}

pub struct PackageOption {
    pub name: &'static str,
    pub desc: &'static str,
    pub size: &'static str,
    pub selected: bool,
    pub ids: PkgIds,
}

impl PackageOption {
    /// The package name this entry has under `pkg_mgr`, if any.
    pub fn id_for(&self, pkg_mgr: &str) -> Option<&'static str> {
        self.ids
            .iter()
            .find(|(mgr, _)| *mgr == pkg_mgr)
            .map(|(_, id)| *id)
    }
}

pub struct PresetOption {
    pub name: &'static str,
    pub desc: &'static str,
    /// POSIX sh snippet that runs inside the chroot.
    pub command: String,
    /// Packages the tweak needs (installed together with the other packages).
    pub packages: PkgIds,
    pub selected: bool,
}

pub fn default_packages() -> Vec<PackageOption> {
    vec![
        PackageOption {
            name: "firefox",
            desc: "Web Browser",
            size: "~250 MB",
            selected: false,
            // Ubuntu ships Firefox as a snap, which cannot work inside a
            // chroot; on Ubuntu-based ISOs this entry reports a warning.
            ids: &[
                ("pacman", "firefox"),
                ("apt", "firefox-esr"),
                ("dnf", "firefox"),
                ("zypper", "MozillaFirefox"),
                ("xbps", "firefox"),
                ("emerge", "www-client/firefox-bin"),
                ("apk", "firefox"),
            ],
        },
        PackageOption {
            name: "steam",
            desc: "Gaming Platform",
            size: "~1.5 GB",
            selected: false,
            // Needs [multilib] on Arch (enabled by the script), non-free /
            // i386 on Debian. Other families need third-party repositories.
            ids: &[
                ("pacman", "steam"),
                ("apt", "steam-installer"),
                ("zypper", "steam"),
            ],
        },
        PackageOption {
            name: "discord",
            desc: "Voice & Text Chat",
            size: "~300 MB",
            selected: false,
            ids: &[("pacman", "discord")],
        },
        PackageOption {
            name: "neovim",
            desc: "Terminal Text Editor",
            size: "~40 MB",
            selected: false,
            ids: &[
                ("pacman", "neovim"),
                ("apt", "neovim"),
                ("dnf", "neovim"),
                ("zypper", "neovim"),
                ("xbps", "neovim"),
                ("emerge", "app-editors/neovim"),
                ("apk", "neovim"),
            ],
        },
        PackageOption {
            name: "htop",
            desc: "Process Monitor",
            size: "~5 MB",
            selected: false,
            ids: &[
                ("pacman", "htop"),
                ("apt", "htop"),
                ("dnf", "htop"),
                ("zypper", "htop"),
                ("xbps", "htop"),
                ("emerge", "sys-process/htop"),
                ("apk", "htop"),
            ],
        },
        PackageOption {
            name: "git",
            desc: "Version Control",
            size: "~50 MB",
            selected: false,
            ids: &[
                ("pacman", "git"),
                ("apt", "git"),
                ("dnf", "git"),
                ("zypper", "git"),
                ("xbps", "git"),
                ("emerge", "dev-vcs/git"),
                ("apk", "git"),
            ],
        },
        PackageOption {
            name: "docker",
            desc: "Container Runtime",
            size: "~700 MB",
            selected: false,
            ids: &[
                ("pacman", "docker"),
                ("apt", "docker.io"),
                ("dnf", "moby-engine"),
                ("zypper", "docker"),
                ("xbps", "docker"),
                ("emerge", "app-containers/docker"),
                ("apk", "docker"),
            ],
        },
        PackageOption {
            name: "vlc",
            desc: "Media Player",
            size: "~200 MB",
            selected: false,
            ids: &[
                ("pacman", "vlc"),
                ("apt", "vlc"),
                ("dnf", "vlc"),
                ("zypper", "vlc"),
                ("xbps", "vlc"),
                ("emerge", "media-video/vlc"),
                ("apk", "vlc"),
            ],
        },
        PackageOption {
            name: "curl",
            desc: "Transfer Tool",
            size: "~5 MB",
            selected: false,
            ids: &[
                ("pacman", "curl"),
                ("apt", "curl"),
                ("dnf", "curl"),
                ("zypper", "curl"),
                ("xbps", "curl"),
                ("emerge", "net-misc/curl"),
                ("apk", "curl"),
            ],
        },
        PackageOption {
            name: "tmux",
            desc: "Terminal Multiplexer",
            size: "~2 MB",
            selected: false,
            ids: &[
                ("pacman", "tmux"),
                ("apt", "tmux"),
                ("dnf", "tmux"),
                ("zypper", "tmux"),
                ("xbps", "tmux"),
                ("emerge", "app-misc/tmux"),
                ("apk", "tmux"),
            ],
        },
    ]
}

const SSH_PACKAGES: PkgIds = &[
    ("pacman", "openssh"),
    ("apt", "openssh-server"),
    ("dnf", "openssh-server"),
    ("zypper", "openssh"),
    ("xbps", "openssh"),
    ("emerge", "net-misc/openssh"),
    ("apk", "openssh"),
];

const BLUETOOTH_PACKAGES: PkgIds = &[
    ("pacman", "bluez"),
    ("apt", "bluez"),
    ("dnf", "bluez"),
    ("zypper", "bluez"),
    ("xbps", "bluez"),
    ("emerge", "net-wireless/bluez"),
    ("apk", "bluez"),
];

const FIREWALL_PACKAGES: PkgIds = &[
    ("pacman", "firewalld"),
    ("apt", "firewalld"),
    ("dnf", "firewalld"),
    ("zypper", "firewalld"),
];

const NETWORKMANAGER_PACKAGES: PkgIds = &[
    ("pacman", "networkmanager"),
    ("apt", "network-manager"),
    ("dnf", "NetworkManager"),
    ("zypper", "NetworkManager"),
    ("xbps", "NetworkManager"),
    ("emerge", "net-misc/networkmanager"),
    ("apk", "networkmanager"),
];

/// sh snippet that enables the first of `units` whose `.service` file exists
/// in the rootfs. Unit names differ between distros (`ssh` on Debian, `sshd`
/// elsewhere), so naming one blindly would fail on half of them.
fn enable_unit_command(units: &[&str]) -> String {
    format!(
        "for u in {}; do for d in /usr/lib/systemd/system /lib/systemd/system /etc/systemd/system; do if [ -e \"$d/$u.service\" ]; then systemctl enable \"$u.service\"; exit $?; fi; done; done; echo 'no matching systemd unit found (is the package installed?)'; exit 1",
        units.join(" ")
    )
}

pub fn default_tweaks() -> Vec<PresetOption> {
    vec![
        PresetOption {
            name: "Enable SSH",
            desc: "Install openssh, start sshd on boot",
            command: enable_unit_command(&["sshd", "ssh"]),
            packages: SSH_PACKAGES,
            selected: false,
        },
        PresetOption {
            name: "Timezone (UTC)",
            desc: "Link /etc/localtime to UTC",
            command: "ln -sf /usr/share/zoneinfo/UTC /etc/localtime".to_string(),
            packages: &[],
            selected: false,
        },
        PresetOption {
            name: "Enable Bluetooth",
            desc: "Install bluez, start bluetooth on boot",
            command: enable_unit_command(&["bluetooth"]),
            packages: BLUETOOTH_PACKAGES,
            selected: false,
        },
        PresetOption {
            name: "Enable Firewall",
            desc: "Install firewalld, start it on boot",
            command: enable_unit_command(&["firewalld"]),
            packages: FIREWALL_PACKAGES,
            selected: false,
        },
        PresetOption {
            name: "Enable NetworkManager",
            desc: "Install it, start on boot",
            command: enable_unit_command(&["NetworkManager"]),
            packages: NETWORKMANAGER_PACKAGES,
            selected: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_ids_are_per_manager() {
        let packages = default_packages();
        let docker = packages.iter().find(|p| p.name == "docker").unwrap();
        assert_eq!(docker.id_for("apt"), Some("docker.io"));
        assert_eq!(docker.id_for("dnf"), Some("moby-engine"));
        let discord = packages.iter().find(|p| p.name == "discord").unwrap();
        assert_eq!(discord.id_for("apt"), None);
    }
}
