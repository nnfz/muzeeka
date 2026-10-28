use std::collections::HashSet;

const INSTRUMENTAL_GAP_MS: u32 = 3000;
pub const LINE_LEAD_SEC: f64 = 0.15;
const LYRIC_ENDING_SEC: f64 = 0.5;

#[derive(Clone, Debug, PartialEq)]
pub struct LyricPart {
    pub start_ms: u32,
    pub duration_ms: u32,
    pub words: String,
    pub background: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LyricLine {
    pub start_ms: u32,
    pub duration_ms: u32,
    pub words: String,
    pub parts: Vec<LyricPart>,
    pub instrumental: bool,
    pub agent: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncType {
    Rich,
    Line,
    None,
}

#[derive(Clone, Debug)]
pub struct Lyrics {
    pub lines: Vec<LyricLine>,
    pub sync: SyncType,
}

pub fn parse(ttml: &str, song_ms: u32) -> Option<Lyrics> {
    let source = declare_missing_namespaces(ttml);
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(&source, options).ok()?;
    let mut lines = Vec::new();
    for paragraph in doc.descendants().filter(|node| {
        node.is_element()
            && node.tag_name().name() == "p"
            && attr(*node, "begin").is_some_and(|value| !value.is_empty())
    }) {
        let begin = parse_time(attr(paragraph, "begin"));
        let end = parse_time(attr(paragraph, "end"));
        let (parts, text, word_synced) = paragraph_parts(paragraph, begin);
        if text.is_empty() {
            continue;
        }
        lines.push(LyricLine {
            start_ms: begin,
            duration_ms: end.saturating_sub(begin),
            words: text,
            parts: if word_synced { parts } else { Vec::new() },
            instrumental: false,
            agent: attr(paragraph, "agent").map(str::to_string),
        });
    }
    if lines.is_empty() {
        return None;
    }
    lines.sort_by_key(|line| line.start_ms);
    if lines
        .iter()
        .all(|line| line.start_ms == 0 && line.parts.is_empty())
    {
        return Some(Lyrics {
            lines,
            sync: SyncType::None,
        });
    }
    finalize_durations(&mut lines, song_ms);
    let lines = insert_instrumental_breaks(lines, song_ms);
    let sync = if lines.iter().any(|line| line.parts.len() > 1) {
        SyncType::Rich
    } else {
        SyncType::Line
    };
    Some(Lyrics { lines, sync })
}

fn attr<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name() == name)
        .map(|attribute| attribute.value())
}

fn text_of(node: roxmltree::Node) -> String {
    node.descendants()
        .filter(|child| child.is_text())
        .filter_map(|child| child.text())
        .collect()
}

fn declare_missing_namespaces(content: &str) -> String {
    let Some(root_start) = content.find("<tt") else {
        return content.to_string();
    };
    let Some(root_len) = content[root_start..].find('>') else {
        return content.to_string();
    };
    let root_end = root_start + root_len;
    let root = &content[root_start..root_end];
    let mut declared: HashSet<&str> = ["xml", "xmlns"].into_iter().collect();
    let declare = regex::Regex::new(r"xmlns:([A-Za-z][\w.-]*)\s*=").expect("regex");
    for capture in declare.captures_iter(root) {
        declared.insert(capture.get(1).map(|m| m.as_str()).unwrap_or(""));
    }
    let tag_prefix = regex::Regex::new(r"</?([A-Za-z][\w.-]*):").expect("regex");
    let attr_prefix = regex::Regex::new(r"\s([A-Za-z][\w.-]*):[\w.-]+\s*=").expect("regex");
    let mut missing: Vec<String> = Vec::new();
    for capture in tag_prefix
        .captures_iter(content)
        .chain(attr_prefix.captures_iter(content))
    {
        let prefix = capture.get(1).map(|m| m.as_str()).unwrap_or("");
        if !prefix.is_empty() && !declared.contains(prefix) && !missing.iter().any(|p| p == prefix)
        {
            missing.push(prefix.to_string());
        }
    }
    if missing.is_empty() {
        return content.to_string();
    }
    let additions: String = missing
        .iter()
        .map(|prefix| format!(" xmlns:{prefix}=\"urn:better-lyrics:unbound:{prefix}\""))
        .collect();
    let mut out = String::with_capacity(content.len() + additions.len());
    out.push_str(&content[..root_end]);
    out.push_str(&additions);
    out.push_str(&content[root_end..]);
    out
}

