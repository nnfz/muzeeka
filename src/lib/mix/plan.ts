/**
 * Shared mix-plan maths for the editor preview and for playlist playback.
 *
 * Both paths must read the saved layout identically, otherwise a transition
 * sounds one way in the Mix Transition window and another way in the playlist.
 * The geometry rule is the same everywhere:
 *
 *   t_next = t_prev + (to.viewStart − from.viewStart)
 *
 * Mix time 0 is the cut. In the editor preview the cut is wherever the user parked
 * the playhead; for playlist playback it is the start of the transition block, which
 * is the part of the layout that actually describes the mix. Envelope segments are
 * always expressed relative to that origin.
 */

import {
  blockEnd,
  normalizeCurve,
  normalizeEnvelope,
  primaryTransition,
  sampleEnvelope,
  type EnvelopeCurve,
  type MixBlock,
  type MixBlockTarget,
} from '$lib/mix/blocks';
import {
  loadMixTransitionMemory,
  type MixTransitionMemory,
} from '$lib/mix/memory';
import type { MusicFile } from '$lib/stores/player.svelte';

/** Automation kinds the audio engine understands. */
export type MixEnvKind = 'volume' | 'lowpass' | 'highpass' | 'speed';

/** One automated block on the mix clock, as the Rust side expects it. */
export interface MixEnvSegment {
  startSecs: number;
  durationSecs: number;
  curve: EnvelopeCurve;
  points: { t: number; v: number }[];
}

/**
 * Blocks of one kind on one deck, rebased so `startSecs` is relative to the cut.
 * Segments may be negative (block starts before the cut) — the engine clamps.
 */
export function collectMixEnvelopes(
  blocks: MixBlock[],
  lane: MixBlockTarget,
  kind: MixEnvKind,
  mixOriginSecs: number,
): MixEnvSegment[] {
  return blocks
    .filter(
      (b) =>
        b.kind === kind &&
        (b.targetLane ?? 'from') === lane &&
        b.durationSecs > 0.02,
    )
    .map((b) => ({
      startSecs: b.startFromSecs - mixOriginSecs,
      durationSecs: b.durationSecs,
      curve: normalizeCurve(b.params.curve),
      points: normalizeEnvelope(b.params.envelope).map((p) => ({
        t: p.t,
        v: p.v,
      })),
    }));
}

/** All eight envelope buckets for one mix, keyed as the Tauri commands expect. */
export function mixEnvelopeArgs(blocks: MixBlock[], mixOriginSecs: number) {
  const pick = (lane: MixBlockTarget, kind: MixEnvKind) =>
    collectMixEnvelopes(blocks, lane, kind, mixOriginSecs);
  return {
    fromVol: pick('from', 'volume'),
    toVol: pick('to', 'volume'),
    fromLp: pick('from', 'lowpass'),
    fromHp: pick('from', 'highpass'),
    toLp: pick('to', 'lowpass'),
    toHp: pick('to', 'highpass'),
    fromSpeed: pick('from', 'speed'),
    toSpeed: pick('to', 'speed'),
  };
}

function cueStartOf(track: MusicFile): number {
  const v = track.cue_start_secs;
  return typeof v === 'number' && Number.isFinite(v) ? Math.max(0, v) : 0;
}

