use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use mp3lame_encoder::{Bitrate as Mp3Rate, Builder, FlushNoGap, InterleavedPcm, Quality, VbrMode};
use opus::{Application, Bitrate, Channels, Encoder, Signal};

use crate::hub::Hub;
use crate::meta::{NowPlaying, Track};

pub const SAMPLE_RATES: [u32; 5] = [8000, 12000, 16000, 24000, 48000];
pub const BITRATES: [u32; 8] = [32, 64, 96, 128, 160, 192, 224, 256];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Opus,
    Mp3,
}

impl Format {
    pub fn parse(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "mp3" | "mpeg" => Format::Mp3,
            _ => Format::Opus,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Format::Opus => "opus",
            Format::Mp3 => "mp3",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Format::Opus => "audio/ogg",
            Format::Mp3 => "audio/mpeg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Opus => "Ogg Opus",
            Format::Mp3 => "MP3",
        }
    }
}

const OGG_BOS: u8 = 0x02;
const OGG_EOS: u8 = 0x04;
const FRAME_MS: u32 = 20;
const GRANULE_20MS: u64 = 960;

pub fn snap(value: u32, table: &[u32]) -> u32 {
    *table
        .iter()
        .min_by_key(|v| (**v as i64 - value as i64).abs())
        .unwrap_or(&value)
}

fn crc_table() -> &'static [u32; 256] {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for i in 0..256 {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                if r & 0x8000_0000 != 0 {
                    r = (r << 1) ^ 0x04c11db7;
                } else {
                    r <<= 1;
                }
            }
            table[i] = r;
        }
        table
    })
}

fn ogg_crc(data: &[u8]) -> u32 {
    let table = crc_table();
    let mut crc = 0u32;
    for &byte in data {
        let index = ((crc >> 24) as u8) ^ byte;
        crc = (crc << 8) ^ table[index as usize];
    }
    crc
}

fn ogg_page(serial: u32, seq: u32, granule: u64, flags: u8, packet: Option<&[u8]>) -> Vec<u8> {
    let mut lace = Vec::new();
    let mut body = Vec::new();
    if let Some(packet) = packet {
        if packet.is_empty() {
            lace.push(0);
        } else {
            let mut offset = 0;
            while offset < packet.len() {
                let n = (packet.len() - offset).min(255);
                lace.push(n as u8);
                body.extend_from_slice(&packet[offset..offset + n]);
                offset += n;
            }
            if packet.len().is_multiple_of(255) {
                lace.push(0);
            }
        }
    }

    let mut page = Vec::with_capacity(27 + lace.len() + body.len());
    page.extend_from_slice(b"OggS");
    page.push(0);
    page.push(flags);
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&seq.to_le_bytes());
    page.extend_from_slice(&0u32.to_le_bytes());
    page.push(lace.len() as u8);
    page.extend_from_slice(&lace);
    page.extend_from_slice(&body);
    let crc = ogg_crc(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

fn new_serial() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(1);
    nanos ^ n.wrapping_mul(0x9E37_79B1) | 1
}

struct Mux {
    serial: u32,
    seq: u32,
}

impl Mux {
    fn new() -> Self {
        Mux {
            serial: new_serial(),
            seq: 0,
        }
    }

    fn packet(&mut self, packet: &[u8], granule: u64, flags: u8) -> Vec<u8> {
        let page = ogg_page(self.serial, self.seq, granule, flags, Some(packet));
        self.seq = self.seq.wrapping_add(1);
        page
    }

    fn eos(&mut self, granule: u64) -> Vec<u8> {
        let page = ogg_page(self.serial, self.seq, granule, OGG_EOS, None);
        self.seq = self.seq.wrapping_add(1);
        page
    }
}

fn opus_head(preskip: u16, rate: u32) -> Vec<u8> {
    let mut packet = Vec::with_capacity(19);
    packet.extend_from_slice(b"OpusHead");
    packet.push(1);
    packet.push(2);
    packet.extend_from_slice(&preskip.to_le_bytes());
    packet.extend_from_slice(&rate.to_le_bytes());
    packet.extend_from_slice(&0i16.to_le_bytes());
    packet.push(0);
    packet
}