fn parse_time(value: Option<&str>) -> u32 {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return 0;
    };
    let ms = |seconds: f64| (seconds * 1000.0).round().max(0.0) as u32;
    for (suffix, scale) in [("ms", 0.001), ("h", 3600.0), ("m", 60.0), ("s", 1.0)] {
        if let Some(number) = value.strip_suffix(suffix) {
            if let Ok(number) = number.parse::<f64>() {
                return ms(number * scale);
            }
        }
    }
    let parts: Vec<f64> = value
        .split(':')
        .map(|part| {
            part.chars()
                .filter(|ch| ch.is_ascii_digit() || *ch == '.')
                .collect::<String>()
                .parse::<f64>()
                .unwrap_or(0.0)
        })
        .collect();
    match parts.as_slice() {
        [seconds] => ms(*seconds),
        [minutes, seconds] => ms(minutes.trunc() * 60.0 + seconds),
        [hours, minutes, seconds] => ms(hours.trunc() * 3600.0 + minutes.trunc() * 60.0 + seconds),
        _ => 0,
    }
}

fn next_start(parts: &[LyricPart], begin: u32) -> u32 {
    parts
        .last()
        .map(|part| part.start_ms + part.duration_ms)
        .unwrap_or(begin)
}

fn push_span(span: roxmltree::Node, parts: &mut Vec<LyricPart>, background: bool, prefix: &str) {
    let words = format!("{prefix}{}", text_of(span));
    if words.is_empty() {
        return;
    }
    let start = parse_time(attr(span, "begin"));
    let end = parse_time(attr(span, "end"));
    parts.push(LyricPart {
        start_ms: start,
        duration_ms: end.saturating_sub(start),
        words,
        background,
    });
}

fn paragraph_parts(paragraph: roxmltree::Node, begin: u32) -> (Vec<LyricPart>, String, bool) {
    let mut parts = Vec::new();
    let mut word_synced = false;
    let mut pending = String::new();
    for node in paragraph.children() {
        if node.is_text() {
            let chunk = node.text().unwrap_or("");
            if chunk.is_empty() {
                continue;
            }
            if chunk.trim().is_empty() {
                pending.push_str(chunk);
            } else {
                parts.push(LyricPart {
                    start_ms: next_start(&parts, begin),
                    duration_ms: 0,
                    words: chunk.to_string(),
                    background: false,
                });
            }
            continue;
        }
        if !node.is_element() {
            continue;
        }
        if attr(node, "role") == Some("x-bg") {
            let spans: Vec<_> = node
                .children()
                .filter(|child| child.is_element() && child.tag_name().name() == "span")
                .collect();
            let spans = if spans.is_empty() { vec![node] } else { spans };
            for span in spans {
                push_span(span, &mut parts, true, &pending);
                pending.clear();
                if !text_of(span).is_empty() {
                    word_synced = true;
                }
            }
            continue;
        }
        let name = node.tag_name().name();
        if name == "span" || name == "text" {
            push_span(node, &mut parts, false, &pending);
            pending.clear();
            if !text_of(node).is_empty() {
                word_synced = true;
            }
            continue;
        }
        let chunk = text_of(node);
        if !chunk.is_empty() {
            parts.push(LyricPart {
                start_ms: next_start(&parts, begin),
                duration_ms: 0,
                words: format!("{pending}{chunk}"),
                background: false,
            });
            pending.clear();
        }
    }
    if !pending.is_empty() {
        if let Some(last) = parts.last_mut() {
            last.words.push_str(&pending);
        }
    }
    let text = text_of(paragraph)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !word_synced {
        return (Vec::new(), text, false);
    }
    (normalize_part_spaces(parts), text, true)
}

