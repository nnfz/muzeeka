/**
 * Extract the juiciest (most vibrant) color from a cover image
 * and push it into the global CSS accent variables.
 */

const DEFAULT_ACCENT = {
  accent: '#8b5cf6',
  hover: '#7c3aed',
  glow: 'rgba(139, 92, 246, 0.35)',
  soft: 'rgba(139, 92, 246, 0.12)',
  border: 'rgba(139, 92, 246, 0.25)',
};

let applyToken = 0;
let lastSrc: string | null = null;

function clamp01(n: number): number {
  return Math.min(1, Math.max(0, n));
}

function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  r /= 255;
  g /= 255;
  b /= 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h = 0;
  switch (max) {
    case r:
      h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
      break;
    case g:
      h = ((b - r) / d + 2) / 6;
      break;
    default:
      h = ((r - g) / d + 4) / 6;
      break;
  }
  return [h, s, l];
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
  if (s === 0) {
    const v = Math.round(l * 255);
    return [v, v, v];
  }
  const hue2rgb = (p: number, q: number, t: number) => {
    let tt = t;
    if (tt < 0) tt += 1;
    if (tt > 1) tt -= 1;
    if (tt < 1 / 6) return p + (q - p) * 6 * tt;
    if (tt < 1 / 2) return q;
    if (tt < 2 / 3) return p + (q - p) * (2 / 3 - tt) * 6;
    return p;
  };
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  return [
    Math.round(hue2rgb(p, q, h + 1 / 3) * 255),
    Math.round(hue2rgb(p, q, h) * 255),
    Math.round(hue2rgb(p, q, h - 1 / 3) * 255),
  ];
}

function toHex(r: number, g: number, b: number): string {
  return (
    '#' +
    [r, g, b]
      .map((v) => Math.max(0, Math.min(255, v)).toString(16).padStart(2, '0'))
      .join('')
  );
}

/** Score how “juicy” a pixel is — high sat, mid lightness wins. */
function juicyScore(r: number, g: number, b: number): number {
  const [, s, l] = rgbToHsl(r, g, b);
  if (s < 0.18) return 0;
  if (l < 0.08 || l > 0.92) return 0;
  // Prefer vivid mid-tones (not muddy darks / pastel lights)
  const lightPref = 1 - Math.abs(l - 0.48) * 1.4;
  if (lightPref <= 0) return 0;
  // Saturation squared emphasizes the “juiciest”
  return s * s * lightPref * (0.55 + s * 0.45);
}

/**
 * Nudge extracted color so it works as a UI accent on a dark theme:
 * keep hue, boost saturation a bit, clamp lightness.
 */
function tuneForAccent(r: number, g: number, b: number): [number, number, number] {
  let [h, s, l] = rgbToHsl(r, g, b);
  s = clamp01(Math.max(s, 0.52) * 1.08);
  l = clamp01(Math.min(0.62, Math.max(0.42, l)));
  return hslToRgb(h, s, l);
}

function darken(r: number, g: number, b: number, amount: number): [number, number, number] {
  return [
    Math.round(r * (1 - amount)),
    Math.round(g * (1 - amount)),
    Math.round(b * (1 - amount)),
  ];
}

function applyRgb(r: number, g: number, b: number) {
  const [tr, tg, tb] = tuneForAccent(r, g, b);
  const [hr, hg, hb] = darken(tr, tg, tb, 0.12);
  const root = document.documentElement;
  root.style.setProperty('--accent', toHex(tr, tg, tb));
  root.style.setProperty('--accent-hover', toHex(hr, hg, hb));
  root.style.setProperty('--accent-glow', `rgba(${tr}, ${tg}, ${tb}, 0.35)`);
  root.style.setProperty('--accent-soft', `rgba(${tr}, ${tg}, ${tb}, 0.12)`);
  root.style.setProperty('--border-accent', `rgba(${tr}, ${tg}, ${tb}, 0.25)`);
}

