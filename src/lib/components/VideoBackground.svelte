<script lang="ts">
  interface Props {
    /** Asset-protocol URL of the looping video, or null for nothing. */
    src?: string | null;
    /** Fullscreen is open. */
    active?: boolean;
    /** Audio is paused — the video freezes with it. */
    paused?: boolean;
    class?: string;
  }

  let {
    src = null,
    active = true,
    paused = false,
    class: className = '',
  }: Props = $props();

  let videoEl = $state<HTMLVideoElement | undefined>();
  let ready = $state(false);
  let failed = $state(false);
  /** OS window focus — same reasoning as KawarpBackground: don't decode while hidden. */
  let windowActive = $state(
    typeof document === 'undefined'
      ? true
      : document.visibilityState === 'visible'
  );

  let shouldPlay = $derived(active && windowActive && !paused && !failed && !!src);

  // Reset per source so a failed file doesn't hide a later working one.
  $effect(() => {
    void src;
    ready = false;
    failed = false;
  });

  // WebView2 rejects play() while the tab is backgrounded, so it is retried
  // whenever any input to shouldPlay changes rather than only on src change.
  $effect(() => {
    const video = videoEl;
    if (!video) return;
    if (!shouldPlay) {
      video.pause();
      return;
    }
    const attempt = video.play();
    if (attempt) {
      attempt.catch(() => {
        // Autoplay refused (usually a transient background-tab block). The next
        // focus/visibility change re-runs this effect.
      });
    }
  });

  /**
   * Only page visibility gates playback, not OS focus (unlike KawarpBackground,
   * which stops its WebGL loop on blur). A background video that freezes because
   * the user clicked another window — with fullscreen still visible on a second
   * monitor — looks broken; video decode is also far cheaper than the warp shader.
   */
  $effect(() => {
    const sync = () => {
      windowActive = document.visibilityState === 'visible';
    };
    sync();
    document.addEventListener('visibilitychange', sync);
    return () => document.removeEventListener('visibilitychange', sync);
  });
</script>

<div class="video-background {className}">
  {#if src && !failed}
    <!-- svelte-ignore a11y_media_has_caption -->
    <video
      bind:this={videoEl}
      class:is-ready={ready}
      src={src}
      loop
      muted
      playsinline
      preload="auto"
      disablepictureinpicture
      tabindex="-1"
      aria-hidden="true"
      oncanplay={() => (ready = true)}
      onerror={() => {
        failed = true;
        ready = false;
      }}
    ></video>
  {/if}
</div>

<style>
  .video-background {
    position: absolute;
    inset: 0;
    z-index: 0;
    overflow: hidden;
    background: #050508;
    isolation: isolate;
  }

  .video-background::before {
    content: '';
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    z-index: 1;
    pointer-events: none;
  }

  .video-background video {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    opacity: 0;
    transition: opacity 480ms ease;
    /* Match the Kawarp look so switching background modes is not jarring. */
    filter: saturate(1.15) brightness(0.85);
  }

  .video-background video.is-ready {
    opacity: 1;
  }
</style>
