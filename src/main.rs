use std::collections::HashSet;
use std::env;
use std::fs::File;
use std::io::{self, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
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

/// "1 ISO" / "3 ISOs" — the unit used in scan report lines.
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
/// (not just its subfolders) so an ISO sitting in `~` — or in `/root` when
/// running as root — is found.
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

/// Every `.iso` file in the default folders — with its size — plus a
/// per-folder debug line such as `"/root (1 ISO)"` or
/// `"⚠ Permission denied: /media"`.
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
                debug.push(format!("⚠ Permission denied: {}", root.display()));
            }
            Err(err) => {
                debug.push(format!("⚠ Could not read {}: {}", root.display(), err));
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

/// `(device, inode)` — identifies the physical file behind a path, so the
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
                        self.iso_path = iso.path.clone();
                        self.next_step();
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
                    self.iso_path = path;
                    self.next_step();
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
                self.generate_script = true;
                self.should_quit = true;
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
            Step::Preview => Step::Preview,
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
        };
        (current, 6)
    }
}

// ---------------------------------------------------------------------------
// Terminal lifecycle and event loop
// ---------------------------------------------------------------------------

fn main() -> Result<(), io::Error> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    let mut app = App::new();
    run_tui(&mut app)?;

    if app.generate_script {
        write_script(&app)?;
    }

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
        // Pick up finished deep scans (non-blocking) before every frame.
        app.poll_deep_scan();

        terminal.draw(|frame| ui(frame, app))?;

        // While a deep scan runs, wake up periodically so the loading state
        // is refreshed and results appear without extra key presses. Input is
        // still read live, so [q]/[Ctrl+C] quit immediately.
        let event = if app.deep_scan_rx.is_some() {
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
                KeyCode::Up | KeyCode::Char('k') => app.move_cursor(-1),
                KeyCode::Down | KeyCode::Char('j') => app.move_cursor(1),
                KeyCode::Char(' ') => app.toggle_selected(),
                KeyCode::Char('m') => app.toggle_iso_mode(),
                KeyCode::Char('d') => {
                    if app.step == Step::IsoPath && app.iso_mode == IsoMode::Auto {
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
    };

    let title = Paragraph::new(format!(
        "🧠 iso-builder | Step {} of {}: {}",
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
            None,
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
                " Extra Packages — {} selected ",
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
                " System Tweaks — {} selected ",
                app.selected_tweaks().len()
            );
            draw_checklist(frame, chunks[1], &title, &rows, app.tweak_cursor);
        }
        Step::Confirm => {
            let summary = format!(
                "Original ISO: {}\nDesktop Env: {}\nPackages: {}\nSystem Tweaks: {}\n\nPress [Enter] to build the script for review.",
                app.iso_path,
                app.desktop,
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
                .constraints([Constraint::Min(6), Constraint::Length(6)])
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

            let debug = format!(
                "Packages to install: {}\nTweaks to apply: {}\nNote: PKG_MGR is detected at runtime inside the script (see the if/elif block above).",
                app.packages_summary(),
                app.tweaks_summary()
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
            Step::Preview => " [j/k] Scroll | [Enter] Save & Exit | [Esc] Back | [Ctrl+C] Quit ",
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
        .title(format!(" Auto-discovered ISOs — {} found ", isos.len()))
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
    }
}

/// The " Scan Report " panel: one line per scanned folder. Failures are
/// flagged with `⚠` and highlighted in red.
fn draw_scan_report(frame: &mut Frame, area: Rect, scanned_paths: &[String]) {
    let lines: Vec<Line> = if scanned_paths.is_empty() {
        vec![
            Line::from("No folders were scanned yet.").style(Style::default().fg(Color::DarkGray))
        ]
    } else {
        scanned_paths
            .iter()
            .map(|line| {
                let style = if line.starts_with('⚠') {
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
        Line::from("🔍 Deep scanning for ISOs > 500MB...")
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
        lines.push(Line::from(format!("⚠ {}", err)).style(Style::default().fg(Color::Red)));
    }
    if let Some(note) = note {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("ℹ {}", note)).style(Style::default().fg(Color::Yellow)));
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

    script.push_str(
        r#"
# ---------------------------------------------------------------------------
# Package manager reference (who uses what):
#   pacman -> Arch, CachyOS, Manjaro, EndeavourOS, Garuda, Artix
#   apt    -> Debian, Ubuntu, Linux Mint, Pop!_OS, Kali, Zorin OS
#   dnf    -> Fedora, RHEL, CentOS, Rocky, AlmaLinux, Bazzite
#   apk    -> Alpine Linux, postmarketOS
#
# Detection is layered and never trusts a single tool:
#   1) the ISO's own file listing is inspected with whichever tool is
#      installed (7z, xorriso, isoinfo or bsdtar),
#   2) if that finds nothing, the extracted rootfs is probed from inside the
#      chroot.
# If the final answer is still wrong, force it via PKG_MGR_OVERRIDE below.
PKG_MGR_OVERRIDE=""

if [ "$(id -u)" -ne 0 ]; then echo "Please run this script as root (sudo)."; exit 1; fi
if [ ! -f "$SOURCE_ISO" ]; then echo "Error: ISO file not found at $SOURCE_ISO"; exit 1; fi

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

if [ -z "$PKG_MGR" ]; then
    echo "Pre-extraction detection failed. Will detect inside chroot..."
fi

WORK_DIR="./remaster-workspace"
rm -rf "$WORK_DIR" && mkdir -p "$WORK_DIR/iso-root" "$WORK_DIR/squashfs-root"

echo "Extracting ISO..."
xorriso -osirrox on -indev "$SOURCE_ISO" -extract / "$WORK_DIR/iso-root"
chmod -R +w "$WORK_DIR/iso-root"

echo "Extracting Squashfs..."
SQFS_FILE=$(find "$WORK_DIR/iso-root" -name "*.sfs" -o -name "*.squashfs" | head -n 1)
if [ -z "$SQFS_FILE" ]; then echo "Error: no squashfs image found inside the ISO"; exit 1; fi
unsquashfs -d "$WORK_DIR/squashfs-root" "$SQFS_FILE"

if [ -z "$PKG_MGR" ]; then
    echo "Detecting package manager inside chroot..."
    if arch-chroot "$WORK_DIR/squashfs-root" command -v pacman >/dev/null 2>&1; then
        PKG_MGR="pacman"
    elif arch-chroot "$WORK_DIR/squashfs-root" command -v apt-get >/dev/null 2>&1; then
        PKG_MGR="apt"
    elif arch-chroot "$WORK_DIR/squashfs-root" command -v dnf >/dev/null 2>&1; then
        PKG_MGR="dnf"
    elif arch-chroot "$WORK_DIR/squashfs-root" command -v apk >/dev/null 2>&1; then
        PKG_MGR="apk"
    else
        echo "FATAL: Could not detect package manager!"
        exit 1
    fi
fi
case "$PKG_MGR" in
    pacman|apt|dnf|apk) ;;
    *) echo "Error: unknown package manager '$PKG_MGR' (use pacman, apt, dnf or apk)"; exit 1 ;;
esac
echo "Using package manager: $PKG_MGR"
echo "If this is wrong, set PKG_MGR_OVERRIDE at the top of this script and re-run."

echo "Mounting virtual filesystems for chroot..."
mount -t proc /proc "$WORK_DIR/squashfs-root/proc"
mount --rbind /sys "$WORK_DIR/squashfs-root/sys"
mount --rbind /dev "$WORK_DIR/squashfs-root/dev"
cp /etc/resolv.conf "$WORK_DIR/squashfs-root/etc/resolv.conf" 2>/dev/null || true
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
            "    pacman) arch-chroot \"$WORK_DIR/squashfs-root\" bash -c {} ;;\n",
            shell_single_quote(&format!("pacman -S --noconfirm {}", packages))
        ));
        script.push_str(&format!(
            "    apt) arch-chroot \"$WORK_DIR/squashfs-root\" bash -c {} ;;\n",
            shell_single_quote(&format!("apt-get update && apt-get install -y {}", packages))
        ));
        script.push_str(&format!(
            "    dnf) arch-chroot \"$WORK_DIR/squashfs-root\" bash -c {} ;;\n",
            shell_single_quote(&format!("dnf install -y {}", packages))
        ));
        script.push_str(&format!(
            "    apk) arch-chroot \"$WORK_DIR/squashfs-root\" bash -c {} ;;\n",
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
                "arch-chroot \"$WORK_DIR/squashfs-root\" bash -c {}\n",
                shell_single_quote(command)
            ));
        }
    }

    script.push_str(
        r#"
echo "Unmounting filesystems..."
umount -l "$WORK_DIR/squashfs-root/proc"
umount -l "$WORK_DIR/squashfs-root/sys"
umount -l "$WORK_DIR/squashfs-root/dev"

echo "Repacking Squashfs..."
rm -f "$SQFS_FILE"
mksquashfs "$WORK_DIR/squashfs-root" "$SQFS_FILE" -comp xz -b 1M

echo "Repacking final ISO..."
# NOTE: these xorriso flags assume an archiso-style boot layout
# (isolinux + EFI/archiso/efiboot.img); other distros need their own layout.
xorriso -as mkisofs -o "custom-remaster.iso" -isohybrid-mbr /usr/lib/syslinux/bios/isohdpfx.bin -c isolinux/boot.cat -b isolinux/isolinux.bin -no-emul-boot -boot-load-size 4 -boot-info-table -eltorito-alt-boot -e EFI/archiso/efiboot.img -no-emul-boot -isohybrid-gpt-basdat "$WORK_DIR/iso-root"

echo "✅ Done! Saved as custom-remaster.iso"
"#,
    );
    script.push('\n');

    script
}

/// Writes the reviewed script to `run-builder.sh` and marks it executable.
/// What is written is exactly what the preview showed.
fn write_script(app: &App) -> io::Result<()> {
    println!("\n🔨 Generating script for: {}", app.desktop);

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

    println!("✅ Successfully generated run-builder.sh!");
    Ok(())
}
