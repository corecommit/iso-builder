//! The two abstractions the rest of the tool builds on: a package manager
//! knows *how* to install things, a distro adapter knows *which* one a
//! rootfs uses and how to recognize the distro from `os-release`.

pub trait PackageManager: Send + Sync {
    fn name(&self) -> &'static str; // "pacman", "apt", etc.
    fn install_command(&self, packages: &[String]) -> String;
    fn update_command(&self) -> String;
    fn clean_command(&self) -> String;
    fn binary_name(&self) -> &'static str; // "pacman", "apt-get", etc.
}

pub trait DistroAdapter: Send + Sync {
    fn id(&self) -> &'static str; // "arch", "debian", "fedora"
    fn name(&self) -> &'static str; // "CachyOS/Arch", etc.
    fn matches_os_release(&self, id_like: &str) -> bool;
    fn package_manager(&self) -> Box<dyn PackageManager>;
}
