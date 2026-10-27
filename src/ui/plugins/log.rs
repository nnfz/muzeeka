//! Recent lines from `log.info` / `log.error`, shown on the plugin card.
//! The same line is kept once: turning a plugin off and on used to stack
//! its startup note ("папка osu!lazer: …") under the previous copy.

use std::collections::{HashSet, VecDeque};

use parking_lot::Mutex;

const LIMIT: usize = 40;
const SHOWN: usize = 8;

struct Line {
    plugin_id: String,
    level: &'static str,
    message: String,
}

static LINES: Mutex<VecDeque<Line>> = Mutex::new(VecDeque::new());

pub fn push(level: &'static str, plugin_id: &str, message: &str) {
    let message = message.trim();
    if message.is_empty() {
        return;
    }
    let mut lines = LINES.lock();
    let repeated = lines.iter().rev().find(|line| line.plugin_id == plugin_id).is_some_and(
        |line| line.level == level && line.message == message,
    );
    if repeated {
        return;
    }
    eprintln!("[plugin {plugin_id}] {level}: {message}");
    lines.push_back(Line {
        plugin_id: plugin_id.to_string(),
        level,
        message: message.to_string(),
    });
    while lines.len() > LIMIT * 4 {
        lines.pop_front();
    }
}

/// Newest distinct lines, oldest first. A repeated startup note stays on the
/// row of its latest copy instead of appearing again underneath.
pub fn recent(plugin_id: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut picked = Vec::new();
    for line in LINES.lock().iter().rev() {
        if line.plugin_id != plugin_id {
            continue;
        }
        let text = format!("{}: {}", line.level, line.message);
        if !seen.insert(text.clone()) {
            continue;
        }
        picked.push(text);
        if picked.len() == SHOWN {
            break;
        }
    }
    picked.reverse();
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_startup_note_is_shown_once_after_the_plugin_starts_again() {
        let id = "test.osu-log-dedup";
        push("info", id, "папка osu!lazer: C:\\osu");
        push("info", id, "в картах 193 песен, добавляю 1 новых в плейлист osu!lazer");
        push("info", id, "папка osu!lazer: C:\\osu");
        push("info", id, "папка osu!lazer: C:\\osu");
        assert_eq!(
            recent(id),
            vec![
                "info: в картах 193 песен, добавляю 1 новых в плейлист osu!lazer".to_string(),
                "info: папка osu!lazer: C:\\osu".to_string(),
            ]
        );
    }
}
