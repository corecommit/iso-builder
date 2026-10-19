//! Building and writing the reviewed `run-builder.sh` script.

use std::fs::{self, File};
use std::io::{self, Write};

use crate::app::state::App;
use crate::iso::workspace::work_dir_for;
use crate::packages::{PackageOption, PKG_MANAGERS};

/// Script written when iso-builder exits.
pub const SCRIPT_FILE: &str = "run-builder.sh";

/// Wraps a value in single quotes for bash, escaping embedded single quotes.
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// First half of the script: settings, safety nets (cleanup trap, mount
/// guard), helpers, extraction, package manager detection and the chroot
/// mounts. Everything dynamic (the ISO path, workspace, selections) is
/// injected around this static text.
const SCRIPT_PRELUDE: &str = r##"
# Package manager reference (who uses what):
#   pacman -> Arch, CachyOS, Manjaro, EndeavourOS, Garuda, Artix
#   apt    -> Debian, Ubuntu, Linux Mint, Pop!_OS, Kali, Zorin OS
#   dnf    -> Fedora, RHEL, CentOS, Rocky, AlmaLinux
#   zypper -> openSUSE, SUSE Linux Enterprise
#   xbps   -> Void Linux
#   emerge -> Gentoo, Funtoo
#   apk    -> Alpine Linux, postmarketOS

SCRIPT_DIR=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
cd "$SCRIPT_DIR"
WORK_DIR="$SCRIPT_DIR/$WORK_REL"
ROOTFS="$WORK_DIR/rootfs"
ISO_ROOT="$WORK_DIR/iso-root"
MARKER="$WORK_DIR/.extracted"
OUTPUT_ISO="$SCRIPT_DIR/$OUTPUT_NAME"

# Settings you can override from the environment, e.g.
#   sudo KEEP_WORKSPACE=1 ./run-builder.sh
PKG_MGR_OVERRIDE="${PKG_MGR_OVERRIDE:-}"  # force pacman|apt|dnf|zypper|xbps|emerge|apk
SQUASH_COMP="${SQUASH_COMP:-xz}"          # xz = smallest image, zstd = fastest build
KEEP_WORKSPACE="${KEEP_WORKSPACE:-0}"     # 1 = keep the extracted files afterwards
FORCE="${FORCE:-}"                        # any value = skip the free-disk-space check

if [ "$(id -u)" -ne 0 ]; then
    echo "Please run this script as root:  sudo ./run-builder.sh"
    exit 1
fi

MISSING=""
for tool in xorriso unsquashfs mksquashfs chroot mountpoint sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || MISSING="$MISSING $tool"
done
if [ -n "$MISSING" ]; then
    echo "Missing required tools:$MISSING"
    echo "Install xorriso, squashfs-tools, coreutils and util-linux, then run this script again."
    exit 1
fi
if [ ! -f "$SOURCE_ISO" ]; then
    echo "Error: ISO file not found at $SOURCE_ISO"
    exit 1
fi
if [ "$(readlink -f "$OUTPUT_ISO")" = "$(readlink -f "$SOURCE_ISO")" ]; then
    echo "Error: the output ISO would overwrite the source ISO. Choose another output name."
    exit 1
fi

FAILED=()
RESOLV="$ROOTFS/etc/resolv.conf"
RESOLV_ACTIVE=""

chroot_sh() { chroot "$ROOTFS" /bin/sh -c "$1"; }

# Unmounts everything this script mounted under the rootfs. Safe to call at
# any time, any number of times.
unmount_chroot() {
    for m in dev sys proc; do
        if mountpoint -q "$ROOTFS/$m" 2>/dev/null; then
            umount -R "$ROOTFS/$m" 2>/dev/null || umount -R -l "$ROOTFS/$m" 2>/dev/null || true
        fi
    done
    if mountpoint -q "$ROOTFS" 2>/dev/null; then
        umount "$ROOTFS" 2>/dev/null || umount -l "$ROOTFS" 2>/dev/null || true
    fi
}