fn comment(tag: &str, value: &str) -> Option<Vec<u8>> {
    let value = value.trim();
    if value.is_empty() || value.contains('\0') {
        return None;
    }
    Some(format!("{tag}={value}").into_bytes())
}

fn opus_tags(track: &Track) -> Vec<u8> {
    let vendor = b"stream-server";
    let mut comments = vec![b"ENCODER=stream-server".to_vec()];
    if let Some(c) = comment("ARTIST", &track.artist) {
        comments.push(c);
    }
    if let Some(c) = comment("TITLE", &track.title) {
        comments.push(c);
    }
    if let Some(c) = comment("ALBUM", &track.album) {
        comments.push(c);
    }
    let mut packet = Vec::new();
    packet.extend_from_slice(b"OpusTags");
    packet.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    packet.extend_from_slice(vendor);
    packet.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in comments {
        packet.extend_from_slice(&(c.len() as u32).to_le_bytes());
        packet.extend_from_slice(&c);
    }
    packet
}

fn opus_headers(serial: u32, preskip: u16, rate: u32, track: &Track) -> Vec<u8> {
    let mut mux = Mux { serial, seq: 0 };
    let mut headers = Vec::new();
    headers.extend(mux.packet(&opus_head(preskip, rate), 0, OGG_BOS));
    headers.extend(mux.packet(&opus_tags(track), 0, 0));
    headers
}

fn mp3_bitrate(kbps: u32) -> Mp3Rate {
    match snap(kbps, &BITRATES) {
        32 => Mp3Rate::Kbps32,
        64 => Mp3Rate::Kbps64,
        96 => Mp3Rate::Kbps96,
        160 => Mp3Rate::Kbps160,
        192 => Mp3Rate::Kbps192,
        224 => Mp3Rate::Kbps224,
        256 => Mp3Rate::Kbps256,
        _ => Mp3Rate::Kbps128,
    }
}

struct Resampler {
    step: f64,
    pos: f64,
    prev: [f32; 2],
}

impl Resampler {
    fn new(from: u32, to: u32) -> Self {
        Resampler {
            step: from as f64 / to as f64,
            pos: 0.0,
            prev: [0.0; 2],
        }
    }

    fn is_passthrough(&self) -> bool {
        (self.step - 1.0).abs() < f64::EPSILON
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let frames = input.len() / 2;
        if frames == 0 {
            return;
        }
        let limit = (frames - 1) as f64;
        while self.pos < limit {
            let floor = self.pos.floor();
            let frac = (self.pos - floor) as f32;
            let idx = floor as isize;
            for ch in 0..2 {
                let a = if idx < 0 {
                    self.prev[ch]
                } else {
                    input[idx as usize * 2 + ch]
                };
                let b = input[(idx + 1) as usize * 2 + ch];
                out.push(a + (b - a) * frac);
            }
            self.pos += self.step;
        }
        self.pos -= frames as f64;
        self.prev = [input[(frames - 1) * 2], input[(frames - 1) * 2 + 1]];
    }
}

enum Engine {
    Opus {
        encoder: Encoder,
        mux: Mux,
        packet: Vec<u8>,
        granule: u64,
        preskip: u16,
        frame: usize,
    },
    Mp3 {
        encoder: mp3lame_encoder::Encoder,
        packet: Vec<u8>,
    },
}

pub struct Pipeline {
    src_rate: u32,
    out_rate: u32,
    gain: f32,
    resampler: Resampler,
    engine: Engine,
    hub: Arc<Hub>,
    now: Arc<NowPlaying>,
    tag_gen: u64,
    scratch: Vec<f32>,
    pending: Vec<f32>,
    frame_buf: Vec<f32>,
}

