//! Wizard state: selections, background jobs and the step machine.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use crate::commands::script::build_script;
use crate::iso::discovery::{deep_scan_worker, discover_isos, IsoEntry};
use crate::iso::workspace::{
    cache_is_ready, cleanup_workspace, disk_space_warning, set_active_work_dir, work_dir_for,
    workspace_is_preexisting, EXTRACTING_MESSAGE, EXTRACTION_RUNNING,
};
use crate::iso::{prepare_iso, IsoPrep, EXTRACTION_PHASE, PHASE_IDLE};
use crate::packages::{default_packages, default_tweaks, PackageOption, PresetOption};
use crate::utils::{
    complete_path, detect_tools, is_safe_package_token, is_valid_hostname, iso_count_unit,
    normalize_user_path, output_name_for,
};

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
pub enum Step {
    IsoPath,
    Packages,
    /// Desktops the distro's installer offers; skipped when none were found.
    Desktops,
    Tweaks,
    Custom,
    Confirm,
    Preview,
    Finished,
}

/// One desktop the ISO's installer offers. Unchecking it (`keep == false`)
/// removes it from the installer's chooser and package list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopOption {
    pub id: String,
    pub name: String,
    pub keep: bool,
}

/// How the user picks the source ISO: from the auto-discovered list or by
/// typing a path themselves.
#[derive(PartialEq, Clone, Copy)]
pub enum IsoMode {
    Auto,
    Manual,
}

/// Indexes into `App::custom`, the text fields of the Custom step.
pub const FIELD_PACKAGES: usize = 0;
pub const FIELD_COMMAND: usize = 1;
pub const FIELD_HOSTNAME: usize = 2;
pub const FIELD_OUTPUT: usize = 3;
const CUSTOM_FIELD_COUNT: usize = 4;

/// File the review screen's [s] writes and `--preset` reads.
const PRESET_FILE: &str = "iso-builder-preset.conf";

