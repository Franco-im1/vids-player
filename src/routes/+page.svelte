<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { onMount, onDestroy, tick } from "svelte";
  import { check } from "@tauri-apps/plugin-updater";
  import { relaunch } from "@tauri-apps/plugin-process";
  import { ask } from "@tauri-apps/plugin-dialog";

  let videoEl = $state<HTMLVideoElement | null>(null);
  let containerEl = $state<HTMLDivElement | null>(null);

  let serverPort = 0;
  let videoSrc = $state<string | null>(null);
  let paused = $state(true);
  let currentTime = $state(0);
  let duration = $state(0);
  let volume = $state(1);
  let muted = $state(false);
  let showControls = $state(false);
  let looping = $state(localStorage.getItem("looping") === "true");
  let zoomLevel = $state(1);
  let panX = $state(0);
  let panY = $state(0);
  let isPanning = $state(false);
  let rotation = $state(0); // 0, 90, 180, 270
  let updating = $state(false);
  let updateDownloaded = $state(0);
  let updateTotal = $state(0);

  let hideTimer: ReturnType<typeof setTimeout> | undefined;
  let unlistenDrop: (() => void) | null = null;
  let unlistenOpened: (() => void) | null = null;
  let panOriginX = 0, panOriginY = 0;
  let panOriginOffX = 0, panOriginOffY = 0;
  let didPan = false;

  $effect(() => {
    if (videoEl) videoEl.loop = looping;
    localStorage.setItem("looping", String(looping));
  });

  function fmt(s: number | undefined) {
    if (!s || isNaN(s)) return "0:00";
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = Math.floor(s % 60);
    if (h > 0)
      return `${h}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}`;
    return `${m}:${String(sec).padStart(2, "0")}`;
  }

  function scheduleHide() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => { showControls = false; }, 2500);
  }

  function onMouseMove() {
    showControls = true;
    scheduleHide();
  }

  function onMouseLeave() {
    clearTimeout(hideTimer);
    showControls = false;
  }

  // ── File loading ──────────────────────────────────────────────────────────────

  async function loadPath(path: string) {
    resetZoom();
    rotation = 0;
    const dir = path.substring(0, path.lastIndexOf("/"));
    if (dir) localStorage.setItem("lastDir", dir);
    const url = `http://127.0.0.1:${serverPort}/video?path=${encodeURIComponent(path)}`;
    videoSrc = url;
    await tick();
    if (videoEl) {
      videoEl.load();
      videoEl.play().catch(() => {});
    }
  }

  async function openFile() {
    try {
      const startDir = localStorage.getItem("lastDir") ?? undefined;
      const path = await invoke<string>("pick_video", { startDir });
      await loadPath(path);
    } catch (_) {}
  }

  // ── Zoom / Pan ────────────────────────────────────────────────────────────────

  function resetZoom() {
    zoomLevel = 1; panX = 0; panY = 0;
  }

  function rotateLeft()  { rotation = (rotation + 270) % 360; panX = 0; panY = 0; }
  function rotateRight() { rotation = (rotation +  90) % 360; panX = 0; panY = 0; }

  function applyZoom(factor: number) {
    zoomLevel = Math.max(1, Math.min(8, zoomLevel * factor));
    if (zoomLevel === 1) { panX = 0; panY = 0; }
  }

  function clampPan(x: number, y: number) {
    if (!containerEl) { panX = x; panY = y; return; }
    const r = containerEl.getBoundingClientRect();
    const maxX = (r.width / 2) * (zoomLevel - 1);
    const maxY = (r.height / 2) * (zoomLevel - 1);
    panX = Math.max(-maxX, Math.min(maxX, x));
    panY = Math.max(-maxY, Math.min(maxY, y));
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    applyZoom(e.deltaY < 0 ? 1.1 : 1 / 1.1);
  }

  function onVideoPointerDown(e: PointerEvent) {
    if (e.button !== 0 || zoomLevel <= 1) return;
    isPanning = true;
    didPan = false;
    panOriginX = e.clientX;
    panOriginY = e.clientY;
    panOriginOffX = panX;
    panOriginOffY = panY;
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    e.stopPropagation();
  }

  function onVideoPointerMove(e: PointerEvent) {
    if (!isPanning) return;
    const dx = e.clientX - panOriginX;
    const dy = e.clientY - panOriginY;
    if (Math.abs(dx) > 4 || Math.abs(dy) > 4) didPan = true;
    clampPan(panOriginOffX + dx, panOriginOffY + dy);
  }

  function onVideoPointerUp() {
    isPanning = false;
  }

  function onVideoClick() {
    if (didPan) { didPan = false; return; }
    togglePlay();
  }

  function onVideoDblClick() {
    if (zoomLevel > 1) resetZoom();
    else toggleFullscreen();
  }

  // ── Playback control ──────────────────────────────────────────────────────────

  function togglePlay() {
    if (!videoEl) return;
    if (videoEl.paused) videoEl.play().catch(() => {});
    else videoEl.pause();
  }

  function toggleFullscreen() {
    if (document.fullscreenElement) document.exitFullscreen();
    else document.documentElement.requestFullscreen();
  }

  function onProgressClick(e: MouseEvent) {
    if (!videoEl || !duration) return;
    const rect = (e.currentTarget as Element).getBoundingClientRect();
    const pct = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    videoEl.currentTime = pct * duration;
  }

  function onVolumeInput(e: Event) {
    const v = parseFloat((e.target as HTMLInputElement).value);
    volume = v;
    if (videoEl) {
      videoEl.volume = v;
      videoEl.muted = v === 0;
    }
  }

  // ── Keyboard ──────────────────────────────────────────────────────────────────

  function onKeyDown(e: KeyboardEvent) {
    if (e.code === "Space") {
      e.preventDefault();
      if (videoSrc) togglePlay();
      return;
    }

    switch (e.code) {
      case "ArrowRight":
        e.preventDefault();
        if (videoEl) videoEl.currentTime = Math.min(duration, currentTime + 5);
        break;
      case "ArrowLeft":
        e.preventDefault();
        if (videoEl) videoEl.currentTime = Math.max(0, currentTime - 5);
        break;
      case "ArrowUp":
        e.preventDefault();
        if (videoEl) videoEl.volume = Math.min(1, volume + 0.05);
        break;
      case "ArrowDown":
        e.preventDefault();
        if (videoEl) videoEl.volume = Math.max(0, volume - 0.05);
        break;
      case "KeyM":
        if (videoEl) videoEl.muted = !videoEl.muted;
        break;
      case "KeyF":
        toggleFullscreen();
        break;
      case "KeyO":
        openFile();
        break;
      case "KeyL":
        looping = !looping;
        if (videoEl) videoEl.loop = looping;
        break;
      case "Equal":
      case "NumpadAdd":
        e.preventDefault();
        applyZoom(1.2);
        break;
      case "Minus":
      case "NumpadSubtract":
        e.preventDefault();
        applyZoom(1 / 1.2);
        break;
      case "Digit0":
      case "Numpad0":
        e.preventDefault();
        resetZoom();
        break;
      case "BracketLeft":
        e.preventDefault();
        rotateLeft();
        break;
      case "BracketRight":
        e.preventDefault();
        rotateRight();
        break;
    }
  }

  // ── Lifecycle ─────────────────────────────────────────────────────────────────

  async function checkForUpdates() {
    try {
      const update = await check();
      if (!update?.available) return;
      const yes = await ask(
        `Nueva versión disponible: v${update.version}\n\n¿Descargar e instalar ahora?`,
        { title: "Actualización disponible" }
      );
      if (!yes) return;
      updating = true;
      updateDownloaded = 0;
      updateTotal = 0;
      await update.downloadAndInstall((event) => {
        switch (event.event) {
          case 'Started':
            updateTotal = event.data.contentLength ?? 0;
            break;
          case 'Progress':
            updateDownloaded += event.data.chunkLength;
            break;
        }
      });
      await relaunch();
    } catch (_) {
      // Sin conexión o sin endpoint configurado — ignorar silenciosamente
    }
  }

  onMount(async () => {
    serverPort = await invoke("get_server_port");

    const initialFile = await invoke<string | null>("get_initial_file");
    if (initialFile) await loadPath(initialFile);

    // macOS warm start: app ya abierta cuando el usuario abre otro archivo
    unlistenOpened = await listen<string[]>("opened", (e) => {
      if (e.payload[0]) loadPath(e.payload[0]);
    });

    setTimeout(checkForUpdates, 4000);
    window.addEventListener("keydown", onKeyDown);
    containerEl!.addEventListener("wheel", onWheel, { passive: false });
    window.addEventListener("dragover", (e) => e.preventDefault());
    window.addEventListener("drop", (e) => e.preventDefault());

    const win = getCurrentWebviewWindow();
    unlistenDrop = await win.onDragDropEvent((ev) => {
      if (ev.payload.type === "drop" && ev.payload.paths?.length > 0) {
        loadPath(ev.payload.paths[0]);
      }
    });
  });

  onDestroy(() => {
    window.removeEventListener("keydown", onKeyDown);
    containerEl?.removeEventListener("wheel", onWheel);
    clearTimeout(hideTimer);
    unlistenDrop?.();
    unlistenOpened?.();
  });