impl Pipeline {
    pub fn new(
        src_rate: u32,
        out_rate: u32,
        kbps: u32,
        gain: f32,
        hub: Arc<Hub>,
        format: Format,
        now: Arc<NowPlaying>,
    ) -> Result<Self, String> {
        let out_rate = snap(out_rate, &SAMPLE_RATES);
        let kbps = snap(kbps, &BITRATES);
        let track = now.track();
        let tag_gen = now.generation();
        let engine = match format {
            Format::Opus => {
                let mut encoder = Encoder::new(out_rate, Channels::Stereo, Application::Audio)
                    .map_err(|e| format!("opus: {e}"))?;
                encoder
                    .set_bitrate(Bitrate::Bits(kbps as i32 * 1000))
                    .map_err(|e| format!("opus bitrate: {e}"))?;
                encoder.set_vbr(true).map_err(|e| format!("opus vbr: {e}"))?;
                encoder
                    .set_vbr_constraint(true)
                    .map_err(|e| format!("opus vbr constraint: {e}"))?;
                encoder
                    .set_signal(Signal::Music)
                    .map_err(|e| format!("opus signal: {e}"))?;
                encoder
                    .set_complexity(8)
                    .map_err(|e| format!("opus complexity: {e}"))?;
                let lookahead = encoder
                    .get_lookahead()
                    .map_err(|e| format!("opus lookahead: {e}"))?
                    .max(0) as u64;
                let preskip = (lookahead * 48_000 / out_rate as u64).min(u16::MAX as u64) as u16;
                let mux = Mux::new();
                let headers = opus_headers(mux.serial, preskip, out_rate, &track);
                hub.set_headers(Arc::new(headers));
                let frame = (out_rate / (1000 / FRAME_MS)) as usize;
                Engine::Opus {
                    encoder,
                    mux,
                    packet: vec![0u8; 4000],
                    granule: 0,
                    preskip,
                    frame,
                }
            }
            Format::Mp3 => {
                let encoder = Builder::new()
                    .ok_or("lame init failed")?
                    .with_num_channels(2)
                    .map_err(|e| format!("lame channels: {e}"))?
                    .with_sample_rate(out_rate)
                    .map_err(|e| format!("lame sample rate: {e}"))?
                    .with_output_sample_rate(NonZeroU32::new(out_rate))
                    .map_err(|e| format!("lame output rate: {e}"))?
                    .with_brate(mp3_bitrate(kbps))
                    .map_err(|e| format!("lame bitrate: {e}"))?
                    .with_quality(Quality::NearBest)
                    .map_err(|e| format!("lame quality: {e}"))?
                    .with_vbr_mode(VbrMode::Off)
                    .map_err(|e| format!("lame vbr: {e}"))?
                    .with_to_write_vbr_tag(false)
                    .map_err(|e| format!("lame vbr tag: {e}"))?
                    .build()
                    .map_err(|e| format!("lame: {e}"))?;
                hub.set_headers(Arc::new(Vec::new()));
                Engine::Mp3 {
                    encoder,
                    packet: Vec::new(),
                }
            }
        };
        Ok(Pipeline {
            src_rate,
            out_rate,
            gain,
            resampler: Resampler::new(src_rate, out_rate),
            engine,
            hub,
            now,
            tag_gen,
            scratch: Vec::new(),
            pending: Vec::new(),
            frame_buf: Vec::new(),
        })
    }

    pub fn set_src_rate(&mut self, rate: u32) {
        if self.src_rate != rate {
            self.src_rate = rate;
            self.resampler = Resampler::new(rate, self.out_rate);
        }
    }

    pub fn push(&mut self, stereo: &[f32]) {
        self.refresh_tags();
        let stereo = if stereo.len().is_multiple_of(2) {
            stereo
        } else {
            &stereo[..stereo.len() - stereo.len() % 2]
        };
        if stereo.is_empty() {
            return;
        }
        if self.resampler.is_passthrough() {
            self.pending.extend_from_slice(stereo);
        } else {
            self.scratch.clear();
            self.resampler.process(stereo, &mut self.scratch);
            self.pending.extend_from_slice(&self.scratch);
        }
        self.drain_frames();
    }

    fn refresh_tags(&mut self) {
        let (serial, preskip) = match &self.engine {
            Engine::Opus { mux, preskip, .. } => (mux.serial, *preskip),
            Engine::Mp3 { .. } => return,
        };
        let gen = self.now.generation();
        if gen == self.tag_gen {
            return;
        }
        self.tag_gen = gen;
        let track = self.now.track();
        let headers = opus_headers(serial, preskip, self.out_rate, &track);
        self.hub.update_headers(Arc::new(headers));
    }

