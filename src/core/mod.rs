//! Core ports: the traits every distro adapter and package manager implements.

pub mod traits;

pub use traits::{DistroAdapter, PackageManager};