export function resetCoverAccent() {
  applyToken += 1;
  lastSrc = null;
  const root = document.documentElement;
  root.style.setProperty('--accent', DEFAULT_ACCENT.accent);
  root.style.setProperty('--accent-hover', DEFAULT_ACCENT.hover);
  root.style.setProperty('--accent-glow', DEFAULT_ACCENT.glow);
  root.style.setProperty('--accent-soft', DEFAULT_ACCENT.soft);
  root.style.setProperty('--border-accent', DEFAULT_ACCENT.border);
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    // Asset / convertFileSrc URLs are same-origin in Tauri; data: needs no CORS.
    if (!src.startsWith('data:')) {
      img.crossOrigin = 'anonymous';
    }
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error('cover load failed'));
    img.src = src;
  });
}

/**
 * Sample a downscaled cover and return the juiciest RGB, or null.
 */
export async function extractVibrantFromCover(
  imageSrc: string,
): Promise<[number, number, number] | null> {
  const img = await loadImage(imageSrc);
  const size = 48;
  const canvas = document.createElement('canvas');
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  if (!ctx) return null;

  ctx.drawImage(img, 0, 0, size, size);
  let data: ImageData;
  try {
    data = ctx.getImageData(0, 0, size, size);
  } catch {
    // Tainted canvas — can't sample
    return null;
  }

  // Quantize into buckets so we pick a dominant juicy cluster, not a single noisy pixel.
  const buckets = new Map<string, { r: number; g: number; b: number; score: number; n: number }>();

  const { data: px } = data;
  for (let i = 0; i < px.length; i += 4) {
    const a = px[i + 3];
    if (a < 200) continue;
    const r = px[i];
    const g = px[i + 1];
    const b = px[i + 2];
    const score = juicyScore(r, g, b);
    if (score <= 0) continue;

    // 5-bit quantization (~32 levels)
    const key = `${r >> 3},${g >> 3},${b >> 3}`;
    const bucket = buckets.get(key);
    if (bucket) {
      bucket.r += r;
      bucket.g += g;
      bucket.b += b;
      bucket.score += score;
      bucket.n += 1;
    } else {
      buckets.set(key, { r, g, b, score, n: 1 });
    }
  }

  if (buckets.size === 0) {
    // Fallback: average mid-tone non-extreme pixels
    let sr = 0;
    let sg = 0;
    let sb = 0;
    let n = 0;
    for (let i = 0; i < px.length; i += 4) {
      if (px[i + 3] < 200) continue;
      const r = px[i];
      const g = px[i + 1];
      const b = px[i + 2];
      const [, , l] = rgbToHsl(r, g, b);
      if (l < 0.15 || l > 0.85) continue;
      sr += r;
      sg += g;
      sb += b;
      n += 1;
    }
    if (n === 0) return null;
    return [Math.round(sr / n), Math.round(sg / n), Math.round(sb / n)];
  }

  let best: { r: number; g: number; b: number; score: number; n: number } | null = null;
  for (const bucket of buckets.values()) {
    // Weight by total juicy score (frequency * vibrance)
    if (!best || bucket.score > best.score) best = bucket;
  }
  if (!best) return null;
  return [
    Math.round(best.r / best.n),
    Math.round(best.g / best.n),
    Math.round(best.b / best.n),
  ];
}

/**
 * Update global accent from a cover image URL (asset path, convertFileSrc, or data URL).
 * Pass null to restore the default purple.
 */
export async function setAccentFromCoverSrc(src: string | null | undefined) {
  const next = src?.trim() || null;
  if (next === lastSrc) return;
  lastSrc = next;

  if (!next) {
    resetCoverAccent();
    return;
  }

  const token = ++applyToken;
  try {
    const rgb = await extractVibrantFromCover(next);
    if (token !== applyToken) return;
    if (!rgb) {
      // Keep previous accent if sampling failed mid-track
      return;
    }
    applyRgb(rgb[0], rgb[1], rgb[2]);
  } catch {
    if (token !== applyToken) return;
    // Leave current accent; don't thrash to default on one bad load
  }
}
