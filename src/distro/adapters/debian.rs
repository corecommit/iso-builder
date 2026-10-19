//! Debian family: Debian, Ubuntu, Mint, Pop!_OS, Kali and derivatives.

use crate::core::traits::{DistroAdapter, PackageManager};

/// apt, the package manager of the Debian family.
pub struct Apt;

impl PackageManager for Apt {
    fn name(&self) -> &'static str {
        "apt"
    }

    fn install_command(&self, packages: &[String]) -> String {
        if packages.is_empty() {
            "apt-get install -y".to_string()
        } else {
            format!("apt-get install -y {}", packages.join(" "))
        }
    }

    fn update_command(&self) -> String {
        "apt-get update".to_string()
    }

    fn clean_command(&self) -> String {
        "apt-get clean".to_string()
    }

    fn binary_name(&self) -> &'static str {
        "apt-get"
    }
}

/// Adapter for `ID=debian` / `ID=ubuntu` root filesystems.
pub struct DebianAdapter;

impl DistroAdapter for DebianAdapter {
    fn id(&self) -> &'static str {
        "debian"
    }

    fn name(&self) -> &'static str {
        "Debian/Ubuntu"
    }

    fn matches_os_release(&self, id_like: &str) -> bool {
        id_like.split_whitespace().any(|token| {
            token.eq_ignore_ascii_case("debian") || token.eq_ignore_ascii_case("ubuntu")
        })
    }

    fn package_manager(&self) -> Box<dyn PackageManager> {
        Box::new(Apt)
    }
}
