<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { onMount, onDestroy, tick } from "svelte";
  import type { ParsedFrame } from "gifuct-js";
  import { check } from "@tauri-apps/plugin-updater";
  import { relaunch } from "@tauri-apps/plugin-process";
  import { ask } from "@tauri-apps/plugin-dialog";

  let videoEl = $state<HTMLVideoElement | null>(null);
  let containerEl = $state<HTMLDivElement | null>(null);
  let gifCanvas = $state<HTMLCanvasElement | null>(null);

  let serverPort = 0;
  let videoSrc = $state<string | null>(null);
  let isGif = $state(false);
  let paused = $state(true);
  let currentTime = $state(0);
  let duration = $state(0);
  let volume = $state(1);
  let muted = $state(false);
  let showControls = $state(false);
  let looping = $state(false);
  let zoomLevel = $state(1);
  let panX = $state(0);
  let panY = $state(0);
  let isPanning = $state(false);

  let hideTimer: ReturnType<typeof setTimeout> | undefined;
  let unlistenDrop: (() => void) | null = null;
  let panOriginX = 0, panOriginY = 0;
  let panOriginOffX = 0, panOriginOffY = 0;
  let didPan = false;

  let gifDisplayWidth = $state(0);
  let gifDisplayHeight = $state(0);
  let gifSpeed = $state(1);

  // GIF playback state (non-reactive for perf)
  let gifFrames: ParsedFrame[] = [];
  let gifFrameIndex = 0;
  let gifTimer: ReturnType<typeof setTimeout> | undefined;
  let gifCtx: CanvasRenderingContext2D | null = null;
  let gifTmpCanvas: HTMLCanvasElement | null = null;
  let gifTmpCtx: CanvasRenderingContext2D | null = null;
  let gifWidth = 0;
  let gifHeight = 0;
  let gifFrameTimes: number[] = [];
  let gifPrevDisposal = 0;
  let gifSavedImageData: ImageData | null = null;

  $effect(() => {
    if (videoEl) videoEl.loop = looping;
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

  // ── GIF playback ──────────────────────────────────────────────────────────────

  function stopGif() {
    clearTimeout(gifTimer);
    gifTimer = undefined;
    gifFrames = [];
    gifFrameIndex = 0;
    gifFrameTimes = [];
    gifCtx = null;
    gifTmpCanvas = null;
    gifTmpCtx = null;
    gifWidth = 0;
    gifHeight = 0;
    gifPrevDisposal = 0;
    gifSavedImageData = null;
  }

  function renderGifFrame(index: number) {
    if (!gifCtx || !gifTmpCtx || !gifTmpCanvas || !gifFrames.length) return;
    const frame = gifFrames[index];

    // Dispose previous frame
    if (gifPrevDisposal === 2) {
      gifCtx.clearRect(0, 0, gifWidth, gifHeight);
    } else if (gifPrevDisposal === 3 && gifSavedImageData) {
      gifCtx.putImageData(gifSavedImageData, 0, 0);
      gifSavedImageData = null;
    }

    if (frame.disposalType === 3) {
      gifSavedImageData = gifCtx.getImageData(0, 0, gifWidth, gifHeight);
    }

    // Draw via tmp canvas so alpha compositing respects transparency
    gifTmpCtx.clearRect(0, 0, frame.dims.width, frame.dims.height);
    gifTmpCtx.putImageData(
      new ImageData(frame.patch, frame.dims.width, frame.dims.height),
      0, 0
    );
    gifCtx.drawImage(
      gifTmpCanvas,
      0, 0, frame.dims.width, frame.dims.height,
      frame.dims.left, frame.dims.top, frame.dims.width, frame.dims.height
    );

    gifPrevDisposal = frame.disposalType;
  }

  // Seek to a specific frame with correct disposal state.
  // If advancing forward by one frame we can skip the full re-render from 0.
  function seekToFrame(targetIdx: number) {
    if (!gifCtx || !gifFrames.length) return;
    const canIncrement = targetIdx === gifFrameIndex + 1 && targetIdx < gifFrames.length;
    if (canIncrement) {
      // Just render the next frame directly; disposal state is already correct
      gifFrameIndex = targetIdx;
      renderGifFrame(targetIdx);
    } else {
      // Full re-render from 0 to guarantee correct disposal chain
      gifCtx.clearRect(0, 0, gifWidth, gifHeight);
      gifPrevDisposal = 0;
      gifSavedImageData = null;
      for (let i = 0; i <= targetIdx; i++) renderGifFrame(i);
      gifFrameIndex = targetIdx;
    }
    currentTime = gifFrameTimes[targetIdx] / 1000;
  }

  function gifPlayLoop() {
    if (paused || !gifFrames.length || !gifCtx) return;
    renderGifFrame(gifFrameIndex);

    const frame = gifFrames[gifFrameIndex];
    // Mínimo 10ms (igual que los navegadores); dividir por speed para acelerar/ralentizar
    const delay = Math.max(10, (frame.delay || 10) * 10) / gifSpeed;

    gifTimer = setTimeout(() => {
      if (paused) return;
      const next = (gifFrameIndex + 1) % gifFrames.length;
      if (next === 0 && !looping) {
        paused = true;
        currentTime = duration;
        return;
      }
      gifFrameIndex = next;
      currentTime = gifFrameTimes[gifFrameIndex] / 1000;
      gifPlayLoop();
    }, delay);
  }

  function cycleGifSpeed() {
    const speeds = [0.5, 1, 2, 4];
    gifSpeed = speeds[(speeds.indexOf(gifSpeed) + 1) % speeds.length];
    // Reiniciar el timer con la nueva velocidad de inmediato
    if (!paused) {
      clearTimeout(gifTimer);
      gifTimer = undefined;
      gifPlayLoop();
    }
  }

  async function loadGif(url: string) {
    stopGif();
    paused = true;
    currentTime = 0;
    duration = 0;
    gifFrameIndex = 0;

    try {
      const resp = await fetch(url);
      const buffer = await resp.arrayBuffer();
      const { parseGIF, decompressFrames } = await import("gifuct-js");
      const gif = parseGIF(buffer);
      gifFrames = decompressFrames(gif, true);

      if (!gifFrames.length) return;

      gifWidth = gif.lsd.width;
      gifHeight = gif.lsd.height;

      // Build per-frame cumulative start times
      gifFrameTimes = [];
      let t = 0;
      for (const frame of gifFrames) {
        gifFrameTimes.push(t);
        t += Math.max(20, (frame.delay || 10) * 10);
      }
      duration = t / 1000;

      // Compute CSS display size (fill viewport, maintain aspect ratio)
      if (containerEl) {
        const r = containerEl.getBoundingClientRect();
        const scale = Math.min(r.width / gifWidth, r.height / gifHeight);
        gifDisplayWidth = Math.round(gifWidth * scale);
        gifDisplayHeight = Math.round(gifHeight * scale);
      }

      // Reusable offscreen canvas for frame compositing
      const maxW = Math.max(...gifFrames.map(f => f.dims.width));
      const maxH = Math.max(...gifFrames.map(f => f.dims.height));
      gifTmpCanvas = document.createElement("canvas");
      gifTmpCanvas.width = maxW;
      gifTmpCanvas.height = maxH;
      gifTmpCtx = gifTmpCanvas.getContext("2d");

      await tick();

      if (gifCanvas) {
        gifCtx = gifCanvas.getContext("2d");
        gifCanvas.width = gifWidth;
        gifCanvas.height = gifHeight;
        gifPrevDisposal = 0;
        gifSavedImageData = null;
        paused = false;
        gifPlayLoop();
      }
    } catch (e) {
      console.error("GIF load error:", e);
    }
  }

  // ── File loading ──────────────────────────────────────────────────────────────

  async function loadPath(path: string) {
    resetZoom();
    gifSpeed = 1;
    const url = `http://127.0.0.1:${serverPort}/video?path=${encodeURIComponent(path)}`;
    const gif = path.toLowerCase().endsWith(".gif");

    if (!gif) {
      stopGif();
      isGif = false;
      videoSrc = url;
      await tick();
      if (videoEl) {
        videoEl.load();
        videoEl.play().catch(() => {});
      }
    } else {
      isGif = true;
      videoSrc = url; // truthy → hides drop zone, shows controls
      await loadGif(url);
    }
  }

  async function openFile() {
    try {
      const path = await invoke<string>("pick_video");
      await loadPath(path);
    } catch (_) {}
  }

  // ── Zoom / Pan ────────────────────────────────────────────────────────────────

  function resetZoom() {
    zoomLevel = 1; panX = 0; panY = 0;
  }

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
    if (isGif) {
      if (paused) {
        // Si llegó al final en modo no-bucle, reiniciar desde el principio
        if (!looping && currentTime >= duration && gifFrames.length > 0) {
          seekToFrame(0);
        }
        paused = false;
        gifPlayLoop();
      } else {
        paused = true;
        clearTimeout(gifTimer);
        gifTimer = undefined;
      }
      return;
    }
    if (!videoEl) return;
    if (videoEl.paused) videoEl.play().catch(() => {});
    else videoEl.pause();
  }

  function toggleFullscreen() {
    if (document.fullscreenElement) document.exitFullscreen();
    else document.documentElement.requestFullscreen();
  }

  function onProgressClick(e: MouseEvent) {
    if (!videoSrc) return;
    const rect = (e.currentTarget as Element).getBoundingClientRect();
    const pct = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));

    if (isGif) {
      if (!gifFrames.length) return;
      const targetMs = pct * duration * 1000;
      let idx = 0;
      for (let i = gifFrameTimes.length - 1; i >= 0; i--) {
        if (gifFrameTimes[i] <= targetMs) { idx = i; break; }
      }
      if (gifTimer !== undefined) cancelAnimationFrame(gifTimer);
      gifTimer = undefined;
      seekToFrame(idx);
      if (!paused) gifPlayLoop();
      return;
    }

    if (!videoEl || !duration) return;
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
        if (isGif && gifFrames.length) {
          seekToFrame(Math.min(gifFrames.length - 1, gifFrameIndex + 1));
        } else if (videoEl) {
          videoEl.currentTime = Math.min(duration, currentTime + 5);
        }
        break;
      case "ArrowLeft":
        e.preventDefault();
        if (isGif && gifFrames.length) {
          seekToFrame(Math.max(0, gifFrameIndex - 1));
        } else if (videoEl) {
          videoEl.currentTime = Math.max(0, currentTime - 5);
        }
        break;
      case "ArrowUp":
        e.preventDefault();
        if (!isGif && videoEl) videoEl.volume = Math.min(1, volume + 0.05);
        break;
      case "ArrowDown":
        e.preventDefault();
        if (!isGif && videoEl) videoEl.volume = Math.max(0, volume - 0.05);
        break;
      case "KeyM":
        if (!isGif && videoEl) videoEl.muted = !videoEl.muted;
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
      await update.downloadAndInstall();
      await relaunch();
    } catch (_) {
      // Sin conexión o sin endpoint configurado — ignorar silenciosamente
    }
  }

  onMount(async () => {
    serverPort = await invoke("get_server_port");
    // Verificar actualizaciones en segundo plano después del arranque
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
    stopGif();
    unlistenDrop?.();
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
      <p class="formats">mp4 · mkv · avi · mov · webm · flv · gif</p>
    </button>
  {:else if isGif}
    <canvas
      bind:this={gifCanvas}
      style="width: {gifDisplayWidth}px; height: {gifDisplayHeight}px; transform: translate({panX}px, {panY}px) scale({zoomLevel}); transform-origin: center;"
      onpointerdown={onVideoPointerDown}
      onpointermove={onVideoPointerMove}
      onpointerup={onVideoPointerUp}
      onclick={onVideoClick}
      ondblclick={onVideoDblClick}
    ></canvas>
  {:else}
    <video
      bind:this={videoEl}
      src={videoSrc}
      bind:currentTime
      bind:duration
      bind:paused
      bind:volume
      bind:muted
      style="transform: translate({panX}px, {panY}px) scale({zoomLevel}); transform-origin: center;"
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
        {#if !isGif}
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
        {/if}
        {#if isGif}
          <button class="btn speed-btn" onclick={cycleGifSpeed} title="Velocidad de reproducción">
            {gifSpeed}×
          </button>
        {/if}
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
  .root.zoomed video,
  .root.zoomed canvas {
    cursor: grab;
  }
  .root.zoomed.panning video,
  .root.zoomed.panning canvas {
    cursor: grabbing;
  }

  video {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }

  canvas {
    display: block;
    background: #000;
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

  .speed-btn {
    font-family: ui-monospace, monospace;
    font-size: 11px;
    min-width: 28px;
    letter-spacing: -0.02em;
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
</style>
