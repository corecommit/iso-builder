use std::collections::HashSet;
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame, Terminal,
};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Package / tweak data
// ---------------------------------------------------------------------------

struct PackageOption {
    name: &'static str,
    desc: &'static str,
    size: &'static str,
    selected: bool,
}

struct PresetOption {
    name: &'static str,
    desc: &'static str,
    command: &'static str,
    selected: bool,
}

fn default_packages() -> Vec<PackageOption> {
    vec![
        PackageOption { name: "firefox", desc: "Web Browser", size: "~250 MB", selected: false },
        PackageOption { name: "steam", desc: "Gaming Platform", size: "~1.5 GB", selected: false },
        PackageOption { name: "discord", desc: "Voice & Text Chat", size: "~300 MB", selected: false },
        PackageOption { name: "neovim", desc: "Terminal Text Editor", size: "~40 MB", selected: false },
        PackageOption { name: "htop", desc: "Process Monitor", size: "~5 MB", selected: false },
        PackageOption { name: "git", desc: "Version Control", size: "~50 MB", selected: false },
        PackageOption { name: "docker", desc: "Container Runtime", size: "~700 MB", selected: false },
        PackageOption { name: "vlc", desc: "Media Player", size: "~200 MB", selected: false },
    ]
}

fn default_tweaks() -> Vec<PresetOption> {
    vec![
        PresetOption {
            name: "Enable SSH",
            desc: "Start sshd on boot",
            command: "systemctl enable sshd",
            selected: false,
        },
        PresetOption {
            name: "Timezone (UTC)",
            desc: "Link /etc/localtime to UTC",
            command: "ln -sf /usr/share/zoneinfo/UTC /etc/localtime",
            selected: false,
        },
        PresetOption {
            name: "Enable Bluetooth",
            desc: "Start bluetooth on boot",
            command: "systemctl enable bluetooth",
            selected: false,
        },
        PresetOption {
            name: "Enable Firewall",
            desc: "Start firewalld on boot",
            command: "systemctl enable firewalld",
            selected: false,
        },
    ]
}

// ---------------------------------------------------------------------------
// ISO auto-discovery
// ---------------------------------------------------------------------------

/// One discovered ISO image and its size on disk. The size is kept in bytes
/// (not whole megabytes) so partial downloads still show up as e.g. "512 KB"
/// instead of rounding down to "0 MB".
struct IsoEntry {
    path: String,
    size_bytes: u64,
}

/// Formats a byte count for humans: "~3.0 GB", "350 MB", "12 KB", "480 B".
fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;

    let value = bytes as f64;
    if value >= GB {
        format!("~{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{} MB", (value / MB).round() as u64)
    } else if value >= KB {
        format!("{} KB", (value / KB).round() as u64)
    } else {
        format!("{} B", bytes)
    }
}

/// "1 ISO" / "3 ISOs" - the unit used in scan report lines.
fn iso_count_unit(count: usize) -> &'static str {
    if count == 1 {
        "ISO"
    } else {
        "ISOs"
    }
}