# Deleting the workspace while /dev is still bind-mounted into it would wipe
# the host's device nodes, so every rm -rf below is guarded by this check.
assert_unmounted() {
    for m in dev sys proc; do
        if mountpoint -q "$ROOTFS/$m" 2>/dev/null; then
            echo "FATAL: $ROOTFS/$m is still mounted; refusing to touch the workspace."
            echo "Unmount it first:  umount -R '$ROOTFS/$m'"
            exit 1
        fi
    done
}

# The rootfs must not keep the build machine's resolv.conf.
setup_resolv() {
    if [ -e "$RESOLV" ] || [ -L "$RESOLV" ]; then
        mv -f "$RESOLV" "$RESOLV.iso-builder-bak"
    fi
    RESOLV_SRC=/etc/resolv.conf
    if grep -q '127.0.0.53' /etc/resolv.conf 2>/dev/null && [ -f /run/systemd/resolve/resolv.conf ]; then
        RESOLV_SRC=/run/systemd/resolve/resolv.conf
    fi
    cp -L "$RESOLV_SRC" "$RESOLV" 2>/dev/null || true
    RESOLV_ACTIVE=1
}
restore_resolv() {
    [ -n "$RESOLV_ACTIVE" ] || return 0
    rm -f "$RESOLV"
    if [ -e "$RESOLV.iso-builder-bak" ] || [ -L "$RESOLV.iso-builder-bak" ]; then
        mv -f "$RESOLV.iso-builder-bak" "$RESOLV"
    fi
    RESOLV_ACTIVE=""
}

# Runs on every exit, success or failure: never leave mounts behind.
cleanup_on_exit() {
    rc=$?
    restore_resolv
    unmount_chroot
    if [ "$rc" -ne 0 ]; then
        echo "[FAILED] The build stopped (exit code $rc). The workspace was kept: $WORK_DIR"
    fi
}
trap cleanup_on_exit EXIT

# step "label" "sh command": a failing step is reported at the end, not fatal.
step() {
    echo "  - $1"
    if ! chroot_sh "$2"; then
        echo "    [WARN] failed: $1"
        FAILED+=("$1")
    fi
}

pm_update() {
    case "$PKG_MGR" in
        # The live image ships without a keyring or sync databases.
        pacman) chroot_sh "pacman-key --init && pacman-key --populate && pacman -Sy --noconfirm" ;;
        apt)    chroot_sh "DEBIAN_FRONTEND=noninteractive apt-get update" ;;
        zypper) chroot_sh "zypper -n --gpg-auto-import-keys refresh" ;;
        apk)    chroot_sh "apk update" ;;
        *)      return 0 ;;
    esac
}

pm_install_cmd() {
    case "$PKG_MGR" in
        pacman) echo "pacman -S --noconfirm --needed $*" ;;
        apt)    echo "DEBIAN_FRONTEND=noninteractive apt-get install -y $*" ;;
        dnf)    echo "dnf install -y $*" ;;
        zypper) echo "zypper -n install -y $*" ;;
        xbps)   echo "xbps-install -Sy $*" ;;
        emerge) echo "emerge --noreplace $*" ;;
        apk)    echo "apk add $*" ;;
    esac
}

# Tries one transaction first; if it fails, retries package by package so a
# single unavailable package does not cost you all the others.
install_packages() {
    [ "$#" -gt 0 ] || return 0
    if chroot_sh "$(pm_install_cmd "$@")"; then
        return 0
    fi
    echo "  [WARN] the combined install failed, retrying one package at a time..."
    for p in "$@"; do
        if ! chroot_sh "$(pm_install_cmd "$p")"; then
            echo "  [WARN] could not install $p"
            FAILED+=("package $p")
        fi
    done
}

pm_clean() {
    case "$PKG_MGR" in
        pacman) chroot_sh "rm -rf /var/cache/pacman/pkg/*" ;;
        apt)    chroot_sh "apt-get clean" ;;
        dnf)    chroot_sh "dnf clean all" ;;
        zypper) chroot_sh "zypper -n clean --all" ;;
        xbps)   chroot_sh "xbps-remove -yO" ;;
        apk)    chroot_sh "rm -rf /var/cache/apk/*" ;;
        *)      return 0 ;;
    esac
}

