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
    warpIntensity = 0.85,
    blurPasses = 6,
    animationSpeed = 1,
    transitionDuration = 900,
    saturation = 1.65,
    tintColor = BG_TINT,
    tintIntensity = 0.06,
    dithering = 0.006,
    scale = 1.05,
  }: Props = $props();

  let containerEl = $state<HTMLDivElement | undefined>();
  let canvasEl = $state<HTMLCanvasElement | undefined>();
  let webglFailed = $state(false);
  let imageReady = $state(false);

  let kawarp: import('@kawarp/core').Kawarp | null = null;
  let currentSrc: string | null = null;
  let resizeObserver: ResizeObserver | null = null;
  let resizeTimeout: ReturnType<typeof setTimeout> | null = null;
  let initGeneration = 0;

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

    resizeObserver = new ResizeObserver(scheduleResize);
    resizeObserver.observe(container);
    updateSize();

    const url = untrack(() => src?.trim() || null);
    if (url) {
      try {
        await loadCover(instance, url);
        if (generation !== initGeneration) return;
        currentSrc = url;
        imageReady = true;
      } catch {
        imageReady = false;
      }
    }

    if (generation !== initGeneration) return;

    if (untrack(() => active)) {
      instance.start();
    }
  }

  $effect(() => {
    const canvas = canvasEl;
    const container = containerEl;
    const shouldRun = active && !webglFailed;

    if (!shouldRun || !canvas || !container) {
      kawarp?.stop();
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
    if (!kawarp || webglFailed || !active) return;

    const next = src?.trim() || null;
    if (!next || next === currentSrc) return;

    const instance = kawarp;
    const url = next;
    imageReady = false;

    void loadCover(instance, url)
      .then(() => {
        if (kawarp !== instance) return;
        currentSrc = url;
        imageReady = true;
      })
      .catch(() => {
        imageReady = false;
      });
  });
</script>

<div class="kawarp-background {className}" bind:this={containerEl}>
  {#if src && (!imageReady || webglFailed)}
    <img class="kawarp-fallback" src={src} alt="" />
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
    overflow: hidden;
  }

  .kawarp-background canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    opacity: 0;
    transition: opacity 320ms ease;
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
    opacity: 0.72;
  }
</style>