/// Best-effort home directory: `$HOME` first, `$USERPROFILE` on Windows, and
/// `/root` as a last resort when running as root with neither variable set.
fn home_dir() -> Option<PathBuf> {
    if let Some(home) = env::var_os("HOME").filter(|home| !home.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(home) = env::var_os("USERPROFILE").filter(|home| !home.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if env::var("USER").map_or(false, |user| user == "root") {
        return Some(PathBuf::from("/root"));
    }
    None
}

/// `<home>/<suffix>`; an empty suffix yields the home directory itself.
fn home_subdir(home: &Option<PathBuf>, suffix: &str) -> Option<PathBuf> {
    home.as_ref().map(|home| {
        if suffix.is_empty() {
            home.clone()
        } else {
            home.join(suffix)
        }
    })
}

/// Default folders that may contain `.iso` files, chosen from
/// `std::env::consts::OS`. The home directory is always included directly
/// (not just its subfolders) so an ISO sitting in `~` - or in `/root` when
/// running as root - is found.
fn get_search_paths() -> Vec<PathBuf> {
    let home = home_dir();
    let mut paths = Vec::new();

    match env::consts::OS {
        "windows" => {
            paths.push(PathBuf::from(r"C:\Users\Public\Downloads"));
            paths.push(PathBuf::from(r"D:\"));
            paths.push(PathBuf::from(r"E:\"));
            for suffix in ["Downloads", "Desktop"] {
                if let Some(path) = home_subdir(&home, suffix) {
                    paths.push(path);
                }
            }
        }
        "macos" => {
            for suffix in ["", "Downloads", "Desktop", "ISO"] {
                if let Some(path) = home_subdir(&home, suffix) {
                    paths.push(path);
                }
            }
            paths.push(PathBuf::from("/Volumes"));
        }
        _ => {
            // Linux and other unix-likes.
            for suffix in ["", "Downloads", "Desktop", "ISO"] {
                if let Some(path) = home_subdir(&home, suffix) {
                    paths.push(path);
                }
            }
            paths.push(PathBuf::from("/media"));
            paths.push(PathBuf::from("/mnt"));
            paths.push(PathBuf::from("/root"));
        }
    }

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
fn discover_isos() -> (Vec<IsoEntry>, Vec<String>) {
    let mut isos: Vec<IsoEntry> = Vec::new();
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
            if !isos.iter().any(|existing| existing.path == entry.path) {
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

/// System / cache folders the deep scan prunes immediately, per target OS.
#[cfg(windows)]
const DEEP_SCAN_SKIPPED_DIRS: &[&str] = &[
    "Windows",
    "Program Files",
    "Program Files (x86)",
    "ProgramData",
    "AppData",
    "$Recycle.Bin",
    "System Volume Information",
    "Recovery",
];
#[cfg(not(windows))]
const DEEP_SCAN_SKIPPED_DIRS: &[&str] = &[
    "proc", "sys", "dev", "run", "tmp", "usr", "var", "lib", "lib64", "bin", "sbin", "etc",
    "boot", "snap", ".cache", ".git", "node_modules",
];

/// Where the deep scan starts: `/` on Linux/macOS, otherwise every drive
/// letter that actually exists.
fn deep_scan_roots() -> Vec<PathBuf> {
    if env::consts::OS == "windows" {
        ('A'..='Z')
            .map(|letter| PathBuf::from(format!("{}:\\", letter)))
            .filter(|root| root.is_dir())
            .collect()
    } else {
        vec![PathBuf::from("/")]
    }
}

/// True for folders the deep scan must never descend into.
fn is_deep_scan_pruned(name: &str) -> bool {
    if cfg!(windows) {
        DEEP_SCAN_SKIPPED_DIRS
            .iter()
            .any(|skipped| name.eq_ignore_ascii_case(skipped))
    } else {
        DEEP_SCAN_SKIPPED_DIRS.contains(&name)
    }
}

/// `(device, inode)` - identifies the physical file behind a path, so the
/// same ISO seen through several mounts (e.g. `/sdcard` and `/mnt/sdcard`) is
/// listed only once. `None` on platforms without inode semantics.
#[cfg(unix)]
fn file_identity(metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// Walks the whole filesystem looking for `.iso` files over 500 MB, pruning
/// system folders with `filter_entry()` so the walk stays fast. Runs on a
/// background thread; `cancel` lets [Esc] stop it early.
fn deep_scan_worker(cancel: &AtomicBool) -> Vec<IsoEntry> {
    let mut matches: Vec<(IsoEntry, Option<(u64, u64)>)> = Vec::new();

    'roots: for root in deep_scan_roots() {
        let walker = WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                !entry.file_type().is_dir()
                    || !is_deep_scan_pruned(&entry.file_name().to_string_lossy())
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

// ---------------------------------------------------------------------------
// Extraction workspace and native detection
// ---------------------------------------------------------------------------

/// Root for the per-ISO workspaces: `./iso-builder-work/<name-without-ext>/`.
/// The TUI extracts each ISO here (outer ISO + SquashFS) and then detects
/// everything by reading the extracted files; the generated script reuses
/// the same folder when it is still around.
const WORK_ROOT: &str = "./iso-builder-work";

/// Shown while the extraction runs.
const EXTRACTING_MESSAGE: &str = "Extracting ISO... This may take a few minutes.";

/// Shown when [Enter] is pressed again while an extraction is already busy.
const EXTRACTION_RUNNING: &str = "Extraction is already running, please wait...";

/// Everything the TUI learns while preparing an ISO.
struct IsoPrep {
    /// `./iso-builder-work/<filename without extension>`.
    work_dir: String,
    /// From `etc/os-release` in the extracted rootfs, e.g. "CachyOS Linux".
    distro: String,
    /// Binary found in the rootfs (`pacman`, `apt`, ...) or "Unknown".
    pkg_mgr: String,
    /// Desktop session file found in the rootfs or "Unknown".
    desktop: String,
}

/// Workspace path for an ISO: `./iso-builder-work/<filename without ext>/`.
fn work_dir_for(iso_path: &str) -> String {
    let stem = Path::new(iso_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("image");
    format!("{}/{}", WORK_ROOT, stem)
}

/// Prepares `iso_path` for the wizard: extracts it into the cached workspace
/// (unless a previous extraction is already there) and then reads the
/// extracted rootfs with plain `std::fs` calls - no `grep`, no external
/// detector, no second archive pass.
fn prepare_iso(iso_path: &str) -> Result<IsoPrep, String> {
    let work_dir = work_dir_for(iso_path);
    let iso_root = format!("{}/iso-root", work_dir);
    let rootfs = format!("{}/rootfs", work_dir);

    if cache_is_ready(&rootfs) {
        // Extracted on a previous run: skip straight to detection.
    } else {
        // Fresh extraction: the outer ISO with xorriso (7z as fallback),
        // then the SquashFS rootfs. This is the slow part.
        let _ = fs::remove_dir_all(&iso_root);
        fs::create_dir_all(&iso_root)
            .map_err(|err| format!("could not create {}: {}", iso_root, err))?;
        fs::create_dir_all(&rootfs)
            .map_err(|err| format!("could not create {}: {}", rootfs, err))?;

        let squashfs = extract_iso(iso_path, &iso_root)?;
        extract_squashfs(&squashfs, &rootfs)?;
    }

    let mut distro = distro_name_from(&rootfs);
    if distro.is_empty() {
        distro = "Unknown".to_string();
    }
    Ok(IsoPrep {
        work_dir,
        distro,
        pkg_mgr: package_manager_in(&rootfs).unwrap_or("Unknown").to_string(),
        desktop: desktop_environment_in(&rootfs).unwrap_or("Unknown").to_string(),
    })
}

/// True when `rootfs/` already holds a finished extraction. An empty folder
/// (e.g. left behind by a run that was cancelled mid-extract) does not
/// count, so the caller re-extracts instead of detecting nothing.
fn cache_is_ready(rootfs: &str) -> bool {
    fs::read_dir(rootfs)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// PID of the extraction tool currently running, so the exit/panic cleanup
/// can stop it before deleting the workspace. 0 = nothing running.
static EXTRACTION_PID: AtomicU32 = AtomicU32::new(0);

/// Runs a tool with its output captured (never on the TUI's terminal) and
/// remembers its pid while it runs.
fn run_captured(command: &mut Command) -> io::Result<Output> {
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    EXTRACTION_PID.store(child.id(), Ordering::Relaxed);
    let output = child.wait_with_output();
    EXTRACTION_PID.store(0, Ordering::Relaxed);
    output
}

/// Stops an extraction tool that may still be writing (otherwise the child
/// would keep writing into the folder after iso-builder already exited).
/// Best effort only; a 150 ms pause lets the process release its files.
fn stop_active_extraction() {
    let pid = EXTRACTION_PID.swap(0, Ordering::Relaxed);
    if pid == 0 {
        return;
    }
    let _ = Command::new("sh")
        .args(["-c", &format!("kill {} 2>/dev/null || true", pid)])
        .status();
    thread::sleep(Duration::from_millis(150));
}

/// Extracts the outer ISO into `iso_root` with `xorriso`, falling back to
/// `7z`, and returns the path of the largest `.sfs`/`.squashfs` inside it -
/// that is the installer rootfs (Ubuntu ships 20+ small variants next to
/// the real one, so "first match" is not good enough).
fn extract_iso(iso_path: &str, iso_root: &str) -> Result<String, String> {
    let xorriso = run_captured(
        Command::new("xorriso")
            .args(["-osirrox", "on", "-indev", iso_path, "-extract", "/", iso_root]),
    );
    let xorriso_error = match &xorriso {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(command_error("xorriso", output)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            Some("xorriso is not installed".to_string())
        }
        Err(err) => Some(format!("could not run xorriso: {}", err)),
    };

    if let Some(primary_error) = xorriso_error {
        // Restart the fallback from a clean folder so xorriso leftovers
        // cannot mix with the 7z extraction.
        let _ = fs::remove_dir_all(iso_root);
        fs::create_dir_all(iso_root)
            .map_err(|err| format!("could not create {}: {}", iso_root, err))?;

        let output_arg = format!("-o{}", iso_root);
        match run_captured(Command::new("7z").args(["x", "-y", iso_path, output_arg.as_str()])) {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return Err(format!(
                    "{}; {}",
                    primary_error,
                    command_error("7z", &output)
                ));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(format!(
                    "{} and 7z is not installed (install xorriso or p7zip-full)",
                    primary_error
                ));
            }
            Err(err) => return Err(format!("{}; could not run 7z: {}", primary_error, err)),
        }
    }

    find_squashfs_image(iso_root)
        .map(|(path, _size)| path)
        .ok_or_else(|| "no .sfs or .squashfs image was found inside the ISO".to_string())
}

/// Extracts the SquashFS rootfs. `-f` overwrites leftovers from an earlier
/// attempt; the destination folder itself may already exist.
fn extract_squashfs(squashfs: &str, rootfs: &str) -> Result<(), String> {
    let output = run_captured(Command::new("unsquashfs").args(["-f", "-d", rootfs, squashfs]))
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                "unsquashfs is not installed (install squashfs-tools)".to_string()
            } else {
                format!("could not run unsquashfs: {}", err)
            }
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error("unsquashfs", &output))
    }
}

/// Largest `.sfs`/`.squashfs` file under `iso_root`.
fn find_squashfs_image(iso_root: &str) -> Option<(String, u64)> {
    let mut best: Option<(String, u64)> = None;
    for entry in WalkDir::new(iso_root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if !(name.ends_with(".sfs") || name.ends_with(".squashfs")) {
            continue;
        }
        let size = entry.metadata().map(|data| data.len()).unwrap_or(0);
        if best.as_ref().map_or(true, |(_, best_size)| size > *best_size) {
            best = Some((entry.path().to_string_lossy().into_owned(), size));
        }
    }
    best
}

/// Distro name for the review screen, read from `etc/os-release`.
/// `PRETTY_NAME` wins over `NAME`; values may be double- or single-quoted.
fn distro_name_from(rootfs: &str) -> String {
    let Ok(contents) = fs::read_to_string(Path::new(rootfs).join("etc/os-release")) else {
        return String::new();
    };
    let mut name = None;
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        match key.trim() {
            "PRETTY_NAME" => return value,
            "NAME" => name = Some(value),
            _ => {}
        }
    }
    name.unwrap_or_default()
}

/// Package manager binaries, checked in a fixed order (a rootfs only ever
/// has one of them). `apt` (not `apt-get`) keeps this list matching the
/// generated script's rootfs probe.
const PKG_MGR_BINARIES: &[(&str, &str)] = &[
    ("usr/bin/pacman", "pacman"),
    ("usr/bin/apt", "apt"),
    ("usr/bin/dnf", "dnf"),
    ("usr/bin/zypper", "zypper"),
    ("usr/bin/xbps-install", "xbps"),
    ("usr/bin/emerge", "emerge"),
    ("sbin/apk", "apk"),
];

/// Package manager whose binary exists in the rootfs.
fn package_manager_in(rootfs: &str) -> Option<&'static str> {
    PKG_MGR_BINARIES
        .iter()
        .find(|(relative, _)| Path::new(rootfs).join(relative).is_file())
        .map(|(_, name)| *name)
}

/// Desktop session files that map to a desktop environment. `plasma.desktop`
/// (Plasma 6) and `ubuntu.desktop` (Ubuntu's GNOME session) are not in the
/// task sketch's list but are what current releases actually ship.
const DE_SESSION_FILES: &[(&str, &str)] = &[
    ("hyprland.desktop", "Hyprland"),
    ("plasmawayland.desktop", "KDE Plasma"),
    ("plasma.desktop", "KDE Plasma"),
    ("gnome.desktop", "GNOME"),
    ("ubuntu.desktop", "GNOME"),
    ("xfce.desktop", "XFCE"),
    ("sway.desktop", "Sway"),
];

/// Desktop environment found by listing the extracted session folders.
fn desktop_environment_in(rootfs: &str) -> Option<&'static str> {
    let mut sessions: Vec<String> = Vec::new();
    for folder in ["usr/share/wayland-sessions", "usr/share/xsessions"] {
        let Ok(entries) = fs::read_dir(Path::new(rootfs).join(folder)) else {
            continue;
        };
        for entry in entries.flatten() {
            sessions.push(entry.file_name().to_string_lossy().to_ascii_lowercase());
        }
    }

    DE_SESSION_FILES
        .iter()
        .find(|(file, _)| sessions.iter().any(|session| session == file))
        .map(|(_, label)| *label)
}

/// Last non-empty stderr line of a failed tool, for compact error messages.
fn command_error(tool: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(200).collect::<String>())
        .unwrap_or_default();
    if line.is_empty() {
        format!("{} exited with status {}", tool, output.status)
    } else {
        format!("{}: {}", tool, line)
    }
}

// ---------------------------------------------------------------------------
// Workspace cleanup
// ---------------------------------------------------------------------------

/// Copy of `App::active_work_dir` for the panic hook: `Box<dyn Fn>` cannot
/// borrow the App state, so the path also lives in a global.
static ACTIVE_WORK_DIR: Mutex<Option<String>> = Mutex::new(None);

/// Updates the global copy the panic hook reads.
fn set_active_work_dir(path: Option<String>) {
    if let Ok(mut slot) = ACTIVE_WORK_DIR.lock() {
        *slot = path;
    }
}

/// Deletes an extracted workspace (best effort). Runs on normal exit and
/// from the panic hook, so it must never panic itself: stdout and removal
/// errors are reported but ignored, and a missing folder is not an error.
fn cleanup_workspace(path: &str) {
    if !Path::new(path).is_dir() {
        return;
    }
    let _ = writeln!(io::stdout(), "[CLEANUP] Cleaning up processes and temporary files...");
    // A running extractor would keep writing into the folder after this
    // process exits, so stop it before removing anything.
    stop_active_extraction();
    let _ = writeln!(io::stdout(), "Removing extracted workspace: {}", path);
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
fn cleanup_active_workspace() {
    let path = ACTIVE_WORK_DIR
        .lock()
        .ok()
        .and_then(|slot| slot.clone());
    if let Some(path) = path {
        cleanup_workspace(&path);
    }
}

/// Normal-exit cleanup, called after the terminal is restored: deletes the
/// extracted workspace unless the user chose to keep it on the finish
/// screen, then clears the global so a later panic cannot delete again.
fn finish_session(app: &App) {
    if let Some(path) = app.active_work_dir.as_deref() {
        if app.keep_workspace {
            println!("Keeping extracted workspace: {}", path);
        } else {
            cleanup_workspace(path);
        }
    }
    set_active_work_dir(None);
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum Step {
    IsoPath,
    Desktop,
    Packages,
    Tweaks,
    Confirm,
    Preview,
    Finished,
}

/// How the user picks the source ISO: from the auto-discovered list or by
/// typing a path themselves.
#[derive(PartialEq, Clone, Copy)]
enum IsoMode {
    Auto,
    Manual,
}

struct App {
    step: Step,
    iso_mode: IsoMode,
    /// Confirmed ISO path (used by the review screen and the generated script).
    iso_path: String,
    /// Text buffer for manual path entry.
    iso_input: String,
    discovered_isos: Vec<IsoEntry>,
    iso_cursor: usize,
    /// Debug report from the last scan: one line per searched folder with the
    /// number of ISOs found in it (or why it could not be read).
    scanned_paths: Vec<String>,
    /// [d] toggle for the scan-report overlay on narrow terminals.
    show_scan_report: bool,
    /// Receiver for the background deep scan ([f]); `None` when idle.
    deep_scan_rx: Option<Receiver<Vec<IsoEntry>>>,
    /// Cooperative cancel flag for the running deep scan.
    deep_scan_cancel: Option<Arc<AtomicBool>>,
    /// Receiver for the background extraction/detection started on [Enter];
    /// `None` when nothing is being prepared.
    iso_prep_rx: Option<Receiver<Result<IsoPrep, String>>>,
    /// Status line about the extraction/detection, shown on the Desktop step:
    /// the loading message while it runs, then "Detected: ...".
    iso_info_note: Option<String>,
    /// True after a failed extraction; [Esc] then skips straight to the
    /// Desktop step so everything can be typed manually.
    extraction_failed: bool,
    /// Extraction workspace of the selected ISO
    /// (`./iso-builder-work/<filename-without-ext>`).
    work_dir: String,
    /// The workspace to clean up on exit; `None` until an ISO was picked.
    active_work_dir: Option<String>,
    /// Distro name read from the extracted rootfs (e.g. "CachyOS Linux").
    distro_name: String,
    /// Set on the finish screen ([K]); `false` means the exit cleanup
    /// deletes the extracted workspace.
    keep_workspace: bool,
    /// Package manager detected from the selected ISO (or "Unknown").
    pkg_mgr: String,
    /// Informational line shown on the ISO step (e.g. "no ISOs found").
    iso_notice: Option<String>,
    desktop: String,
    packages: Vec<PackageOption>,
    package_cursor: usize,
    tweaks: Vec<PresetOption>,
    tweak_cursor: usize,
    is_typing: bool,
    should_quit: bool,
    generate_script: bool,
    /// Full bash script built from the current selections, shown on the
    /// preview step and written to disk when the user confirms it there.
    generated_script: String,
    /// Vertical scroll offset (in lines) for the preview pane.
    preview_scroll: u16,
    error_msg: Option<String>,
}

impl App {
    fn new() -> Self {
        let (discovered_isos, scanned_paths) = discover_isos();

        Self {
            step: Step::IsoPath,
            // Always start on the auto list: when nothing is found, the UI
            // shows the scan report and points the user at [m].
            iso_mode: IsoMode::Auto,
            iso_path: String::new(),
            iso_input: String::new(),
            discovered_isos,
            iso_cursor: 0,
            scanned_paths,
            show_scan_report: false,
            deep_scan_rx: None,
            deep_scan_cancel: None,
            iso_prep_rx: None,
            iso_info_note: None,
            extraction_failed: false,
            work_dir: String::new(),
            active_work_dir: None,
            distro_name: "Unknown".to_string(),
            keep_workspace: false,
            pkg_mgr: "Unknown".to_string(),
            iso_notice: None,
            desktop: "Hyprland".to_string(),
            packages: default_packages(),
            package_cursor: 0,
            tweaks: default_tweaks(),
            tweak_cursor: 0,
            is_typing: false,
            should_quit: false,
            generate_script: false,
            generated_script: String::new(),
            preview_scroll: 0,
            error_msg: None,
        }
    }

    fn selected_packages(&self) -> Vec<&'static str> {
        self.packages
            .iter()
            .filter(|p| p.selected)
            .map(|p| p.name)
            .collect()
    }

    fn selected_tweaks(&self) -> Vec<(&'static str, &'static str)> {
        self.tweaks
            .iter()
            .filter(|t| t.selected)
            .map(|t| (t.name, t.command))
            .collect()
    }

    /// Comma separated package names for the review / preview screens.
    fn packages_summary(&self) -> String {
        let names = self.selected_packages();
        if names.is_empty() {
            "None".to_string()
        } else {
            names.join(", ")
        }
    }

    /// Comma separated tweak names for the review / preview screens.
    fn tweaks_summary(&self) -> String {
        let names: Vec<&str> = self.selected_tweaks().iter().map(|(name, _)| *name).collect();
        if names.is_empty() {
            "None".to_string()
        } else {
            names.join(", ")
        }
    }

    /// Only these steps accept free text; packages/tweaks use the checkbox UI.
    /// On the ISO step typing is only possible in manual mode.
    fn has_text_field(&self) -> bool {
        match self.step {
            Step::IsoPath => self.iso_mode == IsoMode::Manual,
            Step::Desktop => true,
            _ => false,
        }
    }

    fn editable_field_mut(&mut self) -> Option<&mut String> {
        match self.step {
            Step::IsoPath => Some(&mut self.iso_input),
            Step::Desktop => Some(&mut self.desktop),
            _ => None,
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        match self.step {
            Step::IsoPath => {
                if self.iso_mode == IsoMode::Auto {
                    if let Some(max) = self.discovered_isos.len().checked_sub(1) {
                        self.iso_cursor = if delta < 0 {
                            self.iso_cursor.saturating_sub(1)
                        } else {
                            (self.iso_cursor + 1).min(max)
                        };
                    }
                }
            }
            Step::Packages => {
                if let Some(max) = self.packages.len().checked_sub(1) {
                    self.package_cursor = if delta < 0 {
                        self.package_cursor.saturating_sub(1)
                    } else {
                        (self.package_cursor + 1).min(max)
                    };
                }
            }
            Step::Tweaks => {
                if let Some(max) = self.tweaks.len().checked_sub(1) {
                    self.tweak_cursor = if delta < 0 {
                        self.tweak_cursor.saturating_sub(1)
                    } else {
                        (self.tweak_cursor + 1).min(max)
                    };
                }
            }
            Step::Preview => {
                // Scroll the script view; clamp so we never scroll past the end.
                let max_lines = self.generated_script.lines().count().saturating_sub(1);
                let max = max_lines.min(u16::MAX as usize) as u16;
                self.preview_scroll = if delta < 0 {
                    self.preview_scroll.saturating_sub(3)
                } else {
                    self.preview_scroll.saturating_add(3).min(max)
                };
            }
            _ => {}
        }
    }

    fn toggle_selected(&mut self) {
        match self.step {
            Step::Packages => {
                if let Some(option) = self.packages.get_mut(self.package_cursor) {
                    option.selected = !option.selected;
                }
            }
            Step::Tweaks => {
                if let Some(option) = self.tweaks.get_mut(self.tweak_cursor) {
                    option.selected = !option.selected;
                }
            }
            _ => {}
        }
    }

    /// Re-scans the default folders for ISO files and refreshes the debug
    /// report (`scanned_paths`); [r] calls this.
    fn rescan_isos(&mut self) {
        let (isos, scanned) = discover_isos();
        self.discovered_isos = isos;
        self.scanned_paths = scanned;
        self.iso_cursor = 0;
        self.iso_notice = None;
    }

    /// [f] on the ISO step: walk the whole filesystem for installer-sized
    /// ISOs on a background thread. Results come back through a channel that
    /// `poll_deep_scan` checks every frame, so the UI stays responsive.
    fn start_deep_scan(&mut self) {
        if self.deep_scan_rx.is_some() {
            return; // A scan is already running.
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        thread::spawn(move || {
            let isos = deep_scan_worker(&worker_cancel);
            // The send fails only when the UI dropped the receiver
            // (cancelled or quit); that is not an error.
            let _ = tx.send(isos);
        });
        self.deep_scan_rx = Some(rx);
        self.deep_scan_cancel = Some(cancel);
        self.iso_notice = None;
    }

    /// [Esc] while a deep scan runs: ask the worker to stop and drop the
    /// receiver, so a result that arrives anyway is ignored.
    fn cancel_deep_scan(&mut self) {
        if let Some(cancel) = self.deep_scan_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.deep_scan_rx = None;
        self.iso_notice = Some("Deep scan cancelled.".to_string());
    }

    /// Non-blocking check for a finished deep scan; called every loop tick.
    fn poll_deep_scan(&mut self) {
        let received = self.deep_scan_rx.as_ref().map(|rx| rx.try_recv());
        match received {
            Some(Ok(isos)) => {
                let count = isos.len();
                self.discovered_isos = isos;
                self.scanned_paths = vec![format!(
                    "Deep scan: {} {} over 500 MB.",
                    count,
                    iso_count_unit(count)
                )];
                self.iso_cursor = 0;
                self.iso_notice = None;
                self.deep_scan_rx = None;
                self.deep_scan_cancel = None;
            }
            // The worker is gone without sending; just clear the state.
            Some(Err(TryRecvError::Disconnected)) => {
                self.deep_scan_rx = None;
                self.deep_scan_cancel = None;
            }
            // Still running (Empty) or no scan at all (None).
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    /// [Enter] on the ISO step: extract the selected ISO into its workspace
    /// (`./iso-builder-work/<name>/`) and detect the distro details - both on
    /// a background thread, because extraction can take minutes. The result
    /// arrives through a channel that `poll_iso_prep` checks every frame.
    fn start_iso_extraction(&mut self) {
        if self.iso_path.is_empty() || self.iso_prep_rx.is_some() {
            return;
        }

        // The workspace path is fixed by the filename; the worker creates
        // the folders. Tracking it right away means even a quit during the
        // extraction cleans up whatever was already written.
        let work_dir = work_dir_for(&self.iso_path);
        self.work_dir = work_dir.clone();
        self.active_work_dir = Some(work_dir.clone());
        set_active_work_dir(Some(work_dir));

        self.extraction_failed = false;
        self.is_typing = false;
        let path = self.iso_path.clone();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            // A failed send only means the receiver was dropped (the user
            // quit); that is not an error.
            let _ = tx.send(prepare_iso(&path));
        });
        self.iso_prep_rx = Some(rx);
        self.iso_info_note = Some(EXTRACTING_MESSAGE.to_string());
        self.iso_notice = Some(EXTRACTING_MESSAGE.to_string());
    }

    /// Non-blocking check for a finished extraction/detection; called every
    /// loop tick. On success the Desktop field, package manager and distro
    /// name are pre-filled and the wizard moves on to the Desktop step; on
    /// failure the red error stays on the ISO step so the user can pick
    /// another image, retry, or skip ahead with [Esc].
    fn poll_iso_prep(&mut self) {
        let received = self.iso_prep_rx.as_ref().map(|rx| rx.try_recv());
        match received {
            Some(Ok(Ok(prep))) => {
                // Re-affirm the tracked workspace (the panic hook reads the
                // global) before pre-filling the wizard fields.
                self.work_dir = prep.work_dir.clone();
                self.active_work_dir = Some(prep.work_dir.clone());
                set_active_work_dir(Some(prep.work_dir.clone()));
                if prep.desktop != "Unknown" {
                    self.desktop = prep.desktop.clone();
                }
                self.pkg_mgr = prep.pkg_mgr.clone();
                self.distro_name = prep.distro.clone();
                let distro_note = if prep.distro == "Unknown" {
                    String::new()
                } else {
                    format!(" - {}", prep.distro)
                };
                self.iso_info_note =
                    Some(format!("Detected: {} / {}{}", prep.desktop, prep.pkg_mgr, distro_note));
                self.iso_notice = None;
                self.extraction_failed = false;
                self.iso_prep_rx = None;
                if self.step == Step::IsoPath {
                    self.next_step();
                }
            }
            Some(Ok(Err(message))) => {
                self.error_msg = Some(format!("Extraction failed: {}", message));
                self.iso_notice = Some(
                    "Press [Enter] to retry or [Esc] to continue without extraction.".to_string(),
                );
                self.iso_info_note = None;
                self.extraction_failed = true;
                self.iso_prep_rx = None;
            }
            // The worker is gone without sending; just clear the state.
            Some(Err(TryRecvError::Disconnected)) => {
                self.iso_prep_rx = None;
            }
            // Still running (Empty) or no extraction at all (None).
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    /// [Esc] on the ISO step after a failed extraction: continue to the
    /// Desktop step without pre-fill, so the user can type everything.
    fn skip_failed_extraction(&mut self) {
        self.extraction_failed = false;
        self.next_step();
        self.iso_info_note = Some(
            "Extraction skipped - enter the Desktop Environment manually.".to_string(),
        );
    }

    /// Finish screen: [K] keeps the extracted workspace, [D] (the default)
    /// lets the exit cleanup delete it.
    fn finish_session_choice(&mut self, keep: bool) {
        self.keep_workspace = keep;
        self.should_quit = true;
    }

    /// [m] on the ISO step: switch between the auto list and manual entry.
    /// Switching back to Auto re-scans so a freshly plugged-in USB is found.
    fn toggle_iso_mode(&mut self) {
        if self.step != Step::IsoPath {
            return;
        }
        match self.iso_mode {
            IsoMode::Auto => {
                self.iso_mode = IsoMode::Manual;
                self.iso_notice = None;
                if self.iso_input.is_empty() {
                    self.iso_input = self.iso_path.clone();
                }
            }
            IsoMode::Manual => {
                self.iso_mode = IsoMode::Auto;
                self.rescan_isos();
            }
        }
    }

    /// Handles [Enter]: validates the current step and advances only if it
    /// passes. Runs for both typing and navigation mode, so [Enter] can never
    /// skip validation.
    fn submit_current_step(&mut self) {
        self.error_msg = None;

        match self.step {
            Step::IsoPath => match self.iso_mode {
                IsoMode::Auto => match self.discovered_isos.get(self.iso_cursor) {
                    Some(iso) => {
                        if self.iso_prep_rx.is_some() {
                            self.iso_notice = Some(EXTRACTION_RUNNING.to_string());
                            return;
                        }
                        self.iso_path = iso.path.clone();
                        self.iso_notice = None;
                        // The wizard advances to the Desktop step on its own
                        // once extraction and detection finish.
                        self.start_iso_extraction();
                    }
                    None => {
                        self.iso_mode = IsoMode::Manual;
                        self.iso_notice =
                            Some("No ISOs found. Please enter path manually.".to_string());
                    }
                },
                IsoMode::Manual => {
                    let path = self.iso_input.trim().to_string();
                    if path.is_empty() {
                        self.error_msg =
                            Some("Please enter the path to your Linux ISO file.".to_string());
                        return;
                    }
                    if !Path::new(&path).is_file() {
                        self.error_msg = Some(format!("File not found: {}", path));
                        return;
                    }
                    if self.iso_prep_rx.is_some() {
                        self.iso_notice = Some(EXTRACTION_RUNNING.to_string());
                        return;
                    }
                    self.iso_path = path;
                    self.iso_notice = None;
                    self.start_iso_extraction();
                }
            },
            Step::Confirm => {
                // Build the script in memory; the preview step shows it and
                // [Enter] there is what actually writes run-builder.sh.
                let script = build_script(self);
                self.generated_script = script;
                self.preview_scroll = 0;
                self.next_step();
            }
            Step::Preview => {
                // The script is written once the TUI exits; the finish
                // screen now asks what should happen to the workspace.
                self.generate_script = true;
                self.next_step();
            }
            Step::Finished => {
                // [Enter] takes the default: delete the workspace on exit.
                self.finish_session_choice(false);
            }
            _ => self.next_step(),
        }
    }

    fn next_step(&mut self) {
        self.error_msg = None;
        self.is_typing = false;

        self.step = match self.step {
            Step::IsoPath => Step::Desktop,
            Step::Desktop => Step::Packages,
            Step::Packages => Step::Tweaks,
            Step::Tweaks => Step::Confirm,
            Step::Confirm => Step::Preview,
            Step::Preview => Step::Finished,
            Step::Finished => Step::Finished,
        };
    }

    fn prev_step(&mut self) {
        self.error_msg = None;
        self.step = match self.step {
            Step::IsoPath => Step::IsoPath,
            Step::Desktop => Step::IsoPath,
            Step::Packages => Step::Desktop,
            Step::Tweaks => Step::Packages,
            Step::Confirm => Step::Tweaks,
            Step::Preview => Step::Confirm,
            Step::Finished => Step::Preview,
        };
        self.is_typing = false;
    }

    fn current_step_number(&self) -> (usize, usize) {
        let current = match self.step {
            Step::IsoPath => 1,
            Step::Desktop => 2,
            Step::Packages => 3,
            Step::Tweaks => 4,
            Step::Confirm => 5,
            Step::Preview => 6,
            Step::Finished => 7,
        };
        (current, 7)
    }
}

// ---------------------------------------------------------------------------
// Terminal lifecycle and event loop
// ---------------------------------------------------------------------------

fn main() -> Result<(), io::Error> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // Restore the terminal first so the cleanup messages are readable,
        // then delete whatever extraction workspace was active.
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        cleanup_active_workspace();
        original_hook(panic_info);
    }));

    let mut app = App::new();
    let tui_result = run_tui(&mut app);

    let script_result = if tui_result.is_ok() && app.generate_script {
        write_script(&app)
    } else {
        Ok(())
    };

    // Normal-exit cleanup: the extracted workspace is deleted unless the
    // user chose [K] on the finish screen. Happens after the terminal is
    // back to normal text mode.
    finish_session(&app);

    tui_result?;
    script_result?;
    Ok(())
}

/// Owns the terminal lifecycle: raw mode and the alternate screen are always
/// restored, even when the event loop or a draw returns an error.
fn run_tui(app: &mut App) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(err) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(err);
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Err(err);
        }
    };

    let result = event_loop(&mut terminal, app);

    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
    result
}

fn event_loop<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<()> {
    while !app.should_quit {
        // Pick up finished deep scans / ISO preparations (non-blocking)
        // before every frame.
        app.poll_deep_scan();
        app.poll_iso_prep();

        terminal.draw(|frame| ui(frame, app))?;

        // While a background job runs, wake up periodically so the loading
        // state is refreshed and results appear without extra key presses.
        // Input is still read live, so [q]/[Ctrl+C] quit immediately.
        let event = if app.deep_scan_rx.is_some() || app.iso_prep_rx.is_some() {
            if event::poll(Duration::from_millis(150))? {
                event::read()?
            } else {
                continue;
            }
        } else {
            event::read()?
        };

        let Event::Key(key) = event else {
            continue; // ignore resize/mouse events
        };
        // Some terminals report key releases too; only react to actual presses.
        if key.kind != KeyEventKind::Press {
            continue;
        }

        // Ctrl+C must always quit, in every mode, typing or not.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            app.should_quit = true;
            continue;
        }

        if app.is_typing {
            match key.code {
                KeyCode::Esc => app.is_typing = false,
                KeyCode::Enter => app.submit_current_step(),
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(field) = app.editable_field_mut() {
                        field.clear();
                    }
                }
                // Plain characters only: Ctrl+<key> must never insert letters.
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(field) = app.editable_field_mut() {
                        field.push(c);
                    }
                }
                KeyCode::Backspace => {
                    if let Some(field) = app.editable_field_mut() {
                        field.pop();
                    }
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Char('q') => app.should_quit = true,
                KeyCode::Esc => {
                    if app.step == Step::IsoPath && app.deep_scan_rx.is_some() {
                        app.cancel_deep_scan();
                    } else if app.step == Step::IsoPath && app.extraction_failed {
                        app.skip_failed_extraction();
                    } else {
                        app.prev_step();
                    }
                }
                KeyCode::Enter => app.submit_current_step(),
                KeyCode::Char('i') | KeyCode::Char('a') => {
                    if app.has_text_field() {
                        app.is_typing = true;
                    }
                }
                KeyCode::Up => app.move_cursor(-1),
                KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1),
                KeyCode::Char('k') => {
                    if app.step == Step::Finished {
                        app.finish_session_choice(true);
                    } else {
                        app.move_cursor(-1);
                    }
                }
                KeyCode::Char(' ') => app.toggle_selected(),
                KeyCode::Char('m') => app.toggle_iso_mode(),
                KeyCode::Char('d') => {
                    if app.step == Step::Finished {
                        app.finish_session_choice(false);
                    } else if app.step == Step::IsoPath && app.iso_mode == IsoMode::Auto {
                        app.show_scan_report = !app.show_scan_report;
                    }
                }
                KeyCode::Char('f') => {
                    if app.step == Step::IsoPath && app.iso_mode == IsoMode::Auto {
                        app.start_deep_scan();
                    }
                }
                KeyCode::Char('r') => {
                    if app.step == Step::IsoPath && app.iso_mode == IsoMode::Auto {
                        app.rescan_isos();
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// One rendered line of a checkbox list.
struct CheckRow {
    text: String,
    selected: bool,
}

fn ui(frame: &mut Frame, app: &App) {
    let terminal_size = frame.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Length(3), Constraint::Min(10), Constraint::Length(3)])
        .split(terminal_size);

    let (current, total) = app.current_step_number();

    let step_name = match app.step {
        Step::IsoPath => "Original ISO File",
        Step::Desktop => "Desktop Environment",
        Step::Packages => "Extra Packages",
        Step::Tweaks => "System Tweaks",
        Step::Confirm => "Review & Generate",
        Step::Preview => "Script Preview & Debug",
        Step::Finished => "Finish & Cleanup",
    };

    let title = Paragraph::new(format!(
        " ISO-BUILDER | Step {} of {}: {} ",
        current, total, step_name
    ))
    .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);

    match app.step {
        Step::IsoPath => match app.iso_mode {
            IsoMode::Auto => draw_auto_view(frame, chunks[1], app),
            IsoMode::Manual => draw_input(
                frame,
                chunks[1],
                " Absolute path to your .iso file ",
                &app.iso_input,
                app.is_typing,
                app.error_msg.as_deref(),
                app.iso_notice.as_deref(),
            ),
        },
        Step::Desktop => draw_input(
            frame,
            chunks[1],
            " Desktop Env (e.g. Hyprland, KDE, GNOME) ",
            &app.desktop,
            app.is_typing,
            app.error_msg.as_deref(),
            app.iso_info_note.as_deref(),
        ),
        Step::Packages => {
            let rows: Vec<CheckRow> = app
                .packages
                .iter()
                .map(|p| CheckRow {
                    text: format!("{:<14} - {} ({})", p.name, p.desc, p.size),
                    selected: p.selected,
                })
                .collect();
            let title = format!(
                " Extra Packages - {} selected ",
                app.selected_packages().len()
            );
            draw_checklist(frame, chunks[1], &title, &rows, app.package_cursor);
        }
        Step::Tweaks => {
            let rows: Vec<CheckRow> = app
                .tweaks
                .iter()
                .map(|t| CheckRow {
                    text: format!("{:<22} - {}", t.name, t.desc),
                    selected: t.selected,
                })
                .collect();
            let title = format!(
                " System Tweaks - {} selected ",
                app.selected_tweaks().len()
            );
            draw_checklist(frame, chunks[1], &title, &rows, app.tweak_cursor);
        }
        Step::Confirm => {
            let summary = format!(
                "Original ISO: {}\nDistro: {}\nDesktop Env: {}\nPackage Mgr: {}\nPackages: {}\nSystem Tweaks: {}\n\nPress [Enter] to build the script for review.",
                app.iso_path,
                app.distro_name,
                app.desktop,
                app.pkg_mgr,
                app.packages_summary(),
                app.tweaks_summary()
            );
            frame.render_widget(
                Paragraph::new(summary)
                    .wrap(Wrap { trim: true })
                    .block(Block::default().title(" Review ").borders(Borders::ALL)),
                chunks[1],
            );
        }
        Step::Preview => {
            // Script view on top, debug summary pinned underneath.
            let panes = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(6), Constraint::Length(8)])
                .split(chunks[1]);

            let script = Paragraph::new(app.generated_script.as_str())
                .wrap(Wrap { trim: false })
                .scroll((app.preview_scroll, 0))
                .block(
                    Block::default()
                        .title(" Script Preview & Debug (Scroll with j/k) ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                );
            frame.render_widget(script, panes[0]);

            let workspace = if app.work_dir.is_empty() {
                "(not extracted)"
            } else {
                app.work_dir.as_str()
            };
            let debug = format!(
                "Packages to install: {}\nTweaks to apply: {}\nDistro / package manager: {} / {}\nExtracted to: {}\nNote: PKG_MGR is re-checked at runtime inside the script.",
                app.packages_summary(),
                app.tweaks_summary(),
                app.distro_name,
                app.pkg_mgr,
                workspace
            );
            frame.render_widget(
                Paragraph::new(debug)
                    .wrap(Wrap { trim: true })
                    .block(
                        Block::default()
                            .title(" Debug ")
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Yellow)),
                    ),
                panes[1],
            );
        }
        Step::Finished => {
            let workspace = app.active_work_dir.as_deref().unwrap_or("(no extraction)");
            let summary = format!(
                "[OK] run-builder.sh is built and will be written when iso-builder exits.\n\n\
                 Extracted workspace:\n  {}\n\n\
                 Clean up the extracted files when iso-builder exits?\n\n\
                 \u{20}\u{20}[K] Keep extracted files (for debugging/reuse)\n\
                 \u{20}\u{20}[D] Delete them now  (default)\n\n\
                 Press [K] or [D] to finish, [Esc] to go back.",
                workspace
            );
            frame.render_widget(
                Paragraph::new(summary)
                    .wrap(Wrap { trim: true })
                    .block(
                        Block::default()
                            .title(" Finish & Cleanup ")
                            .borders(Borders::ALL),
                    ),
                chunks[1],
            );
        }
    }

    let footer_text = if app.is_typing {
        " [Typing] [Enter] submit | [Esc] stop editing | [Ctrl+U] clear | [Ctrl+C] quit "
    } else {
        match app.step {
            Step::Confirm => " [Enter] Build & Preview | [Esc] to go back | [q]/[Ctrl+C] to quit ",
            Step::Packages | Step::Tweaks => {
                " [j/k or Up/Down] move | [Space] toggle | [Enter] continue | [Esc] back | [q] quit "
            }
            Step::IsoPath => match app.iso_mode {
                IsoMode::Auto => {
                    let wide = terminal_size.width > 120;
                    let overlay_visible =
                        !wide && !app.discovered_isos.is_empty() && app.show_scan_report;
                    let report_visible = wide || app.discovered_isos.is_empty();
                    if overlay_visible {
                        " [d] Hide Report | [Enter] Select | [m] Manual Path | [f] Deep Scan | [r] Rescan | [q] Quit "
                    } else if report_visible {
                        " [Enter] Select | [m] Manual Path | [f] Deep Scan | [r] Rescan | [q] Quit "
                    } else {
                        " [Enter] Select | [m] Manual Path | [f] Deep Scan | [r] Rescan | [d] Scan Report | [q] Quit "
                    }
                }
                IsoMode::Manual => " [i] to type | [Enter] Select | [m] Auto Detect | [q]/[Ctrl+C] Quit ",
            },
            Step::Preview => " [j/k] Scroll | [Enter] Review Cleanup | [Esc] Back | [q] Quit ",
            Step::Finished => {
                " [K] Keep extracted files | [D] Delete (default) | [Enter] Delete | [Esc] Back "
            }
            _ => " [i] to type | [Enter] to continue | [Esc] to go back | [q]/[Ctrl+C] to quit ",
        }
    };
    let footer_style = if app.is_typing {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    frame.render_widget(
        Paragraph::new(footer_text)
            .style(footer_style)
            .block(Block::default().borders(Borders::ALL)),
        chunks[2],
    );
}

/// Renders a scrollable checkbox list: `[x] name - detail`, with the row under
/// the cursor highlighted in yellow.
fn draw_checklist(frame: &mut Frame, area: Rect, title: &str, rows: &[CheckRow], cursor: usize) {
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let visible = inner.height as usize;
    // Scroll window that always keeps the cursor row on screen.
    let first = cursor.saturating_sub(visible.saturating_sub(1));

    for (index, row) in rows.iter().enumerate().skip(first).take(visible) {
        let marker = if row.selected { "[x]" } else { "[ ]" };
        let text = format!("{} {}", marker, row.text);
        let style = if index == cursor {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if row.selected {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        let line_area = Rect {
            x: inner.x,
            y: inner.y + (index - first) as u16,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(text).style(style), line_area);
    }
}

/// Renders the auto-discovered ISO list as `path  [size]`, with the row under
/// the cursor highlighted (black on yellow, bold). When nothing was found, a
/// big "No ISOs found" message is shown instead.
fn draw_iso_list(frame: &mut Frame, area: Rect, isos: &[IsoEntry], cursor: usize) {
    let block = Block::default()
        .title(format!(" Auto-discovered ISOs - {} found ", isos.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    if isos.is_empty() {
        let message = vec![
            Line::from("No ISOs found automatically.")
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Line::from(""),
            Line::from("Press [m] to enter a manual path.")
                .style(Style::default().fg(Color::Yellow)),
        ];
        frame.render_widget(Paragraph::new(message).alignment(Alignment::Center), inner);
        return;
    }

    let visible = inner.height as usize;
    // Scroll window that always keeps the cursor row on screen.
    let first = cursor.saturating_sub(visible.saturating_sub(1));

    for (index, iso) in isos.iter().enumerate().skip(first).take(visible) {
        let (marker, style) = if index == cursor {
            (
                "> ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("  ", Style::default().fg(Color::Yellow))
        };
        let label = format!("{}{}  [{}]", marker, iso.path, human_size(iso.size_bytes));
        let line_area = Rect {
            x: inner.x,
            y: inner.y + (index - first) as u16,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(label).style(style), line_area);
    }
}

/// Auto-mode layout. Wide terminals (>120 cols) get the scan report as a
/// right-hand panel (60/40); narrow ones keep it one keypress away with the
/// [d] overlay so the step still fits a 24-row terminal. With no ISOs at all,
/// the big "No ISOs found" message sits above the scan report.
fn draw_auto_view(frame: &mut Frame, area: Rect, app: &App) {
    let wide = frame.size().width > 120;

    if app.discovered_isos.is_empty() {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(5), Constraint::Min(6)])
            .split(area);
        draw_iso_list(frame, rows[0], &app.discovered_isos, app.iso_cursor);
        draw_scan_report(frame, rows[1], &app.scanned_paths);
    } else if wide {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);
        draw_iso_list(frame, cols[0], &app.discovered_isos, app.iso_cursor);
        draw_scan_report(frame, cols[1], &app.scanned_paths);
    } else {
        draw_iso_list(frame, area, &app.discovered_isos, app.iso_cursor);
        if app.show_scan_report {
            draw_scan_report_overlay(frame, area, &app.scanned_paths);
        }
    }

    // The loading state sits on top of whatever the ISO step is showing.
    if app.deep_scan_rx.is_some() {
        draw_deep_scan_overlay(frame, area);
    } else if app.iso_prep_rx.is_some() {
        draw_extraction_overlay(frame, area);
    } else if let Some(message) = app.error_msg.as_deref() {
        draw_error_overlay(frame, area, message);
    }
}

/// The " Scan Report " panel: one line per scanned folder. Failures are
/// flagged with `[WARN]` and highlighted in red.
fn draw_scan_report(frame: &mut Frame, area: Rect, scanned_paths: &[String]) {
    let lines: Vec<Line> = if scanned_paths.is_empty() {
        vec![
            Line::from("No folders were scanned yet.").style(Style::default().fg(Color::DarkGray))
        ]
    } else {
        scanned_paths
            .iter()
            .map(|line| {
                let style = if line.starts_with("[WARN]") {
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(line.clone()).style(style)
            })
            .collect()
    };

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .title(" Scan Report ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        area,
    );
}

/// Centered popup with the scan report, drawn over the ISO list on narrow
/// terminals; toggled with [d].
fn draw_scan_report_overlay(frame: &mut Frame, area: Rect, scanned_paths: &[String]) {
    let width = (area.width.saturating_sub(8)).clamp(10, 70).min(area.width);
    let height = (area.height.saturating_sub(2)).clamp(4, 14).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    draw_scan_report(frame, popup, scanned_paths);
}

/// Centered loading popup shown over the ISO list while the [f] deep scan
/// runs in the background.
fn draw_deep_scan_overlay(frame: &mut Frame, area: Rect) {
    let width = (area.width.saturating_sub(4)).clamp(20, 62).min(area.width);
    let height = (area.height.saturating_sub(2)).clamp(3, 5).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let message = vec![
        Line::from(""),
        Line::from("[SCAN] Deep scanning for ISOs > 500MB...")
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Line::from("Skip system folders. Press [Esc] to cancel.")
            .style(Style::default().fg(Color::Yellow)),
    ];
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Deep Scan ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
        popup,
    );
}

/// Centered loading popup shown over the ISO list while the selected ISO is
/// extracted into its workspace and inspected (xorriso/7z + unsquashfs).
fn draw_extraction_overlay(frame: &mut Frame, area: Rect) {
    let width = (area.width.saturating_sub(4)).clamp(20, 72).min(area.width);
    let height = (area.height.saturating_sub(2)).clamp(5, 6).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let message = vec![
        Line::from(""),
        Line::from(EXTRACTING_MESSAGE)
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Line::from("The workspace is cleaned up on exit; [q] quits.")
            .style(Style::default().fg(Color::DarkGray)),
    ];
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Extracting ISO ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
        popup,
    );
}

/// Centered red popup with the last failure (e.g. a failed extraction),
/// drawn over the ISO list.
fn draw_error_overlay(frame: &mut Frame, area: Rect, message: &str) {
    let width = (area.width.saturating_sub(4)).clamp(24, 72).min(area.width);
    let height = (area.height.saturating_sub(2)).clamp(6, 9).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let text = vec![
        Line::from(""),
        Line::from("[WARN] Error".to_string())
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        Line::from(message.to_string()).style(Style::default().fg(Color::Red)),
        Line::from(""),
        Line::from("Press [Enter] to retry or [Esc] to continue without extraction.")
            .style(Style::default().fg(Color::DarkGray)),
    ];
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Extraction ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            ),
        popup,
    );
}

fn draw_input(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    text: &str,
    is_typing: bool,
    error: Option<&str>,
    note: Option<&str>,
) {
    let border_color = if error.is_some() {
        Color::Red
    } else if is_typing {
        Color::Green
    } else {
        Color::Reset
    };

    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let inner_height = area.height.saturating_sub(2).max(1) as usize;

    // Hard-wrap ourselves so the caret math below matches exactly what is
    // rendered (ratatui's word wrapper breaks long paths at different spots).
    let mut lines: Vec<Line> = hard_wrap(text, inner_width)
        .into_iter()
        .map(|line| Line::from(line))
        .collect();
    if let Some(err) = error {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("[WARN] {}", err)).style(Style::default().fg(Color::Red)));
    }
    if let Some(note) = note {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("[INFO] {}", note)).style(Style::default().fg(Color::Yellow)));
    }

    // Where the caret sits once the text wraps, plus the scroll needed to keep
    // that row inside the visible area.
    let cursor_chars = text.chars().count();
    let cursor_row = cursor_chars / inner_width;
    let cursor_col = cursor_chars % inner_width;
    let scroll_y = cursor_row.saturating_sub(inner_height.saturating_sub(1)) as u16;

    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(if is_typing { Color::Green } else { Color::Reset }))
        .wrap(Wrap { trim: false })
        .scroll((scroll_y, 0))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        );
    frame.render_widget(paragraph, area);

    if is_typing && area.width > 2 && area.height > 2 {
        let row_on_screen = cursor_row - scroll_y as usize;
        frame.set_cursor(
            area.x + 1 + cursor_col as u16,
            area.y + 1 + row_on_screen as u16,
        );
    }
}

