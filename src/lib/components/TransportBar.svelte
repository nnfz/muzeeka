<script lang="ts">
  import { getCoverSrc, resolveCoverSrc } from "$lib/coverCache";
  import { setAccentFromCoverSrc } from "$lib/coverAccent";
  import { COVER_PLACEHOLDER_SRC } from "$lib/coverPlaceholder";
  import {
    getPlayerStore,
    isEditablePlaylist,
    VIRTUAL_ALL_ID,
    VIRTUAL_LIKED_ID,
  } from "$lib/stores/player.svelte";
  import { exportAudioPathForTrack } from "$lib/trackPaths";
  import { startFileDrag } from "$lib/fileDrag";
  import {
    openContextMenuFromEvent,
    type ContextMenuItem,
  } from "$lib/contextMenu";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import { openTrackPropertiesWindow } from "$lib/stores/trackProperties.svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import FullscreenPlayer from "./FullscreenPlayer.svelte";
  import MediaSlider from "./MediaSlider.svelte";
  import TrackCover from "./TrackCover.svelte";
  import LikeButton from "./LikeButton.svelte";

  interface Props {
    fullscreenOpen?: boolean;
  }

  let { fullscreenOpen = $bindable(false) }: Props = $props();

  const player = getPlayerStore();

  const DRAG_THRESHOLD = 6;
  const MENU_WIDTH = 176;
  const SUBMENU_WIDTH = 176;
  const MENU_GAP = 4;
  const MENU_PADDING = 4;
  const MENU_ITEM_HEIGHT = 34;

  let contextMenu = $state<{ x: number; y: number } | null>(null);
  let playlistSubmenu = $state<{
    x: number;
    y: number;
    paths: string[];
    sourcePlaylistId: string;
    targetPlaylists: { id: string; name: string }[];
  } | null>(null);
  let menuToast = $state<string | null>(null);
  let menuToastTimer: ReturnType<typeof setTimeout> | null = null;

  let fileDragSession = $state<{
    x: number;
    y: number;
    path: string;
    iconPath: string | null;
    started: boolean;
    openFullscreenOnClick?: boolean;
  } | null>(null);
  let fileDragCaptureEl = $state<HTMLElement | null>(null);
  let fileDragPointerId = $state<number | null>(null);

  function cleanupFileDragSession() {
    window.removeEventListener("pointermove", onPlayerPointerMove);
    window.removeEventListener("pointerup", onPlayerPointerUp);
    window.removeEventListener("pointercancel", onPlayerPointerUp);
    window.removeEventListener("blur", onPlayerPointerCancel);
    document.removeEventListener(
      "visibilitychange",
      onPlayerFileDragVisibility,
    );

    if (fileDragCaptureEl && fileDragPointerId !== null) {
      try {
        if (fileDragCaptureEl.hasPointerCapture(fileDragPointerId)) {
          fileDragCaptureEl.releasePointerCapture(fileDragPointerId);
        }
      } catch {
        /* pointer may already be released */
      }
    }

    fileDragCaptureEl = null;
    fileDragPointerId = null;
    fileDragSession = null;
  }

  function onPlayerPointerCancel() {
    cleanupFileDragSession();
  }

  function onPlayerFileDragVisibility() {
    if (document.visibilityState === "hidden") {
      onPlayerPointerCancel();
    }
  }

  function beginFileDragSession(
    e: PointerEvent,
    options?: { openFullscreenOnClick?: boolean },
  ) {
    if (e.button !== 0) return;
    if (!player.currentFile || !player.currentTrack) return;
    if ((e.target as HTMLElement).closest(".like-btn-transport")) return;

    const path = exportAudioPathForTrack(
      player.currentTrack,
      player.currentFile,
    );
    if (!path) return;

    cleanupFileDragSession();

    fileDragSession = {
      x: e.clientX,
      y: e.clientY,
      path,
      iconPath: player.currentTrack.cover_path ?? null,
      started: false,
      openFullscreenOnClick: options?.openFullscreenOnClick,
    };
    fileDragCaptureEl = e.currentTarget as HTMLElement;
    fileDragPointerId = e.pointerId;
    fileDragCaptureEl.setPointerCapture(e.pointerId);

    window.addEventListener("pointermove", onPlayerPointerMove);
    window.addEventListener("pointerup", onPlayerPointerUp);
    window.addEventListener("pointercancel", onPlayerPointerUp);
    window.addEventListener("blur", onPlayerPointerCancel);
    document.addEventListener("visibilitychange", onPlayerFileDragVisibility);
  }

  function onCoverPointerDown(e: PointerEvent) {
    beginFileDragSession(e, { openFullscreenOnClick: true });
  }

  function onTextPointerDown(e: PointerEvent) {
    beginFileDragSession(e);
  }

  function showMenuToast(message: string, ms = 2400) {
    menuToast = message;
    if (menuToastTimer) clearTimeout(menuToastTimer);
    menuToastTimer = setTimeout(() => {
      menuToast = null;
      menuToastTimer = null;
    }, ms);
  }

  function closeContextMenu() {
    contextMenu = null;
    playlistSubmenu = null;
  }

  function closePlaylistSubmenu() {
    playlistSubmenu = null;
  }

  /** Playlist the current track is playing from (for menu actions). */
  function nowPlayingSourcePlaylistId(): string {
    return player.playingPlaylistId ?? player.activePlaylistId ?? "";
  }

  function openNowPlayingContextMenu(e: MouseEvent) {
    if (!player.currentTrack || !player.currentFile) return;
    const position = openContextMenuFromEvent(e, { width: 220, height: 264 });
    playlistSubmenu = null;
    contextMenu = position;
  }

  function revealCurrentOnDisk() {
    const track = player.currentTrack;
    const file = player.currentFile;
    if (!track || !file) return;
    const path = exportAudioPathForTrack(track, file);
    if (!path) return;
    void revealItemInDir(path).catch((err) => {
      console.error("Failed to reveal track on disk:", err);
      showMenuToast("Could not find track on disk");
    });
  }

  function getPlaylistSubmenuPosition(targetPlaylistsCount: number) {
    if (!contextMenu) return { x: 8, y: 8 };

    const estimatedHeight = Math.min(
      window.innerHeight - 16,
      MENU_PADDING * 2 + targetPlaylistsCount * MENU_ITEM_HEIGHT,
    );
    const preferredX = contextMenu.x + MENU_WIDTH + MENU_GAP;
    const x =
      preferredX + SUBMENU_WIDTH <= window.innerWidth - 8
        ? preferredX
        : Math.max(8, contextMenu.x - SUBMENU_WIDTH - MENU_GAP);
    const y = Math.min(
      Math.max(8, contextMenu.y + MENU_PADDING + MENU_ITEM_HEIGHT),
      window.innerHeight - estimatedHeight - 8,
    );

    return { x, y };
  }

  function confirmAddTracksToPlaylist(
    targetId: string,
    paths: string[],
    sourcePlaylistId: string,
  ) {
    const added = player.copyTracksToPlaylist(
      paths,
      targetId,
      sourcePlaylistId,
    );
    if (added > 0) {
      const targetName =
        player.playlists.find((playlist) => playlist.id === targetId)?.name ??
        "playlist";
      showMenuToast(
        `Added ${added} track${added !== 1 ? "s" : ""} to ${targetName}`,
      );
    }
    closeContextMenu();
  }

  function openPlaylistSubmenuForAdd(
    paths: string[],
    sourcePlaylistId: string,
  ) {
    const targetPlaylists = player.playlists.filter(
      (playlist) => playlist.id !== sourcePlaylistId,
    );
    if (targetPlaylists.length === 0) {
      closePlaylistSubmenu();
      return;
    }

    if (targetPlaylists.length === 1) {
      confirmAddTracksToPlaylist(
        targetPlaylists[0].id,
        paths,
        sourcePlaylistId,
      );
      return;
    }

    playlistSubmenu = {
      ...getPlaylistSubmenuPosition(targetPlaylists.length),
      paths,
      sourcePlaylistId,
      targetPlaylists: targetPlaylists.map(({ id, name }) => ({ id, name })),
    };
  }

  let trackMenuItems = $derived.by((): ContextMenuItem[] => {
    if (!contextMenu) return [];
    const track = player.currentTrack;
    const file = player.currentFile;
    if (!track || !file) return [];

    const path = track.path;
    const sourcePlaylistId = nowPlayingSourcePlaylistId();
    const items: ContextMenuItem[] = [];

    items.push({
      id: "find-on-disk",
      label: "Найти на диске",
      icon: "folder",
      onSelect: () => revealCurrentOnDisk(),
    });

    items.push({
      id: "properties",
      label: "Properties",
      icon: "properties",
      onSelect: () => void openTrackPropertiesWindow(track),
    });

    const availableTargetPlaylists = player.playlists.filter(
      (playlist) => playlist.id !== sourcePlaylistId,
    );
    items.push({
      id: "add-to-playlist",
      label: "Добавить в плейлист ›",
      icon: "playlist",
      disabled: availableTargetPlaylists.length === 0,
      closeOnSelect: false,
      onSelect: () => openPlaylistSubmenuForAdd([path], sourcePlaylistId),
    });

    const liked = player.isLiked(path);
    items.push({
      id: "like",
      label: liked ? "Remove from Liked" : "Add to Liked",
      icon: "heart",
      onSelect: () => player.toggleLike(path),
    });

    const pid = sourcePlaylistId;
    const isRealPlaylist =
      pid && pid !== VIRTUAL_ALL_ID && pid !== VIRTUAL_LIKED_ID;
    if (isRealPlaylist && isEditablePlaylist(pid)) {
      const inPlaylist = player.playlists
        .find((p) => p.id === pid)
        ?.tracks.some((t) => t.path === path);
      if (inPlaylist) {
        items.push({
          id: "delete",
          label: "Delete",
          icon: "delete",
          danger: true,
          onSelect: () => player.removeTrack(path, pid),
        });
      }
    }

    return items;
  });

  let playlistSubmenuItems = $derived.by((): ContextMenuItem[] => {
    if (!playlistSubmenu) return [];
    return playlistSubmenu.targetPlaylists.map((playlist) => ({
      id: `playlist-${playlist.id}`,
      label: playlist.name,
      icon: "playlist" as const,
      onSelect: () =>
        confirmAddTracksToPlaylist(
          playlist.id,
          playlistSubmenu!.paths,
          playlistSubmenu!.sourcePlaylistId,
        ),
    }));
  });

  function stopWindowClickForMenus(e: MouseEvent) {
    const target = e.target;
    if (target instanceof HTMLElement && target.closest(".context-menu"))
      return;
    closeContextMenu();
  }

  function stopWindowContextMenuForMenus(e: MouseEvent) {
    const target = e.target;
    if (target instanceof HTMLElement && target.closest(".context-menu"))
      return;
    closeContextMenu();
  }

  function handleMenuWindowKeydown(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    if (!contextMenu && !playlistSubmenu) return;
    e.preventDefault();
    if (playlistSubmenu) {
      closePlaylistSubmenu();
      return;
    }
    closeContextMenu();
  }

  function stopMenuEventPropagation(e: Event) {
    e.stopPropagation();
  }

  function onPlayerPointerMove(e: PointerEvent) {
    const session = fileDragSession;
    if (!session || session.started) return;

    const dx = e.clientX - session.x;
    const dy = e.clientY - session.y;
    if (Math.hypot(dx, dy) < DRAG_THRESHOLD) return;

    const { path, iconPath } = session;
    cleanupFileDragSession();
    void startFileDrag([path], { iconPath }).catch((err) => {
      console.error("Failed to start file drag:", err);
    });
  }

  function onPlayerPointerUp() {
    const session = fileDragSession;
    const shouldOpenFullscreen =
      !!session?.openFullscreenOnClick && !session.started;
    cleanupFileDragSession();
    if (shouldOpenFullscreen) {
      fullscreenOpen = true;
    }
  }

  $effect(() => {
    document.documentElement.classList.toggle(
      "fullscreen-active",
      fullscreenOpen,
    );
    return () => {
      document.documentElement.classList.remove("fullscreen-active");
    };
  });

  // Drop the menu if the current track disappears (stop / delete).
  $effect(() => {
    if (!player.hasTrack && (contextMenu || playlistSubmenu)) {
      closeContextMenu();
    }
  });

  // Accent color from the juiciest hue on the current cover (or placeholder).
  $effect(() => {
    const track = player.currentTrack;
    const hasTrack = player.hasTrack;
    const path = track?.cover_path ?? track?.cover_path_full ?? null;

    if (!hasTrack) {
      void setAccentFromCoverSrc(null);
      return;
    }

    let cancelled = false;

    const apply = (src: string) => {
      if (!cancelled) void setAccentFromCoverSrc(src);
    };

    if (!path) {
      apply(COVER_PLACEHOLDER_SRC);
      return () => {
        cancelled = true;
      };
    }

    const immediate = getCoverSrc(path);
    if (immediate) apply(immediate);

    void resolveCoverSrc(path).then((src) => {
      if (src) apply(src);
      else if (!cancelled) apply(COVER_PLACEHOLDER_SRC);
    });

    return () => {
      cancelled = true;
    };
  });
