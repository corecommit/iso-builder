//! ISO discovery: the quick scan of usual folders and the [f] deep scan.

use std::collections::HashSet;
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use walkdir::WalkDir;

use crate::utils::{home_dir, home_subdir, iso_count_unit};

/// One discovered ISO image and its size on disk. The size is kept in bytes
/// (not whole megabytes) so partial downloads still show up as e.g. "512 KB"
/// instead of rounding down to "0 MB".
pub struct IsoEntry {
    pub path: String,
    pub size_bytes: u64,
}

/// Default folders that may contain `.iso` files. The home directory is
/// always included directly (not just its subfolders) so an ISO sitting in
/// `~` - or in `/root` when running as root - is found. Under `sudo` the
/// invoking user's home is searched too, since `$HOME` may point at `/root`.
fn get_search_paths() -> Vec<PathBuf> {
    let home = home_dir();
    let mut paths = Vec::new();

    for suffix in ["", "Downloads", "Desktop", "ISO"] {
        if let Some(path) = home_subdir(&home, suffix) {
            paths.push(path);
        }
    }

    if let Some(sudo_user) = env::var_os("SUDO_USER").filter(|user| !user.is_empty()) {
        let sudo_home = PathBuf::from("/home").join(&sudo_user);
        for suffix in ["", "Downloads", "Desktop", "ISO"] {
            paths.push(if suffix.is_empty() {
                sudo_home.clone()
            } else {
                sudo_home.join(suffix)
            });
        }
    }

    // Removable media: udisks mounts under /run/media/<user>, older setups
    // under /media/<user>.
    if let Some(user) = env::var_os("USER").filter(|user| !user.is_empty()) {
        paths.push(PathBuf::from("/run/media").join(&user));
        paths.push(PathBuf::from("/media").join(&user));
    }
    paths.push(PathBuf::from("/media"));
    paths.push(PathBuf::from("/mnt"));
    paths.push(PathBuf::from("/root"));

    // `~` and `/root` are often the same directory; scan each folder once.
    let mut unique = Vec::new();
    for path in paths {
        if !unique.contains(&path) {
            unique.push(path);
        }
    }
    unique
}

/// How many folder levels the scan descends into: the search path itself and
/// one level of subfolders (so `~/ISOs/CachyOS.iso` is found).
const SCAN_DEPTH: u8 = 2;

/// Folder names that are never worth recursing into.
const SKIPPED_DIRS: [&str; 4] = ["node_modules", ".cache", ".git", "target"];

/// Hidden folders (leading dot) and well-known heavyweight folders are never
/// recursed into.
fn should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with('.') || SKIPPED_DIRS.contains(&name)
}

/// Every `.iso` file in the default folders - with its size - plus a
/// per-folder debug line such as `"/root (1 ISO)"` or
/// `"[WARN] Permission denied: /media"`.
pub fn discover_isos() -> (Vec<IsoEntry>, Vec<String>) {
    let mut isos: Vec<IsoEntry> = Vec::new();
    // Search roots often overlap (for example, home and Downloads). Keep
    // output order stable while making duplicate checks O(1) on average.
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut debug = Vec::new();

    for root in get_search_paths() {
        if !root.is_dir() {
            // A default folder that does not exist on this machine is not an
            // error: skip it silently instead of spamming the scan report.
            continue;
        }

        // Collect per folder first so the report counts exactly what lives
        // there even when search roots overlap (e.g. `~` and `~/Downloads`).
        let mut local: Vec<IsoEntry> = Vec::new();
        match scan_for_isos(&root, SCAN_DEPTH, &mut local) {
            Ok(()) => {
                let count = local.len();
                debug.push(format!(
                    "{} ({} {})",
                    root.display(),
                    count,
                    iso_count_unit(count)
                ));
            }
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                debug.push(format!("[WARN] Permission denied: {}", root.display()));
            }
            Err(err) => {
                debug.push(format!("[WARN] Could not read {}: {}", root.display(), err));
            }
        }

        for entry in local {
            if seen_paths.insert(entry.path.clone()) {
                isos.push(entry);
            }
        }
    }

    (isos, debug)
}

