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
    let repeated = lines
        .iter()
        .rev()
        .find(|line| line.plugin_id == plugin_id)
        .is_some_and(|line| line.level == level && line.message == message);
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
