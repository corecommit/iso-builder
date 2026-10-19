//! iso-builder: a terminal wizard for customizing Linux installer ISOs.
//!
//! Select a base ISO (for example, CachyOS), choose a desktop environment,
//! remove or install packages, apply tweaks, and add custom shell commands.
//! The TUI prepares a reviewed `run-builder.sh` script; that script performs
//! the actual root filesystem changes, package operations, and ISO repacking.
//! Linux only: the generated script needs root, `chroot` and `mount`.
//!
//! The crate is split into layers:
//!
//! - `core`     - ports (traits) for distros and package managers
//! - `distro`   - adapters (arch, debian, fedora, generic) and the registry
//! - `iso`      - discovery, extraction, inspection and the workspace
//! - `commands` - the generated remaster script
//! - `app`      - state machine, CLI and event loop
//! - `ui`       - rendering and widgets
//! - `config`   - embedded distro metadata plus `$XDG_CONFIG_HOME` overrides
//! - `packages` - package and tweak catalog
//! - `utils`    - small shared helpers
//! - `error`    - the shared `IsoBuilderError` type

#[cfg(not(unix))]
compile_error!(
    "iso-builder only supports Linux/unix: the generated script needs root, chroot and mount."
);

pub mod app;
pub mod commands;
pub mod config;
pub mod core;
pub mod distro;
pub mod error;
pub mod iso;
pub mod packages;
pub mod ui;
pub mod utils;