/// Collects `.iso` files inside `dir`, recursing at most `depth` levels
/// (2 = the folder itself plus one level of subfolders). Symlinks are never
/// followed, hidden folders and heavy build caches are skipped, and a missing
/// directory never panics.
fn scan_for_isos(dir: &Path, depth: u8, found: &mut Vec<IsoEntry>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        // Must be checked before is_dir(): is_dir() follows symlinks.
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if depth > 1 && !should_skip_dir(&path) {
                // Unreadable subdirectories are skipped silently; only the
                // search path itself can produce a reportable error.
                let _ = scan_for_isos(&path, depth - 1, found);
            }
        } else if path
            .extension()
            .map_or(false, |ext| ext.eq_ignore_ascii_case("iso"))
        {
            let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            found.push(IsoEntry {
                path: path.display().to_string(),
                size_bytes,
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Deep scan ([f])
// ---------------------------------------------------------------------------

/// Size filter for the deep scan: only installer-sized images count. Anything
/// at or below this is likely a small bootable utility (500 MB).
const DEEP_SCAN_MIN_SIZE: u64 = 500 * 1024 * 1024;

/// System folders the deep scan skips, but only as direct children of `/`
/// (a project folder called `~/dev` or `~/lib` must still be searched).
const DEEP_SCAN_SYSTEM_DIRS: &[&str] = &[
    "proc", "sys", "dev", "run", "tmp", "usr", "var", "lib", "lib64", "bin", "sbin", "etc", "boot",
    "snap",
];

/// Cache / VCS folders skipped at any depth.
const DEEP_SCAN_ANYWHERE_DIRS: &[&str] = &[".cache", ".git", "node_modules"];

/// Where the deep scan starts.
fn deep_scan_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}

/// True for folders the deep scan must never descend into. `depth` is the
/// walk depth (the root is 0, its children 1).
fn is_deep_scan_pruned(depth: usize, name: &str) -> bool {
    (depth == 1 && DEEP_SCAN_SYSTEM_DIRS.contains(&name)) || DEEP_SCAN_ANYWHERE_DIRS.contains(&name)
}

/// `(device, inode)` - identifies the physical file behind a path, so the
/// same ISO seen through several mounts (e.g. `/sdcard` and `/mnt/sdcard`) is
/// listed only once.
fn file_identity(metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

/// Walks the whole filesystem looking for `.iso` files over 500 MB, pruning
/// system folders with `filter_entry()` so the walk stays fast. Runs on a
/// background thread; `cancel` lets [Esc] stop it early.
pub fn deep_scan_worker(cancel: &AtomicBool) -> Vec<IsoEntry> {
    let mut matches: Vec<(IsoEntry, Option<(u64, u64)>)> = Vec::new();

    'roots: for root in deep_scan_roots() {
        let walker = WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                !entry.file_type().is_dir()
                    || !is_deep_scan_pruned(entry.depth(), &entry.file_name().to_string_lossy())
            });

        for entry in walker.flatten() {
            if cancel.load(Ordering::Relaxed) {
                break 'roots;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let is_iso = path
                .extension()
                .map_or(false, |ext| ext.eq_ignore_ascii_case("iso"));
            if !is_iso {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() > DEEP_SCAN_MIN_SIZE {
                let identity = file_identity(&metadata);
                matches.push((
                    IsoEntry {
                        path: path.display().to_string(),
                        size_bytes: metadata.len(),
                    },
                    identity,
                ));
            }
        }
    }

    // Sort by path so the output is stable, then drop aliases of the same
    // physical file (same device + inode); the first path wins.
    matches.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    let mut seen = HashSet::new();
    matches
        .into_iter()
        .filter(|(_, identity)| match identity {
            Some(id) => seen.insert(*id),
            None => true,
        })
        .map(|(entry, _)| entry)
        .collect()
}