fn normalize_part_spaces(mut parts: Vec<LyricPart>) -> Vec<LyricPart> {
    for index in 0..parts.len() {
        let words = &parts[index].words;
        let rest = words.trim_start();
        if rest.len() == words.len() {
            continue;
        }
        let lead = words[..words.len() - rest.len()].to_string();
        let rest = rest.to_string();
        if index > 0 {
            parts[index - 1].words.push_str(&lead);
        }
        parts[index].words = rest;
    }
    parts.retain(|part| !part.words.is_empty());
    parts
}

fn finalize_durations(lines: &mut [LyricLine], song_ms: u32) {
    for index in 0..lines.len() {
        let next_start = lines.get(index + 1).map(|next| next.start_ms);
        let line = &mut lines[index];
        if line.duration_ms == 0 {
            line.duration_ms = next_start.unwrap_or(song_ms).saturating_sub(line.start_ms);
        }
        let line_end = line.start_ms + line.duration_ms;
        for part_index in 0..line.parts.len() {
            if line.parts[part_index].duration_ms != 0 {
                continue;
            }
            let start = line.parts[part_index].start_ms;
            let duration = match line.parts.get(part_index + 1) {
                Some(next) => next.start_ms.saturating_sub(start),
                None => {
                    let remaining = line_end.saturating_sub(start);
                    let guess = match part_index.checked_sub(1) {
                        Some(prev) => line.parts[prev].duration_ms.max(250),
                        None => remaining.min(600),
                    };
                    remaining.min(guess)
                }
            };
            line.parts[part_index].duration_ms = duration;
        }
    }
}

fn instrumental(start_ms: u32, duration_ms: u32) -> LyricLine {
    LyricLine {
        start_ms,
        duration_ms,
        words: String::new(),
        parts: Vec::new(),
        instrumental: true,
        agent: None,
    }
}

fn insert_instrumental_breaks(lines: Vec<LyricLine>, song_ms: u32) -> Vec<LyricLine> {
    let mut out = Vec::with_capacity(lines.len() + 2);
    if let Some(first) = lines.first() {
        if first.start_ms > INSTRUMENTAL_GAP_MS {
            out.push(instrumental(0, first.start_ms));
        }
    }
    let starts: Vec<u32> = lines.iter().map(|line| line.start_ms).collect();
    for (index, line) in lines.into_iter().enumerate() {
        let end = line.start_ms + line.duration_ms;
        out.push(line);
        let gap_end = starts.get(index + 1).copied().unwrap_or(song_ms);
        let gap = gap_end.saturating_sub(end);
        if gap > INSTRUMENTAL_GAP_MS {
            out.push(instrumental(end, gap));
        }
    }
    out
}

pub fn line_end_sec(lines: &[LyricLine], index: usize) -> f64 {
    match lines.get(index + 1) {
        Some(next) => next.start_ms as f64 / 1000.0,
        None => {
            let line = &lines[index];
            (line.start_ms + line.duration_ms) as f64 / 1000.0
        }
    }
}

pub fn line_until_sec(lines: &[LyricLine], index: usize) -> f64 {
    let line = &lines[index];
    let own = (line.start_ms + line.duration_ms) as f64 / 1000.0;
    match lines.get(index + 1) {
        Some(next) => own.max(next.start_ms as f64 / 1000.0),
        None => own,
    }
}

pub fn active_lines(lines: &[LyricLine], time: f64) -> Vec<usize> {
    (0..lines.len())
        .filter(|&index| {
            time >= lines[index].start_ms as f64 / 1000.0 && time < line_until_sec(lines, index)
        })
        .collect()
}

pub fn scroll_anchor(lines: &[LyricLine], time: f64) -> Option<usize> {
    let last = lines.len().checked_sub(1)?;
    let active = active_lines(lines, time);
    if let Some(&latest) = active.last() {
        let sung = active.iter().copied().find(|&index| {
            let line = &lines[index];
            time < (line.start_ms + line.duration_ms) as f64 / 1000.0 - LYRIC_ENDING_SEC
        });
        return Some(sung.unwrap_or(latest));
    }
    if time >= lines[last].start_ms as f64 / 1000.0 {
        return Some(last);
    }
    Some(0)
}

