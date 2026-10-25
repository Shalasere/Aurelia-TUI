//! Compact completion-time rows for the detail pane.
use crate::interface::hltb::HltbState;

fn duration(seconds: Option<u64>) -> String {
    let Some(seconds) = seconds.filter(|s| *s > 0) else {
        return "—".into();
    };
    let minutes = (seconds.saturating_add(30) / 60).max(1);
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

pub(super) fn lines(state: &HltbState, width: usize) -> Vec<String> {
    let text: &str = match state {
        HltbState::Ready { data, stale } => {
            let mut parts = vec![
                format!("Main Story {}", duration(data.main)),
                format!("Main + Extras {}", duration(data.extra)),
                format!("Completionist {}", duration(data.completionist)),
            ];
            if *stale {
                parts.insert(0, "Cached".into());
            }
            let mut lines = Vec::new();
            let mut line = String::new();
            for part in parts {
                let candidate = if line.is_empty() {
                    part.clone()
                } else {
                    format!("{line} · {part}")
                };
                if candidate.chars().count() <= width.max(1) {
                    line = candidate;
                } else {
                    if !line.is_empty() {
                        lines.push(std::mem::take(&mut line));
                    }
                    let mut wrapped = super::detail::wrap(&part, width.max(1));
                    line = wrapped.pop().unwrap_or_default();
                    lines.extend(wrapped);
                }
            }
            if !line.is_empty() {
                lines.push(line);
            }
            return lines;
        }
        HltbState::Pending => "Waiting for selection…",
        HltbState::Loading => "Loading…",
        HltbState::NotFound => "No verified match",
        HltbState::Unavailable(_) => "Unavailable; r retries",
    };
    super::detail::wrap(text, width.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_short_times_are_distinct() {
        assert_eq!(duration(None), "—");
        assert_eq!(duration(Some(0)), "—");
        assert_eq!(duration(Some(1)), "1m");
        assert_eq!(duration(Some(3600)), "1h");
        assert_eq!(duration(Some(11230)), "3h 7m");
    }
}