/// Hard-wraps `text` at `width` characters, returning at least one line so the
/// caret always has a row to sit on.
fn hard_wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    let mut lines: Vec<String> = chars
        .chunks(width)
        .map(|chunk| chunk.iter().copied().collect::<String>())
        .collect();
    if chars.len() % width == 0 {
        lines.push(String::new());
    }
    lines
}

// ---------------------------------------------------------------------------
// Script generation
// ---------------------------------------------------------------------------

/// Wraps a value in single quotes for bash, escaping embedded single quotes.
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Builds the complete `run-builder.sh` in memory. The preview step renders
/// this string so the generated logic (package manager detection, package
/// installs, tweaks) can be inspected before anything touches the disk.
fn build_script(app: &App) -> String {
    let mut script = String::new();
    script.push_str("#!/bin/bash\n");
    script.push_str("set -e\n");
    script.push_str(&format!("# Generated by iso-builder for {}.\n", app.desktop));
    script.push_str(&format!(
        "echo {}\n",
        shell_single_quote(&format!("Starting remaster with {}", app.desktop))
    ));
    script.push_str(&format!("SOURCE_ISO={}\n", shell_single_quote(&app.iso_path)));

    // The script shares its workspace with the TUI: when the extracted
    // files are still around (the user kept them on exit) they are reused
    // as-is; otherwise the script extracts the ISO itself, so it keeps
    // working on another machine.
    let work_dir = if app.work_dir.is_empty() {
        format!("{}/image", WORK_ROOT)
    } else {
        app.work_dir.clone()
    };
    script.push_str(&format!("WORK_DIR={}\n", shell_single_quote(&work_dir)));

    script.push_str(
        r#"
# ---------------------------------------------------------------------------
# Package manager reference (who uses what):
#   pacman -> Arch, CachyOS, Manjaro, EndeavourOS, Garuda, Artix
#   apt    -> Debian, Ubuntu, Linux Mint, Pop!_OS, Kali, Zorin OS
#   dnf    -> Fedora, RHEL, CentOS, Rocky, AlmaLinux, Bazzite
#   zypper -> openSUSE, SUSE Linux Enterprise
#   xbps   -> Void Linux
#   emerge -> Gentoo, Funtoo
#   apk    -> Alpine Linux, postmarketOS
#
# WORK_DIR above is the extraction workspace shared with iso-builder
# (WORK_DIR/iso-root + WORK_DIR/rootfs). When it still holds a finished
# extraction, that copy is reused and nothing is extracted again; when the
# folder is missing (fresh machine, or it was cleaned up on exit), the ISO
# is extracted here first. Detection is layered and never trusts a single
# tool:
#   1) the ISO's own file listing is inspected with whichever tool is
#      installed (7z, xorriso, isoinfo or bsdtar),
#   2) the extracted rootfs is checked for the package manager's binary,
#   3) as a last resort the same check runs inside the chroot.
# If the final answer is still wrong, force it via PKG_MGR_OVERRIDE below.
PKG_MGR_OVERRIDE=""

if [ "$(id -u)" -ne 0 ]; then echo "Please run this script as root (sudo)."; exit 1; fi

ROOTFS="$WORK_DIR/rootfs"
SQFS_FILE=$(find "$WORK_DIR/iso-root" \( -iname "*.sfs" -o -iname "*.squashfs" \) 2>/dev/null | head -n 1 || true)
REUSE_EXTRACTION=""
if [ -n "$SQFS_FILE" ] && [ -d "$ROOTFS" ] && [ -n "$(ls -A "$ROOTFS" 2>/dev/null)" ]; then
    REUSE_EXTRACTION=1
    echo "Reusing existing extraction in $WORK_DIR (delete it to force a fresh extraction)."
elif [ ! -f "$SOURCE_ISO" ]; then
    echo "Error: ISO file not found at $SOURCE_ISO and no extracted workspace under $WORK_DIR to reuse."
    exit 1
fi

echo "Detecting distro structure..."

DETECT_CMD=""
if command -v 7z >/dev/null 2>&1; then
    DETECT_CMD="7z"
elif command -v xorriso >/dev/null 2>&1; then
    DETECT_CMD="xorriso"
elif command -v isoinfo >/dev/null 2>&1; then
    DETECT_CMD="isoinfo"
elif command -v bsdtar >/dev/null 2>&1; then
    DETECT_CMD="bsdtar"
fi

PKG_MGR=""
if [ -n "$PKG_MGR_OVERRIDE" ]; then
    PKG_MGR="$PKG_MGR_OVERRIDE"
    echo "Using manual override: $PKG_MGR"
elif [ -n "$REUSE_EXTRACTION" ]; then
    echo "Using the cached extraction in $WORK_DIR."
elif [ "$DETECT_CMD" = "7z" ]; then
    ISO_LIST=$(7z l "$SOURCE_ISO" 2>/dev/null || true)
    echo "$ISO_LIST" | grep -q "airootfs.sfs" && PKG_MGR="pacman"
    echo "$ISO_LIST" | grep -q "filesystem.squashfs" && PKG_MGR="apt"
    echo "$ISO_LIST" | grep -q "squashfs.img" && PKG_MGR="dnf"
elif [ "$DETECT_CMD" = "xorriso" ]; then
    ISO_LIST=$(xorriso -indev "$SOURCE_ISO" -find / -type f 2>/dev/null || true)
    echo "$ISO_LIST" | grep -q "airootfs.sfs" && PKG_MGR="pacman"
    echo "$ISO_LIST" | grep -q "filesystem.squashfs" && PKG_MGR="apt"
    echo "$ISO_LIST" | grep -q "squashfs.img" && PKG_MGR="dnf"
elif [ "$DETECT_CMD" = "isoinfo" ]; then
    ISO_LIST=$(isoinfo -f -i "$SOURCE_ISO" 2>/dev/null || true)
    echo "$ISO_LIST" | grep -q "airootfs.sfs" && PKG_MGR="pacman"
    echo "$ISO_LIST" | grep -q "filesystem.squashfs" && PKG_MGR="apt"
elif [ "$DETECT_CMD" = "bsdtar" ]; then
    ISO_LIST=$(bsdtar -tf "$SOURCE_ISO" 2>/dev/null || true)
    echo "$ISO_LIST" | grep -q "airootfs.sfs" && PKG_MGR="pacman"
    echo "$ISO_LIST" | grep -q "filesystem.squashfs" && PKG_MGR="apt"
fi

if [ -z "$REUSE_EXTRACTION" ]; then
    echo "Extracting ISO... This may take a few minutes."
    rm -rf "$WORK_DIR/iso-root" "$ROOTFS"
    mkdir -p "$WORK_DIR/iso-root" "$ROOTFS"
    if command -v xorriso >/dev/null 2>&1 && xorriso -osirrox on -indev "$SOURCE_ISO" -extract / "$WORK_DIR/iso-root"; then
        echo "Extracted with xorriso."
    else
        rm -rf "$WORK_DIR/iso-root" && mkdir -p "$WORK_DIR/iso-root"
        if command -v 7z >/dev/null 2>&1 && 7z x -y "$SOURCE_ISO" -o"$WORK_DIR/iso-root" >/dev/null; then
            echo "Extracted with 7z."
        else
            echo "FATAL: neither xorriso nor 7z could extract the ISO."
            exit 1
        fi
    fi
    chmod -R +w "$WORK_DIR/iso-root" 2>/dev/null || true

    echo "Extracting Squashfs..."
    SQFS_FILE=$(find "$WORK_DIR/iso-root" \( -iname "*.sfs" -o -iname "*.squashfs" \) | head -n 1)
    if [ -z "$SQFS_FILE" ]; then echo "Error: no squashfs image found inside the ISO"; exit 1; fi
    unsquashfs -f -d "$ROOTFS" "$SQFS_FILE"
fi

if [ -z "$PKG_MGR" ]; then
    echo "Detecting package manager from the extracted rootfs..."
    if [ -x "$ROOTFS/usr/bin/pacman" ]; then PKG_MGR="pacman"
    elif [ -x "$ROOTFS/usr/bin/apt" ]; then PKG_MGR="apt"
    elif [ -x "$ROOTFS/usr/bin/dnf" ]; then PKG_MGR="dnf"
    elif [ -x "$ROOTFS/usr/bin/zypper" ]; then PKG_MGR="zypper"
    elif [ -x "$ROOTFS/usr/bin/xbps-install" ]; then PKG_MGR="xbps"
    elif [ -x "$ROOTFS/usr/bin/emerge" ]; then PKG_MGR="emerge"
    elif [ -x "$ROOTFS/sbin/apk" ] || [ -x "$ROOTFS/usr/bin/apk" ]; then PKG_MGR="apk"
    fi
fi
if [ -z "$PKG_MGR" ]; then
    echo "Detecting package manager inside chroot..."
    if arch-chroot "$ROOTFS" command -v pacman >/dev/null 2>&1; then
        PKG_MGR="pacman"
    elif arch-chroot "$ROOTFS" command -v apt-get >/dev/null 2>&1; then
        PKG_MGR="apt"
    elif arch-chroot "$ROOTFS" command -v dnf >/dev/null 2>&1; then
        PKG_MGR="dnf"
    elif arch-chroot "$ROOTFS" command -v zypper >/dev/null 2>&1; then
        PKG_MGR="zypper"
    elif arch-chroot "$ROOTFS" command -v xbps-install >/dev/null 2>&1; then
        PKG_MGR="xbps"
    elif arch-chroot "$ROOTFS" command -v emerge >/dev/null 2>&1; then
        PKG_MGR="emerge"
    elif arch-chroot "$ROOTFS" command -v apk >/dev/null 2>&1; then
        PKG_MGR="apk"
    else
        echo "FATAL: Could not detect package manager!"
        exit 1
    fi
fi
case "$PKG_MGR" in
    pacman|apt|dnf|zypper|xbps|emerge|apk) ;;
    *) echo "Error: unknown package manager '$PKG_MGR' (use pacman, apt, dnf, zypper, xbps, emerge or apk)"; exit 1 ;;
