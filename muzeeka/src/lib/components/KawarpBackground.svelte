<script lang="ts">
  import { tick, untrack } from 'svelte';
  import type { KawarpOptions } from '@kawarp/core';

  interface Props extends KawarpOptions {
    src?: string | null;
    active?: boolean;
    class?: string;
  }

  const BG_TINT: [number, number, number] = [
    10 / 255,
    10 / 255,
    15 / 255,
  ];

  let {
    src = null,
    active = true,
    class: className = '',
    warpIntensity = 3.85,
    blurPasses = 8,
    animationSpeed = 1,
    transitionDuration = 700,
    saturation = 1.65,
    tintColor = BG_TINT,
    tintIntensity = 0.06,
    dithering = 0.01,
    scale = 1.25,
  }: Props = $props();

  let containerEl = $state<HTMLDivElement | undefined>();
  let canvasEl = $state<HTMLCanvasElement | undefined>();
  let webglFailed = $state(false);
  let imageReady = $state(false);
  let windowActive = $state(
    typeof document === 'undefined' ? true : document.visibilityState === 'visible' && document.hasFocus()
  );

  let kawarp: import('@kawarp/core').Kawarp | null = null;
  let kawarpEpoch = $state(0);
  let currentSrc: string | null = null;
  let loadToken = 0;
  let resizeObserver: ResizeObserver | null = null;
  let resizeTimeout: ReturnType<typeof setTimeout> | null = null;
  let initGeneration = 0;

  // Dual-layer blurred fallback for smooth track switches
  let fbA = $state<string | null>(null);
  let fbB = $state<string | null>(null);
  let fbShowB = $state(false);
  let fbToken = 0;

  function getOptions(): KawarpOptions {
    return {
      warpIntensity,
      blurPasses,
      animationSpeed,
      transitionDuration,
      saturation,
      tintColor,
      tintIntensity,
      dithering,
      scale,
    };
  }

  function preloadImageOk(url: string): Promise<boolean> {
    return new Promise((resolve) => {
      const img = new Image();
      img.onload = () => resolve(true);
      img.onerror = () => resolve(false);
      img.src = url;
    });
  }

  async function crossfadeFallback(next: string | null) {
    // Never clear existing fallback to null mid-session — only swap to a loaded URL.
    if (!next) return;

    const current = fbShowB ? fbB : fbA;
    if (current === next) return;

    // First paint: show immediately (no crossfade from empty).
    if (!current) {
      fbA = next;
      fbB = null;
      fbShowB = false;
      return;
    }

    const token = ++fbToken;
    const ok = await preloadImageOk(next);
    if (token !== fbToken || !ok) return;

    if (fbShowB) {
      fbA = next;
      fbShowB = false;
    } else {
      fbB = next;
      fbShowB = true;
    }
  }

  async function loadCover(
    instance: import('@kawarp/core').Kawarp,
    url: string,
  ): Promise<void> {
    if (!url.startsWith('data:')) {
      try {
        const response = await fetch(url);
        if (!response.ok) throw new Error('fetch failed');
        const blob = await response.blob();
        await instance.loadBlob(blob);
        return;
      } catch {
        // Fall through to Image() — works for asset URLs and data URLs.
      }
    }

    const img = new Image();
    await new Promise<void>((resolve, reject) => {
      img.onload = () => resolve();
      img.onerror = () => reject(new Error('image load failed'));
      img.src = url;
    });
    instance.loadImageElement(img);
  }

  function updateSize() {
    if (!containerEl || !canvasEl || !kawarp) return;

    const rect = containerEl.getBoundingClientRect();
    const width = Math.max(1, Math.round(rect.width));
    const height = Math.max(1, Math.round(rect.height));
    if (canvasEl.width === width && canvasEl.height === height) return;

    canvasEl.width = width;
    canvasEl.height = height;
    kawarp.resize();
  }

  function scheduleResize() {
    if (resizeTimeout) clearTimeout(resizeTimeout);
    resizeTimeout = setTimeout(updateSize, 100);
  }

  function shouldAnimate(): boolean {
    return active && windowActive;
  }

  function syncAnimationState() {
    if (!kawarp || webglFailed) return;
    if (shouldAnimate()) {
      kawarp.start();
    } else {
      kawarp.stop();
    }
  }

  function disposeKawarp() {
    resizeObserver?.disconnect();
    resizeObserver = null;

    if (resizeTimeout) {
      clearTimeout(resizeTimeout);
      resizeTimeout = null;
    }

    kawarp?.dispose();
    kawarp = null;
    currentSrc = null;
    imageReady = false;
    kawarpEpoch += 1;
  }

  async function applyCover(
    instance: import('@kawarp/core').Kawarp,
    url: string,
    token: number,
  ): Promise<void> {
    await loadCover(instance, url);
    if (token !== loadToken || kawarp !== instance) return;
    currentSrc = url;
    imageReady = true;
  }

  async function bootstrap(
    generation: number,
    canvas: HTMLCanvasElement,
    container: HTMLDivElement,
  ) {
    await tick();
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    if (generation !== initGeneration) return;

    const { Kawarp } = await import('@kawarp/core');
    if (generation !== initGeneration) return;

    let instance: import('@kawarp/core').Kawarp;
    try {
      instance = untrack(() => new Kawarp(canvas, getOptions()));
    } catch {
      webglFailed = true;
      return;
    }

    if (generation !== initGeneration) {
      instance.dispose();
      return;
    }

    kawarp = instance;
    kawarpEpoch += 1;

    resizeObserver = new ResizeObserver(scheduleResize);
    resizeObserver.observe(container);
    updateSize();

    const url = untrack(() => src?.trim() || null);
    if (url) {
      const token = ++loadToken;
      try {
        await applyCover(instance, url, token);
      } catch {
        if (token === loadToken) imageReady = false;
      }
    }

    if (generation !== initGeneration) return;

    if (untrack(() => shouldAnimate())) {
      instance.start();
    }
  }

  $effect(() => {
    const canvas = canvasEl;
    const container = containerEl;
    if (webglFailed || !canvas || !container) {
      return;
    }

    const generation = ++initGeneration;
    void bootstrap(generation, canvas, container);

    return () => {
      initGeneration += 1;
      disposeKawarp();
    };
  });

  $effect(() => {
    void kawarpEpoch;
    void active;
    void windowActive;
    syncAnimationState();
  });

  $effect(() => {
    const updateActive = () => {
      windowActive = document.visibilityState === 'visible' && document.hasFocus();
    };

    updateActive();
    window.addEventListener('focus', updateActive);
    window.addEventListener('blur', updateActive);
    document.addEventListener('visibilitychange', updateActive);
    return () => {
      window.removeEventListener('focus', updateActive);
      window.removeEventListener('blur', updateActive);
      document.removeEventListener('visibilitychange', updateActive);
    };
  });

  // Blur fallback crossfade (always, even before WebGL is ready)
  $effect(() => {
    const next = src?.trim() || null;
    void crossfadeFallback(next);
  });

  // WebGL texture update — Kawarp transitionDuration handles the GL crossfade
  $effect(() => {
    const next = src?.trim() || null;
    const epoch = kawarpEpoch;
    const failed = webglFailed;

    if (failed || epoch === 0) return;

    const instance = kawarp;
    if (!instance) return;

    if (!next) {
      currentSrc = null;
      imageReady = false;
      return;
    }

    if (next === currentSrc) return;

    const token = ++loadToken;
    void applyCover(instance, next, token).catch(() => {
      if (token === loadToken) imageReady = false;
    });
  });
