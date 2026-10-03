use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Track {
    pub artist: String,
    pub title: String,
    pub album: String,
}

impl Track {
    /// `Artist - Title`, the string players show as StreamTitle.
    pub fn stream_title(&self) -> String {
        let artist = self.artist.trim();
        let title = self.title.trim();
        if !artist.is_empty() && !title.is_empty() {
            format!("{artist} - {title}")
        } else if !title.is_empty() {
            title.to_string()
        } else {
            artist.to_string()
        }
    }
}

/// Current track. `generation` increments whenever the label changes.
pub struct NowPlaying {
    track: Mutex<Track>,
    gen: AtomicU64,
}

impl NowPlaying {
    pub fn new() -> Self {
        NowPlaying {
            track: Mutex::new(Track::default()),
            gen: AtomicU64::new(0),
        }
    }

    pub fn update(&self, track: Track) {
        let mut slot = self.track.lock().unwrap_or_else(|e| e.into_inner());
        if *slot == track {
            return;
        }
        *slot = track;
        self.gen.fetch_add(1, Ordering::Release);
    }

    pub fn generation(&self) -> u64 {
        self.gen.load(Ordering::Acquire)
    }

    pub fn track(&self) -> Track {
        self.track.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn icy(&self) -> (u64, String) {
        let slot = self.track.lock().unwrap_or_else(|e| e.into_inner());
        (self.gen.load(Ordering::Acquire), slot.stream_title())
    }
}

pub fn escape_icy(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars().take(180) {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// One ICY metadata interval: a length byte, then `StreamTitle='…';` padded to 16 bytes.
/// An empty block (`[0]`) means "title unchanged".
pub fn icy_block(title: &str) -> Vec<u8> {
    let body = format!("StreamTitle='{}';", escape_icy(title));
    let mut bytes = body.into_bytes();
    let max = 255 * 16;
    if bytes.len() > max {
        bytes.truncate(max);
        while !bytes.is_empty() && !is_char_end(&bytes) {
            bytes.pop();
        }
    }
    let padded = bytes.len().div_ceil(16) * 16;
    bytes.resize(padded, 0);
    let mut out = Vec::with_capacity(1 + bytes.len());
    out.push((bytes.len() / 16) as u8);
    out.extend_from_slice(&bytes);
    out
}

fn is_char_end(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok()
}

/// Insert a metadata block every `metaint` audio bytes. `left` is how many audio
/// bytes remain before the next block. `on_meta` appends that block.
pub fn mux_icy(
    audio: &[u8],
    left: &mut usize,
    metaint: usize,
    mut on_meta: impl FnMut(&mut Vec<u8>),
    out: &mut Vec<u8>,
) {
    if metaint == 0 {
        out.extend_from_slice(audio);
        return;
    }
    let mut i = 0;
    while i < audio.len() {
        if *left == 0 {
            on_meta(out);
            *left = metaint;
        }
        let n = (*left).min(audio.len() - i);
        out.extend_from_slice(&audio[i..i + n]);
        i += n;
        *left -= n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_title_joins_artist_and_title() {
        let track = Track {
            artist: "Кино".into(),
            title: "Группа крови".into(),
            album: String::new(),
        };
        assert_eq!(track.stream_title(), "Кино - Группа крови");
    }

    #[test]
    fn icy_block_is_padded_and_carries_utf8() {
        let block = icy_block("Кино - Группа крови");
        assert!(block.len() > 1);
        assert_eq!((block.len() - 1) % 16, 0);
        assert_eq!(block[0] as usize * 16, block.len() - 1);
        let text = std::str::from_utf8(&block[1..]).unwrap().trim_end_matches('\0');
        assert_eq!(text, "StreamTitle='Кино - Группа крови';");
    }

    #[test]
    fn unchanged_title_is_a_zero_block() {
        assert_eq!(icy_block_or_skip(1, 1, "same"), vec![0]);
    }

    fn icy_block_or_skip(sent: u64, gen: u64, title: &str) -> Vec<u8> {
        if sent == gen {
            vec![0]
        } else {
            icy_block(title)
        }
    }

    #[test]
    fn mux_inserts_metadata_on_the_interval() {
        let audio = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let mut left = 4usize;
        let mut out = Vec::new();
        mux_icy(&audio, &mut left, 4, |out| out.push(0), &mut out);
        assert_eq!(out, vec![1, 2, 3, 4, 0, 5, 6, 7, 8, 0, 9, 10]);
        assert_eq!(left, 2);
    }
}