PM_PAIRS="pacman:pacman apt:apt dnf:dnf zypper:zypper xbps:xbps-install emerge:emerge apk:apk"
detect_pkg_mgr() {
    for pair in $PM_PAIRS; do
        name=${pair%%:*}
        bin=${pair##*:}
        for d in usr/bin bin usr/sbin sbin; do
            if [ -x "$ROOTFS/$d/$bin" ]; then echo "$name"; return 0; fi
        done
    done
    for pair in $PM_PAIRS; do
        name=${pair%%:*}
        bin=${pair##*:}
        if chroot "$ROOTFS" /bin/sh -c "command -v $bin" >/dev/null 2>&1; then echo "$name"; return 0; fi
    done
    return 1
}

# --- Extraction ------------------------------------------------------------
# The workspace is reused only when it holds a *finished* extraction that was
# made as root (an extraction made by a regular user records the wrong file
# owners, which would corrupt the remastered system).
REUSE=""
if [ -f "$MARKER" ] && [ "$(sed -n 2p "$MARKER")" = "root" ] && [ -d "$ROOTFS" ] && [ -d "$ISO_ROOT" ]; then
    REUSE=1
fi

ISO_KB=$(du -Lk "$SOURCE_ISO" | cut -f1)
AVAIL_KB=$(df -Pk "$SCRIPT_DIR" | awk 'NR==2 {print $4}')
if [ -n "$REUSE" ]; then FACTOR=2; else FACTOR=5; fi
if [ -z "$FORCE" ] && [ -n "$AVAIL_KB" ] && [ "$AVAIL_KB" -lt $((ISO_KB * FACTOR)) ]; then
    echo "Error: low disk space in $SCRIPT_DIR ($AVAIL_KB KB free, about $((ISO_KB * FACTOR)) KB needed)."
    echo "Free up some space, or run again with FORCE=1 to continue anyway."
    exit 1
fi

if [ -n "$REUSE" ]; then
    echo "Reusing the finished extraction in $WORK_DIR."
    SQFS_FILE="$WORK_DIR/$(sed -n 1p "$MARKER")"
else
    echo "Extracting ISO... This may take a few minutes."
    assert_unmounted
    rm -rf "$ISO_ROOT" "$ROOTFS" "$MARKER"
    mkdir -p "$ISO_ROOT" "$ROOTFS"
    xorriso -osirrox on -indev "$SOURCE_ISO" -extract / "$ISO_ROOT"
    chmod -R u+w "$ISO_ROOT" 2>/dev/null || true

    # The largest squashfs is the system image (Ubuntu ships many small
    # layer files next to the real one).
    SQFS_FILE=$(find "$ISO_ROOT" -type f \( -iname '*.sfs' -o -iname '*.squashfs' \) -printf '%s %p\n' | sort -rn | head -n 1 | cut -d' ' -f2-)
    if [ -z "$SQFS_FILE" ]; then
        echo "Error: no squashfs image found inside the ISO"
        exit 1
    fi
    echo "Extracting Squashfs ($SQFS_FILE)..."
    unsquashfs -f -d "$ROOTFS" "$SQFS_FILE"
    printf '%s\nroot\n' "${SQFS_FILE#"$WORK_DIR"/}" > "$MARKER"
fi

if [ ! -f "$SQFS_FILE" ]; then
    echo "Error: squashfs image $SQFS_FILE is missing; delete $WORK_DIR and run again."
    exit 1
fi
if [ -f "$ROOTFS/LiveOS/rootfs.img" ]; then
    echo "Error: this ISO keeps its system in a nested LiveOS/rootfs.img (Fedora-style live image)."
    echo "That layout is not supported by this script yet."
    exit 1
fi

# --- Package manager -------------------------------------------------------
PKG_MGR="$PKG_MGR_OVERRIDE"
if [ -z "$PKG_MGR" ]; then PKG_MGR=$(detect_pkg_mgr || true); fi
if [ -z "$PKG_MGR" ] && [ -n "$PKG_MGR_HINT" ]; then PKG_MGR="$PKG_MGR_HINT"; fi
case "$PKG_MGR" in
    pacman|apt|dnf|zypper|xbps|emerge|apk) ;;
    "")
        echo "FATAL: could not detect the package manager."
        echo "Set PKG_MGR_OVERRIDE to pacman, apt, dnf, zypper, xbps, emerge or apk and run again."
        exit 1
        ;;
    *)
        echo "Error: unknown package manager '$PKG_MGR' (use pacman, apt, dnf, zypper, xbps, emerge or apk)"
        exit 1
        ;;
