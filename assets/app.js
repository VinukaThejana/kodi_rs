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

  // ── Trackpad (browser mode) ─────────────────────────────────────
  // While the browser is on the TV (the status card's data-browser="true"), the pad drives its
  // pointer: drag to move, tap to click, two fingers to scroll, a two-finger tap to right-click.
  // These listeners are registered before the clickpad's below and stop them in that mode.

  const inBrowser = () => document.querySelector('.status[data-browser="true"]') !== null;
  const TAP_TRAVEL = 8; // px a finger may drift and still tap
  const TAP_MS = 280;
  const SCROLL = 3; // wheel units (120 per notch) per px of two-finger travel
  const SCROLL_EDGE = 0.82; // a drag that starts right of this fraction of the pad scrolls
  const fingers = new Map(); // pointerId → { x, y, t } at its last event
  let gesture = null; // { start, travel, most } for the touch in progress
  const pending = { move: [0, 0], scroll: [0, 0] };
  const sending = { move: false, scroll: false };

  /** Sends what has built up for `kind`, with one request in flight per kind, so a slow network
   * gets fewer, larger steps instead of a growing backlog. */
  function flush(kind) {
    const [dx, dy] = pending[kind].map(Math.trunc);
    if (sending[kind] || (dx === 0 && dy === 0)) return;
    pending[kind] = [pending[kind][0] - dx, pending[kind][1] - dy];
    sending[kind] = true;
    fetch(`browser/${kind}`, { method: "POST", body: new URLSearchParams({ dx, dy }) })
      .catch(() => {})
      .finally(() => {
        sending[kind] = false;
        flush(kind);
      });
  }

  pad.addEventListener("pointerdown", (e) => {
    if (!inBrowser()) return;
    e.stopImmediatePropagation();
    // A mouse doesn't drag the pad: clicking it hands the mouse to the TV (mouse mode, below).
    if (e.pointerType === "mouse") return mouseDown(e);
    pad.setPointerCapture(e.pointerId);
    fingers.set(e.pointerId, { x: e.clientX, y: e.clientY, t: e.timeStamp });
    if (fingers.size === 1) {
      const r = pad.getBoundingClientRect();
      gesture = { start: e.timeStamp, travel: 0, most: 1, edge: e.clientX - r.left > r.width * SCROLL_EDGE };
    }
    else gesture.most = Math.max(gesture.most, fingers.size);
    const p = point(e);
    pad.style.setProperty("--x", `${p.x}px`);
    pad.style.setProperty("--y", `${p.y}px`);
    pad.classList.add("touching");
  });

  pad.addEventListener("pointermove", (e) => {
    const last = fingers.get(e.pointerId);
    if (!last) return;
    e.stopImmediatePropagation();
    const dx = e.clientX - last.x;
    const dy = e.clientY - last.y;
    const dist = Math.hypot(dx, dy);
    fingers.set(e.pointerId, { x: e.clientX, y: e.clientY, t: e.timeStamp });
    gesture.travel += dist;
    if (fingers.size === 1 && gesture.edge) {
      // The scroll strip: one finger scrolls, the page following it like two fingers would.
      pending.scroll[1] += dy * SCROLL;
      flush("scroll");
    } else if (fingers.size >= 2) {
      // Natural scrolling: the page follows the fingers. Each finger reports its own moves,
      // so divide by the count to scroll by their average.
      pending.scroll[0] += (-dx * SCROLL) / fingers.size;
      pending.scroll[1] += (dy * SCROLL) / fingers.size;
      flush("scroll");
    } else {
      // Pointer acceleration: slow drags are precise, fast flicks cross the screen.
      const speed = dist / Math.max(1, e.timeStamp - last.t); // px per ms
      const gain = 1.4 + Math.min(speed, 2.5) * 1.3;
      pending.move[0] += dx * gain;
      pending.move[1] += dy * gain;
      flush("move");
      const p = point(e);
      pad.style.setProperty("--x", `${p.x}px`);
      pad.style.setProperty("--y", `${p.y}px`);
    }
  });

  function liftFinger(e, cancelled) {
    if (!fingers.delete(e.pointerId)) return;
    e.stopImmediatePropagation();
    if (fingers.size > 0) return;
    const tap = !cancelled && gesture.travel < TAP_TRAVEL && e.timeStamp - gesture.start < TAP_MS;
    if (tap) {
      fetch("browser/click", {
        method: "POST",
        body: new URLSearchParams({ button: gesture.most >= 2 ? "right" : "left" }),
      }).catch(() => {});
      flash("select");
    }
    gesture = null;
    pad.classList.remove("touching");
  }
  pad.addEventListener("pointerup", (e) => liftFinger(e, false));
  pad.addEventListener("pointercancel", (e) => liftFinger(e, true));

  // ── Mouse mode (browser mode, laptop) ───────────────────────────
  // Clicking the pad with a mouse locks the pointer to the page (Pointer Lock): from then on
  // every movement anywhere moves the TV's pointer, clicks click there and the wheel scrolls,
  // like a remote-desktop app. Esc gives the cursor back. Browsers only lock the pointer
  // on a click, which is also what keeps clicks on the remote's own buttons apart.

  const locked = () => document.pointerLockElement === pad;
  const WHEEL = 1.2; // wheel units per pixel of wheel delta
  let unlockedAt = 0;

  function mouseDown(e) {
    if (!locked()) {
      pad.requestPointerLock()?.catch?.(() => {});
      return;
    }
    const button = e.button === 2 ? "right" : e.button === 0 ? "left" : null;
    if (!button) return;
    fetch("browser/click", { method: "POST", body: new URLSearchParams({ button }) }).catch(() => {});
    flash("select");
  }

  document.addEventListener("pointerlockchange", () => {
    pad.toggleAttribute("data-captured", locked());
    if (!locked()) unlockedAt = performance.now();
  });

  document.addEventListener("mousemove", (e) => {
    if (!locked()) return;
    pending.move[0] += e.movementX;
    pending.move[1] += e.movementY;
    flush("move");
  });

  document.addEventListener(
    "wheel",
    (e) => {
      if (!locked()) return;
      e.preventDefault();
      // A wheel's deltaY is positive scrolling down; the TV's wheel axis is positive up.
      const lines = e.deltaMode === WheelEvent.DOM_DELTA_LINE ? 40 : 1;
      pending.scroll[0] += e.deltaX * lines * WHEEL;
      pending.scroll[1] += -e.deltaY * lines * WHEEL;
      flush("scroll");
    },
    { passive: false },
  );

  pad.addEventListener("contextmenu", (e) => inBrowser() && e.preventDefault());

  // Leaving browser mode (Back to Kodi) gives the cursor back.
  document.body.addEventListener("htmx:afterSettle", () => {
    if (locked() && !inBrowser()) document.exitPointerLock();
  });

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
    // The Esc that ends mouse mode is the browser's, not Back on the TV.
    if (e.key === "Escape" && (locked() || performance.now() - unlockedAt < 500)) return;
    // Typing in a field (the library search) is text, not remote control.
    if (e.target instanceof Element && e.target.closest("input, textarea, select, [contenteditable]")) return;
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

  // Phones show one view at a time: the remote, the library or search. Library and search
  // share the #media pane (so Back from a show found by search still works), and
  // `app.dataset.pane` says which of the two it holds. Laptops show the remote beside the
  // pane, so their tab bar only switches the pane.

  const VIEWS = ["remote", "library", "search"];
  const wide = matchMedia("(min-width: 900px)");

  /** Fills #media for the current pane: search, or the selected library segment. While the
   * browser is on the TV, both are Apple TV's instead: its pages, and a search that runs there. */
  function loadPane() {
    if (inBrowser()) {
      const path = app.dataset.pane === "library" ? "browser/apple-tv" : "browser/search";
      htmx.ajax("GET", path, { source: document.getElementById("media"), target: "#media" });
      return;
    }
    const segment = app.dataset.pane === "library" ? document.querySelector(".segmented [aria-selected='true']") : null;
    htmx.ajax("GET", segment?.getAttribute("hx-get") ?? "search", {
      source: segment ?? document.getElementById("media"),
      target: "#media",
    });
  }

  function setView(view) {
    app.dataset.view = view;
    if (view !== "remote" && view !== app.dataset.pane) {
      app.dataset.pane = view;
      loadPane();
    }
    const shown = view === "remote" && wide.matches ? app.dataset.pane : view;
    for (const tab of document.querySelectorAll(".tabbar [data-view]")) {
      tab.setAttribute("aria-pressed", String(tab.dataset.view === shown));
    }
    try { localStorage.setItem("view", view); } catch {}
  }

  let initialView = "remote";
  try {
    const saved = localStorage.getItem("view");
    if (VIEWS.includes(saved)) initialView = saved;
  } catch {}
  if (VIEWS.includes(location.hash.slice(1))) initialView = location.hash.slice(1);
  app.dataset.pane = initialView === "search" ? "search" : "library";
  setView(initialView);
  document.addEventListener("DOMContentLoaded", loadPane, { once: true });
  wide.addEventListener("change", () => setView(app.dataset.view));
  // Kodi was just switched on: reload the pane so it stops saying "Kodi is off".
  document.body.addEventListener("launched", loadPane);

  // Switching between Kodi and the browser changes what the panes hold.
  let browserShown = inBrowser();
  document.body.addEventListener("htmx:afterSettle", () => {
    if (inBrowser() === browserShown) return;
    browserShown = inBrowser();
    loadPane();
  });

  document.addEventListener("click", (e) => {
    const tab = e.target.closest(".tabbar [data-view]");
    if (tab) setView(tab.dataset.view);
    // In the browser, the Search tab opens the site's own search on the TV.
    if (tab?.dataset.view === "search" && inBrowser()) post("browser/go", tab, { page: "search" });

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

  // The text sheet starts empty each time, however it was closed.
  document.getElementById("text-sheet").addEventListener("close", (e) => e.target.querySelector("form").reset());

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
    if (el.matches("[data-on-tv]")) {
      setView("remote");
      toast(el.dataset.onTv);
    }
  });

  // A link in the search bar plays on the TV instead of searching. This runs in the capture
  // phase, ahead of htmx's own submit handler on the form.
  document.addEventListener("submit", (e) => {
    const form = e.target.closest(".search-bar");
    const input = form?.elements.q; // the browser's search field is "term": links don't apply
    if (!input || !/https?:\/\//.test(input.value)) return;
    e.preventDefault();
    e.stopPropagation();
    post("link", form, { url: input.value });
    input.blur();
  }, true);

  // Sent by /link once Kodi starts the link.
  document.body.addEventListener("played", (e) => {
    const input = document.querySelector(".search-bar input");
    if (input) input.value = "";
    setView("remote");
    toast(`Playing ${e.detail.value}`);
  });

  // ── Streaming add-ons (Netflix, Apple TV) ────────────────────────
  // Add-on folders have no parent link, so remember the pages passed on the way in. Each
  // add-on page swapped into the pane pushes the one it replaced; Back pops instead.
  // Reaching an add-on's home, or leaving the add-ons, starts a fresh trail.

  const addonTrail = [];
  let addonShown = null; // URL of the add-on page in the pane, if one is
  let goingBack = false;

  document.body.addEventListener("htmx:afterSwap", (e) => {
    if (e.detail.target.id !== "media") return;
    const page = e.detail.target.querySelector("[data-addon-url]");
    if (!page || page.hasAttribute("data-addon-home")) addonTrail.length = 0;
    else if (!goingBack && addonShown) addonTrail.push(addonShown);
    addonShown = page?.dataset.addonUrl ?? null;
    goingBack = false;
  });

  document.addEventListener("click", (e) => {
    const back = e.target.closest("[data-addon-back]");
    if (!back) return;
    goingBack = true;
    htmx.ajax("GET", addonTrail.pop() ?? back.dataset.addonBack, { source: back, target: "#media" })
      .finally(() => { goingBack = false; });
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

  // The device's own audio output delay (speakers ~10 ms, Bluetooth headphones far more), for
  // auto sync. Read from an AudioContext, which has to be created in a tap too.
  let outputContext = null;
  const outputMs = () => Math.round(((outputContext?.outputLatency || outputContext?.baseLatency) ?? 0) * 1000);

  /** Must run inside a tap: starts the (still empty) audio element so the stream can join it. */
  function primeAudio() {
    try { outputContext ??= new AudioContext({ latencyHint: "interactive" }); } catch {}
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
    // Leave the browser's adaptive jitter buffer alone. jitterBufferTarget can't shorten it:
    // Safari ignores it (40 ms still gave ~210 ms) and Chrome treats it as a minimum.
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
  // server log. Loss, jitter and concealment are only visible here, not on the PC. bufferMs is
  // the jitter buffer over the last 5s; the stats only give a running total since the start.
  let lastBuffer = null;
  setInterval(async () => {
    if (!peer || peer.connectionState !== "connected") return;
    const report = { ua: navigator.userAgent.replace(/^.*\) /, ""), outputMs: outputMs() };
    for (const s of (await peer.getStats()).values()) {
      if (s.type === "inbound-rtp" && s.kind === "audio") {
        Object.assign(report, {
          received: s.packetsReceived, lost: s.packetsLost, jitterMs: Math.round(s.jitter * 1000),
          concealedMs: Math.round((s.concealedSamples / 48000) * 1000), concealEvents: s.concealmentEvents,
          bufferMs: (() => {
            const prev = lastBuffer;
            lastBuffer = { delay: s.jitterBufferDelay, count: s.jitterBufferEmittedCount };
            const count = s.jitterBufferEmittedCount - (prev?.count ?? 0);
            return count > 0 ? Math.round(((s.jitterBufferDelay - (prev?.delay ?? 0)) / count) * 1000) : null;
          })(),
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
    // Auto sync: how long after reaching this device the sound plays (half the round trip, the
    // jitter buffer, the output). The server adds its own part and moves Kodi's sound earlier.
    if (report.bufferMs != null && report.rttMs != null) {
      const ms = report.bufferMs + Math.round(report.rttMs / 2) + outputMs();
      fetch("listen/delay", { method: "POST", body: new URLSearchParams({ ms }) }).catch(() => {});
    }
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
