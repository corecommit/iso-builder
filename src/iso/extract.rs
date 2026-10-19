//! Finding and extracting the rootfs image from an ISO.
//!
//! The TUI only needs the SquashFS image for its read-only inspection, so
//! instead of unpacking the whole ISO it asks `xorriso` for the `.sfs` /
//! `.squashfs` files and extracts exactly one of them to
//! `<work_dir>/rootfs.sfs`. `unsquashfs` then reads that image without ever
//! unpacking it (see `inspect.rs`).

use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

/// Shown when the ISO holds no SquashFS image at all (Fedora-style EROFS
/// images, for example, are not supported).
pub const NO_SQUASHFS_MESSAGE: &str =
    "Could not find a SquashFS image. This ISO may use EROFS (Fedora) or an unsupported format.";

/// PID of the extraction tool currently running, so the exit/panic cleanup
/// can stop it before deleting the workspace. 0 = nothing running.
static EXTRACTION_PID: AtomicU32 = AtomicU32::new(0);

/// Runs a tool without flooding the TUI or buffering potentially enormous
/// file-by-file output in RAM. Only stderr is captured because error reporting
/// uses it; stdout from xorriso is not consumed.
fn run_captured(command: &mut Command) -> io::Result<Output> {
    run_tracked(command, false)
}

/// Like [`run_captured`], but keeps stdout for parsing (`xorriso` listings
/// are small enough to hold in memory).
fn run_captured_stdout(command: &mut Command) -> io::Result<Output> {
    run_tracked(command, true)
}

fn run_tracked(command: &mut Command, capture_stdout: bool) -> io::Result<Output> {
    let stdout = if capture_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let child = command.stdout(stdout).stderr(Stdio::piped()).spawn()?;
    EXTRACTION_PID.store(child.id(), Ordering::Relaxed);
    let output = child.wait_with_output();
    EXTRACTION_PID.store(0, Ordering::Relaxed);
    output
}

/// Stops an extraction tool that may still be writing (otherwise the child
/// would keep writing into the folder after iso-builder already exited).
/// Best effort only; a 150 ms pause lets the process release its files.
pub fn stop_active_extraction() {
    let pid = EXTRACTION_PID.swap(0, Ordering::Relaxed);
    if pid == 0 {
        return;
    }
    let _ = Command::new("sh")
        .args(["-c", &format!("kill {} 2>/dev/null || true", pid)])
        .status();
    thread::sleep(Duration::from_millis(150));
}

/// Extracts the rootfs image of `iso_path` to `<work_dir>/rootfs.sfs`.
///
/// Only the largest `.sfs`/`.squashfs` file is copied out of the ISO - the
/// installer's system image (Ubuntu ships dozens of small language variants
/// next to the real one, so "first match" is not good enough).
pub fn extract_iso(iso_path: &str, work_dir: &str) -> Result<(), String> {
    if !Path::new(iso_path).is_file() {
        return Err(format!("ISO file not found: {}", iso_path));
    }

    let candidates = find_squashfs_images(iso_path)?;
    let squashfs = pick_largest(iso_path, &candidates)?;

    fs::create_dir_all(work_dir)
        .map_err(|err| format!("could not create {}: {}", work_dir, err))?;
    let destination = Path::new(work_dir).join("rootfs.sfs");
    // xorriso refuses to overwrite an existing file; leftovers from a
    // cancelled run must not block a fresh extraction.
    let _ = fs::remove_file(&destination);
    let destination = destination.to_string_lossy().into_owned();

    let output = run_captured(Command::new("xorriso").args([
        "-osirrox",
        "on",
        "-indev",
        iso_path,
        "-extract",
        squashfs.as_str(),
        destination.as_str(),
    ]))
    .map_err(|err| tool_start_error("xorriso", err))?;
    if !output.status.success() {
        return Err(command_error("xorriso", &output));
    }

    let extracted = fs::metadata(&destination)
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false);
    if extracted {
        Ok(())
    } else {
        Err(format!(
            "xorriso did not extract the rootfs image to {}",
            destination
        ))
    }
}

