//! Generic adapter for families that need no bespoke Rust code.
//!
//! The three built-in adapters (arch, debian, fedora) cover the most common
//! targets. openSUSE, Void, Gentoo and Alpine only differ in the command
//! names, so they are described by a small table instead of one file each.

use crate::core::traits::{DistroAdapter, PackageManager};

/// A package manager described by its command templates.
#[derive(Clone, Copy)]
pub struct GenericPackageManager {
    name: &'static str,
    binary_name: &'static str,
    install: &'static str,
    update: &'static str,
    clean: &'static str,
}

impl GenericPackageManager {
    pub const fn new(
        name: &'static str,
        binary_name: &'static str,
        install: &'static str,
        update: &'static str,
        clean: &'static str,
    ) -> Self {
        Self {
            name,
            binary_name,
            install,
            update,
            clean,
        }
    }
}

impl PackageManager for GenericPackageManager {
    fn name(&self) -> &'static str {
        self.name
    }

    fn install_command(&self, packages: &[String]) -> String {
        if self.install.is_empty() || packages.is_empty() {
            self.install.to_string()
        } else {
            format!("{} {}", self.install, packages.join(" "))
        }
    }

    fn update_command(&self) -> String {
        self.update.to_string()
    }

    fn clean_command(&self) -> String {
        self.clean.to_string()
    }

    fn binary_name(&self) -> &'static str {
        self.binary_name
    }
}

/// Adapter driven by a list of `os-release` IDs and a generic package manager.
pub struct GenericAdapter {
    id: &'static str,
    name: &'static str,
    os_ids: &'static [&'static str],
    manager: GenericPackageManager,
}

impl GenericAdapter {
    pub const fn new(
        id: &'static str,
        name: &'static str,
        os_ids: &'static [&'static str],
        manager: GenericPackageManager,
    ) -> Self {
        Self {
            id,
            name,
            os_ids,
            manager,
        }
    }

    /// openSUSE and SUSE Linux Enterprise (`zypper`).
    pub const fn zypper() -> Self {
        Self::new(
            "opensuse",
            "openSUSE/SUSE",
            &["opensuse", "suse", "sles", "sled"],
            GenericPackageManager::new(
                "zypper",
                "zypper",
                "zypper -n install -y",
                "zypper -n --gpg-auto-import-keys refresh",
                "zypper -n clean --all",
            ),
        )
    }

    /// Void Linux (`xbps-install`).
    pub const fn xbps() -> Self {
        Self::new(
            "void",
            "Void Linux",
            &["void"],
            GenericPackageManager::new(
                "xbps",
                "xbps-install",
                "xbps-install -Sy",
                "xbps-install -S",
                "xbps-remove -yO",
            ),
        )
    }

    /// Gentoo and Funtoo (`emerge`).
    pub const fn emerge() -> Self {
        Self::new(
            "gentoo",
            "Gentoo",
            &["gentoo", "funtoo"],
            GenericPackageManager::new(
                "emerge",
                "emerge",
                "emerge --noreplace",
                "emerge --sync",
                "emerge --depclean",
            ),
        )
    }

    /// Alpine Linux and postmarketOS (`apk`).
    pub const fn apk() -> Self {
        Self::new(
            "alpine",
            "Alpine Linux",
            &["alpine", "postmarketos"],
            GenericPackageManager::new(
                "apk",
                "apk",
                "apk add",
                "apk update",
                "rm -rf /var/cache/apk/*",
            ),
        )
    }

    /// Fallback for unknown distros: recognizes nothing, so detection falls
    /// through to `NAME` in `os-release` instead of guessing wrong.
    pub const fn fallback() -> Self {
        Self::new(
            "generic",
            "Generic Linux",
            &[],
            GenericPackageManager::new("unknown", "", "", "", ""),
        )
    }
}

impl DistroAdapter for GenericAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn matches_os_release(&self, id_like: &str) -> bool {
        id_like.split_whitespace().any(|token| {
            self.os_ids
                .iter()
                .any(|known| token.eq_ignore_ascii_case(known))
        })
    }

    fn package_manager(&self) -> Box<dyn PackageManager> {
        Box::new(self.manager)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_commands(adapter: &GenericAdapter, binary: &str, install: &str) {
        let manager = adapter.package_manager();
        assert_eq!(manager.binary_name(), binary);
        let packages = vec!["vim".to_string(), "git".to_string()];
        assert_eq!(
            manager.install_command(&packages),
            format!("{} vim git", install)
        );
        assert!(!manager.update_command().is_empty());
        assert!(!manager.clean_command().is_empty());
    }

    #[test]
    fn generic_adapters_match_their_os_release_ids() {
        let zypper = GenericAdapter::zypper();
        assert_eq!(zypper.id(), "opensuse");
        assert_eq!(zypper.name(), "openSUSE/SUSE");
        for id in ["opensuse", "suse", "sles", "sled", "SUSE"] {
            assert!(zypper.matches_os_release(id), "zypper must match {id}");
        }

        let xbps = GenericAdapter::xbps();
        assert_eq!(xbps.id(), "void");
        assert_eq!(xbps.name(), "Void Linux");
        assert!(xbps.matches_os_release("void"));
        assert!(xbps.matches_os_release("VOID"));

        let emerge = GenericAdapter::emerge();
        assert_eq!(emerge.id(), "gentoo");
        assert_eq!(emerge.name(), "Gentoo");
        assert!(emerge.matches_os_release("gentoo"));
        assert!(emerge.matches_os_release("funtoo"));

        let apk = GenericAdapter::apk();
        assert_eq!(apk.id(), "alpine");
        assert_eq!(apk.name(), "Alpine Linux");
        assert!(apk.matches_os_release("alpine"));
        assert!(apk.matches_os_release("postmarketos"));
    }

    #[test]
    fn generic_adapters_do_not_match_unrelated_families() {
        for adapter in [
            GenericAdapter::zypper(),
            GenericAdapter::xbps(),
            GenericAdapter::emerge(),
            GenericAdapter::apk(),
        ] {
            for id in ["arch", "debian", "ubuntu", "fedora", ""] {
                assert!(
                    !adapter.matches_os_release(id),
                    "{} must not match {id}",
                    adapter.id()
                );
            }
        }
        assert!(!GenericAdapter::fallback().matches_os_release("anything"));
    }

    #[test]
    fn generic_package_managers_build_the_expected_commands() {
        assert_commands(&GenericAdapter::zypper(), "zypper", "zypper -n install -y");
        assert_commands(&GenericAdapter::xbps(), "xbps-install", "xbps-install -Sy");
        assert_commands(&GenericAdapter::emerge(), "emerge", "emerge --noreplace");
        assert_commands(&GenericAdapter::apk(), "apk", "apk add");

        let fallback = GenericAdapter::fallback().package_manager();
        assert_eq!(fallback.name(), "unknown");
        assert_eq!(fallback.binary_name(), "");
        assert_eq!(fallback.install_command(&["vim".to_string()]), "");
        assert_eq!(fallback.update_command(), "");
        assert_eq!(fallback.clean_command(), "");
    }
}
