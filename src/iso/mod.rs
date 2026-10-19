//! ISO preparation: discovery, lightweight extraction, inspection and the
//! workspace.
//!
//! `prepare_iso` is the single orchestration point the TUI calls: a cached
//! `rootfs.sfs` is reused as-is, otherwise xorriso copies just the SquashFS
//! image out of the ISO, and then `unsquashfs` reads distro details directly
//! from that image without ever unpacking it.

pub mod discovery;
pub mod extract;
pub mod inspect;
pub mod workspace;

use std::fs;
use std::sync::atomic::{AtomicU8, Ordering};

pub use discovery::{deep_scan_worker, discover_isos, IsoEntry};
pub use inspect::{
    desktop_environment_in, distro_name_from, parse_distro_name, parse_pkg_mgr_from_listing,
    squashfs_listing,
};
pub use workspace::{
    cache_is_ready, cleanup_active_workspace, cleanup_workspace, disk_space_warning,
    set_active_work_dir, work_dir_for, workspace_has_mounts, workspace_is_preexisting,
    EXTRACTING_MESSAGE, EXTRACTION_RUNNING, WORK_ROOT,
};

use extract::extract_iso;

/// What the extraction thread is doing right now, for the loading popup.
pub static EXTRACTION_PHASE: AtomicU8 = AtomicU8::new(PHASE_IDLE);
pub const PHASE_IDLE: u8 = 0;
pub const PHASE_EXTRACT: u8 = 1;
pub const PHASE_DETECT: u8 = 2;

pub fn phase_label(phase: u8) -> &'static str {
    match phase {
        PHASE_EXTRACT => "Extracting rootfs image...",
        PHASE_DETECT => "Detecting distro, package manager and desktop",
        _ => "Starting",
    }
}

/// Everything the TUI learns while preparing an ISO.
pub struct IsoPrep {
    /// `./iso-builder-work/<name>-<hash>`.
    pub work_dir: String,
    /// From `etc/os-release` inside the image, e.g. "CachyOS Linux".
    pub distro: String,
    /// Binary found in the image (`pacman`, `apt`, ...) or "Unknown".
    pub pkg_mgr: String,
    /// Desktop session file found in the image or "Unknown".
    pub desktop: String,
    /// True when the cached `rootfs.sfs` from an earlier run was reused.
    pub reused: bool,
}

/// Prepares `iso_path` for the wizard: extracts only its SquashFS rootfs
/// image into the workspace (unless a cache hit is there already) and then
/// reads that image with read-only `unsquashfs` commands.
pub fn prepare_iso(iso_path: &str) -> Result<IsoPrep, String> {
    let work_dir = work_dir_for(iso_path);
    prepare_iso_with(iso_path, &work_dir, extract_iso)
}

/// Cache-aware core of [`prepare_iso`] with the extraction step injected, so
/// tests can prove that a second run does not extract again.
fn prepare_iso_with<F>(iso_path: &str, work_dir: &str, mut extract: F) -> Result<IsoPrep, String>
where
    F: FnMut(&str, &str) -> Result<(), String>,
{
    let reused = cache_is_ready(work_dir);
    if !reused {
        // Never delete through a filesystem a previous build left mounted.
        if workspace_has_mounts(work_dir) {
            return Err(format!(
                "{} still has filesystems mounted from an earlier build. Unmount them first: sudo umount -R {}",
                work_dir, work_dir
            ));
        }
        // Start from a clean slate: a cancelled run leaves a partial image.
        // A full extraction made by run-builder.sh is preserved instead.
        if !workspace::script_cache_ready(work_dir) {
            let _ = fs::remove_dir_all(work_dir);
        }
        fs::create_dir_all(work_dir)
            .map_err(|err| format!("could not create {}: {}", work_dir, err))?;

        EXTRACTION_PHASE.store(PHASE_EXTRACT, Ordering::Relaxed);
        extract(iso_path, work_dir)?;

        if !cache_is_ready(work_dir) {
            return Err(format!(
                "no rootfs image was written to {}/rootfs.sfs",
                work_dir
            ));
        }
    }

    EXTRACTION_PHASE.store(PHASE_DETECT, Ordering::Relaxed);
    let rootfs_image = format!("{}/rootfs.sfs", work_dir);
    let distro = distro_name_from(&rootfs_image);
    // One listing feeds both parsers; a broken image just yields "Unknown".
    let listing = squashfs_listing(&rootfs_image).unwrap_or_default();
    Ok(IsoPrep {
        work_dir: work_dir.to_string(),
        distro,
        pkg_mgr: parse_pkg_mgr_from_listing(&listing).unwrap_or_else(|| "Unknown".to_string()),
        desktop: desktop_environment_in(&listing)
            .unwrap_or("Unknown")
            .to_string(),
        reused,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::Path;

    #[test]
    fn prepare_iso_extracts_the_rootfs_image_only_once() {
        let temp =
            std::env::temp_dir().join(format!("iso-builder-prepare-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let iso = temp.join("fake.iso");
        fs::write(&iso, b"not a real ISO").unwrap();
        let work = temp.join("work");
        let iso = iso.to_str().unwrap().to_string();
        let work = work.to_str().unwrap().to_string();

        let extractions = Cell::new(0);
        let mut extract = |_iso: &str, dir: &str| {
            extractions.set(extractions.get() + 1);
            fs::create_dir_all(dir).map_err(|err| err.to_string())?;
            fs::write(Path::new(dir).join("rootfs.sfs"), b"pretend squashfs")
                .map_err(|err| err.to_string())
        };

        let first = prepare_iso_with(&iso, &work, &mut extract).unwrap();
        assert!(!first.reused, "the first run must extract");
        assert_eq!(extractions.get(), 1);

        // A fake image makes detection degrade to "Unknown" instead of
        // failing; the point here is only that extraction is skipped.
        let second = prepare_iso_with(&iso, &work, &mut extract).unwrap();
        assert!(second.reused, "the second run must use the cache");
        assert_eq!(extractions.get(), 1, "the .sfs must not be extracted twice");
        assert!(cache_is_ready(&work));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn prepare_keeps_a_full_extraction_made_by_the_generated_script() {
        let temp = std::env::temp_dir().join(format!(
            "iso-builder-prepare-script-cache-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp);
        let work = temp.join("work");
        fs::create_dir_all(work.join("rootfs")).unwrap();
        fs::create_dir_all(work.join("iso-root/images")).unwrap();
        fs::write(
            work.join(".extracted"),
            "iso-root/images/airootfs.sfs\nroot\n",
        )
        .unwrap();
        fs::write(work.join("rootfs/sentinel"), b"keep me").unwrap();

        let work_str = work.to_str().unwrap().to_string();
        let mut extract = |_iso: &str, dir: &str| {
            fs::write(Path::new(dir).join("rootfs.sfs"), b"pretend squashfs")
                .map_err(|err| err.to_string())
        };
        let prep = prepare_iso_with("/does/not/matter.iso", &work_str, &mut extract).unwrap();

        assert!(!prep.reused, "only the .sfs extraction is cached");
        assert!(work.join("rootfs/sentinel").is_file());
        assert!(work.join("iso-root/images").is_dir());
        assert!(cache_is_ready(&work_str));

        let _ = fs::remove_dir_all(&temp);
    }
}