esac
echo "Using package manager: $PKG_MGR"

# --- Chroot ----------------------------------------------------------------
echo "Mounting virtual filesystems for chroot..."
mkdir -p "$ROOTFS/proc" "$ROOTFS/sys" "$ROOTFS/dev"
mount --bind "$ROOTFS" "$ROOTFS"
mount -t proc proc "$ROOTFS/proc"
mount --rbind /sys "$ROOTFS/sys"
mount --make-rslave "$ROOTFS/sys"
mount --rbind /dev "$ROOTFS/dev"
mount --make-rslave "$ROOTFS/dev"
setup_resolv

"##;

/// Second half of the script: cleanup, repack, checksums.
const SCRIPT_EPILOGUE: &str = r##"
echo "Cleaning up inside the chroot..."
pm_clean || true
restore_resolv
unmount_chroot
assert_unmounted

# --- Repack ----------------------------------------------------------------
echo "Repacking Squashfs ($SQUASH_COMP)..."
SQFS_NEW="$SQFS_FILE.new"
rm -f "$SQFS_NEW"
mksquashfs "$ROOTFS" "$SQFS_NEW" -comp "$SQUASH_COMP" -b 1M -noappend
mv -f "$SQFS_NEW" "$SQFS_FILE"

# Checksums and signatures that describe the old image would make the boot
# fail verification, so refresh them where a distro uses them.
SQFS_DIR=$(dirname "$SQFS_FILE")
if [ -f "$SQFS_DIR/airootfs.sha512" ]; then
    (cd "$SQFS_DIR" && sha512sum "$(basename "$SQFS_FILE")" > airootfs.sha512)
fi
rm -f "$SQFS_FILE.sig"
if [ -f "$ISO_ROOT/casper/filesystem.size" ]; then
    printf '%s' "$(du -sx --block-size=1 "$ROOTFS" | cut -f1)" > "$ISO_ROOT/casper/filesystem.size"
fi

# xorriso recreates the El Torito catalog; a copy extracted from the source
# ISO (or 7z's [BOOT] folder) would only get in the way.
find "$ISO_ROOT" -maxdepth 3 -type f \( -name boot.cat -o -name boot.catalog \) -delete
rm -rf "$ISO_ROOT/[BOOT]"
if [ -f "$ISO_ROOT/md5sum.txt" ]; then
    echo "Refreshing md5sum.txt..."
    (cd "$ISO_ROOT" && find . -type f ! -name md5sum.txt -print0 | xargs -0 md5sum > md5sum.txt)
fi

echo "Repacking final ISO..."
# Reuse the source ISO's own boot setup (BIOS/UEFI, hybrid MBR/GPT, volume
# label) instead of guessing a layout, so this works for any distro.
BOOT_OPTS=$(xorriso -indev "$SOURCE_ISO" -report_el_torito as_mkisofs 2>/dev/null | tr '\n' ' ' || true)
if [ -z "${BOOT_OPTS// /}" ]; then
    echo "FATAL: could not read the boot configuration of $SOURCE_ISO (is it a bootable ISO?)."
    exit 1
fi
rm -f "$OUTPUT_ISO"
eval "set -- $BOOT_OPTS"
xorriso -as mkisofs -r -iso-level 3 -full-iso9660-filenames -joliet -joliet-long "$@" -o "$OUTPUT_ISO" "$ISO_ROOT"
if [ ! -s "$OUTPUT_ISO" ]; then
    echo "FATAL: xorriso did not produce $OUTPUT_ISO"
    exit 1
fi
(cd "$SCRIPT_DIR" && sha256sum "$OUTPUT_NAME" > "$OUTPUT_NAME.sha256")

if [ "$KEEP_WORKSPACE" = "1" ]; then
    echo "Keeping the workspace: $WORK_DIR"
