//! The list of adapters consulted for detection, in priority order.

use crate::core::traits::DistroAdapter;
use crate::distro::adapters::arch::ArchAdapter;
use crate::distro::adapters::debian::DebianAdapter;
use crate::distro::adapters::fedora::FedoraAdapter;
use crate::distro::adapters::generic::GenericAdapter;

/// Every adapter known to the tool. Detection walks this list in order, so
/// the dedicated families come first and the generic table entries after.
pub fn distros() -> Vec<Box<dyn DistroAdapter>> {
    vec![
        Box::new(ArchAdapter),
        Box::new(DebianAdapter),
        Box::new(FedoraAdapter),
        Box::new(GenericAdapter::zypper()),
        Box::new(GenericAdapter::xbps()),
        Box::new(GenericAdapter::emerge()),
        Box::new(GenericAdapter::apk()),
        Box::new(GenericAdapter::fallback()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lists_every_adapter_in_detection_order() {
        let adapters = distros();
        let ids: Vec<&str> = adapters.iter().map(|adapter| adapter.id()).collect();
        assert_eq!(
            ids,
            vec!["arch", "debian", "fedora", "opensuse", "void", "gentoo", "alpine", "generic"]
        );

        let managers: Vec<&str> = adapters
            .iter()
            .map(|adapter| adapter.package_manager().name())
            .collect();
        assert_eq!(
            managers,
            vec!["pacman", "apt", "dnf", "zypper", "xbps", "emerge", "apk", "unknown"]
        );
    }
}
