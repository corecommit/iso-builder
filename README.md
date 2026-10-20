# 🧠 iso-builder

An interactive terminal application built in Rust that helps you create custom Linux ISOs without writing complex bash scripts.

## What it does

1. Scans your PC for existing `.iso` files.
2. Lets you select an ISO using your arrow keys.
3. Lets you choose the desktop environment that should be the default session.
4. Keeps existing desktops installed and requests installation of the selected desktop when a package mapping is available.
5. Offers optional system tweaks and custom additions.
6. Generates a reviewed bash script called `run-builder.sh`.
7. You run that script as root to modify the root filesystem and repack the ISO into a new image.

## How to compile and run

### 1. Install Rust
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. Clone the repository

```bash
git clone https://github.com/corecommit/custom-linux-iso.git
cd custom-linux-iso
```

### 3. Compile and run

```bash
cargo run --release
```

### To just build the binary without running it:

```bash
cargo build --release
```

## Desktop environment selection

After choosing a source ISO, select the desktop environment you want to use by default. The builder preserves any desktops already present, requests installation of the selected desktop when its package mapping is supported, and attempts to configure the selected session using common LightDM, SDDM, AccountsService, and `/etc/skel` conventions. Choose **Keep current default** to leave the existing session configuration unchanged.

Desktop package names and session configuration vary by distribution. Unsupported package-manager mappings are reported by the generated script rather than guessed. The generated script must be reviewed and run as root on Linux; generating it does not itself build the ISO.


### Desktop discovery across distributions

Desktop discovery first reads installed X11/Wayland session files. It also scans
installer metadata inside the selected SquashFS image without assuming a
particular distribution: Calamares `packagechooser_desktop.conf` files are
parsed for package choices, and Debian-family tasksel descriptor files are
checked for desktop tasks. Detected installer choices are merged with installed
sessions and shown in the ISO status line.

Not every distribution's installer publishes a desktop chooser. For example,
Ubuntu Desktop's installer does not necessarily contain a list of alternative
desktops; when neither installer metadata nor repository indexes are present in
the ISO, an offline scan cannot truthfully enumerate every package available in
remote repositories. Those cases need a repository-aware discovery backend
using that target system's configured package sources. A detected choice also
does not guarantee package installability unless the selected package manager
has a valid package mapping.