else
    echo "[CLEANUP] Removing the extracted workspace..."
    assert_unmounted
    rm -rf "$WORK_DIR"
    rmdir "$SCRIPT_DIR/iso-builder-work" 2>/dev/null || true
fi

echo "[OK] Done! Saved as $OUTPUT_ISO"
echo "     Checksum: $OUTPUT_ISO.sha256"
echo "     Try it:   qemu-system-x86_64 -m 4G -enable-kvm -cdrom '$OUTPUT_ISO'"
if [ "${#FAILED[@]}" -gt 0 ]; then
    echo "[WARN] These steps did not complete:"
    printf '  - %s\n' "${FAILED[@]}"
fi
"##;

fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// Package installs, per detected package manager. The manager is only known
/// when the script runs, so there is one `case` arm per manager, each with
/// the right package names (or a `[SKIP]` line for packages that manager
/// does not offer).
fn push_package_section(script: &mut String, app: &App) {
    let selected: Vec<&PackageOption> = app.packages.iter().filter(|p| p.selected).collect();
    let tweaks = app.selected_tweaks();
    let custom = app.custom_package_list();

    let has_work = !selected.is_empty()
        || tweaks.iter().any(|tweak| !tweak.packages.is_empty())
        || !custom.is_empty();
    if !has_work {
        return;
    }

    script.push_str("echo 'Installing packages...'\n");
    if selected.iter().any(|package| package.name == "steam") {
        // Steam lives in the [multilib] repository, disabled by default.
        script.push_str(
            r##"if [ "$PKG_MGR" = pacman ] && [ -f "$ROOTFS/etc/pacman.conf" ]; then
    echo '  - enabling [multilib] for steam'
    sed -i '/^#\[multilib\]/{s/^#//;n;s/^#//}' "$ROOTFS/etc/pacman.conf"
fi
"##,
        );
    }
    script.push_str("pm_update || echo '  [WARN] could not refresh the package databases'\n");

    script.push_str("case \"$PKG_MGR\" in\n");
    for manager in PKG_MANAGERS {
        let mut names: Vec<String> = Vec::new();
        let mut skipped: Vec<&str> = Vec::new();

        for package in &selected {
            match package.id_for(manager) {
                Some(id) => push_unique(&mut names, id.to_string()),
                None => skipped.push(package.name),
            }
        }
        for tweak in &tweaks {
            for (tweak_manager, id) in tweak.packages.iter() {
                if *tweak_manager == manager {
                    push_unique(&mut names, id.to_string());
                }
            }
        }
        for token in &custom {
            push_unique(&mut names, token.clone());
        }

        script.push_str(&format!("    {})\n", manager));
        for name in skipped {
            script.push_str(&format!(
                "        echo {}\n",
                shell_single_quote(&format!(
                    "  [SKIP] {}: no known package for {}",
                    name, manager
                ))
            ));
        }
        if !names.is_empty() {
            let quoted: Vec<String> = names.iter().map(|name| shell_single_quote(name)).collect();
            script.push_str(&format!("        install_packages {}\n", quoted.join(" ")));
        }
        script.push_str("        ;;\n");
    }
    script.push_str("esac\n\n");
}

/// Tweaks, hostname and the custom command, each as a non-fatal `step`.
fn push_tweak_section(script: &mut String, app: &App) {
    let tweaks = app.selected_tweaks();
    let hostname = app.hostname();
    let command = app.custom_command();
    if tweaks.is_empty() && hostname.is_empty() && command.is_empty() {
        return;
    }

    script.push_str("echo 'Applying system tweaks...'\n");
    for tweak in &tweaks {
        script.push_str(&format!(
            "step {} {}\n",
            shell_single_quote(tweak.name),
            shell_single_quote(&tweak.command)
        ));
    }
    if !hostname.is_empty() {
        let set_hostname = format!("echo {} > /etc/hostname", shell_single_quote(hostname));
        script.push_str(&format!(
            "step 'Set hostname' {}\n",
            shell_single_quote(&set_hostname)
        ));
    }
    if !command.is_empty() {
        script.push_str(&format!(
            "step 'Custom command' {}\n",
            shell_single_quote(command)
        ));
    }
    script.push('\n');
}