/// Runs the exact `xorriso -find` query from the task and returns the
/// `.sfs` / `.squashfs` paths it reports.
fn find_squashfs_images(iso_path: &str) -> Result<Vec<String>, String> {
    let output = run_captured_stdout(Command::new("xorriso").args([
        "-indev",
        iso_path,
        "-find",
        "/",
        "-name",
        "*.sfs",
        "-o",
        "-name",
        "*.squashfs",
    ]))
    .map_err(|err| tool_start_error("xorriso", err))?;
    if !output.status.success() {
        return Err(command_error("xorriso", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let candidates = parse_find_paths(&stdout);
    if candidates.is_empty() {
        return Err(NO_SQUASHFS_MESSAGE.to_string());
    }
    Ok(candidates)
}

/// Picks the largest of several images by asking xorriso for a long listing
/// (file sizes) once. With a single candidate no extra process is needed.
fn pick_largest(iso_path: &str, candidates: &[String]) -> Result<String, String> {
    if candidates.len() == 1 {
        return Ok(candidates[0].clone());
    }

    let output = run_captured_stdout(Command::new("xorriso").args([
        "-indev",
        iso_path,
        "-find",
        "/",
        "-name",
        "*.sfs",
        "-o",
        "-name",
        "*.squashfs",
        "-exec",
        "lsdl",
    ]))
    .map_err(|err| tool_start_error("xorriso", err))?;
    if !output.status.success() {
        return Err(command_error("xorriso", &output));
    }

    let listing = String::from_utf8_lossy(&output.stdout);
    largest_candidate(candidates, &parse_lsdl_sizes(&listing))
        .ok_or_else(|| "could not determine the largest SquashFS image".to_string())
}

/// Paths reported by `xorriso -find`, one per line and single-quoted when
/// they contain special characters. Non-path lines (xorriso notes) and
/// anything that is not a SquashFS image are ignored.
fn parse_find_paths(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let path = line
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
                .map(|inner| inner.replace("''", "'"))
                .unwrap_or_else(|| line.to_string());
            let lower = path.to_ascii_lowercase();
            (path.starts_with('/') && (lower.ends_with(".sfs") || lower.ends_with(".squashfs")))
                .then_some(path)
        })
        .collect()
}

/// `size` and `path` from `xorriso -exec lsdl` lines:
/// `-r--r--r--    1 0        0        3432136704 Aug 26 06:21 '/casper/minimal.squashfs'`
fn parse_lsdl_sizes(stdout: &str) -> Vec<(String, u64)> {
    stdout.lines().filter_map(parse_lsdl_line).collect()
}

fn parse_lsdl_line(line: &str) -> Option<(String, u64)> {
    let (metadata, quoted) = line.split_once('\'')?;
    let path = quoted.strip_suffix('\'')?;
    let size = metadata.split_whitespace().nth(4)?.parse::<u64>().ok()?;
    (!path.is_empty()).then(|| (path.to_string(), size))
}

/// Largest path that appears both in `candidates` and in the size listing.
fn largest_candidate(candidates: &[String], sizes: &[(String, u64)]) -> Option<String> {
    candidates
        .iter()
        .filter_map(|candidate| {
            sizes
                .iter()
                .find(|(path, _)| path == candidate)
                .map(|(_, size)| (candidate, *size))
        })
        .max_by_key(|(_, size)| *size)
        .map(|(candidate, _)| candidate.clone())
}

/// Human-readable start failure for an external tool.
fn tool_start_error(tool: &str, err: io::Error) -> String {
    if err.kind() == io::ErrorKind::NotFound {
        format!("{} is not installed (install xorriso)", tool)
    } else {
        format!("could not run {}: {}", tool, err)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_output_is_unquoted_and_filtered() {
        let stdout = "\
xorriso : NOTE : Loading ISO image tree from LBA 0
'/arch/x86_64/airootfs.sfs'
'/casper/my rootfs.squashfs'
not a path
";
        assert_eq!(
            parse_find_paths(stdout),
            vec![
                "/arch/x86_64/airootfs.sfs".to_string(),
                "/casper/my rootfs.squashfs".to_string(),
            ]
        );
    }

    #[test]
    fn largest_image_wins_over_ubuntu_language_variants() {
        let listing = "\
-r--r--r--    1 0        0         2969600 Aug 26 06:26 '/casper/minimal.de.squashfs'
-r--r--r--    1 0        0      3432136704 Aug 26 06:21 '/casper/minimal.squashfs'
-r--r--r--    1 0        0       590053376 Aug 26 06:21 '/casper/minimal.standard.squashfs'
";
        let candidates = vec![
            "/casper/minimal.de.squashfs".to_string(),
            "/casper/minimal.standard.squashfs".to_string(),
            "/casper/minimal.squashfs".to_string(),
        ];
        let sizes = parse_lsdl_sizes(listing);
        assert_eq!(
            largest_candidate(&candidates, &sizes),
            Some("/casper/minimal.squashfs".to_string())
        );
    }
}