pub struct App {
    pub step: Step,
    pub iso_mode: IsoMode,
    /// Confirmed ISO path (used by the review screen and the generated script).
    pub iso_path: String,
    /// Text buffer for manual path entry.
    pub iso_input: String,
    pub discovered_isos: Vec<IsoEntry>,
    pub iso_cursor: usize,
    /// Debug report from the last scan: one line per searched folder with the
    /// number of ISOs found in it (or why it could not be read).
    pub scanned_paths: Vec<String>,
    /// [d] toggle for the scan-report overlay on narrow terminals.
    pub show_scan_report: bool,
    /// Receiver for the background deep scan ([f]); `None` when idle.
    pub deep_scan_rx: Option<Receiver<Vec<IsoEntry>>>,
    /// Cooperative cancel flag for the running deep scan.
    pub deep_scan_cancel: Option<Arc<AtomicBool>>,
    /// Receiver for the background extraction/detection started on [Enter];
    /// `None` when nothing is being prepared.
    pub iso_prep_rx: Option<Receiver<Result<IsoPrep, String>>>,
    /// When the running extraction started (drives the spinner and timer).
    pub prep_started: Option<Instant>,
    /// Status line about the extraction/detection, shown on the Packages
    /// step: "Detected: ..." or why detection was skipped.
    pub iso_info_note: Option<String>,
    /// Shown in the loading popup when the disk looks too small.
    pub disk_warning: Option<String>,
    /// True after a failed extraction; [Esc] then skips detection and
    /// continues without it.
    pub extraction_failed: bool,
    /// Extraction workspace of the selected ISO
    /// (`./iso-builder-work/<name>-<hash>`).
    pub work_dir: String,
    /// The workspace to clean up on exit; `None` until an ISO was picked.
    pub active_work_dir: Option<String>,
    /// True when `active_work_dir` was extracted by an earlier run. Such a
    /// workspace is never deleted unless the user asks for it.
    pub workspace_reused: bool,
    /// Distro name read from the SquashFS image (e.g. "CachyOS Linux").
    pub distro_name: String,
    /// Set on the finish screen ([K] / [Enter]): keep the workspace.
    pub keep_workspace: bool,
    /// Set on the finish screen ([D]): delete the workspace, even a reused one.
    pub delete_requested: bool,
    /// Package manager detected from the selected ISO (or "Unknown").
    pub pkg_mgr: String,
    /// Informational line shown on the ISO step (e.g. "no ISOs found").
    pub iso_notice: Option<String>,
    /// Desktop environment detected in the ISO (display only).
    pub desktop: String,
    /// Desktops the installer inside the ISO offers (empty when unsupported).
    pub desktops: Vec<DesktopOption>,
    pub desktop_cursor: usize,
    /// Rootfs-relative installer files the script edits to drop desktops.
    pub chooser_path: String,
    pub netinstall_paths: Vec<String>,
    /// `drop_desktop=` ids from a preset, applied once detection finishes.
    pub preset_desktop_drops: Vec<String>,
    pub packages: Vec<PackageOption>,
    pub package_cursor: usize,
    pub tweaks: Vec<PresetOption>,
    pub tweak_cursor: usize,
    /// Free-text fields of the Custom step, see the `FIELD_*` constants.
    pub custom: [String; CUSTOM_FIELD_COUNT],
    pub custom_focus: usize,
    /// Field that failed validation (drawn with a red border).
    pub custom_invalid: Option<usize>,
    /// Result of the last [s] preset save, shown on the review screen.
    pub status_msg: Option<String>,
    /// Required tools and whether they are installed.
    pub tools: Vec<(&'static str, bool)>,
    pub is_typing: bool,
    pub should_quit: bool,
    pub generate_script: bool,
    /// Full bash script built from the current selections, shown on the
    /// preview step and written to disk when the user confirms it there.
    pub generated_script: String,
    /// Vertical scroll offset (in lines) for the preview pane.
    pub preview_scroll: u16,
    pub error_msg: Option<String>,
}

impl App {
    /// `scan` is false for the headless modes, which never show a list.
    pub fn new(scan: bool) -> Self {
        let (discovered_isos, scanned_paths) = if scan {
            discover_isos()
        } else {
            (Vec::new(), Vec::new())
        };

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
            prep_started: None,
            iso_info_note: None,
            disk_warning: None,
            extraction_failed: false,
            work_dir: String::new(),
            active_work_dir: None,
            workspace_reused: false,
            distro_name: "Unknown".to_string(),
            keep_workspace: false,
            delete_requested: false,
            pkg_mgr: "Unknown".to_string(),
            iso_notice: None,
            desktop: "Unknown".to_string(),
            desktops: Vec::new(),
            desktop_cursor: 0,
            chooser_path: String::new(),
            netinstall_paths: Vec::new(),
            preset_desktop_drops: Vec::new(),
            packages: default_packages(),
            package_cursor: 0,
            tweaks: default_tweaks(),
            tweak_cursor: 0,
            custom: Default::default(),
            custom_focus: 0,
            custom_invalid: None,
            status_msg: None,
            tools: detect_tools(),
            is_typing: false,
            should_quit: false,
            generate_script: false,
            generated_script: String::new(),
            preview_scroll: 0,
            error_msg: None,
        }
    }

    /// Whether the exit cleanup should delete the workspace: always when the
    /// user pressed [D]; otherwise only a workspace this run extracted and
    /// the user did not explicitly keep.
    pub fn should_delete_workspace(&self) -> bool {
        self.delete_requested || (!self.keep_workspace && !self.workspace_reused)
    }

