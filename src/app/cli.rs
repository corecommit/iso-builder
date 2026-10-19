//! Command line parsing and the headless (--yes / --dry-run) mode.

use std::env;
use std::fs;
use std::io;

use crate::app::state::App;
use crate::commands::script::{build_script, write_script};
use crate::iso::workspace::work_dir_for;
use crate::utils::normalize_user_path;

pub const USAGE: &str = "\
iso-builder - build a reviewed remaster script for a Linux ISO

USAGE:
    iso-builder [OPTIONS]

OPTIONS:
        --preset FILE   Load choices from a preset (press [s] on the review screen to save one)
        --iso PATH      Source ISO (pre-fills the manual path field)
        --output NAME   File name of the remastered ISO
        --dry-run       Print the generated script to stdout and exit (no TUI)
    -y, --yes           Write run-builder.sh without the TUI and exit
    -h, --help          Show this help

The script it writes must be run as root:  sudo ./run-builder.sh";

pub struct Cli {
    pub preset: Option<String>,
    pub iso: Option<String>,
    pub output: Option<String>,
    pub yes: bool,
    pub dry_run: bool,
    pub help: bool,
}

pub fn parse_cli() -> Result<Cli, String> {
    let mut cli = Cli {
        preset: None,
        iso: None,
        output: None,
        yes: false,
        dry_run: false,
        help: false,
    };
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => cli.help = true,
            "-y" | "--yes" => cli.yes = true,
            "--dry-run" => cli.dry_run = true,
            "--preset" => cli.preset = Some(args.next().ok_or("--preset needs a file name")?),
            "--iso" => cli.iso = Some(args.next().ok_or("--iso needs a path")?),
            "--output" => cli.output = Some(args.next().ok_or("--output needs a file name")?),
            other => return Err(format!("unknown option '{}' (try --help)", other)),
        }
    }
    Ok(cli)
}

/// `--yes` / `--dry-run`: build the script from the preset and flags alone,
/// without the TUI and without extracting anything (the script detects the
/// package manager itself when it runs).
pub fn run_headless(mut app: App, dry_run: bool) -> io::Result<()> {
    let raw = app.iso_input.trim().to_string();
    if raw.is_empty() {
        eprintln!(
            "iso-builder: --yes and --dry-run need an ISO (--iso PATH, or iso=PATH in the preset)"
        );
        std::process::exit(2);
    }
    let path = normalize_user_path(&raw);
    app.iso_path = match fs::canonicalize(&path) {
        Ok(resolved) => resolved.to_string_lossy().into_owned(),
        Err(err) => {
            eprintln!("iso-builder: cannot open {}: {}", path, err);
            std::process::exit(2);
        }
    };
    app.work_dir = work_dir_for(&app.iso_path);
    if !app.validate_custom() {
        eprintln!("iso-builder: {}", app.error_msg.clone().unwrap_or_default());
        std::process::exit(2);
    }

    app.generated_script = build_script(&app);
    if dry_run {
        print!("{}", app.generated_script);
        Ok(())
    } else {
        write_script(&app)
    }
}