esac
echo "Using package manager: $PKG_MGR"
echo "If this is wrong, set PKG_MGR_OVERRIDE at the top of this script and re-run."

echo "Mounting virtual filesystems for chroot..."
mount -t proc /proc "$ROOTFS/proc"
mount --rbind /sys "$ROOTFS/sys"
mount --rbind /dev "$ROOTFS/dev"
cp /etc/resolv.conf "$ROOTFS/etc/resolv.conf" 2>/dev/null || true
"#,
    );
    script.push('\n');

    // Only selected packages make it into the script; the install command
    // follows whatever package manager was detected at runtime.
    let selected_packages = app.selected_packages();
    if !selected_packages.is_empty() {
        let packages = selected_packages.join(" ");
        script.push_str("echo 'Installing selected packages...'\n");
        script.push_str("case \"$PKG_MGR\" in\n");
        script.push_str(&format!(
            "    pacman) arch-chroot \"$ROOTFS\" bash -c {} ;;\n",
            shell_single_quote(&format!("pacman -S --noconfirm {}", packages))
        ));
        script.push_str(&format!(
            "    apt) arch-chroot \"$ROOTFS\" bash -c {} ;;\n",
            shell_single_quote(&format!("apt-get update && apt-get install -y {}", packages))
        ));
        script.push_str(&format!(
            "    dnf) arch-chroot \"$ROOTFS\" bash -c {} ;;\n",
            shell_single_quote(&format!("dnf install -y {}", packages))
        ));
        script.push_str(&format!(
            "    zypper) arch-chroot \"$ROOTFS\" bash -c {} ;;\n",
            shell_single_quote(&format!("zypper -n install -y {}", packages))
        ));
        script.push_str(&format!(
            "    xbps) arch-chroot \"$ROOTFS\" bash -c {} ;;\n",
            shell_single_quote(&format!("xbps-install -Sy {}", packages))
        ));
        script.push_str(&format!(
            "    emerge) arch-chroot \"$ROOTFS\" bash -c {} ;;\n",
            shell_single_quote(&format!("emerge --noreplace {}", packages))
        ));
        script.push_str(&format!(
            "    apk) arch-chroot \"$ROOTFS\" bash -c {} ;;\n",
            shell_single_quote(&format!("apk add {}", packages))
        ));
        script.push_str("esac\n");
    }

    // Only selected tweaks make it into the script.
    let selected_tweaks = app.selected_tweaks();
    if !selected_tweaks.is_empty() {
        script.push_str("echo 'Applying system tweaks...'\n");
        for (name, command) in selected_tweaks {
            script.push_str(&format!(
                "echo {}\n",
                shell_single_quote(&format!("  - {}", name))
            ));
            script.push_str(&format!(
                "arch-chroot \"$ROOTFS\" bash -c {}\n",
                shell_single_quote(command)
            ));
        }
    }

    script.push_str(
        r#"
echo "Unmounting filesystems..."
umount -l "$ROOTFS/proc"
umount -l "$ROOTFS/sys"
umount -l "$ROOTFS/dev"

echo "Repacking Squashfs..."
rm -f "$SQFS_FILE"
mksquashfs "$ROOTFS" "$SQFS_FILE" -comp xz -b 1M

echo "Repacking final ISO..."
# NOTE: these xorriso flags assume an archiso-style boot layout
# (isolinux + EFI/archiso/efiboot.img); other distros need their own layout.
xorriso -as mkisofs -o "custom-remaster.iso" -isohybrid-mbr /usr/lib/syslinux/bios/isohdpfx.bin -c isolinux/boot.cat -b isolinux/isolinux.bin -no-emul-boot -boot-load-size 4 -boot-info-table -eltorito-alt-boot -e EFI/archiso/efiboot.img -no-emul-boot -isohybrid-gpt-basdat "$WORK_DIR/iso-root"

echo "[OK] Done! Saved as custom-remaster.iso"

echo "[CLEANUP] Cleaning up the extracted workspace..."
rm -rf "$WORK_DIR"
"#,
    );
    script.push('\n');

    script
}

/// Writes the reviewed script to `run-builder.sh` and marks it executable.
/// What is written is exactly what the preview showed.
fn write_script(app: &App) -> io::Result<()> {
    println!("\n[BUILD] Generating script for: {}", app.desktop);

    let mut file = File::create("run-builder.sh")?;
    file.write_all(app.generated_script.as_bytes())?;
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata("run-builder.sh")?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions("run-builder.sh", permissions)?;
    }

    println!("[OK] Successfully generated run-builder.sh!");
    Ok(())
}
