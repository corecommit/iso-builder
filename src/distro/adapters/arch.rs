//! Arch family: Arch Linux, CachyOS, Manjaro and friends.

use crate::core::traits::{DistroAdapter, PackageManager};

/// pacman, the package manager of the Arch family.
pub struct Pacman;

impl PackageManager for Pacman {
    fn name(&self) -> &'static str {
        "pacman"
    }

    fn install_command(&self, packages: &[String]) -> String {
        if packages.is_empty() {
            "pacman -S --noconfirm".to_string()
        } else {
            format!("pacman -S --noconfirm {}", packages.join(" "))
        }
    }

    fn update_command(&self) -> String {
        "pacman -Sy --noconfirm".to_string()
    }

    fn clean_command(&self) -> String {
        "rm -rf /var/cache/pacman/pkg/*".to_string()
    }

    fn binary_name(&self) -> &'static str {
        "pacman"
    }
}

/// Adapter for `ID=arch` and `ID=cachyos` root filesystems.
pub struct ArchAdapter;

impl DistroAdapter for ArchAdapter {
    fn id(&self) -> &'static str {
        "arch"
    }

    fn name(&self) -> &'static str {
        "CachyOS/Arch"
    }

    fn matches_os_release(&self, id_like: &str) -> bool {
        id_like.split_whitespace().any(|token| {
            token.eq_ignore_ascii_case("arch") || token.eq_ignore_ascii_case("cachyos")
        })
    }

    fn package_manager(&self) -> Box<dyn PackageManager> {
        Box::new(Pacman)
    }
}