</script>

<div class="kawarp-background {className}" bind:this={containerEl}>
  {#if fbA}
    <img
      class="kawarp-fallback"
      class:is-visible={!fbShowB}
      src={fbA}
      alt=""
    />
  {/if}
  {#if fbB}
    <img
      class="kawarp-fallback"
      class:is-visible={fbShowB}
      src={fbB}
      alt=""
    />
  {/if}
  {#if !webglFailed}
    <canvas
      bind:this={canvasEl}
      class:image-ready={imageReady}
      aria-hidden="true"
    ></canvas>
  {/if}
</div>

<style>
  .kawarp-background {
    position: absolute;
    inset: 0;
    z-index: 0;
    overflow: hidden;
    background: #0a0a0f;
    /* Keep WebGL / fallback layers inside this box only */
    isolation: isolate;
  }

  .kawarp-background canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    opacity: 0;
    transition: opacity 480ms ease;
    z-index: 1;
  }

  .kawarp-background canvas.image-ready {
    opacity: 1;
  }

  .kawarp-fallback {
    position: absolute;
    inset: -20%;
    width: 140%;
    height: 140%;
    object-fit: cover;
    filter: blur(48px) saturate(1.55);
    transform: scale(1.1);
    opacity: 0;
    z-index: 0;
    transition: opacity 560ms cubic-bezier(0.33, 1, 0.68, 1);
    will-change: opacity;
  }

  .kawarp-fallback.is-visible {
    opacity: 0.88;
  }
</style>