    pub fn selected_packages(&self) -> Vec<&'static str> {
        self.packages
            .iter()
            .filter(|p| p.selected)
            .map(|p| p.name)
            .collect()
    }

    pub fn selected_tweaks(&self) -> Vec<&PresetOption> {
        self.tweaks.iter().filter(|t| t.selected).collect()
    }

    /// Installer desktops the user unchecked.
    pub fn removed_desktops(&self) -> Vec<&DesktopOption> {
        self.desktops.iter().filter(|d| !d.keep).collect()
    }

    /// Review-screen text for the Desktops step.
    pub fn desktops_summary(&self) -> String {
        if self.desktops.is_empty() {
            return "Not offered by this ISO".to_string();
        }
        let removed = self.removed_desktops();
        if removed.is_empty() {
            format!("All {} kept", self.desktops.len())
        } else {
            let names: Vec<&str> = removed.iter().map(|d| d.name.as_str()).collect();
            format!("Removing {}", names.join(", "))
        }
    }

    /// Extra package names typed on the Custom step.
    pub fn custom_package_list(&self) -> Vec<String> {
        self.custom[FIELD_PACKAGES]
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    pub fn custom_command(&self) -> &str {
        self.custom[FIELD_COMMAND].trim()
    }

    pub fn hostname(&self) -> &str {
        self.custom[FIELD_HOSTNAME].trim()
    }

    /// File name of the remastered ISO (typed name or `<source>-custom.iso`).
    pub fn output_name(&self) -> String {
        output_name_for(&self.custom[FIELD_OUTPUT], &self.iso_path)
            .unwrap_or_else(|_| "custom-remaster.iso".to_string())
    }

    /// Checks the Custom step fields. On failure the error is stored, the
    /// offending field gets the focus and `false` is returned.
    pub fn validate_custom(&mut self) -> bool {
        self.custom_invalid = None;

        for token in self.custom[FIELD_PACKAGES].split_whitespace() {
            if !is_safe_package_token(token) {
                self.error_msg = Some(format!(
                    "Invalid package name '{}': use letters, digits and + . _ : @ / - = only.",
                    token
                ));
                self.custom_invalid = Some(FIELD_PACKAGES);
                self.custom_focus = FIELD_PACKAGES;
                return false;
            }
        }

        let host = self.custom[FIELD_HOSTNAME].trim();
        if !host.is_empty() && !is_valid_hostname(host) {
            self.error_msg = Some(format!(
                "Invalid hostname '{}': letters, digits and hyphens only.",
                host
            ));
            self.custom_invalid = Some(FIELD_HOSTNAME);
            self.custom_focus = FIELD_HOSTNAME;
            return false;
        }

        if let Err(message) = output_name_for(&self.custom[FIELD_OUTPUT], &self.iso_path) {
            self.error_msg = Some(message);
            self.custom_invalid = Some(FIELD_OUTPUT);
            self.custom_focus = FIELD_OUTPUT;
            return false;
        }
        true
    }

    /// Moves the focus between the Custom step's fields (wraps around).
    pub fn cycle_custom_focus(&mut self, backwards: bool) {
        self.custom_focus = if backwards {
            (self.custom_focus + CUSTOM_FIELD_COUNT - 1) % CUSTOM_FIELD_COUNT
        } else {
            (self.custom_focus + 1) % CUSTOM_FIELD_COUNT
        };
    }

    /// [Tab] in the manual path field.
    pub fn complete_iso_path(&mut self) {
        self.iso_input = complete_path(&self.iso_input);
    }

    /// The current choices as a `key=value` preset file.
    pub fn to_preset(&self) -> String {
        let mut out = String::from("# iso-builder preset (load with: iso-builder --preset FILE)\n");
        if !self.iso_path.is_empty() {
            out.push_str(&format!("iso={}\n", self.iso_path));
        }
        for package in self.packages.iter().filter(|p| p.selected) {
            out.push_str(&format!("package={}\n", package.name));
        }
        for tweak in self.tweaks.iter().filter(|t| t.selected) {
            out.push_str(&format!("tweak={}\n", tweak.name));
        }
        let mut dropped: Vec<&str> = self.removed_desktops().iter().map(|d| d.id.as_str()).collect();
        for id in &self.preset_desktop_drops {
            if !self.desktops.iter().any(|d| d.id == *id) {
                dropped.push(id);
            }
        }
        for id in dropped {
            out.push_str(&format!("drop_desktop={}\n", id));
        }
        for (key, field) in [
            ("custom_packages", FIELD_PACKAGES),
            ("custom_command", FIELD_COMMAND),
            ("hostname", FIELD_HOSTNAME),
            ("output", FIELD_OUTPUT),
        ] {
            let value = self.custom[field].trim();
            if !value.is_empty() {
                out.push_str(&format!("{}={}\n", key, value));
            }
        }
        out
    }

    /// Applies a preset file. Unknown keys and names are ignored so presets
    /// keep working when options are added or renamed.
    pub fn apply_preset(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "iso" => {
                    if !value.is_empty() {
                        self.iso_input = value.to_string();
                        self.iso_mode = IsoMode::Manual;
                    }
                }
                "package" => {
                    if let Some(package) = self.packages.iter_mut().find(|p| p.name == value) {
                        package.selected = true;
                    }
                }
                "tweak" => {
                    if let Some(tweak) = self.tweaks.iter_mut().find(|t| t.name == value) {
                        tweak.selected = true;
                    }
                }
                "drop_desktop" => {
                    if !value.is_empty() {
                        self.preset_desktop_drops.push(value.to_string());
                        if let Some(desktop) = self.desktops.iter_mut().find(|d| d.id == value) {
                            desktop.keep = false;
                        }
                    }
                }
                "custom_packages" => self.custom[FIELD_PACKAGES] = value.to_string(),
                "custom_command" => self.custom[FIELD_COMMAND] = value.to_string(),
                "hostname" => self.custom[FIELD_HOSTNAME] = value.to_string(),
                "output" => self.custom[FIELD_OUTPUT] = value.to_string(),
                _ => {}
            }
        }
    }

    /// [s] on the review screen.
    pub fn save_preset(&mut self) {
        self.status_msg = Some(match fs::write(PRESET_FILE, self.to_preset()) {
            Ok(()) => format!("Saved your choices to {}", PRESET_FILE),
            Err(err) => format!("Could not save {}: {}", PRESET_FILE, err),
        });
    }

    /// Comma separated package names for the review / preview screens.
    pub fn packages_summary(&self) -> String {
        let names = self.selected_packages();
        if names.is_empty() {
            "None".to_string()
        } else {
            names.join(", ")
        }
    }

    /// Comma separated tweak names for the review / preview screens.
    pub fn tweaks_summary(&self) -> String {
        let names: Vec<&str> = self.selected_tweaks().iter().map(|t| t.name).collect();
        if names.is_empty() {
            "None".to_string()
        } else {
            names.join(", ")
        }
    }

    /// Only these steps accept free text; packages/tweaks use the checkbox UI.
    /// On the ISO step typing is only possible in manual mode.
    pub fn has_text_field(&self) -> bool {
        match self.step {
            Step::IsoPath => self.iso_mode == IsoMode::Manual,
            Step::Custom => true,
            _ => false,
        }
    }

    pub fn editable_field_mut(&mut self) -> Option<&mut String> {
        match self.step {
            Step::IsoPath => Some(&mut self.iso_input),
            Step::Custom => self.custom.get_mut(self.custom_focus),
            _ => None,
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
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
            Step::Desktops => {
                if let Some(max) = self.desktops.len().checked_sub(1) {
                    self.desktop_cursor = if delta < 0 {
                        self.desktop_cursor.saturating_sub(1)
                    } else {
                        (self.desktop_cursor + 1).min(max)
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
            Step::Custom => {
                self.custom_focus = if delta < 0 {
                    self.custom_focus.saturating_sub(1)
                } else {
                    (self.custom_focus + 1).min(CUSTOM_FIELD_COUNT - 1)
                };
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

    pub fn toggle_selected(&mut self) {
        match self.step {
            Step::Packages => {
                if let Some(option) = self.packages.get_mut(self.package_cursor) {
                    option.selected = !option.selected;
                }
            }
            Step::Desktops => {
                if let Some(option) = self.desktops.get_mut(self.desktop_cursor) {
                    option.keep = !option.keep;
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
    pub fn rescan_isos(&mut self) {
        let (isos, scanned) = discover_isos();
        self.discovered_isos = isos;
        self.scanned_paths = scanned;
        self.iso_cursor = 0;
        self.iso_notice = None;
    }

    /// [f] on the ISO step: walk the whole filesystem for installer-sized
    /// ISOs on a background thread. Results come back through a channel that
    /// `poll_deep_scan` checks every frame, so the UI stays responsive.
    pub fn start_deep_scan(&mut self) {
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
    pub fn cancel_deep_scan(&mut self) {
        if let Some(cancel) = self.deep_scan_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.deep_scan_rx = None;
        self.iso_notice = Some("Deep scan cancelled.".to_string());
    }

    /// Non-blocking check for a finished deep scan; called every loop tick.
    pub fn poll_deep_scan(&mut self) {
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

    /// [Enter] on the ISO step: extract the SquashFS rootfs image into its
    /// workspace (`./iso-builder-work/<name>-<hash>/`) and detect the distro
    /// details - both on a background thread, because copying the image can
    /// still take a few seconds. The result arrives through a channel that
    /// `poll_iso_prep` checks every frame.
    pub fn start_iso_extraction(&mut self) {
        if self.iso_path.is_empty() || self.iso_prep_rx.is_some() {
            return;
        }

        // The workspace path is fixed by the ISO path; the worker creates
        // the folders. Switching to another ISO first cleans up the previous
        // fresh workspace so it does not linger on disk.
        let work_dir = work_dir_for(&self.iso_path);
        if let Some(previous) = self.active_work_dir.clone() {
            if previous != work_dir && self.should_delete_workspace() {
                cleanup_workspace(&previous);
            }
        }

        // Anything from an earlier run - the TUI's `rootfs.sfs` image or a
        // full extraction kept for run-builder.sh - must survive a crash or
        // an early quit, so only a fresh workspace is registered for cleanup
        // (which also covers a quit mid-extraction).
        let cached_image = cache_is_ready(&work_dir);
        self.workspace_reused = workspace_is_preexisting(&work_dir);
        self.work_dir = work_dir.clone();
        self.active_work_dir = Some(work_dir.clone());
        set_active_work_dir(if self.workspace_reused {
            None
        } else {
            Some(work_dir)
        });
        self.keep_workspace = false;
        self.delete_requested = false;

        self.disk_warning = if cached_image {
            None
        } else {
            disk_space_warning(&self.iso_path)
        };
        EXTRACTION_PHASE.store(PHASE_IDLE, Ordering::Relaxed);
        self.prep_started = Some(Instant::now());

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
        let message = if cached_image {
            "Using cached rootfs image..."
        } else {
            EXTRACTING_MESSAGE
        };
        self.iso_info_note = Some(message.to_string());
        self.iso_notice = Some(message.to_string());
    }

    /// Non-blocking check for a finished extraction/detection; called every
    /// loop tick. On success the detected desktop, package manager and distro
    /// name are stored and the wizard moves on to the Packages step; on
    /// failure the red error stays on the ISO step so the user can pick
    /// another image, retry, or skip ahead with [Esc].
    pub fn poll_iso_prep(&mut self) {
        let received = self.iso_prep_rx.as_ref().map(|rx| rx.try_recv());
        match received {
            Some(Ok(Ok(prep))) => {
                // Re-affirm the tracked workspace (the panic hook reads the
                // global) before storing what was detected.
                self.work_dir = prep.work_dir.clone();
                self.active_work_dir = Some(prep.work_dir.clone());
                set_active_work_dir(if self.workspace_reused {
                    None
                } else {
                    Some(prep.work_dir.clone())
                });
                self.desktop = prep.desktop.clone();
                self.chooser_path = prep.desktops.chooser_path.clone();
                self.netinstall_paths = prep.desktops.netinstall_paths.clone();
                self.desktops = prep
                    .desktops
                    .choices
                    .iter()
                    .map(|choice| DesktopOption {
                        id: choice.id.clone(),
                        name: choice.name.clone(),
                        keep: !self.preset_desktop_drops.contains(&choice.id),
                    })
                    .collect();
                self.desktop_cursor = 0;
                self.pkg_mgr = prep.pkg_mgr.clone();
                self.distro_name = prep.distro.clone();
                let distro_note = if prep.distro == "Unknown" {
                    String::new()
                } else {
                    format!(" - {}", prep.distro)
                };
                self.iso_info_note = Some(format!(
                    "Detected: {} / {}{}",
                    prep.desktop, prep.pkg_mgr, distro_note
                ));
                self.iso_notice = None;
                self.extraction_failed = false;
                self.iso_prep_rx = None;
                self.prep_started = None;
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
                self.prep_started = None;
            }
            // The worker is gone without sending; just clear the state.
            Some(Err(TryRecvError::Disconnected)) => {
                self.iso_prep_rx = None;
                self.prep_started = None;
            }
            // Still running (Empty) or no extraction at all (None).
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    /// [Esc] on the ISO step after a failed extraction: continue without
    /// detection. Package availability is then not checked, and the script
    /// detects the package manager itself when it runs.
    pub fn skip_failed_extraction(&mut self) {
        self.extraction_failed = false;
        self.next_step();
        self.iso_info_note =
            Some("Extraction skipped - distro and package manager are not checked.".to_string());
    }

    /// Finish screen: [K] keeps the cached rootfs image, [D] deletes it right
    /// away on exit.
    pub fn finish_session_choice(&mut self, keep: bool) {
        self.keep_workspace = keep;
        self.delete_requested = !keep;
        self.should_quit = true;
    }

    /// What [Enter] / [q] do on the finish screen: a workspace that already
    /// existed before this run is a cache the user chose to keep, so it stays
    /// by default; a fresh one is deleted unless the user asks for [K].
    pub fn default_keep(&self) -> bool {
        self.workspace_reused
    }

    /// [m] on the ISO step: switch between the auto list and manual entry.
    /// Switching back to Auto re-scans so a freshly plugged-in USB is found.
    pub fn toggle_iso_mode(&mut self) {
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
                self.is_typing = true;
            }
            IsoMode::Manual => {
                self.iso_mode = IsoMode::Auto;
                self.is_typing = false;
                self.rescan_isos();
            }
        }
    }

    /// Handles [Enter]: validates the current step and advances only if it
    /// passes. Runs for both typing and navigation mode, so [Enter] can never
    /// skip validation.
    pub fn submit_current_step(&mut self) {
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
                        // The wizard advances to the Packages step on its own
                        // once extraction and detection finish.
                        self.start_iso_extraction();
                    }
                    None => {
                        self.iso_mode = IsoMode::Manual;
                        self.is_typing = true;
                        self.iso_notice =
                            Some("No ISOs found. Please enter path manually.".to_string());
                    }
                },
                IsoMode::Manual => {
                    let raw = self.iso_input.trim().to_string();
                    if raw.is_empty() {
                        self.error_msg =
                            Some("Please enter the path to your Linux ISO file.".to_string());
                        return;
                    }
                    let path = normalize_user_path(&raw);
                    if !Path::new(&path).is_file() {
                        self.error_msg = Some(format!("File not found: {}", path));
                        return;
                    }
                    if self.iso_prep_rx.is_some() {
                        self.iso_notice = Some(EXTRACTION_RUNNING.to_string());
                        return;
                    }
                    // The script runs from another directory and its reuse
                    // check compares paths, so store the absolute path.
                    self.iso_path = fs::canonicalize(&path)
                        .map(|resolved| resolved.to_string_lossy().into_owned())
                        .unwrap_or(path);
                    self.iso_notice = None;
                    self.start_iso_extraction();
                }
            },
            Step::Custom => {
                if self.validate_custom() {
                    self.next_step();
                }
            }
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
                let keep = self.default_keep();
                self.finish_session_choice(keep);
            }
            _ => self.next_step(),
        }
    }

    /// Wizard order. The Desktops step only exists when the ISO's installer
    /// offered a desktop list.
    fn step_order(&self) -> Vec<Step> {
        let mut order = vec![Step::IsoPath, Step::Packages];
        if !self.desktops.is_empty() {
            order.push(Step::Desktops);
        }
        order.extend([
            Step::Tweaks,
            Step::Custom,
            Step::Confirm,
            Step::Preview,
            Step::Finished,
        ]);
        order
    }

    fn step_position(&self, order: &[Step]) -> usize {
        order.iter().position(|step| *step == self.step).unwrap_or(0)
    }

    pub fn next_step(&mut self) {
        self.error_msg = None;
        self.custom_invalid = None;
        self.status_msg = None;

        let order = self.step_order();
        let next = (self.step_position(&order) + 1).min(order.len() - 1);
        self.step = order[next];
        // The Custom step is all text fields, so start in edit mode.
        self.is_typing = self.step == Step::Custom;
    }

    pub fn prev_step(&mut self) {
        self.error_msg = None;
        self.custom_invalid = None;
        self.status_msg = None;
        // Leaving the preview/finish screens withdraws the "write the
        // script" confirmation; otherwise a later [q] would still write it.
        self.generate_script = false;

        let order = self.step_order();
        let previous = self.step_position(&order).saturating_sub(1);
        self.step = order[previous];
        self.is_typing = self.step == Step::Custom;
    }

    pub fn current_step_number(&self) -> (usize, usize) {
        let order = self.step_order();
        (self.step_position(&order) + 1, order.len())
    }
}

/// Normal-exit cleanup, called after the terminal is restored: deletes the
/// extracted workspace unless it should be kept (see
/// `App::should_delete_workspace`), then clears the global so a later panic
/// cannot delete again.
pub fn finish_session(app: &App) {
    if let Some(path) = app.active_work_dir.as_deref() {
        if app.should_delete_workspace() {
            cleanup_workspace(path);
        } else {
            println!("Keeping workspace: {}", path);
        }
    }
    set_active_work_dir(None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_iso(path: &str) -> App {
        let mut app = App::new(false);
        app.iso_path = path.to_string();
        app
    }

    #[test]
    fn preset_round_trip() {
        let mut app = App::new(false);
        app.apply_preset(
            "# comment\npackage=git\ntweak=Enable SSH\nhostname=box\ncustom_packages=foo bar\nunknown=1\npackage=does-not-exist\n",
        );
        assert_eq!(app.selected_packages(), vec!["git"]);
        assert_eq!(app.tweaks_summary(), "Enable SSH");
        assert_eq!(app.hostname(), "box");

        let saved = app.to_preset();
        assert!(saved.contains("package=git\n"));
        assert!(saved.contains("tweak=Enable SSH\n"));
        assert!(saved.contains("custom_packages=foo bar\n"));

        let mut again = App::new(false);
        again.apply_preset(&saved);
        assert_eq!(again.selected_packages(), vec!["git"]);
        assert_eq!(again.custom_package_list(), vec!["foo", "bar"]);
    }

    #[test]
    fn custom_fields_are_validated() {
        let mut app = app_with_iso("/tmp/x.iso");
        assert!(app.validate_custom());

        app.custom[FIELD_PACKAGES] = "--noconfirm".to_string();
        assert!(!app.validate_custom());
        assert_eq!(app.custom_invalid, Some(FIELD_PACKAGES));

        app.custom[FIELD_PACKAGES].clear();
        app.custom[FIELD_HOSTNAME] = "bad host".to_string();
        assert!(!app.validate_custom());
        assert_eq!(app.custom_invalid, Some(FIELD_HOSTNAME));
    }

    fn app_with_desktops() -> App {
        let mut app = app_with_iso("/tmp/x.iso");
        app.desktops = ["KDE-Desktop", "Hyprland"]
            .iter()
            .map(|id| DesktopOption {
                id: id.to_string(),
                name: id.to_string(),
                keep: true,
            })
            .collect();
        app
    }

    #[test]
    fn desktops_step_is_skipped_when_the_iso_offers_none() {
        let mut app = app_with_iso("/tmp/x.iso");
        app.step = Step::Packages;
        app.next_step();
        assert!(app.step == Step::Tweaks);
        assert_eq!(app.current_step_number(), (3, 7));
        app.prev_step();
        assert!(app.step == Step::Packages);
    }

    #[test]
    fn desktops_step_sits_between_packages_and_tweaks() {
        let mut app = app_with_desktops();
        app.step = Step::Packages;
        app.next_step();
        assert!(app.step == Step::Desktops);
        assert_eq!(app.current_step_number(), (3, 8));
        app.next_step();
        assert!(app.step == Step::Tweaks);
        app.prev_step();
        app.prev_step();
        assert!(app.step == Step::Packages);
    }

    #[test]
    fn unchecking_a_desktop_is_toggled_summarized_and_saved_in_presets() {
        let mut app = app_with_desktops();
        app.step = Step::Desktops;
        app.move_cursor(1);
        app.toggle_selected();
        assert!(!app.desktops[1].keep);
        assert_eq!(app.desktops_summary(), "Removing Hyprland");
        assert!(app.to_preset().contains("drop_desktop=Hyprland"));

        let mut fresh = app_with_desktops();
        fresh.apply_preset(&app.to_preset());
        assert!(fresh.desktops[0].keep);
        assert!(!fresh.desktops[1].keep);
    }
}