    fn drain_frames(&mut self) {
        match &mut self.engine {
            Engine::Opus {
                encoder,
                mux,
                packet,
                granule,
                frame,
                ..
            } => {
                let need = *frame * 2;
                while self.pending.len() >= need {
                    self.frame_buf.clear();
                    self.frame_buf.extend(
                        self.pending
                            .drain(..need)
                            .map(|s| (s * self.gain).clamp(-1.0, 1.0)),
                    );
                    let n = match encoder.encode_float(&self.frame_buf, packet) {
                        Ok(n) if n > 0 => n,
                        _ => continue,
                    };
                    // Granule is samples a 48 kHz decoder would have produced, pre-skip included.
                    *granule = granule.saturating_add(GRANULE_20MS);
                    let page = mux.packet(&packet[..n], *granule, 0);
                    self.hub.broadcast(Arc::new(page));
                }
            }
            Engine::Mp3 { encoder, packet } => {
                if self.pending.is_empty() {
                    return;
                }
                self.frame_buf.clear();
                self.frame_buf.extend(
                    self.pending
                        .drain(..)
                        .map(|s| (s * self.gain).clamp(-1.0, 1.0)),
                );
                let samples = self.frame_buf.len() / 2;
                packet.clear();
                packet.reserve(mp3lame_encoder::max_required_buffer_size(samples));
                let wrote = encoder
                    .encode_to_vec(InterleavedPcm(&self.frame_buf), packet)
                    .unwrap_or(0);
                if wrote > 0 {
                    self.hub.broadcast(Arc::new(packet.clone()));
                }
            }
        }
    }

