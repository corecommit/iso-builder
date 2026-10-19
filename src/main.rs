//! iso-builder: a terminal wizard for customizing Linux installer ISOs.
//!
//! This binary is only the CLI entry point; the implementation lives in the
//! library modules (`app`, `iso`, `commands`, `ui`, ...).

use std::fs;
use std::io;
use std::panic;

use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};

use iso_builder::app::cli::{parse_cli, run_headless, USAGE};
use iso_builder::app::state::{finish_session, App, IsoMode, FIELD_OUTPUT};
use iso_builder::app::update::run_tui;
use iso_builder::commands::script::write_script;
use iso_builder::iso::workspace::cleanup_active_workspace;

fn main() -> Result<(), io::Error> {
    let cli = match parse_cli() {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("iso-builder: {}", message);
            std::process::exit(2);
        }
    };
    if cli.help {
        println!("{}", USAGE);
        return Ok(());
    }

    let headless = cli.yes || cli.dry_run;
    let mut app = App::new(!headless);
    if let Some(preset) = cli.preset.as_deref() {
        match fs::read_to_string(preset) {
            Ok(text) => app.apply_preset(&text),
            Err(err) => {
                eprintln!("iso-builder: cannot read preset {}: {}", preset, err);
                std::process::exit(2);
            }
        }
    }
    if let Some(output) = cli.output {
        app.custom[FIELD_OUTPUT] = output;
    }
    if let Some(iso) = cli.iso {
        app.iso_input = iso;
        app.iso_mode = IsoMode::Manual;
    }
    if headless {
        return run_headless(app, cli.dry_run);
    }

    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // Restore the terminal first so the cleanup messages are readable,
        // then delete whatever extraction workspace was active.
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        cleanup_active_workspace();
        original_hook(panic_info);
    }));

    let tui_result = run_tui(&mut app);

    let script_result = if tui_result.is_ok() && app.generate_script {
        write_script(&app)
    } else {
        Ok(())
    };

    // Normal-exit cleanup: see `App::should_delete_workspace`. Happens after
    // the terminal is back to normal text mode.
    finish_session(&app);

    tui_result?;
    script_result?;
    Ok(())
}