/// Builds the complete `run-builder.sh` in memory. The preview step renders
/// this string so the generated logic (package manager detection, package
/// installs, tweaks) can be inspected before anything touches the disk.
pub fn build_script(app: &App) -> String {
    let work_dir = if app.work_dir.is_empty() {
        work_dir_for(&app.iso_path)
    } else {
        app.work_dir.clone()
    };
    let work_rel = work_dir.trim_start_matches("./");
    let detected = format!("{} / {}", app.distro_name, app.desktop)
        .replace('\n', " ")
        .replace('\r', " ");
    let hint = if app.pkg_mgr == "Unknown" {
        ""
    } else {
        app.pkg_mgr.as_str()
    };

    let mut script = String::new();
    script.push_str("#!/bin/bash\n");
    script.push_str(&format!(
        "# Generated by iso-builder (detected: {}).\n",
        detected
    ));
    script.push_str("# Run as root:  sudo ./run-builder.sh\n");
    script.push_str("set -e\n\n");
    script.push_str(&format!(
        "SOURCE_ISO={}\n",
        shell_single_quote(&app.iso_path)
    ));
    script.push_str(&format!("WORK_REL={}\n", shell_single_quote(work_rel)));
    script.push_str(&format!(
        "OUTPUT_NAME={}\n",
        shell_single_quote(&app.output_name())
    ));
    script.push_str(&format!("PKG_MGR_HINT={}\n", shell_single_quote(hint)));

    script.push_str(SCRIPT_PRELUDE);
    push_package_section(&mut script, app);
    push_tweak_section(&mut script, app);
    script.push_str(SCRIPT_EPILOGUE);
    script
}

/// Writes the reviewed script to `run-builder.sh` and marks it executable.
/// What is written is exactly what the preview showed.
pub fn write_script(app: &App) -> io::Result<()> {
    println!("\n[BUILD] Writing {} for: {}", SCRIPT_FILE, app.iso_path);

    let mut file = File::create(SCRIPT_FILE)?;
    file.write_all(app.generated_script.as_bytes())?;
    drop(file);

    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(SCRIPT_FILE)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(SCRIPT_FILE, permissions)?;
    }

    println!("[OK] Successfully generated {}!", SCRIPT_FILE);
    println!("     Run it with:  sudo ./{}", SCRIPT_FILE);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{App, FIELD_HOSTNAME, FIELD_PACKAGES};

    fn app_with_iso(path: &str) -> App {
        let mut app = App::new(false);
        app.iso_path = path.to_string();
        app
    }

    #[test]
    fn single_quotes_are_escaped() {
        assert_eq!(shell_single_quote("plain"), "'plain'");
        assert_eq!(shell_single_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn generated_script_reflects_the_selection() {
        let mut app = app_with_iso("/tmp/it's.iso");
        for package in app.packages.iter_mut() {
            if package.name == "git" || package.name == "discord" {
                package.selected = true;
            }
        }
        app.custom[FIELD_PACKAGES] = "foo".to_string();
        app.custom[FIELD_HOSTNAME] = "box".to_string();
        let script = build_script(&app);

        assert!(script.starts_with("#!/bin/bash"));
        assert!(script.contains("SOURCE_ISO='/tmp/it'\\''s.iso'"));
        assert!(script.contains("OUTPUT_NAME='it'\\''s-custom.iso'"));
        // Order follows the package list (discord comes before git).
        assert!(script.contains("install_packages 'discord' 'git' 'foo'"));
        // apt has no discord package: it must be reported, not guessed.
        assert!(script.contains("[SKIP] discord: no known package for apt"));
        assert!(script.contains("step 'Set hostname'"));
        assert!(!script.contains("arch-chroot"));
    }

    #[test]
    fn script_without_selection_installs_nothing() {
        let app = app_with_iso("/tmp/x.iso");
        let script = build_script(&app);
        assert!(!script.contains("install_packages '"));
        assert!(!script.contains("step 'Set hostname'"));
        assert!(script.contains("trap cleanup_on_exit EXIT"));
    }
}
