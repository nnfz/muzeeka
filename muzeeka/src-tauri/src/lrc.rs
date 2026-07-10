//! LRC parser and conversion to line-level TTML for the lyrics UI.

#[derive(Debug, Clone, PartialEq, Eq)]
struct LrcLine {
    start_ms: u32,
    text: String,
}

fn parse_lrc_timestamp(raw: &str) -> Option<u32> {
    let raw = raw.trim();
    let (hours, rest) = if let Some((h, rest)) = raw.split_once(':') {
        if rest.contains(':') {
            (h.parse::<u32>().ok()?, rest)
        } else {
            (0, raw)
        }
    } else {
        return None;
    };

    let (minutes, seconds_part) = rest.rsplit_once(':').unwrap_or(("0", rest));
    let minutes: u32 = minutes.parse().ok()?;
    let seconds_part = seconds_part.trim();

    let (seconds, millis) = if let Some((secs, frac)) = seconds_part.split_once('.') {
        let secs: u32 = secs.parse().ok()?;
        let frac = frac.chars().take(3).collect::<String>();
        let millis = match frac.len() {
            0 => 0,
            1 => frac.parse::<u32>().ok()? * 100,
            2 => frac.parse::<u32>().ok()? * 10,
            _ => frac.parse::<u32>().ok()?,
        };
        (secs, millis)
    } else {
        (seconds_part.parse().ok()?, 0)
    };

    Some(hours * 3_600_000 + minutes * 60_000 + seconds * 1_000 + millis)
}

fn parse_lrc_lines(lrc: &str) -> Vec<LrcLine> {
    let mut lines = Vec::new();

    for raw_line in lrc.lines() {
        let raw_line = raw_line.trim();
        if raw_line.is_empty() {
            continue;
        }

        let Some(open) = raw_line.find('[') else {
            continue;
        };
        let Some(close) = raw_line[open..].find(']') else {
            continue;
        };

        let tag = &raw_line[open + 1..open + close];
        if tag.contains(':') && tag.chars().next().is_some_and(|c| !c.is_ascii_digit()) {
            continue;
        }

        let Some(start_ms) = parse_lrc_timestamp(tag) else {
            continue;
        };

        let text = raw_line[open + close + 1..].trim().to_string();
        if text.is_empty() {
            continue;
        }

        lines.push(LrcLine { start_ms, text });
    }

    lines.sort_by_key(|line| line.start_ms);
    lines.dedup_by_key(|line| line.start_ms);
    lines
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn format_ttml_time(ms: u32) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

pub fn lrc_to_ttml(lrc: &str, song_duration_ms: u32) -> Option<String> {
    let lines = parse_lrc_lines(lrc);
    if lines.is_empty() {
        return None;
    }

    let song_end_ms = song_duration_ms.max(lines.last()?.start_ms + 1);

    let mut body = String::new();
    for (index, line) in lines.iter().enumerate() {
        let end_ms = lines
            .get(index + 1)
            .map(|next| next.start_ms)
            .unwrap_or(song_end_ms);
        if end_ms <= line.start_ms {
            continue;
        }

        let begin = format_ttml_time(line.start_ms);
        let end = format_ttml_time(end_ms);
        let text = xml_escape(&line.text);

        body.push_str(&format!(
            "<p begin=\"{begin}\" end=\"{end}\"><span begin=\"{begin}\" end=\"{end}\">{text}</span></p>"
        ));
    }

    if body.is_empty() {
        return None;
    }

    Some(format!(
        "<tt xmlns=\"http://www.w3.org/ns/ttml\" xml:lang=\"en\"><body>{body}</body></tt>"
    ))
}

#[cfg(test)]
mod tests {
    use super::{lrc_to_ttml, parse_lrc_timestamp};

    #[test]
    fn parses_lrc_timestamp() {
        assert_eq!(parse_lrc_timestamp("00:02.12"), Some(2120));
        assert_eq!(parse_lrc_timestamp("01:02:03"), Some(3_723_000));
    }

    #[test]
    fn converts_lrc_to_ttml() {
        let lrc = "[00:02.12] Hello world\n[00:05.00] Second line";
        let ttml = lrc_to_ttml(lrc, 10_000).expect("ttml");
        assert!(ttml.contains("<p begin=\"2.120\""));
        assert!(ttml.contains("Hello world"));
        assert!(ttml.contains("Second line"));
    }
}