</script>

<div
  class="root"
  bind:this={containerEl}
  class:has-video={!!videoSrc}
  class:controls-on={showControls}
  class:zoomed={zoomLevel > 1}
  class:panning={isPanning}
  onmousemove={onMouseMove}
  onmouseleave={onMouseLeave}
>
  {#if !videoSrc}
    <button class="drop-zone" onclick={openFile}>
      <span class="play-icon">▶</span>
      <p class="label">Arrastra un video o haz clic para abrir</p>
      <p class="formats">mp4 · mkv · avi · mov · webm · flv</p>
    </button>
  {:else}
    <video
      bind:this={videoEl}
      src={videoSrc}
      bind:currentTime
      bind:duration
      bind:paused
      bind:volume
      bind:muted
      style="
        transform: rotate({rotation}deg) translate({panX}px, {panY}px) scale({zoomLevel});
        transform-origin: center;
        {rotation === 90 || rotation === 270 ? 'width: 100vh; height: 100vw;' : 'width: 100%; height: 100%;'}
      "
      onpointerdown={onVideoPointerDown}
      onpointermove={onVideoPointerMove}
      onpointerup={onVideoPointerUp}
      onclick={onVideoClick}
      ondblclick={onVideoDblClick}
    ></video>
  {/if}

  <div class="controls" class:visible={showControls && !!videoSrc}>
    <!-- Progress -->
    <div
      class="progress"
      role="slider"
      aria-valuenow={currentTime}
      aria-valuemin="0"
      aria-valuemax={duration}
      onclick={onProgressClick}
    >
      <div class="track">
        <div
          class="fill"
          style="width: {duration ? (currentTime / duration) * 100 : 0}%"
        ></div>
        <div
          class="thumb"
          style="left: {duration ? (currentTime / duration) * 100 : 0}%"
        ></div>
      </div>
    </div>

    <!-- Bar -->
    <div class="bar">
      <div class="left">
        <button class="btn" onclick={togglePlay}>
          {#if paused}
            <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
              <path d="M8 5v14l11-7z" />
            </svg>
          {:else}
            <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
              <path d="M6 19h4V5H6v14zm8-14v14h4V5h-4z" />
            </svg>
          {/if}
        </button>
        <span class="time">{fmt(currentTime)}<span class="sep"> / </span>{fmt(duration)}</span>
      </div>

      <div class="right">
        <button
          class="btn"
          onclick={() => { if (videoEl) videoEl.muted = !videoEl.muted; }}
        >
          {#if muted || volume === 0}
            <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
              <path d="M16.5 12c0-1.77-1.02-3.29-2.5-4.03v2.21l2.45 2.45c.03-.2.05-.41.05-.63zm2.5 0c0 .94-.2 1.82-.54 2.64l1.51 1.51C20.63 14.91 21 13.5 21 12c0-4.28-2.99-7.86-7-8.77v2.06c2.89.86 5 3.54 5 6.71zM4.27 3L3 4.27 7.73 9H3v6h4l5 5v-6.73l4.25 4.25c-.67.52-1.42.93-2.25 1.18v2.06c1.38-.31 2.63-.95 3.69-1.81L19.73 21 21 19.73l-9-9L4.27 3zM12 4L9.91 6.09 12 8.18V4z"/>
            </svg>
          {:else if volume > 0.5}
            <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
              <path d="M3 9v6h4l5 5V4L7 9H3zm13.5 3c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71s-2.11 5.85-5 6.71v2.06c4.01-.91 7-4.49 7-8.77s-2.99-7.86-7-8.77z"/>
            </svg>
          {:else}
            <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
              <path d="M18.5 12c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM5 9v6h4l5 5V4L9 9H5z"/>
            </svg>
          {/if}
        </button>
        <input
          type="range"
          class="vol"
          min="0"
          max="1"
          step="0.02"
          value={muted ? 0 : volume}
          oninput={onVolumeInput}
        />
        <button class="btn" onclick={rotateLeft} title="Girar izquierda ([)">
          <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
            <path d="M7.11 8.53 5.7 7.11C4.8 8.27 4.24 9.61 4.07 11h2.02c.14-.87.49-1.72 1.02-2.47zM6.09 13H4.07c.17 1.39.72 2.73 1.62 3.89l1.41-1.42c-.52-.75-.87-1.59-1.01-2.47zm1.01 5.32c1.16.9 2.51 1.44 3.9 1.61V17.9c-.87-.15-1.71-.49-2.46-1.03L7.1 18.32zM13 4.07V1L8.45 5.55 13 10V6.09c2.84.48 5 2.94 5 5.91s-2.16 5.43-5 5.91v2.02c3.95-.49 7-3.85 7-7.93s-3.05-7.44-7-7.93z"/>
          </svg>
        </button>
        <button class="btn" onclick={rotateRight} title="Girar derecha (])">
          <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
            <path d="M15.55 5.55 11 1v3.07C7.06 4.56 4 7.92 4 12s3.05 7.44 7 7.93v-2.02c-2.84-.48-5-2.94-5-5.91s2.16-5.43 5-5.91V10l4.55-4.45zM19.93 11c-.17-1.39-.72-2.73-1.62-3.89l-1.42 1.42c.54.75.88 1.6 1.02 2.47h2.02zM13 17.9v2.02c1.39-.17 2.74-.71 3.9-1.61l-1.44-1.44c-.75.54-1.59.89-2.46 1.03zm3.89-2.42 1.42 1.41c.9-1.16 1.45-2.5 1.62-3.89h-2.02c-.14.87-.48 1.72-1.02 2.48z"/>
          </svg>
        </button>
        <button
          class="btn"
          class:active={looping}
          onclick={() => { looping = !looping; if (videoEl) videoEl.loop = looping; }}
          title="Bucle (L)"
        >
          <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
            <path d="M12 5V1L7 6l5 5V7c3.31 0 6 2.69 6 6s-2.69 6-6 6-6-2.69-6-6H4c0 4.42 3.58 8 8 8s8-3.58 8-8-3.58-8-8-8z"/>
          </svg>
        </button>
        <button class="btn" onclick={openFile} title="Abrir archivo (O)">
          <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
            <path d="M20 6h-8l-2-2H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm0 12H4V8h16v10z"/>
          </svg>
        </button>
        <button class="btn" onclick={toggleFullscreen} title="Pantalla completa (F)">
          <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
            <path d="M7 14H5v5h5v-2H7v-3zm-2-4h2V7h3V5H5v5zm12 7h-3v2h5v-5h-2v3zM14 5v2h3v3h2V5h-5z"/>
          </svg>
        </button>
      </div>
    </div>
  </div>

  {#if updating}
    <div class="update-overlay">
      <p class="update-label">Descargando actualización…</p>
      <div class="update-bar-wrap">
        {#if updateTotal > 0}
          <div class="update-bar-fill" style="width: {Math.round((updateDownloaded / updateTotal) * 100)}%"></div>
        {:else}
          <div class="update-bar-indeterminate"></div>
        {/if}
      </div>
      {#if updateTotal > 0}
        <p class="update-pct">{Math.round((updateDownloaded / updateTotal) * 100)}%</p>
      {/if}
    </div>
  {/if}
</div>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
  }

  :global(html, body) {
    width: 100%;
    height: 100%;
    margin: 0;
    padding: 0;
    background: #000;
    overflow: hidden;
    user-select: none;
    scrollbar-width: none;
  }

  :global(::-webkit-scrollbar) {
    display: none;
  }

  .root {
    position: fixed;
    inset: 0;
    background: #000;
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
  }

  /* Cursor logic */
  .root.has-video:not(.controls-on):not(.zoomed) {
    cursor: none;
  }
  .root.zoomed video {
    cursor: grab;
  }
  .root.zoomed.panning video {
    cursor: grabbing;
  }

  video {
    object-fit: contain;
    display: block;
  }

  /* ── Empty state ─────────────────────────────── */
  .drop-zone {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
    background: none;
    border: none;
    color: #fff;
    cursor: pointer;
    padding: 48px;
  }

  .play-icon {
    font-size: 56px;
    opacity: 0.25;
    transition: opacity 0.2s;
  }

  .drop-zone:hover .play-icon {
    opacity: 0.7;
  }

  .label {
    font-family: system-ui, sans-serif;
    font-size: 15px;
    opacity: 0.45;
  }

  .formats {
    font-family: ui-monospace, monospace;
    font-size: 12px;
    opacity: 0.2;
    letter-spacing: 0.06em;
  }

  /* ── Controls overlay ────────────────────────── */
  .controls {
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    padding: 48px 14px 12px;
    background: linear-gradient(transparent, rgba(0, 0, 0, 0.8));
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.22s ease;
  }

  .controls.visible {
    opacity: 1;
    pointer-events: auto;
  }

  /* Progress bar */
  .progress {
    width: 100%;
    padding: 6px 0;
    cursor: pointer;
  }

  .track {
    position: relative;
    width: 100%;
    height: 3px;
    background: rgba(255, 255, 255, 0.18);
    border-radius: 2px;
    transition: height 0.15s;
  }

  .progress:hover .track {
    height: 5px;
  }

  .fill {
    height: 100%;
    background: #fff;
    border-radius: 2px;
    pointer-events: none;
  }

  .thumb {
    position: absolute;
    top: 50%;
    transform: translate(-50%, -50%) scale(0);
    width: 13px;
    height: 13px;
    border-radius: 50%;
    background: #fff;
    transition: transform 0.15s;
    pointer-events: none;
  }

  .progress:hover .thumb {
    transform: translate(-50%, -50%) scale(1);
  }

  /* Bar */
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 2px;
    gap: 8px;
  }

  .left,
  .right {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .btn {
    background: none;
    border: none;
    color: #fff;
    cursor: pointer;
    padding: 5px 6px;
    border-radius: 4px;
    opacity: 0.8;
    display: flex;
    align-items: center;
    transition: opacity 0.15s, background 0.15s;
    line-height: 0;
  }

  .btn:hover {
    opacity: 1;
    background: rgba(255, 255, 255, 0.1);
  }

  .btn.active {
    opacity: 1;
    color: #fff;
    background: rgba(255, 255, 255, 0.18);
  }

  .time {
    font-family: ui-monospace, monospace;
    font-size: 12px;
    color: rgba(255, 255, 255, 0.8);
    white-space: nowrap;
    padding: 0 4px;
  }

  .sep {
    opacity: 0.35;
  }

  /* Volume slider */
  .vol {
    -webkit-appearance: none;
    appearance: none;
    width: 72px;
    height: 3px;
    background: rgba(255, 255, 255, 0.25);
    border-radius: 2px;
    outline: none;
    cursor: pointer;
  }

  .vol::-webkit-slider-thumb {
    -webkit-appearance: none;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #fff;
    cursor: pointer;
  }

  .vol::-moz-range-thumb {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #fff;
    cursor: pointer;
    border: none;
  }

  /* ── Update overlay ──────────────────────────── */
  .update-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.88);
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 14px;
    z-index: 100;
  }

  .update-label {
    font-family: system-ui, sans-serif;
    font-size: 14px;
    color: rgba(255, 255, 255, 0.85);
  }

  .update-pct {
    font-family: ui-monospace, monospace;
    font-size: 12px;
    color: rgba(255, 255, 255, 0.45);
  }

  .update-bar-wrap {
    width: 220px;
    height: 3px;
    background: rgba(255, 255, 255, 0.15);
    border-radius: 2px;
    overflow: hidden;
  }

  .update-bar-fill {
    height: 100%;
    background: #fff;
    border-radius: 2px;
    transition: width 0.2s ease;
  }

  @keyframes indeterminate {
    0%   { transform: translateX(-100%); }
    100% { transform: translateX(420%); }
  }

  .update-bar-indeterminate {
    height: 100%;
    width: 25%;
    background: #fff;
    border-radius: 2px;
    animation: indeterminate 1.3s ease-in-out infinite;
  }
</style>
