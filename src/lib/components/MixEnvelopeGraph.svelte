<script lang="ts">
  import {
    formatEnvelopeDisplay,
    normalizeEnvelope,
    sampleEnvelope,
    type EnvelopeCurve,
    type EnvelopePoint,
    type MixBlockKind,
  } from "$lib/mix/blocks";

  interface Props {
    kind: MixBlockKind;
    points: EnvelopePoint[];
    curve?: EnvelopeCurve;
    accent?: string;
    /** Called with a fresh normalized point list. */
    onChange: (points: EnvelopePoint[]) => void;
    /** Linear vs smooth interpolation. */
    onCurveChange?: (curve: EnvelopeCurve) => void;
    /** Once at the start of a drag/add/reset gesture (for undo). */
    onGestureStart?: () => void;
    label?: string;
  }

  let {
    kind,
    points,
    curve = "linear",
    accent = "#7c5cff",
    onChange,
    onCurveChange,
    onGestureStart,
    label = "Envelope",
  }: Props = $props();

  let gestureStarted = false;

  function beginGesture() {
    if (gestureStarted) return;
    gestureStarted = true;
    onGestureStart?.();
  }

  let canvas = $state<HTMLCanvasElement | null>(null);
  let wrap = $state<HTMLDivElement | null>(null);

  let drag = $state<{
    index: number;
    pointerId: number;
  } | null>(null);

  const PAD = { l: 28, r: 8, t: 8, b: 16 };

  function pts(): EnvelopePoint[] {
    return normalizeEnvelope(points);
  }

  function geom() {
    const el = canvas;
    if (!el) return null;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const cssW = el.clientWidth;
    const cssH = el.clientHeight;
    if (cssW < 8 || cssH < 8) return null;
    const w = Math.round(cssW * dpr);
    const h = Math.round(cssH * dpr);
    if (el.width !== w || el.height !== h) {
      el.width = w;
      el.height = h;
    }
    const ctx = el.getContext("2d");
    if (!ctx) return null;
    return { ctx, dpr, cssW, cssH, ...PAD };
  }

  function tToX(t: number, cssW: number): number {
    const inner = cssW - PAD.l - PAD.r;
    return PAD.l + Math.max(0, Math.min(1, t)) * inner;
  }

  function vToY(v: number, cssH: number): number {
    const inner = cssH - PAD.t - PAD.b;
    return PAD.t + (1 - Math.max(0, Math.min(1, v))) * inner;
  }

  function xToT(x: number, cssW: number): number {
    const inner = cssW - PAD.l - PAD.r;
    if (inner <= 0) return 0;
    return Math.max(0, Math.min(1, (x - PAD.l) / inner));
  }

  function yToV(y: number, cssH: number): number {
    const inner = cssH - PAD.t - PAD.b;
    if (inner <= 0) return 0;
    return Math.max(0, Math.min(1, 1 - (y - PAD.t) / inner));
  }

  function draw() {
    const g = geom();
    if (!g) return;
    const { ctx, dpr, cssW, cssH } = g;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);

    // Grid
    ctx.strokeStyle = "rgba(255,255,255,0.06)";
    ctx.lineWidth = 1;
    for (let i = 0; i <= 4; i++) {
      const y = PAD.t + ((cssH - PAD.t - PAD.b) * i) / 4;
      ctx.beginPath();
      ctx.moveTo(PAD.l, y);
      ctx.lineTo(cssW - PAD.r, y);
      ctx.stroke();
    }
    for (let i = 0; i <= 4; i++) {
      const x = PAD.l + ((cssW - PAD.l - PAD.r) * i) / 4;
      ctx.beginPath();
      ctx.moveTo(x, PAD.t);
      ctx.lineTo(x, cssH - PAD.b);
      ctx.stroke();
    }

    // Y labels
    ctx.fillStyle = "rgba(200,200,210,0.55)";
    ctx.font = "9px system-ui, sans-serif";
    ctx.textAlign = "right";
    ctx.textBaseline = "middle";
    for (const v of [1, 0.5, 0]) {
      const y = vToY(v, cssH);
      ctx.fillText(formatEnvelopeDisplay(kind, v), PAD.l - 4, y);
    }

    const list = pts();
    if (list.length < 2) return;

    // Dense samples so smooth mode draws as a curve (not polyline of knots).
    const steps = Math.max(48, Math.floor((cssW - PAD.l - PAD.r) / 2));
    const samples: { t: number; v: number }[] = [];
    for (let i = 0; i <= steps; i++) {
      const t = i / steps;
      samples.push({ t, v: sampleEnvelope(list, t, curve) });
    }

    // Fill under curve
    ctx.beginPath();
    ctx.moveTo(tToX(samples[0]!.t, cssW), vToY(0, cssH));
    for (const p of samples) {
      ctx.lineTo(tToX(p.t, cssW), vToY(p.v, cssH));
    }
    ctx.lineTo(tToX(samples[samples.length - 1]!.t, cssW), vToY(0, cssH));
    ctx.closePath();
    ctx.fillStyle = accent + "33";
    ctx.fill();

    // Curve stroke
    ctx.beginPath();
    ctx.moveTo(tToX(samples[0]!.t, cssW), vToY(samples[0]!.v, cssH));
    for (let i = 1; i < samples.length; i++) {
      ctx.lineTo(tToX(samples[i]!.t, cssW), vToY(samples[i]!.v, cssH));
    }
    ctx.strokeStyle = accent;
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    ctx.stroke();

    // Knots
    for (let i = 0; i < list.length; i++) {
      const p = list[i]!;
      const x = tToX(p.t, cssW);
      const y = vToY(p.v, cssH);
      ctx.beginPath();
      ctx.arc(x, y, drag?.index === i ? 6 : 4.5, 0, Math.PI * 2);
      ctx.fillStyle = "#fff";
      ctx.fill();
      ctx.strokeStyle = accent;
      ctx.lineWidth = 2;
      ctx.stroke();
    }
  }

  $effect(() => {
    void points;
    void kind;
    void accent;
    void curve;
    void canvas;
    // rAF so layout has size after expand
    const id = requestAnimationFrame(() => draw());
    return () => cancelAnimationFrame(id);
  });

  function setCurve(next: EnvelopeCurve) {
    if (next === curve) return;
    beginGesture();
    onCurveChange?.(next);
    gestureStarted = false;
  }

  function clientToLocal(e: PointerEvent) {
    if (!canvas) return null;
    const rect = canvas.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top, cssW: rect.width, cssH: rect.height };
  }

  function hitIndex(x: number, y: number, cssW: number, cssH: number): number {
    const list = pts();
    let best = -1;
    let bestD = 12;
    for (let i = 0; i < list.length; i++) {
      const p = list[i]!;
      const dx = tToX(p.t, cssW) - x;
      const dy = vToY(p.v, cssH) - y;
      const d = Math.hypot(dx, dy);
      if (d < bestD) {
        bestD = d;
        best = i;
      }
    }
    return best;
  }

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0 || !canvas) return;
    e.preventDefault();
    e.stopPropagation();
    beginGesture();
    const loc = clientToLocal(e);
    if (!loc) return;
    let list = pts().map((p) => ({ ...p }));
    let index = hitIndex(loc.x, loc.y, loc.cssW, loc.cssH);
    if (index < 0) {
      // Add point
      const t = xToT(loc.x, loc.cssW);
      const v = yToV(loc.y, loc.cssH);
      list.push({ t, v });
      list = normalizeEnvelope(list);
      // Find new index near t
      index = list.findIndex((p) => Math.abs(p.t - t) < 0.02 && Math.abs(p.v - v) < 0.05);
      if (index < 0) {
        index = list.findIndex((p) => Math.abs(p.t - t) < 0.05);
      }
      if (index < 0) index = Math.max(0, list.length - 2);
      onChange(list);
    }
    canvas.setPointerCapture(e.pointerId);
    drag = { index, pointerId: e.pointerId };
    draw();
  }

  function onPointerMove(e: PointerEvent) {
    if (!drag || drag.pointerId !== e.pointerId) return;
    const loc = clientToLocal(e);
    if (!loc) return;
    const list = pts().map((p) => ({ ...p }));
    const i = drag.index;
    if (i < 0 || i >= list.length) return;
    let t = xToT(loc.x, loc.cssW);
    const v = yToV(loc.y, loc.cssH);
    // Endpoints locked on t=0 / t=1
    if (i === 0) t = 0;
    else if (i === list.length - 1) t = 1;
    else {
      const lo = list[i - 1]!.t + 0.01;
      const hi = list[i + 1]!.t - 0.01;
      t = Math.max(lo, Math.min(hi, t));
    }
    list[i] = { t, v };
    onChange(normalizeEnvelope(list));
  }

  function onPointerUp(e: PointerEvent) {
    if (!drag || drag.pointerId !== e.pointerId) return;
    try {
      canvas?.releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    drag = null;
    gestureStarted = false;
    draw();
  }

  function onDblClick(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (!canvas) return;
    const rect = canvas.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    const index = hitIndex(x, y, rect.width, rect.height);
    // Double-click point (not endpoint) → remove
    if (index > 0 && index < pts().length - 1) {
      beginGesture();
      const list = pts().filter((_, i) => i !== index);
      onChange(normalizeEnvelope(list));
      gestureStarted = false;
    }
  }

  function resetFlat() {
    beginGesture();
    const mid = pts().reduce((s, p) => s + p.v, 0) / Math.max(1, pts().length);
    onChange([
      { t: 0, v: mid },
      { t: 1, v: mid },
    ]);
    gestureStarted = false;
  }
</script>

<div class="mix-env" bind:this={wrap} style:--env-accent={accent}>
  <div class="mix-env-head">
    <span class="mix-env-label">{label}</span>
    <div class="mix-env-curve" role="group" aria-label="Curve mode">
      <button
        type="button"
        class="mix-env-curve-btn"
        class:is-on={curve === "linear"}
        title="Linear segments"
        onclick={() => setCurve("linear")}
      >
        Lin
      </button>
      <button
        type="button"
        class="mix-env-curve-btn"
        class:is-on={curve === "smooth"}
        title="Smooth curve (Catmull-Rom)"
        onclick={() => setCurve("smooth")}
      >
        Smooth
      </button>
    </div>
    <span class="mix-env-hint">drag · click add · dbl-click remove</span>
    <button type="button" class="mix-env-reset" onclick={resetFlat}>Flat</button>
  </div>
  <canvas
    class="mix-env-canvas"
    bind:this={canvas}
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={onPointerUp}
    ondblclick={onDblClick}
    role="img"
    aria-label="{label} automation graph"
  ></canvas>
</div>

<style>
  .mix-env {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-height: 0;
    flex: 1;
    padding: 4px 6px 6px;
    pointer-events: auto;
  }

  .mix-env-head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .mix-env-label {
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--env-accent, #7c5cff);
    flex-shrink: 0;
  }

  .mix-env-curve {
    display: inline-flex;
    flex-shrink: 0;
    border-radius: 5px;
    border: 1px solid rgba(255, 255, 255, 0.1);
    overflow: hidden;
  }

  .mix-env-curve-btn {
    height: 18px;
    padding: 0 7px;
    border: none;
    background: transparent;
    color: rgba(200, 200, 210, 0.65);
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .mix-env-curve-btn + .mix-env-curve-btn {
    border-left: 1px solid rgba(255, 255, 255, 0.1);
  }

  .mix-env-curve-btn:hover {
    color: rgba(240, 240, 245, 0.95);
    background: rgba(255, 255, 255, 0.06);
  }

  .mix-env-curve-btn.is-on {
    color: var(--env-accent, #7c5cff);
    background: color-mix(in srgb, var(--env-accent, #7c5cff) 18%, transparent);
  }

  .mix-env-hint {
    font-size: 9px;
    color: rgba(200, 200, 210, 0.5);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .mix-env-reset {
    margin-left: auto;
    flex-shrink: 0;
    height: 18px;
    padding: 0 6px;
    border-radius: 4px;
    border: 1px solid rgba(255, 255, 255, 0.1);
    background: rgba(255, 255, 255, 0.05);
    color: rgba(240, 240, 245, 0.8);
    font-size: 9px;
    font-weight: 600;
    cursor: pointer;
  }

  .mix-env-reset:hover {
    background: rgba(255, 255, 255, 0.1);
  }

  .mix-env-canvas {
    display: block;
    width: 100%;
    height: 88px;
    border-radius: 6px;
    background: rgba(0, 0, 0, 0.28);
    border: 1px solid rgba(255, 255, 255, 0.06);
    cursor: crosshair;
    touch-action: none;
  }
</style>
