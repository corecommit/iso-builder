//! Extraction workspace layout, disk checks and guaranteed cleanup.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use crate::iso::extract::stop_active_extraction;
use crate::utils::human_size;

/// Root for the per-ISO workspaces: `./iso-builder-work/<name>-<hash>/`.
/// The TUI caches only the extracted SquashFS image (`rootfs.sfs`) here and
/// reads it with `unsquashfs`; the generated script uses the same folder for
/// its full build-time extraction.
pub const WORK_ROOT: &str = "./iso-builder-work";

/// Shown while the rootfs image is extracted and inspected.
pub const EXTRACTING_MESSAGE: &str = "Extracting rootfs image...";

/// Shown when [Enter] is pressed again while an extraction is already busy.
pub const EXTRACTION_RUNNING: &str = "Extraction is already running, please wait...";

/// Rough free space a full generated-script build needs, as a multiple of the
/// ISO size: extracted ISO + unpacked rootfs + new SquashFS + new ISO.
const DISK_FACTOR: u64 = 5;

/// FNV-1a: tiny, dependency-free and stable across Rust versions (unlike
/// `DefaultHasher`), so a workspace name never changes between builds.
fn fnv1a(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Workspace path for an ISO: `./iso-builder-work/<safe-stem>-<hash of path>`.
/// The hash keeps two ISOs with the same file name apart, and the sanitised
/// stem keeps the path free of spaces and shell metacharacters.
pub fn work_dir_for(iso_path: &str) -> String {
    let stem: String = Path::new(iso_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("image")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    format!("{}/{}-{:08x}", WORK_ROOT, stem, fnv1a(iso_path))
}

/// True when the workspace already holds the extracted rootfs image
/// (`rootfs.sfs`, non-empty). That is all the inspector needs, so a cache hit
/// skips extraction completely. A half-written file (cancelled or crashed
/// run) is not reused.
pub fn cache_is_ready(work_dir: &str) -> bool {
    fs::metadata(Path::new(work_dir).join("rootfs.sfs"))
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

/// True when the workspace holds the finished *full* extraction that the
/// generated script reuses (its `.extracted` marker plus both folders). The
/// TUI never writes that marker, but it must not delete such a workspace:
/// `run-builder.sh` would have to unpack everything again.
pub fn script_cache_ready(work_dir: &str) -> bool {
    let base = Path::new(work_dir);
    let marker = match fs::read_to_string(base.join(".extracted")) {
        Ok(marker) => marker,
        Err(_) => return false,
    };
    marker.lines().nth(1) == Some("root")
        && base.join("rootfs").is_dir()
        && base.join("iso-root").is_dir()
}

/// True when `work_dir` already existed before this run with something worth
/// keeping: the TUI's `rootfs.sfs` cache or the script's full extraction.
pub fn workspace_is_preexisting(work_dir: &str) -> bool {
    cache_is_ready(work_dir) || script_cache_ready(work_dir)
}

/// Free bytes on the filesystem holding `path`, via POSIX `df -Pk`.
fn free_space_bytes(path: &str) -> Option<u64> {
    let output = Command::new("df").args(["-Pk", path]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let available_kb: u64 = text
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()?;
    Some(available_kb.saturating_mul(1024))
}

/// A warning when the current folder probably cannot hold a full build.
pub fn disk_space_warning(iso_path: &str) -> Option<String> {
    let iso_size = fs::metadata(iso_path).ok()?.len();
    let free = free_space_bytes(".")?;
    let needed = iso_size.saturating_mul(DISK_FACTOR);
    if free < needed {
        Some(format!(
            "Low disk space: about {} needed here, {} free.",
            human_size(needed),
            human_size(free)
        ))
    } else {
        None
    }
}

/// Copy of `App::active_work_dir` for the panic hook: `Box<dyn Fn>` cannot
/// borrow the App state, so the path also lives in a global. Only workspaces
/// this run extracted itself are registered here; a reused one must survive.
static ACTIVE_WORK_DIR: Mutex<Option<String>> = Mutex::new(None);

/// Updates the global copy the panic hook reads.
pub fn set_active_work_dir(path: Option<String>) {
    if let Ok(mut slot) = ACTIVE_WORK_DIR.lock() {
        *slot = path;
    }
}

/// Decodes the octal escapes (`\040` = space) used in `/proc/mounts`.
fn unescape_mount_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let digits = &bytes[index + 1..index + 4];
            if digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) {
                let value = digits
                    .iter()
                    .fold(0u32, |acc, digit| acc * 8 + u32::from(digit - b'0'));
                if let Ok(byte) = u8::try_from(value) {
                    out.push(byte);
                    index += 4;
                    continue;
                }
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True when anything is mounted at or below `path`. The generated script
/// bind-mounts `/dev`, `/sys` and `/proc` into the rootfs; if a build died
/// before unmounting them, `remove_dir_all` would walk straight into the
/// host's `/dev`. This check makes every deletion refuse in that situation.
pub fn workspace_has_mounts(path: &str) -> bool {
    let Ok(canonical) = fs::canonicalize(path) else {
        return false;
    };
    let Ok(mounts) = fs::read_to_string("/proc/self/mounts") else {
        return false;
    };
    mounts
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(unescape_mount_path)
        .any(|target| Path::new(&target).starts_with(&canonical))
}

/// Deletes an extracted workspace (best effort). Runs on normal exit and
/// from the panic hook, so it must never panic itself: stdout and removal
/// errors are reported but ignored, and a missing folder is not an error.
pub fn cleanup_workspace(path: &str) {
    if !Path::new(path).is_dir() {
        return;
    }
    // A running extractor would keep writing into the folder after this
    // process exits, so stop it before removing anything.
    stop_active_extraction();

    if workspace_has_mounts(path) {
        let _ = writeln!(
            io::stdout(),
            "[WARN] Not deleting {}: filesystems are still mounted inside it (an unfinished build?).\n       Unmount them first, e.g.: sudo umount -R {}/rootfs/dev {}/rootfs/sys {}/rootfs/proc",
            path, path, path, path
        );
        return;
    }

    let _ = writeln!(
        io::stdout(),
        "[CLEANUP] Removing extracted workspace: {}",
        path
    );
    if let Err(err) = fs::remove_dir_all(path) {
        let _ = writeln!(io::stdout(), "[WARN] Could not remove {}: {}", path, err);
    } else if let Some(parent) = Path::new(path).parent() {
        // `./iso-builder-work` only goes away when it is empty, i.e. when no
        // other workspace is stored there; otherwise this fails harmlessly.
        let _ = fs::remove_dir(parent);
    }
}

/// Panic-hook entry point: cleans up whatever workspace was active when the
/// process crashed.
pub fn cleanup_active_workspace() {
    let path = ACTIVE_WORK_DIR.lock().ok().and_then(|slot| slot.clone());
    if let Some(path) = path {
        cleanup_workspace(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_names_are_safe_and_distinct() {
        let a = work_dir_for("/tmp/My Distro (1).iso");
        assert!(a.starts_with("./iso-builder-work/My_Distro__1_-"));
        assert!(!a.contains(' '));
        assert_eq!(a, work_dir_for("/tmp/My Distro (1).iso"));
        assert_ne!(work_dir_for("/a/x.iso"), work_dir_for("/b/x.iso"));
    }

    #[test]
    fn cache_requires_a_non_empty_rootfs_image() {
        let work =
            std::env::temp_dir().join(format!("iso-builder-cache-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).unwrap();
        let work_str = work.to_str().unwrap();
        let image = work.join("rootfs.sfs");

        assert!(!cache_is_ready(work_str));
        fs::write(&image, b"").unwrap();
        assert!(
            !cache_is_ready(work_str),
            "an empty image is not a cache hit"
        );
        fs::write(&image, b"squashfs bytes").unwrap();
        assert!(cache_is_ready(work_str));
        fs::remove_file(&image).unwrap();
        assert!(!cache_is_ready(work_str));

        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn script_cache_is_recognized_and_never_deleted_by_accident() {
        let work =
            std::env::temp_dir().join(format!("iso-builder-script-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(work.join("rootfs")).unwrap();
        fs::create_dir_all(work.join("iso-root/images")).unwrap();
        assert!(!script_cache_ready(work.to_str().unwrap()));

        fs::write(
            work.join(".extracted"),
            "iso-root/images/airootfs.sfs\nroot\n",
        )
        .unwrap();
        assert!(script_cache_ready(work.to_str().unwrap()));
        assert!(workspace_is_preexisting(work.to_str().unwrap()));

        // A marker without the extracted folders is not a finished build.
        fs::remove_dir_all(work.join("rootfs")).unwrap();
        assert!(!script_cache_ready(work.to_str().unwrap()));

        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn mount_paths_are_unescaped() {
        assert_eq!(unescape_mount_path("/mnt/a\\040b"), "/mnt/a b");
        assert_eq!(unescape_mount_path("/plain/path"), "/plain/path");
        assert_eq!(unescape_mount_path("/tail\\04"), "/tail\\04");
    }
}
