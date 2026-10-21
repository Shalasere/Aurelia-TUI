//! The launch-targets overlay ("Play as…", modal popup): every way an installed
//! game can be started — the native Linux build, or the Windows build under each
//! Steam mode — from `aurelia launch-targets`. Enter launches the highlighted one
//! for this run only, without changing the saved per-game config.

use tui::text::{Span, Spans};
use tui::widgets::Paragraph;

use crate::browse::Browser;
use crate::theme;
use crate::ui::{paneled_paragraph, selection_marker};

/// Build the launch-targets overlay from the browser's open state. Each row is a
/// target label, tagged `default` (what plain `play` picks) or with what is
/// missing when it is unavailable. A status/hint line closes it.
pub fn launch_targets(browser: &Browser) -> Paragraph<'static> {
    let mut lines: Vec<Spans<'static>> = Vec::new();

    if browser.launch_targets.is_empty() {
        lines.push(Spans::from(Span::styled(
            "No launch targets.".to_string(),
            theme::dim(),
        )));
    }

    for (i, target) in browser.launch_targets.iter().enumerate() {
        let selected = i == browser.launch_targets_index;
        let style = if selected {
            theme::selection(theme::ACCENT)
        } else if target.available {
            theme::value()
        } else {
            theme::dim()
        };
        let mut spans = vec![
            Span::styled(selection_marker(selected).to_string(), style),
            Span::styled(target.label(), style),
        ];
        if target.default {
            spans.push(Span::styled("  [default]".to_string(), theme::accent()));
        }
        if !target.available {
            let missing = target.missing.as_deref().unwrap_or("unavailable");
            spans.push(Span::styled(format!("  (missing: {missing})"), theme::dim()));
        }
        lines.push(Spans::from(spans));
    }

    lines.push(Spans::from(""));
    let status = if !browser.launch_targets_status.is_empty() {
        browser.launch_targets_status.clone()
    } else {
        "[Enter] launch  [Esc] close".to_string()
    };
    lines.push(Spans::from(Span::styled(status, theme::dim())));

    paneled_paragraph(lines, "Play as…".to_string())
}