    pub fn finish(&mut self) {
        match &mut self.engine {
            Engine::Opus { mux, granule, .. } => {
                let page = mux.eos(*granule);
                self.hub.broadcast(Arc::new(page));
            }
            Engine::Mp3 { encoder, packet } => {
                packet.clear();
                packet.reserve(7200);
                let wrote = encoder.flush_to_vec::<FlushNoGap>(packet).unwrap_or(0);
                if wrote > 0 {
                    self.hub.broadcast(Arc::new(packet.clone()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{NowPlaying, Track};
    use opus::{Channels, Decoder};

    #[test]
    fn ogg_crc_matches_libogg_table() {
        let table = crc_table();
        assert_eq!(table[0], 0x0000_0000);
        assert_eq!(table[1], 0x04c1_1db7);
        assert_eq!(table[2], 0x0982_3b6e);
        assert_eq!(table[3], 0x0d43_26d9);
    }

    struct Page {
        flags: u8,
        granule: u64,
        packets: Vec<Vec<u8>>,
    }

    fn parse_pages(mut data: &[u8]) -> Result<Vec<Page>, String> {
        let mut pages = Vec::new();
        while !data.is_empty() {
            if data.len() < 27 || &data[..4] != b"OggS" {
                return Err(format!("not an ogg page at {}", data.len()));
            }
            let flags = data[5];
            let granule = u64::from_le_bytes(data[6..14].try_into().unwrap());
            let nseg = data[26] as usize;
            if data.len() < 27 + nseg {
                return Err("truncated lace".into());
            }
            let lace = data[27..27 + nseg].to_vec();
            let body_len: usize = lace.iter().map(|s| *s as usize).sum();
            let body_at = 27 + nseg;
            let page_end = body_at + body_len;
            if data.len() < page_end {
                return Err("truncated body".into());
            }
            let mut check = data[..page_end].to_vec();
            let stored = u32::from_le_bytes(check[22..26].try_into().unwrap());
            check[22..26].copy_from_slice(&0u32.to_le_bytes());
            if ogg_crc(&check) != stored {
                return Err("page crc mismatch".into());
            }
            let body = &data[body_at..page_end];
            let mut packets = Vec::new();
            let mut packet = Vec::new();
            let mut offset = 0;
            for seg in lace {
                let n = seg as usize;
                packet.extend_from_slice(&body[offset..offset + n]);
                offset += n;
                if seg < 255 {
                    packets.push(std::mem::take(&mut packet));
                }
            }
            if !packet.is_empty() {
                return Err("packet continued past the page".into());
            }
            pages.push(Page {
                flags,
                granule,
                packets,
            });
            data = &data[page_end..];
        }
        Ok(pages)
    }

    #[test]
    fn sine_roundtrip_is_ogg_opus() {
        let hub = Arc::new(Hub::new());
        let now = Arc::new(NowPlaying::new());
        now.update(Track {
            artist: "A".into(),
            title: "T".into(),
            album: String::new(),
        });
        let mut pipeline =
            Pipeline::new(48_000, 48_000, 96, 1.0, hub.clone(), Format::Opus, now).unwrap();
        let rx = hub.subscribe();
        let mut pcm = Vec::with_capacity(48_000 * 2);
        for i in 0..48_000 {
            let s = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.2;
            pcm.push(s);
            pcm.push(s);
        }
        pipeline.push(&pcm);
        pipeline.finish();

        let mut bytes = hub.headers().as_ref().clone();
        while let Ok(chunk) = rx.try_recv() {
            bytes.extend_from_slice(&chunk);
        }
        let pages = parse_pages(&bytes).unwrap();
        assert!(pages.len() > 4);
        assert_eq!(pages[0].flags & OGG_BOS, OGG_BOS);
        assert_eq!(&pages[0].packets[0][..8], b"OpusHead");
        assert_eq!(pages[0].packets[0][8], 1);
        assert_eq!(pages[0].packets[0][9], 2);
        assert_eq!(&pages[1].packets[0][..8], b"OpusTags");
        assert!(pages[1].packets[0].windows(7).any(|w| w == b"TITLE=T"));
        assert!(pages[1].packets[0].windows(8).any(|w| w == b"ARTIST=A"));

        let preskip = u16::from_le_bytes(pages[0].packets[0][10..12].try_into().unwrap());
        let mut expect = GRANULE_20MS;
        let mut decoder = Decoder::new(48_000, Channels::Stereo).unwrap();
        let mut decoded = Vec::new();
        for page in pages.iter().skip(2) {
            if page.packets.is_empty() {
                assert_eq!(page.flags & OGG_EOS, OGG_EOS);
                continue;
            }
            assert_eq!(page.granule, expect, "granule");
            expect += GRANULE_20MS;
            for packet in &page.packets {
                let mut out = vec![0.0f32; 960 * 2];
                let n = decoder.decode_float(packet, &mut out, false).unwrap();
                decoded.extend_from_slice(&out[..n * 2]);
            }
        }
        let skip = preskip as usize * 2;
        assert!(decoded.len() > skip + 16_000, "decoded {}", decoded.len());
        let audible = &decoded[skip..];
        let energy: f64 = audible.iter().map(|s| (*s as f64) * (*s as f64)).sum();
        let rms = (energy / audible.len() as f64).sqrt();
        assert!(rms > 0.05, "rms {rms}");
    }

    #[test]
    fn sine_is_mp3_frames() {
        let hub = Arc::new(Hub::new());
        let now = Arc::new(NowPlaying::new());
        let rx = hub.subscribe();
        let mut pipeline =
            Pipeline::new(48_000, 48_000, 128, 1.0, hub.clone(), Format::Mp3, now).unwrap();
        assert!(hub.ready());
        assert!(hub.headers().is_empty());
        let mut pcm = Vec::with_capacity(48_000 * 2);
        for i in 0..48_000 {
            let s = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.2;
            pcm.push(s);
            pcm.push(s);
        }
        pipeline.push(&pcm);
        pipeline.finish();
        let mut bytes = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            bytes.extend_from_slice(&chunk);
        }
        assert!(bytes.len() > 4_000, "mp3 bytes {}", bytes.len());
        assert!(
            bytes.windows(2).any(|w| w[0] == 0xFF && w[1] & 0xE0 == 0xE0),
            "no mp3 frame sync"
        );
    }
}
