//! Terminal lifecycle and the keyboard event loop.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;

use crate::app::state::{App, IsoMode, Step};
use crate::ui::draw::ui;

/// Owns the terminal lifecycle: raw mode and the alternate screen are always
/// restored, even when the event loop or a draw returns an error.
pub fn run_tui(app: &mut App) -> io::Result<()> {
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

        let control = key.modifiers.contains(KeyModifiers::CONTROL);

        if app.is_typing {
            match key.code {
                KeyCode::Esc => app.is_typing = false,
                KeyCode::Enter => app.submit_current_step(),
                KeyCode::Tab => match app.step {
                    Step::Custom => app.cycle_custom_focus(false),
                    Step::IsoPath => app.complete_iso_path(),
                    _ => {}
                },
                KeyCode::BackTab | KeyCode::Up => {
                    if app.step == Step::Custom {
                        app.cycle_custom_focus(true);
                    }
                }
                KeyCode::Down => {
                    if app.step == Step::Custom {
                        app.cycle_custom_focus(false);
                    }
                }
                KeyCode::Char('u') if control => {
                    if let Some(field) = app.editable_field_mut() {
                        field.clear();
                    }
                }
                // Plain characters only: Ctrl+<key> must never insert letters.
                KeyCode::Char(c) if !control => {
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
            // The footer shows capital letters ([K], [D], [Q]); accept both
            // cases so Shift or Caps Lock never turns a shortcut into a no-op.
            let code = match key.code {
                KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
                other => other,
            };
            match code {
                KeyCode::Char('q') => {
                    if app.step == Step::Finished {
                        let keep = app.default_keep();
                        app.finish_session_choice(keep);
                    } else {
                        app.should_quit = true;
                    }
                }
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
                KeyCode::Tab => {
                    if app.step == Step::Custom {
                        app.cycle_custom_focus(false);
                    }
                }
                KeyCode::BackTab => {
                    if app.step == Step::Custom {
                        app.cycle_custom_focus(true);
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
                KeyCode::Char('s') => {
                    if app.step == Step::Confirm {
                        app.save_preset();
                    }
                }
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
