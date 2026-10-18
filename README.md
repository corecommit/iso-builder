# iso-builder

![Rust](https://img.shields.io/badge/rust-2021-orange)
![TUI](https://img.shields.io/badge/TUI-ratatui-blueviolet)

An interactive terminal UI for remastering Linux ISOs. `iso-builder` scans your machine for ISO files, lets you pick one, select extra packages and system tweaks, review the generated remaster script, and export it as a ready-to-run `run-builder.sh`.

Nothing touches your ISO until you run the generated script yourself — the TUI is a wizard that produces the script.

```
🧠 iso-builder | Step 1 of 6: Original ISO File
```

## Features

- **ISO auto-discovery** — scans common locations (`~/Downloads`, `~/Desktop`, `~/ISO`, `/media`, `/mnt`, `/root`, `/Volumes`, drive letters on Windows) up to two folder levels deep.
- **Smart Deep Scan (`[f]`)** — walks the whole filesystem in a background thread for `.iso` files larger than 500 MB, aggressively pruning OS folders (`/usr`, `/proc`, `Windows`, `Program Files`, …) so it stays fast. Duplicate hits from overlapping mounts are de-duplicated by device + inode. Press `Esc` to cancel; the UI stays responsive and `[q]`/`Ctrl+C` always quit immediately.
- **Scan report (`[d]`)** — shows every folder that was searched, with per-folder ISO counts and warnings. Displayed side by side on wide terminals, as a toggleable overlay on narrow ones.
- **Wizard flow** — Original ISO → Desktop Environment → Extra Packages → System Tweaks → Review → Script Preview.
- **Packages & tweaks** — checkbox lists with sizes; only what you select is written into the script. Tweaks include enabling SSH/Bluetooth/firewalld and setting the timezone, applied inside the chroot.
- **Script preview** — the exact bytes that will be saved are shown first; nothing is written until you accept.

## Requirements

**To build the TUI:**

- Rust (edition 2021) and Cargo
- A terminal; works on Linux, macOS and Windows (crossterm backend)

**To run the generated `run-builder.sh`** (on the target machine, as root):

- `xorriso` — extract/repack the ISO
- `squashfs-tools` — `unsquashfs` / `mksquashfs`
- `arch-install-scripts` — `arch-chroot` (or an equivalent chroot wrapper)
- One of `7z`, `xorriso`, `isoinfo` or `bsdtar` for package-manager detection (the script falls back to probing the extracted rootfs)

On Arch Linux:

```bash
sudo pacman -S xorriso squashfs-tools arch-install-scripts
```

## Build & run

```bash
cargo run --release
```

The TUI writes `run-builder.sh` into the current directory and marks it executable.

## How it works

1. **Original ISO File** — pick a discovered ISO, press `[m]` to type a path manually, `[r]` to rescan the standard folders, or `[f]` for a Deep Scan of the entire filesystem.
2. **Desktop Environment** — free-text label (e.g. `Hyprland`, `KDE`, `GNOME`); used in the script header and log output.
3. **Extra Packages** — toggle packages such as `firefox`, `steam`, `docker`, `vlc`, `neovim` with their approximate download sizes.
4. **System Tweaks** — toggle post-install tweaks (`systemctl enable sshd`, timezone, Bluetooth, firewall).
5. **Review & Generate** — summary of your choices; `[Enter]` builds the script and moves to the preview.
6. **Script Preview & Debug** — scroll through the generated script, then `[Enter]` saves it as `run-builder.sh` and exits.

### Keys

| Context | Keys |
| --- | --- |
| ISO list (auto) | `[j/k]` or `[↑/↓]` move · `[Enter]` select · `[m]` manual path · `[f]` Deep Scan · `[r]` rescan · `[d]` scan report · `[q]` quit |
| Typing (path/desktop) | `[Enter]` submit · `[Esc]` stop editing · `[Ctrl+U]` clear · `[Ctrl+C]` quit |
| Checkboxes | `[j/k]` or `[↑/↓]` move · `[Space]` toggle · `[Enter]` continue · `[Esc]` back · `[q]` quit |
| Review | `[Enter]` build & preview · `[Esc]` back |
| Preview | `[j/k]` scroll · `[Enter]` save & exit · `[Esc]` back |

## The generated script

`run-builder.sh` is plain, readable bash with `set -e` at the top:

1. Verifies it runs as root and that the source ISO exists.
2. Detects the distro's package manager (`pacman`, `apt`, `dnf` or `apk`) by listing the ISO with `7z`/`xorriso`/`isoinfo`/`bsdtar`, falling back to a chroot probe. If detection gets it wrong, set `PKG_MGR_OVERRIDE` at the top of the script and re-run.
3. Extracts the ISO with `xorriso` and the squashfs rootfs with `unsquashfs`.
4. Mounts `/proc`, `/sys` and `/dev` into the rootfs, installs the selected packages via `arch-chroot`, and applies each selected tweak.
5. Unmounts, repacks the rootfs with `mksquashfs` (`xz`, 1 MB blocks) and rebuilds `custom-remaster.iso` with `xorriso`.

> **Note:** the final ISO repack assumes an archiso-style boot layout (isolinux + `EFI/archiso/efiboot.img`). Distros with a different boot structure need their own `xorriso` flags.

## Project layout

```
src/main.rs   the whole TUI: scanning, UI, script generation
Cargo.toml    ratatui, crossterm, walkdir
prompt.txt    development task spec used to build the app
```

## License

No license has been chosen for this project yet.