function cueEndOf(track: MusicFile): number | null {
  const v = track.cue_end_secs;
  return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

/** Track-relative seconds → absolute content seconds (CUE aware). */
export function absTrackTime(track: MusicFile, rel: number): number {
  return cueStartOf(track) + Math.max(0, rel);
}

/** Playable length of a track (CUE segment length when the track is a CUE entry). */
export function trackPlayableSecs(track: MusicFile): number {
  const end = cueEndOf(track);
  if (end != null) {
    const len = end - cueStartOf(track);
    if (len > 0.05) return len;
  }
  const total = track.duration_secs;
  return typeof total === 'number' && Number.isFinite(total) && total > 0
    ? total
    : 0;
}

export function mixAudioPathFor(track: MusicFile): string | undefined {
  const p = track.audio_path?.trim();
  return p ? p : undefined;
}

/** Argument object for the `player_arm_mix` command. */
export interface ArmMixArgs {
  fromPath: string;
  startAtSecs: number;
  fromDurationSecs: number;
  toPath: string;
  toAudioPath?: string;
  toCueStart?: number;
  toCueEnd?: number;
  toDelaySecs: number;
  /** End of the whole layout in mix seconds — how late the mix may still be entered. */
  spanSecs: number;
  /** Mix seconds at which the UI should show the incoming track (transition midpoint). */
  uiSwitchSecs: number;
  fromVol: MixEnvSegment[];
  toVol: MixEnvSegment[];
  fromLp: MixEnvSegment[];
  fromHp: MixEnvSegment[];
  toLp: MixEnvSegment[];
  toHp: MixEnvSegment[];
  fromSpeed: MixEnvSegment[];
  toSpeed: MixEnvSegment[];
}

/**
 * Where the mix begins on the outgoing track, in track-relative seconds.
 *
 * Not the playhead: that is the editor's preview cursor and defaults to the very end
 * of the outgoing track, so keying playback off it would put every transition past
 * the end of its own track. The layout itself says where the mix is — the transition
 * container if there is one, otherwise the earliest block.
 */
function mixOriginOf(blocks: MixBlock[]): number | null {
  const container = primaryTransition(blocks);
  if (container) return Math.max(0, container.startFromSecs);
  const starts = blocks.map((b) => b.startFromSecs);
  if (starts.length === 0) return null;
  return Math.max(0, Math.min(...starts));
}

/** End of the last thing on the layout, in mix seconds. */
function layoutSpanSecs(blocks: MixBlock[], mixOriginSecs: number): number {
  if (blocks.length === 0) return 0;
  return Math.max(0, Math.max(...blocks.map(blockEnd)) - mixOriginSecs);
}

/**
 * When the UI should flip to the incoming track, in mix seconds.
 *
 * Halfway through the transition block: by then the incoming track is the one the
 * listener is following, so leaving the old title up until the outgoing deck finally
 * dies reads as a stuck UI.
 */
function uiSwitchSecs(blocks: MixBlock[], mixOriginSecs: number): number {
  const container = primaryTransition(blocks);
  if (container) {
    return Math.max(
      0,
      container.startFromSecs - mixOriginSecs + container.durationSecs / 2,
    );
  }
  return layoutSpanSecs(blocks, mixOriginSecs) / 2;
}

/**
 * Where the outgoing deck can be dropped, in mix-clock seconds — or null to let it
 * play to its natural end.
 *
 * Only when the user's own fade lands on silence: the engine holds an envelope's
 * final value, so a deck whose gain ends at 0 is inaudible from then on and there
 * is nothing to gain from decoding the rest of the file. A fade that stops above
 * silence is still audible, so that deck runs out normally — same as the preview.
 */
function fromDeckEndSecs(blocks: MixBlock[], mixOriginSecs: number): number | null {
  const vol = blocks.filter(
    (b) =>
      b.kind === 'volume' &&
      (b.targetLane ?? 'from') === 'from' &&
      b.durationSecs > 0.02,
  );
  if (vol.length === 0) return null;

  const last = vol.reduce((a, b) => (blockEnd(b) >= blockEnd(a) ? b : a));
  const endsSilent =
    sampleEnvelope(last.params.envelope, 1, normalizeCurve(last.params.curve)) <=
    0.02;
  if (!endsSilent) return null;

  // Anything still scheduled past that fade keeps the deck relevant — its own
  // automation, or the transition container the mix lives in. A `to`-lane block
  // does not: this deck is already silent by then.
  const tail = Math.max(
    blockEnd(last),
    ...blocks.filter((b) => b.targetLane !== 'to').map(blockEnd),
  );
  return Math.max(0.05, tail - mixOriginSecs);
}

/** Geometry every consumer of one saved edge starts from. */
interface MixEdgePlan {
  blocks: MixBlock[];
  /** Mix origin on the outgoing track, in track-relative seconds. */
  cut: number;
  /** Incoming track's own position at mix time 0 — negative means it waits. */
  tTo: number;
}

/**
 * Resolve one saved edge, or null when it can't produce an audible transition.
 *
 * Rejected: the layout holds no blocks at all (pan/zoom-only memory is left over
 * from merely opening the editor and must not overlay tracks on its own), the mix
 * starts past the end of the outgoing track, or the incoming track has already
 * finished by then.
 */
function planMixEdge(
  from: MusicFile,
  to: MusicFile,
  memory: MixTransitionMemory,
): MixEdgePlan | null {
  const blocks = memory.blocks ?? [];
  const cut = mixOriginOf(blocks);
  if (cut == null) return null;

  const fromLen = trackPlayableSecs(from);
  const toLen = trackPlayableSecs(to);

  // The cut must leave audible tail on the outgoing track to mix over.
  if (fromLen > 0 && cut >= fromLen - 0.15) return null;

  const tTo = cut + (memory.toViewStart - memory.fromViewStart);
  if (toLen > 0 && tTo >= toLen - 0.05) return null;

  return { blocks, cut, tTo };
}

/**
 * Build the arm payload for one playlist edge from its saved editor state.
 *
 * Returns null when the edge can't produce an audible transition — see `planMixEdge`.
 */
export function buildArmMixArgs(
  from: MusicFile,
  to: MusicFile,
  memory: MixTransitionMemory,
): ArmMixArgs | null {
  const plan = planMixEdge(from, to, memory);
  if (!plan) return null;
  const { blocks, cut, tTo } = plan;

  const fromLen = trackPlayableSecs(from);
  const toDelaySecs = tTo < 0 ? -tTo : 0;
  const toStartRel = tTo < 0 ? 0 : tTo;

  const envelopes = mixEnvelopeArgs(blocks, cut);
  const naturalTail = fromLen > 0 ? fromLen - cut : 600 - cut;
  const fadeEnd = fromDeckEndSecs(blocks, cut);
  const fromDurationSecs = Math.max(
    0.05,
    fadeEnd != null ? Math.min(naturalTail, fadeEnd) : naturalTail,
  );
  const toCueEnd = cueEndOf(to);

  return {
    fromPath: from.path,
    startAtSecs: absTrackTime(from, cut),
    fromDurationSecs,
    toPath: to.path,
    toAudioPath: mixAudioPathFor(to),
    toCueStart: absTrackTime(to, toStartRel),
    toCueEnd: toCueEnd ?? undefined,
    toDelaySecs,
    spanSecs: layoutSpanSecs(blocks, cut),
    uiSwitchSecs: uiSwitchSecs(blocks, cut),
    ...envelopes,
  };
}

/** Where one edge hands over, on each track's own timeline. */
export interface MixEdgeHandoff {
  /** Outgoing track's position where the transition starts — its tail is cut there. */
  fromExitSecs: number;
  /** Incoming track's position at that same instant — its head is already spent. */
  toEnterSecs: number;
}

/**
 * Handoff point of one saved edge, or null when the edge won't fire.
 *
 * Measured at the start of the transition, not its middle: the overlap belongs to both
 * tracks. The outgoing track keeps everything up to the transition and loses the tail
 * that plays over the incoming one; the incoming track keeps everything from wherever
 * the layout drops it, overlap included. Spans therefore overlap rather than tile —
 * two tracks really are sounding at once.
 */
export function mixEdgeHandoff(
  from: MusicFile,
  to: MusicFile,
  memory: MixTransitionMemory,
): MixEdgeHandoff | null {
  const plan = planMixEdge(from, to, memory);
  if (!plan) return null;
  return {
    fromExitSecs: plan.cut,
    toEnterSecs: Math.max(0, plan.tTo),
  };
}

/**
 * How long each track actually runs in a mixed playlist, keyed by track path.
 *
 * A mixed track is cut where its transition into the next one begins, and picked up
 * wherever the previous transition dropped it — so the file's length stops describing
 * what the playlist plays. The overlap counts for both tracks: each number is how long
 * that track is audible, so they sum to more than the playlist's wall-clock length.
 *
 * Only tracks an edge actually moves get an entry — everything else keeps its own
 * duration. `tracks` must be in play order, since the edges are read pairwise exactly
 * as playback walks them.
 */
export function mixedTrackDurations(
  tracks: MusicFile[],
  playlistId: string,
): Map<string, number> {
  const spans = new Map<string, number>();
  if (!playlistId || tracks.length < 2) return spans;

  // Carried from the previous edge: where this track gets picked up.
  let enterSecs = 0;
  for (let i = 0; i < tracks.length; i += 1) {
    const track = tracks[i]!;
    const next = tracks[i + 1];
    const memory = next
      ? loadMixTransitionMemory(playlistId, track.path, next.path)
      : null;
    const edge = next && memory ? mixEdgeHandoff(track, next, memory) : null;

    // An unknown duration caps nothing; a known one keeps a long transition from
    // reporting more track than there is.
    const playable = trackPlayableSecs(track);
    const cap = playable > 0 ? playable : Number.POSITIVE_INFINITY;
    const start = Math.min(enterSecs, cap);
    const end = Math.min(edge ? edge.fromExitSecs : playable, cap);
    if ((edge || start > 0) && end > 0) {
      spans.set(track.path, Math.max(0, end - start));
    }

    enterSecs = edge ? edge.toEnterSecs : 0;
  }
  return spans;
}