</script>

<div class="transport-bar glass">
  <div class="transport-content">
    <div class="transport-info">
      {#if player.hasTrack}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="np-drag-handle"
          oncontextmenu={openNowPlayingContextMenu}
        >
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="np-cover-hit"
            onpointerdown={onCoverPointerDown}
            title="Open fullscreen · drag to share"
          >
            <TrackCover track={player.currentTrack} />
          </div>
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="now-playing-text"
            onpointerdown={onTextPointerDown}
            title="Drag file to share"
          >
            <span class="np-title"
              >{player.nowPlayingTitle ?? player.currentFileName ?? ""}</span
            >
            {#if player.nowPlayingArtist}
              <span class="np-artist">{player.nowPlayingArtist}</span>
            {/if}
          </div>
        </div>

        {#if player.hasTrack && player.currentFile}
          <LikeButton file={player.currentFile} class="like-btn-transport" />
        {/if}
      {/if}
    </div>

    <div class="transport-controls">
      <button
        class="control-btn mode-btn"
        class:active={player.shuffleEnabled}
        onclick={() => player.toggleShuffle()}
        disabled={!player.hasPlayingTracks}
        aria-label={player.shuffleEnabled
          ? "Disable shuffle"
          : "Enable shuffle"}
        title={player.shuffleEnabled ? "Shuffle on" : "Shuffle"}
      >
        {#if player.shuffleEnabled}
          <span
            class="control-icon"
            style:--control-icon={"url('/icons/shuffle.svg')"}
            aria-hidden="true"
          ></span>
        {:else}
          <span
            class="control-icon"
            style:--control-icon={"url('/icons/noshuffle.svg')"}
            aria-hidden="true"
          ></span>
        {/if}
      </button>

      <button
        class="control-btn"
        onclick={() => player.prevTrack()}
        disabled={!player.hasTrack}
        aria-label="Previous track"
      >
        <span
          class="control-icon"
          style:--control-icon={"url('/icons/playbackward.svg')"}
          aria-hidden="true"
        ></span>
      </button>

      <button
        class="control-btn play-btn"
        class:playing={player.isPlaying}
        onclick={() => player.togglePlayPause()}
        disabled={!player.hasPlayingTracks && !player.hasTrack}
        aria-label={player.isPlaying
          ? "Pause"
          : player.isPaused
            ? "Resume"
            : "Play"}
      >
        {#if player.isPlaying}
          <span
            class="control-icon play-icon"
            style:--control-icon={"url('/icons/pause.svg')"}
            aria-hidden="true"
          ></span>
        {:else}
          <span
            class="control-icon play-icon"
            style:--control-icon={"url('/icons/play.svg')"}
            aria-hidden="true"
          ></span>
        {/if}
      </button>

      <button
        class="control-btn"
        onclick={() => player.nextTrack()}
        disabled={!player.hasNext}
        aria-label="Next track"
      >
        <span
          class="control-icon"
          style:--control-icon={"url('/icons/playforward.svg')"}
          aria-hidden="true"
        ></span>
      </button>

      <button
        class="control-btn mode-btn"
        class:active={player.repeatMode !== "off"}
        class:repeat-one={player.repeatMode === "one"}
        onclick={() => player.toggleRepeat()}
        disabled={!player.hasPlayingTracks}
        aria-label={player.repeatMode === "one"
          ? "Disable repeat"
          : player.repeatMode === "all"
            ? "Repeat one"
            : "Repeat all"}
        title={player.repeatMode === "one"
          ? "Repeat one"
          : player.repeatMode === "all"
            ? "Repeat all"
            : "Repeat"}
      >
        <!-- <span class="control-icon" style:--control-icon={"url('/icons/repeat.svg')"} aria-hidden="true"></span> -->
        {#if player.repeatMode === "one"}
          <span
            class="control-icon"
            style:--control-icon={"url('/icons/repeat.svg')"}
            aria-hidden="true"
          ></span>
        {:else if player.repeatMode === "all"}
          <span
            class="control-icon"
            style:--control-icon={"url('/icons/repeatplaylist.svg')"}
            aria-hidden="true"
          ></span>
        {:else}
          <span
            class="control-icon"
            style:--control-icon={"url('/icons/norepeat.svg')"}
            aria-hidden="true"
          ></span>
        {/if}
      </button>
    </div>

    <div class="transport-right">
      <MediaSlider variant="volume" />
    </div>
  </div>
  <div class="transport-progress">
    <MediaSlider variant="progress" />
  </div>
</div>

{#if fullscreenOpen}
  <FullscreenPlayer bind:open={fullscreenOpen} />
{/if}

<svelte:window
  onclick={stopWindowClickForMenus}
  oncontextmenu={stopWindowContextMenuForMenus}
  onkeydown={handleMenuWindowKeydown}
/>

<div class="np-menu-layer">
  <div
    role="presentation"
    onclick={stopMenuEventPropagation}
    onkeydown={stopMenuEventPropagation}
    oncontextmenu={stopMenuEventPropagation}
  >
    <ContextMenu
      open={contextMenu !== null}
      x={contextMenu?.x ?? 0}
      y={contextMenu?.y ?? 0}
      items={trackMenuItems}
      onclose={closeContextMenu}
    />
  </div>

  <div
    role="presentation"
    onclick={stopMenuEventPropagation}
    onkeydown={stopMenuEventPropagation}
    oncontextmenu={stopMenuEventPropagation}
  >
    <ContextMenu
      open={playlistSubmenu !== null}
      x={playlistSubmenu?.x ?? 0}
      y={playlistSubmenu?.y ?? 0}
      items={playlistSubmenuItems}
      onclose={closePlaylistSubmenu}
    />
  </div>
</div>

{#if menuToast}
  <div class="np-menu-toast" role="status">{menuToast}</div>
{/if}

<style>
  @import "./TransportBar.css";
</style>