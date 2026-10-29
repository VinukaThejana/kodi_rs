// Behaviour htmx doesn't cover: the touch surface, scrubbing, keyboard shortcuts, tabs,
// sheets, the volume HUD, toasts and the ambient backdrop. Requests still go through
// htmx so they share the remote's queue (hx-sync) and fire HX-Trigger events.
(() => {
  "use strict";

  const app = document.querySelector(".app");
  const pad = document.getElementById("pad");
  const hud = document.getElementById("hud");
  const toastEl = document.getElementById("toast");
  const ambient = document.getElementById("ambient");

  // iOS only applies :active styles once a touch listener exists.
  document.addEventListener("touchstart", () => {}, { passive: true });

  /** Fire-and-forget POST from `source`, honouring its hx-sync queue. */
  const post = (path, source = pad, values = {}) =>
    htmx.ajax("POST", path, { source, swap: "none", values });

  // ── Touch surface ───────────────────────────────────────────────
  // Tap the centre to select, tap an edge to move, or swipe. Long swipes
  // repeat a step every STEP px so you can glide through lists.

  const SWIPE = 22;
  const STEP = 64;
  const CENTER = 0.24; // radius of the select zone, as a fraction of the pad's width
  let touch = null;

  const direction = (dx, dy) =>
    Math.abs(dx) > Math.abs(dy) ? (dx > 0 ? "right" : "left") : dy > 0 ? "down" : "up";

  const point = (e) => {
    const r = pad.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top, size: r.width };
  };

  const zoneAt = ({ x, y, size }) => {
    const c = size / 2;
    return Math.hypot(x - c, y - c) < size * CENTER ? "select" : direction(x - c, y - c);
  };

  function flash(key) {
    const target = key === "select" ? pad : pad.querySelector(`[data-dir="${key}"]`);
    const cls = key === "select" ? "select-hit" : "hit";
    if (!target) return;
    target.classList.remove(cls);
    void target.offsetWidth; // restart the animation
    target.classList.add(cls);
  }

  function press(key) {
    post(`input/${key}`);
    flash(key);
  }

  pad.addEventListener("pointerdown", (e) => {
    pad.setPointerCapture(e.pointerId);
    const p = point(e);
    touch = { id: e.pointerId, ax: p.x, ay: p.y, swiped: false };
    pad.style.setProperty("--x", `${p.x}px`);
    pad.style.setProperty("--y", `${p.y}px`);
    pad.classList.add("touching");
    pad.dataset.press = zoneAt(p) === "select" ? "center" : zoneAt(p);
  });

  pad.addEventListener("pointermove", (e) => {
    if (touch?.id !== e.pointerId) return;
    const p = point(e);
    pad.style.setProperty("--x", `${p.x}px`);
    pad.style.setProperty("--y", `${p.y}px`);
    const dx = p.x - touch.ax;
    const dy = p.y - touch.ay;
    if (Math.hypot(dx, dy) > SWIPE) delete pad.dataset.press;
    if (Math.max(Math.abs(dx), Math.abs(dy)) >= STEP) {
      press(direction(dx, dy));
      touch.ax = p.x;
      touch.ay = p.y;
      touch.swiped = true;
    }
  });

  function endTouch() {
    touch = null;
    pad.classList.remove("touching");
    delete pad.dataset.press;
  }

  pad.addEventListener("pointerup", (e) => {
    if (touch?.id !== e.pointerId) return;
    const p = point(e);
    const dx = p.x - touch.ax;
    const dy = p.y - touch.ay;
    const moved = Math.hypot(dx, dy) > SWIPE;
    if (moved) press(direction(dx, dy));
    else if (!touch.swiped) press(zoneAt(p));
    endTouch();
  });
  pad.addEventListener("pointercancel", endTouch);

  // ── Keyboard (laptop) ───────────────────────────────────────────
  // macOS keeps the plain volume keys for itself; with fn held they arrive as F10–F12.
  // The Audio* names cover browsers that do forward media keys.

  const KEYS = {
    ArrowUp: "input/up",
    ArrowDown: "input/down",
    ArrowLeft: "input/left",
    ArrowRight: "input/right",
    Enter: "input/select",
    Backspace: "input/back", // the Mac's delete key
    Delete: "input/back", // fn+delete
    Escape: "input/back",
    h: "input/home",
    " ": "player/play-pause",
    "+": "volume/up",
    "=": "volume/up",
    "-": "volume/down",
    m: "mute",
    F10: "mute",
    F11: "volume/down",
    F12: "volume/up",
    AudioVolumeMute: "mute",
    AudioVolumeDown: "volume/down",
    AudioVolumeUp: "volume/up",
  };

  document.addEventListener("keydown", (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey || document.querySelector("dialog[open]")) return;
    if ((e.key === "Enter" || e.key === " ") && e.target.closest("button")) return;
    if (document.querySelector('.status:is([data-state="off"], [data-state="unreachable"])')) return;
    const path = KEYS[e.key];
    if (!path) return;
    e.preventDefault();
    post(path);
    if (path.startsWith("input/")) flash(path.slice("input/".length));
  });

  // ── Scrubbing ───────────────────────────────────────────────────
  // Drag along the progress bar; the seek is sent on release. Polling of
  // the status card pauses meanwhile so it doesn't yank the bar back.

  let scrub = null;

  function preview(e) {
    const r = scrub.getBoundingClientRect();
    const pct = Math.min(100, Math.max(0, ((e.clientX - r.left) / r.width) * 100));
    scrub.style.setProperty("--progress", `${pct}%`);
    return pct;
  }

  document.addEventListener("pointerdown", (e) => {
    const el = e.target.closest(".scrub");
    if (!el) return;
    scrub = el;
    scrub.setPointerCapture(e.pointerId);
    scrub.classList.add("dragging");
    preview(e);
  });
  document.addEventListener("pointermove", (e) => scrub && preview(e));
  document.addEventListener("pointerup", (e) => {
    if (!scrub) return;
    const pct = preview(e);
    const el = scrub;
    scrub = null;
    post("player/seek", el, { percent: pct.toFixed(2) }).finally(() => el.classList.remove("dragging"));
  });
  document.addEventListener("pointercancel", () => {
    scrub?.classList.remove("dragging");
    scrub = null;
  });

  document.body.addEventListener("htmx:beforeRequest", (e) => {
    if (scrub && e.detail.elt.id === "status") e.preventDefault();
  });

  // ── Tabs, sheets ────────────────────────────────────────────────

  function setView(view) {
    app.dataset.view = view;
    for (const tab of document.querySelectorAll(".tabbar [data-view]")) {
      tab.setAttribute("aria-pressed", String(tab.dataset.view === view));
    }
    try { localStorage.setItem("view", view); } catch {}
  }

  try {
    const saved = localStorage.getItem("view");
    if (saved === "remote" || saved === "library") setView(saved);
  } catch {}
  if (location.hash === "#library" || location.hash === "#remote") setView(location.hash.slice(1));

  document.addEventListener("click", (e) => {
    const tab = e.target.closest(".tabbar [data-view]");
    if (tab) setView(tab.dataset.view);

    const link = e.target.closest("[data-view-link]");
    if (link) setView(link.dataset.viewLink);

    const segment = e.target.closest("[data-segment]");
    if (segment) {
      for (const s of segment.parentElement.querySelectorAll("[data-segment]")) {
        s.setAttribute("aria-selected", String(s === segment));
      }
    }

    const opener = e.target.closest("[data-open]");
    if (opener) document.getElementById(opener.dataset.open)?.showModal();

    const closer = e.target.closest("[data-close]");
    if (closer) closer.closest("dialog")?.close();

    // A click on the ::backdrop lands on the dialog itself, outside its box.
    if (e.target instanceof HTMLDialogElement) {
      const r = e.target.getBoundingClientRect();
      const outside = e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom;
      if (outside) e.target.close();
    }
  });

  // Opening a show, going back, or switching Movies/TV Shows starts at the top.
  document.body.addEventListener("htmx:afterSwap", (e) => {
    if (e.detail.target.id === "media") window.scrollTo({ top: 0 });
  });

  document.body.addEventListener("htmx:afterRequest", (e) => {
    const el = e.detail.elt;
    if (!e.detail.successful || !(el instanceof Element)) return;
    if (el.matches("[data-play]")) {
      setView("remote");
      toast(`Playing ${el.dataset.play}`);
    }
    if (el.matches("[data-close-after]")) el.closest("dialog")?.close();
  });

  // ── Volume HUD + toasts ─────────────────────────────────────────

  let hudTimer;
  document.body.addEventListener("hud", (e) => {
    const { level, muted } = e.detail;
    hud.style.setProperty("--level", muted ? 0 : level);
    hud.querySelector("use").setAttribute("href", muted ? "#i-mute" : "#i-speaker");
    hud.classList.add("show");
    clearTimeout(hudTimer);
    hudTimer = setTimeout(() => hud.classList.remove("show"), 1400);
  });

  let toastTimer;
  function toast(message) {
    toastEl.textContent = message;
    toastEl.classList.add("show");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => toastEl.classList.remove("show"), 2600);
  }

  // The status poll fails quietly (the card already says Kodi is off); everything else says why.
  const isPoll = (e) => e.detail.elt?.id === "status";
  document.body.addEventListener("htmx:responseError", (e) => {
    if (!isPoll(e)) toast(e.detail.xhr.responseText || "Something went wrong");
  });
  document.body.addEventListener("htmx:sendError", (e) => {
    if (!isPoll(e)) toast("Can't reach the remote server");
  });
  // A missing hx-target makes htmx drop the request silently; surface it instead.
  document.body.addEventListener("htmx:targetError", (e) => {
    console.error("htmx target not found", e.detail);
    toast("Something went wrong — reload the page");
  });

  // ── Ambient backdrop ────────────────────────────────────────────
  // Mirrors the status card's fanart onto the full-page backdrop, fading in once loaded.

  function syncAmbient() {
    const src = document.getElementById("status")?.dataset.backdrop ?? "";
    if (ambient.dataset.src === src) return;
    ambient.dataset.src = src;
    ambient.classList.remove("shown");
    if (!src) return;
    const img = new Image();
    img.onload = () => {
      if (ambient.dataset.src !== src) return;
      ambient.src = src;
      ambient.classList.add("shown");
    };
    img.src = src;
  }

  syncAmbient();
  document.body.addEventListener("htmx:afterSettle", syncAmbient);

  // ── Listen on this device ───────────────────────────────────────
  // Kodi's sound, streamed over WebRTC by go2rtc (see deploy/go2rtc.yaml). Browsers only
  // let audio start from a tap, so the element starts playing an empty stream inside the
  // tap itself and the WebRTC track joins it once the connection is up. After that the
  // element stays "unlocked", so reconnects after a Wi-Fi blip or sleep need no tap.

  const listenAudio = new Audio();
  const LISTEN_KEY = "listening"; // survives reloads so the next tap can resume
  let listenStream = null;
  let peer = null;
  let wantListening = false;
  let retries = 0;
  let retryTimer;

  const remember = (on) => {
    try { on ? localStorage.setItem(LISTEN_KEY, "1") : localStorage.removeItem(LISTEN_KEY); } catch {}
  };
  const remembered = () => {
    try { return localStorage.getItem(LISTEN_KEY) === "1"; } catch { return false; }
  };

  /** Must run inside a tap: starts the (still empty) audio element so the stream can join it. */
  function primeAudio() {
    listenStream = new MediaStream();
    listenAudio.srcObject = listenStream;
    listenAudio.play().catch(() => {});
  }

  function startListening() {
    if (!window.RTCPeerConnection) {
      toast("This browser can't play the audio stream");
      return;
    }
    wantListening = true;
    remember(true);
    retries = 0;
    primeAudio();
    connect();
  }

  function stopListening() {
    wantListening = false;
    remember(false);
    clearTimeout(retryTimer);
    peer?.close();
    peer = null;
    listenAudio.pause();
    listenAudio.srcObject = null;
  }

  async function connect() {
    clearTimeout(retryTimer);
    peer?.close();
    const pc = new RTCPeerConnection();
    peer = pc;
    // Leave the browser's adaptive jitter buffer alone: pinning it (jitterBufferTarget = 80)
    // made playback choppy on the Mac.
    pc.addTransceiver("audio", { direction: "recvonly" });
    pc.ontrack = (e) => {
      for (const old of listenStream.getTracks()) listenStream.removeTrack(old);
      listenStream.addTrack(e.track);
      listenAudio.play().catch(() => {});
    };
    pc.onconnectionstatechange = () => {
      if (pc !== peer) return;
      if (pc.connectionState === "connected") retries = 0;
      if (pc.connectionState === "failed") reconnectSoon();
      // "disconnected" often heals by itself within a second or two; only step in if it doesn't.
      if (pc.connectionState === "disconnected") {
        setTimeout(() => pc === peer && pc.connectionState !== "connected" && reconnectSoon(), 3000);
      }
    };
    try {
      // After a drop the server may have moved the sound back to the TV; take it back.
      if (retries > 0) await post("listen/start", pad);
      await pc.setLocalDescription(await pc.createOffer());
      await iceGathered(pc);
      const res = await fetch("listen/offer", { method: "POST", body: pc.localDescription.sdp });
      if (!res.ok) throw new Error(await res.text());
      if (pc === peer) await pc.setRemoteDescription({ type: "answer", sdp: await res.text() });
    } catch (err) {
      if (pc !== peer) return;
      if (retries === 0) toast(err.message || "Couldn't start the audio stream");
      reconnectSoon();
    }
  }

  // Temporary diagnostics: every 5s while listening, send the browser's WebRTC stats to the
  // server log. Loss, jitter and concealment are only visible here, not on the PC.
  setInterval(async () => {
    if (!peer || peer.connectionState !== "connected") return;
    const report = { ua: navigator.userAgent.replace(/^.*\) /, "") };
    for (const s of (await peer.getStats()).values()) {
      if (s.type === "inbound-rtp" && s.kind === "audio") {
        Object.assign(report, {
          received: s.packetsReceived, lost: s.packetsLost, jitterMs: Math.round(s.jitter * 1000),
          concealedMs: Math.round((s.concealedSamples / 48000) * 1000), concealEvents: s.concealmentEvents,
          bufferMs: s.jitterBufferEmittedCount ? Math.round((s.jitterBufferDelay / s.jitterBufferEmittedCount) * 1000) : null,
          speedups: s.removedSamplesForAcceleration, slowdowns: s.insertedSamplesForDeceleration,
        });
      }
      if (s.type === "candidate-pair" && s.nominated && s.state === "succeeded") {
        report.rttMs = Math.round((s.currentRoundTripTime ?? 0) * 1000);
        const local = (await peer.getStats()).get(s.localCandidateId);
        report.transport = local?.protocol;
      }
    }
    fetch("debug/webrtc", { method: "POST", body: JSON.stringify(report) }).catch(() => {});
  }, 5000);

  /** Backs off 1s, 2s, 4s… up to 10s between attempts, for as long as listening is wanted. */
  function reconnectSoon() {
    if (!wantListening) return;
    clearTimeout(retryTimer);
    retryTimer = setTimeout(connect, Math.min(10000, 1000 * 2 ** retries++));
  }

  /** go2rtc takes the whole offer in one request, so wait for ICE gathering (quick on a LAN). */
  function iceGathered(pc) {
    if (pc.iceGatheringState === "complete") return Promise.resolve();
    return new Promise((resolve) => {
      pc.addEventListener("icegatheringstatechange", () => pc.iceGatheringState === "complete" && resolve());
      setTimeout(resolve, 2000);
    });
  }

  // Resume after a reload: the first tap primes audio, then reconnects only if Kodi is
  // still playing into the stream (otherwise someone moved the sound elsewhere; let it be).
  async function resumeListening() {
    if (wantListening || !remembered()) return;
    primeAudio();
    const res = await fetch("listen/state").catch(() => null);
    if (res?.ok && (await res.text()) === "true") {
      wantListening = true;
      retries = 0;
      connect();
    } else {
      stopListening();
    }
  }

  document.addEventListener("click", (e) => {
    if (e.target.closest("[data-listen]")) startListening();
    else if (e.target.closest('[hx-post="audio/output"]')) stopListening();
    else resumeListening();
  });
  document.addEventListener("keydown", resumeListening, { once: true });

  // ── System media controls ───────────────────────────────────────
  // Mirrors Kodi into the Media Session API: iOS lock screen / Control Center, macOS
  // Now Playing and media keys. Browsers only hand those controls to a page that is
  // playing audio, so a looping silent track follows Kodi's play state. Autoplay rules
  // mean it needs one tap or keypress on the page first; after that it syncs on its own.

  const media = "mediaSession" in navigator ? navigator.mediaSession : null;
  const kodiState = () => document.getElementById("status")?.dataset ?? {};

  /** A WAV of silence. Longer than 5s, since Chrome hides media controls for shorter clips. */
  function silentWav(seconds) {
    const rate = 8000;
    const samples = rate * seconds;
    const view = new DataView(new ArrayBuffer(44 + samples));
    const ascii = (offset, s) => [...s].forEach((c, i) => view.setUint8(offset + i, c.charCodeAt(0)));
    ascii(0, "RIFF");
    view.setUint32(4, 36 + samples, true);
    ascii(8, "WAVE");
    ascii(12, "fmt ");
    view.setUint32(16, 16, true); // fmt chunk size
    view.setUint16(20, 1, true); // PCM
    view.setUint16(22, 1, true); // mono
    view.setUint32(24, rate, true); // sample rate
    view.setUint32(28, rate, true); // byte rate
    view.setUint16(32, 1, true); // block align
    view.setUint16(34, 8, true); // bits per sample
    ascii(36, "data");
    view.setUint32(40, samples, true);
    new Uint8Array(view.buffer, 44).fill(128); // 8-bit PCM silence is the midpoint, not 0
    return URL.createObjectURL(new Blob([view.buffer], { type: "audio/wav" }));
  }

  if (media) {
    const silence = new Audio(silentWav(10));
    silence.loop = true;
    let unlocked = false;

    function syncMedia() {
      const s = kodiState();
      const active = s.state === "playing" || s.state === "paused";
      if (!active) {
        media.metadata = null;
        media.playbackState = "none";
        silence.pause();
        return;
      }
      if (media.metadata?.title !== s.title || media.metadata?.artist !== s.artist) {
        media.metadata = new MediaMetadata({
          title: s.title,
          artist: s.artist,
          album: "Kodi",
          artwork: s.artwork ? [{ src: new URL(s.artwork, location.href).href }] : [],
        });
      }
      media.playbackState = s.state;
      const duration = Number(s.duration);
      if (duration > 0) {
        try {
          media.setPositionState({ duration, position: Math.min(Number(s.position), duration), playbackRate: 1 });
        } catch {}
      }
      // While listening, the stream itself keeps the media controls alive; a second element
      // playing alongside it only gives Safari more to mix.
      const needSilence = unlocked && s.state === "playing" && !peer;
      needSilence ? silence.play().catch(() => {}) : silence.pause();
    }

    // The first gesture on the page is allowed to start audio; later play() calls then work.
    function unlock() {
      if (unlocked) return;
      unlocked = true;
      silence.play().then(syncMedia, () => { unlocked = false; });
    }
    for (const type of ["click", "touchend", "keydown"]) document.addEventListener(type, unlock, true);

    const act = (path, values) => post(path, pad, values);
    const handle = (action, handler) => {
      try { media.setActionHandler(action, handler); } catch {} // not every browser knows every action
    };
    handle("play", () => {
      if (!peer) silence.play().catch(() => {});
      media.playbackState = "playing";
      if (kodiState().state === "paused") act("player/play-pause");
    });
    handle("pause", () => {
      silence.pause();
      media.playbackState = "paused";
      if (kodiState().state === "playing") act("player/play-pause");
    });
    handle("stop", () => act("player/stop"));
    handle("seekbackward", (d) => act("player/skip", { seconds: -Math.min(600, Math.round(d.seekOffset || 10)) }));
    handle("seekforward", (d) => act("player/skip", { seconds: Math.min(600, Math.round(d.seekOffset || 30)) }));
    // Media keys ⏮/⏭ arrive as track changes; for a movie, skipping is the useful meaning.
    handle("previoustrack", () => act("player/skip", { seconds: -10 }));
    handle("nexttrack", () => act("player/skip", { seconds: 30 }));
    handle("seekto", (d) => {
      const duration = Number(kodiState().duration);
      if (duration > 0) act("player/seek", { percent: Math.min(100, (d.seekTime / duration) * 100).toFixed(2) });
    });

    syncMedia();
    document.body.addEventListener("htmx:afterSettle", syncMedia);
  }
})();