pub fn break_progress(lines: &[LyricLine], index: usize, time: f64) -> f32 {
    let start = lines[index].start_ms as f64 / 1000.0;
    let end = line_end_sec(lines, index);
    ((time - start) / (end - start).max(0.001)).clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_word_synced_ttml_with_undeclared_prefixes() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div>
            <p begin="00:05.000" end="00:07.000" ttm:agent="v1"><span begin="00:05.000" end="00:05.500">Hello</span> <span begin="00:05.500" end="00:07.000">world</span></p>
            <p begin="00:07.000" end="00:09.000"><span begin="00:07.000" end="00:09.000">Again</span><span ttm:role="x-bg"><span begin="00:08.000" end="00:09.000">(ooh)</span></span></p>
        </div></body></tt>"#;
        let lyrics = parse(ttml, 20_000).expect("parsed");
        assert_eq!(lyrics.sync, SyncType::Rich);
        assert_eq!(lyrics.lines.len(), 4);
        assert!(lyrics.lines[0].instrumental);
        let first = &lyrics.lines[1];
        assert_eq!(first.words, "Hello world");
        assert_eq!(first.parts[0].words, "Hello ");
        assert_eq!(first.parts[1].words, "world");
        assert_eq!(first.agent.as_deref(), Some("v1"));
        assert!(lyrics.lines[2].parts[1].background);
        assert!(lyrics.lines[3].instrumental);
    }

    #[test]
    fn lrc_lines_are_line_synced() {
        let ttml = crate::ui::lyrics_store::lrc_to_ttml("[00:01.00] One\n[00:02.00] Two", 3_000)
            .expect("ttml");
        let lyrics = parse(&ttml, 3_000).expect("parsed");
        assert_eq!(lyrics.sync, SyncType::Line);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(scroll_anchor(&lyrics.lines, 1.5), Some(0));
        assert_eq!(scroll_anchor(&lyrics.lines, 2.5), Some(1));
    }

    #[test]
    fn plain_text_is_untimed() {
        let ttml = r#"<tt><body><div><p begin="0.0s" end="36000.0s">A</p><p begin="0.0s" end="36000.0s">B</p></div></body></tt>"#;
        let lyrics = parse(ttml, 10_000).expect("parsed");
        assert_eq!(lyrics.sync, SyncType::None);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[1].words, "B");
    }

    #[test]
    fn long_gap_gets_a_break() {
        let ttml = r#"<tt><body><p begin="1s" end="2s"><span begin="1s" end="2s">A</span></p><p begin="10s" end="11s"><span begin="10s" end="11s">B</span></p></body></tt>"#;
        let lyrics = parse(ttml, 11_000).expect("parsed");
        let flags: Vec<bool> = lyrics.lines.iter().map(|line| line.instrumental).collect();
        assert_eq!(flags, vec![false, true, false]);
    }

    fn line(start_ms: u32, duration_ms: u32) -> LyricLine {
        LyricLine {
            start_ms,
            duration_ms,
            words: "x".into(),
            parts: Vec::new(),
            instrumental: false,
            agent: None,
        }
    }

    #[test]
    fn overlapping_lines_are_current_together() {
        let lines = vec![line(0, 4000), line(2000, 3000), line(6000, 1000)];
        assert_eq!(active_lines(&lines, 1.0), vec![0]);
        assert_eq!(active_lines(&lines, 2.5), vec![0, 1]);
        assert_eq!(scroll_anchor(&lines, 2.5), Some(0));
        assert_eq!(scroll_anchor(&lines, 3.7), Some(1));
        assert_eq!(active_lines(&lines, 5.5), vec![1]);
        assert_eq!(scroll_anchor(&lines, 0.0), Some(0));
    }
}
