//! Fedora family: Fedora and its close rebuilds.

use crate::core::traits::{DistroAdapter, PackageManager};

/// dnf, the package manager of the Fedora family.
pub struct Dnf;

impl PackageManager for Dnf {
    fn name(&self) -> &'static str {
        "dnf"
    }

    fn install_command(&self, packages: &[String]) -> String {
        if packages.is_empty() {
            "dnf install -y".to_string()
        } else {
            format!("dnf install -y {}", packages.join(" "))
        }
    }

    fn update_command(&self) -> String {
        "dnf makecache".to_string()
    }

    fn clean_command(&self) -> String {
        "dnf clean all".to_string()
    }

    fn binary_name(&self) -> &'static str {
        "dnf"
    }
}

/// Adapter for `ID=fedora` root filesystems.
pub struct FedoraAdapter;

impl DistroAdapter for FedoraAdapter {
    fn id(&self) -> &'static str {
        "fedora"
    }

    fn name(&self) -> &'static str {
        "Fedora"
    }

    fn matches_os_release(&self, id_like: &str) -> bool {
        id_like
            .split_whitespace()
            .any(|token| token.eq_ignore_ascii_case("fedora"))
    }

    fn package_manager(&self) -> Box<dyn PackageManager> {
        Box::new(Dnf)
    }
}
