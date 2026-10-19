//! Screen layout and the step-specific panes.

use std::sync::atomic::Ordering;
use std::time::Duration;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::state::{App, IsoMode, Step, FIELD_PACKAGES};
use crate::commands::script::SCRIPT_FILE;
use crate::iso::discovery::IsoEntry;
use crate::iso::{phase_label, EXTRACTION_PHASE};
use crate::ui::widgets::{draw_checklist, draw_field, draw_input, CheckRow};
use crate::utils::human_size;

pub fn ui(frame: &mut Frame, app: &App) {
    let terminal_size = frame.size();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(terminal_size);

    let (current, total) = app.current_step_number();

    let step_name = match app.step {
        Step::IsoPath => "Original ISO File",
        Step::Packages => "Extra Packages",
        Step::Tweaks => "System Tweaks",
        Step::Custom => "Custom Additions",
        Step::Confirm => "Review & Generate",
        Step::Preview => "Script Preview & Debug",
        Step::Finished => "Finish & Cleanup",
    };

    let title = Paragraph::new(format!(
        " ISO-BUILDER | Step {} of {}: {} ",
        current, total, step_name
    ))
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);

    match app.step {
        Step::IsoPath => match app.iso_mode {
            IsoMode::Auto => draw_auto_view(frame, chunks[1], app),
            IsoMode::Manual => {
                draw_input(
                    frame,
                    chunks[1],
                    " Path to your .iso file ([Tab] completes, ~ and quotes are fine) ",
                    &app.iso_input,
                    app.is_typing,
                    app.error_msg.as_deref(),
                    app.iso_notice.as_deref(),
                );
                if app.iso_prep_rx.is_some() {
                    draw_extraction_status(frame, chunks[1], app);
                }
            }
        },
        Step::Packages => {
            let rows: Vec<CheckRow> = app
                .packages
                .iter()
                .map(|p| {
                    // Once the package manager is known, flag entries that
                    // have no package for it (the script would skip them).
                    let unavailable = app.pkg_mgr != "Unknown" && p.id_for(&app.pkg_mgr).is_none();
                    let note = if unavailable {
                        format!("  [n/a on {}]", app.pkg_mgr)
                    } else {
                        String::new()
                    };
                    CheckRow {
                        text: format!("{:<14} - {} ({}){}", p.name, p.desc, p.size, note),
                        selected: p.selected,
                    }
                })
                .collect();
            let detected = match app.iso_info_note.as_deref() {
                Some(note) => format!(" | {}", note),
                None => String::new(),
            };
            let title = format!(
                " Extra Packages - {} selected{} ",
                app.selected_packages().len(),
                detected
            );
            draw_checklist(frame, chunks[1], &title, &rows, app.package_cursor);
        }
        Step::Tweaks => {
            let rows: Vec<CheckRow> = app
                .tweaks
                .iter()
                .map(|t| CheckRow {
                    text: format!("{:<24} - {}", t.name, t.desc),
                    selected: t.selected,
                })
                .collect();
            let title = format!(" System Tweaks - {} selected ", app.selected_tweaks().len());
            draw_checklist(frame, chunks[1], &title, &rows, app.tweak_cursor);
        }
        Step::Custom => {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Min(1),
                ])
                .split(chunks[1]);
            let titles = [
                " Extra packages (space separated, names for the target's package manager) ",
                " Custom command (runs inside the chroot with sh) ",
                " Hostname (optional) ",
                " Output ISO name (optional, default: <source>-custom.iso) ",
            ];
            for (index, field_title) in titles.iter().enumerate() {
                draw_field(
                    frame,
                    rows[index],
                    field_title,
                    &app.custom[index],
                    app.custom_focus == index,
                    app.is_typing,
                    app.custom_invalid == Some(index),
                );
            }
            if let Some(message) = app.error_msg.as_deref() {
                frame.render_widget(
                    Paragraph::new(format!("[WARN] {}", message))
                        .style(Style::default().fg(Color::Red))
                        .wrap(Wrap { trim: true }),
                    rows[4],
                );
            }
        }
        Step::Confirm => {
            let tools: Vec<String> = app
                .tools
                .iter()
                .map(|(name, found)| {
                    if *found {
                        (*name).to_string()
                    } else {
                        format!("{} (MISSING)", name)
                    }
                })
                .collect();
            let or_none = |text: &str| {
                if text.is_empty() {
                    "None".to_string()
                } else {
                    text.to_string()
                }
            };
            let mut summary = format!(
                "Original ISO: {}\nDetected: {} | {} | {}\nPackages: {}\nSystem Tweaks: {}\nExtra packages: {}\nCustom command: {}\nHostname: {} | Output ISO: {}\nTools: {}\n",
                app.iso_path,
                app.distro_name,
                app.desktop,
                app.pkg_mgr,
                app.packages_summary(),
                app.tweaks_summary(),
                or_none(&app.custom[FIELD_PACKAGES]),
                or_none(app.custom_command()),
                or_none(app.hostname()),
                app.output_name(),
                tools.join(", ")
            );
            if let Some(status) = app.status_msg.as_deref() {
                summary.push_str(&format!("\n{}\n", status));
            }
            summary.push_str(
                "\n[Enter] build the script for review | [s] save these choices as a preset",
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
                "Packages to install: {}\nTweaks to apply: {}\nDistro / package manager: {} / {}\nCached rootfs image: {}\nNote: the script re-detects the package manager itself and skips packages that do not exist for it.",
                app.packages_summary(),
                app.tweaks_summary(),
                app.distro_name,
                app.pkg_mgr,
                workspace
            );
            frame.render_widget(
                Paragraph::new(debug).wrap(Wrap { trim: true }).block(
                    Block::default()
                        .title(" Debug ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Yellow)),
                ),
                panes[1],
            );
        }
        Step::Finished => {
            let workspace = app
                .active_work_dir
                .as_deref()
                .unwrap_or("(no cached image)");
            let advice = if app.default_keep() {
                "This rootfs image was cached by an earlier run. run-builder.sh still extracts the full ISO itself; keeping the cache only makes the next iso-builder start instant.\n\n  [K] / [Enter]  Keep cached rootfs image (default)\n  [D]            Delete it now"
            } else {
                "run-builder.sh always extracts the full ISO itself. Keeping the cached rootfs image only makes the next iso-builder start instant.\n\n  [D] / [Enter]  Delete cached rootfs image (default)\n  [K]            Keep it for a faster next start"
            };
            let summary = format!(
                "[OK] {} is built and will be written when iso-builder exits.\n\nNext step:  sudo ./{}\nOutput ISO: {}\n\nCached rootfs image:\n  {}\n\n{}\n\n[Esc] goes back.",
                SCRIPT_FILE,
                SCRIPT_FILE,
                app.output_name(),
                workspace,
                advice
            );
            frame.render_widget(
                Paragraph::new(summary).wrap(Wrap { trim: true }).block(
                    Block::default()
                        .title(" Finish & Cleanup ")
                        .borders(Borders::ALL),
                ),
                chunks[1],
            );
        }
    }

    let footer_text = if app.is_typing {
        match app.step {
            Step::Custom => {
                " [Typing] [Tab/Up/Down] switch field | [Enter] continue | [Esc] stop editing | [Ctrl+U] clear "
            }
            Step::IsoPath => {
                " [Typing] [Enter] submit | [Tab] complete path | [Esc] stop editing | [Ctrl+U] clear | [Ctrl+C] quit "
            }
            _ => " [Typing] [Enter] submit | [Esc] stop editing | [Ctrl+U] clear | [Ctrl+C] quit ",
        }
    } else {
        match app.step {
            Step::Confirm => {
                " [Enter] Build & Preview | [s] Save preset | [Esc] back | [q]/[Ctrl+C] quit "
            }
            Step::Packages | Step::Tweaks => {
                " [j/k or Up/Down] move | [Space] toggle | [Enter] continue | [Esc] back | [q] quit "
            }
            Step::Custom => {
                " [i] edit | [Tab/j/k] switch field | [Enter] continue | [Esc] back | [q] quit "
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
            Step::Preview => " [j/k] Scroll | [Enter] Continue | [Esc] Back | [q] Quit ",
            Step::Finished => {
                if app.default_keep() {
                    " [K]/[Enter] Keep cached rootfs (default) | [D] Delete | [Esc] Back "
                } else {
                    " [D]/[Enter] Delete cached rootfs (default) | [K] Keep | [Esc] Back "
                }
            }
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
            Line::from("No ISOs found automatically.").style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
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
        draw_extraction_status(frame, area, app);
    } else if let Some(message) = app.error_msg.as_deref() {
        draw_error_overlay(frame, area, message);
    }
}

/// The " Scan Report " panel: one line per scanned folder. Failures are
/// flagged with `[WARN]` and highlighted in red.
fn draw_scan_report(frame: &mut Frame, area: Rect, scanned_paths: &[String]) {
    let lines: Vec<Line> = if scanned_paths.is_empty() {
        vec![Line::from("No folders were scanned yet.").style(Style::default().fg(Color::DarkGray))]
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
    let height = (area.height.saturating_sub(2))
        .clamp(4, 14)
        .min(area.height);
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
        Line::from("[SCAN] Deep scanning for ISOs > 500MB...").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
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

/// Loading popup for the running extraction, fed from the app state.
fn draw_extraction_status(frame: &mut Frame, area: Rect, app: &App) {
    let elapsed = app
        .prep_started
        .map(|started| started.elapsed())
        .unwrap_or_default();
    let phase = if app.workspace_reused {
        "Using cached rootfs image..."
    } else {
        phase_label(EXTRACTION_PHASE.load(Ordering::Relaxed))
    };
    draw_extraction_overlay(frame, area, phase, elapsed, app.disk_warning.as_deref());
}

/// Centered loading popup shown over the ISO step while the rootfs image is
/// copied out of the ISO and inspected (xorriso + unsquashfs). It shows the
/// current phase, a spinner and the elapsed time, so even a short copy never
/// looks like a hang.
fn draw_extraction_overlay(
    frame: &mut Frame,
    area: Rect,
    phase: &str,
    elapsed: Duration,
    warning: Option<&str>,
) {
    let width = (area.width.saturating_sub(4)).clamp(20, 72).min(area.width);
    let height = (area.height.saturating_sub(2)).clamp(5, 8).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];
    let spinner = SPINNER[(elapsed.as_millis() / 150) as usize % SPINNER.len()];
    let seconds = elapsed.as_secs();

    let mut message = vec![
        Line::from(""),
        Line::from(format!(
            "{} {}  ({:02}:{:02})",
            spinner,
            phase,
            seconds / 60,
            seconds % 60
        ))
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from("This usually takes a few seconds. [q] quits and cleans up.")
            .style(Style::default().fg(Color::DarkGray)),
    ];
    if let Some(text) = warning {
        message.push(Line::from(format!("[WARN] {}", text)).style(Style::default().fg(Color::Red)));
    }
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title(" Preparing ISO ")
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
