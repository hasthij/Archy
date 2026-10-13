"use strict";
// Everything lives in its own scope: app.js has functions with the same names
// (renderAudio...), and a clash silently replaced them. Only openEditor is shared.
(() => {
// Clip editor: trim, audio mix, text and image layers, export as MP4 or GIF
// (save next to the clip, or copy to the clipboard). Uses app.js helpers.
// Nothing here touches recording; the export runs FFmpeg (see src/editor.rs).
//
// Kept light on purpose: the preview canvas is sized to the screen (not the
// clip) and only redrawn when something on it changes; per-frame work is the
// playhead and the time label. Audio plays from decoded buffers started in step
// with the video, never by seeking media elements.

const FONTS = [
  ["Segoe UI", "Clean"], ["Impact", "Meme"], ["Bahnschrift", "Gamer"], ["Arial Black", "Bold"],
  ["Comic Sans MS", "Fun"], ["Georgia", "Classy"], ["Segoe Script", "Handwritten"], ["Consolas", "Code"],
];
const COLORS = ["#FFFFFF", "#FFE14D", "#4D8DFF", "#FF5C5C", "#5CFF8F", "#000000"];
const GIF_SIZES = [[320, "Small"], [480, "Medium"], [640, "Large"]];
const MAX_LANES = 3;
const TRACK_INFO = {
  Everything: "Everything you heard: the game, your friends, your mic and other apps.",
  Game: "Only the game's sound.",
  Discord: "Only Discord: your friends' voices.",
  Microphone: "Only your microphone: your voice.",
};

let ed = null; // the open edit
let nextId = 1;

const fmt = (s) => {
  s = Math.max(0, s);
  const m = Math.floor(s / 60);
  return `${m}:${(s - m * 60).toFixed(1).padStart(4, "0")}`;
};
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

/// Which track holds everything you heard. Clips from before track names were
/// stored say "Track N"; their track 1 is "Everything" with the default settings.
const everythingTrack = (tracks) => { const i = tracks.indexOf("Everything"); return i < 0 && tracks[0] === "Track 1" ? 0 : i; };

// ---------- drawing (clip coordinates: W x H = the clip's size) ----------
const measure = document.createElement("canvas").getContext("2d");

function textFont(it, H) { return `700 ${Math.round(it.size * H)}px "${it.font}"`; }

/// Size of an item in clip pixels.
function itemSize(it, W, H) {
  if (it.kind === "image") {
    const w = it.w * W;
    return [w, w / it.aspect];
  }
  measure.font = textFont(it, H);
  const lines = it.text.split("\n");
  const px = it.size * H;
  const width = Math.max(...lines.map((l) => measure.measureText(l).width), px * 0.5);
  return [width + px * 0.3, px * 1.15 * lines.length];
}

function drawItem(ctx, it, W, H) {
  const cx = it.x * W, cy = it.y * H;
  if (it.kind === "image") {
    const [w, h] = itemSize(it, W, H);
    ctx.drawImage(it.img, cx - w / 2, cy - h / 2, w, h);
    return;
  }
  const px = it.size * H;
  const lines = it.text.split("\n");
  const lh = px * 1.15;
  ctx.font = textFont(it, H);
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.lineJoin = "round";
  lines.forEach((line, i) => {
    const y = cy - (lh * lines.length) / 2 + lh * (i + 0.5);
    if (it.outline) {
      ctx.strokeStyle = it.color === "#000000" ? "#FFFFFF" : "#000000";
      ctx.lineWidth = Math.max(2, px * 0.14);
      ctx.strokeText(line, cx, y);
    }
    ctx.fillStyle = it.color;
    ctx.fillText(line, cx, y);
  });
}

/// Bottom layer first: image tracks, then text tracks (track 1 lowest).
function drawOrder() {
  const rank = (it) => (it.kind === "image" ? 0 : 10) + it.lane;
  return [...ed.items].sort((a, b) => rank(a) - rank(b) || a.start - b.start);
}

const shownAt = (it, t) => t >= it.start && t < it.end;

// ---------- open / close ----------
async function openEditor(clip, opts = {}) {
  let ready = false;
  try { ready = await invoke("editor_ready"); } catch {}
  if (!ready) { toast("The editor's video tools aren't installed yet. Update or reinstall Archy to get them."); return; }
  let info;
  try { info = await invoke("clip_info", { path: clip.path }); }
  catch (e) { toast(String(e)); return; }
  if (!$("#player").hidden) closePlayer();
  const ev = everythingTrack(info.tracks);
  ed = {
    clip, info, ev,
    start: 0, end: info.duration,
    // Simple: one volume for everything you heard. Separate: each source on its own.
    mode: ev >= 0 ? "simple" : "separate",
    main: 100,
    sep: info.tracks.map(() => 100),
    solo: -1,
    items: [], lanes: { text: 1, image: 1 }, media: [], selected: null,
    format: opts.format || "mp4", gifWidth: 480, gifFps: 15,
    audio: { ctx: null, buffers: null, gains: [], sources: [], t0: 0, at: 0 },
    busy: false, dirty: true, sig: "", lastT: -1, lastLabel: "", driftAt: 0, dom: {},
  };
  buildEditor();
  loadPreviewAudio();
}

function closeEditor() {
  if (!ed) return;
  if (ed.busy) { toast("Exporting: wait for it to finish, or press Cancel export"); return; }
  stopAudio();
  if (ed.audio.ctx) ed.audio.ctx.close().catch(() => {});
  cancelAnimationFrame(ed.raf);
  ed.resize?.disconnect();
  const v = ed.dom.video;
  v.pause(); v.removeAttribute("src"); v.load();
  ed = null;
  $("#editor").hidden = true;
  $("#editor").replaceChildren();
}

// ---------- audio ----------
function volumes() {
  return ed.info.tracks.map((_, i) => {
    if (ed.mode === "simple") return i === ed.ev ? ed.main : 0;
    return i === ed.ev ? 0 : ed.sep[i];
  });
}

async function loadPreviewAudio() {
  const mine = ed;
  let files = [];
  try { files = await invoke("editor_tracks", { path: mine.clip.path, count: mine.info.tracks.length }); }
  catch (e) { toast("Audio preview unavailable: " + e); return; }
  if (ed !== mine || !files.length) return;
  try {
    const ctx = new AudioContext();
    const buffers = await Promise.all(files.map((f) =>
      fetch(fileSrc(f)).then((r) => r.arrayBuffer()).then((b) => ctx.decodeAudioData(b))));
    if (ed !== mine) { ctx.close(); return; }
    mine.audio.ctx = ctx;
    mine.audio.buffers = buffers;
    mine.audio.gains = buffers.map(() => { const g = ctx.createGain(); g.connect(ctx.destination); return g; });
    applyVolumes();
    if (!mine.dom.video.paused) startAudio(mine.dom.video.currentTime);
  } catch (e) { toast("Audio preview unavailable: " + e); }
}

function applyVolumes() {
  const vols = volumes();
  ed.audio.gains.forEach((g, i) => { g.gain.value = ed.solo >= 0 ? (i === ed.solo ? 1 : 0) : vols[i] / 100; });
}

function startAudio(at) {
  stopAudio();
  const a = ed.audio;
  if (!a.buffers) return;
  a.ctx.resume();
  const when = a.ctx.currentTime + 0.02;
  a.sources = a.buffers.map((buf, i) => {
    if (at >= buf.duration) return null;
    const s = a.ctx.createBufferSource();
    s.buffer = buf;
    s.connect(a.gains[i]);
    s.start(when, at);
    return s;
  }).filter(Boolean);
  a.t0 = when;
  a.at = at;
}

function stopAudio() {
  for (const s of ed.audio.sources) { try { s.stop(); } catch {} }
  ed.audio.sources = [];
}

// ---------- layout ----------
function buildEditor() {
  const { info } = ed;
  const d = ed.dom;
  d.video = el("video", { muted: true, playsinline: true, preload: "auto" });
  d.video.muted = true;
  d.video.src = fileSrc(ed.clip.path);
  d.canvas = el("canvas", { class: "ed-canvas" });
  d.stage = el("div", { class: "ed-stage", style: `aspect-ratio:${info.width}/${info.height};width:min(100%, calc(56vh * ${info.width} / ${info.height}))` }, d.video, d.canvas);
  stagePointer(d.canvas);
  dropTarget(d.stage, (m, e) => {
    const r = d.canvas.getBoundingClientRect();
    addImage(m, 0, d.video.currentTime, [(e.clientX - r.left) / r.width, (e.clientY - r.top) / r.height]);
  });

  d.play = el("button", { class: "btn", text: "Play", onclick: togglePlay });
  d.time = el("span", { class: "ed-time" });
  d.range = el("div", { class: "ed-range" });
  d.hStart = el("div", { class: "ed-handle", "aria-label": "Trim start", role: "slider", tabindex: "0" });
  d.hEnd = el("div", { class: "ed-handle", "aria-label": "Trim end", role: "slider", tabindex: "0" });
  d.head = el("div", { class: "ed-head" });
  d.timeline = el("div", { class: "ed-timeline" }, d.range, d.hStart, d.hEnd, d.head);
  timelineDrag(d.timeline);
  d.lanes = el("div", { class: "ed-lanes" });
  d.laneButtons = el("div", { class: "ed-row ed-lane-add" });

  d.bay = el("aside", { class: "ed-bay" });
  d.trimInfo = el("p", { class: "ed-note" });
  d.audio = el("div");
  d.props = el("div");
  d.export = el("div");
  const side = el("div", { class: "ed-side" },
    section("Trim", trimPanel()),
    section("Audio", d.audio),
    section("Selected", d.props),
    section("Export", d.export));

  const box = el("div", { class: "ed-box", role: "dialog", "aria-modal": "true", "aria-label": "Clip editor" },
    el("div", { class: "player-bar" },
      el("span", { text: `Edit · ${ed.clip.name}` }),
      (d.close = el("button", { class: "btn", text: "Close", onclick: closeEditor }))),
    el("div", { class: "ed-body" },
      d.bay,
      el("div", { class: "ed-main" },
        d.stage,
        el("div", { class: "ed-transport" }, d.play, d.time),
        el("div", { class: "ed-tl" }, el("span", { class: "ed-lane-name", text: "Clip" }), d.timeline),
        d.lanes, d.laneButtons),
      side));
  $("#editor").replaceChildren(box);
  $("#editor").hidden = false;

  const v = d.video;
  v.addEventListener("loadedmetadata", () => { v.currentTime = ed.start; });
  v.addEventListener("play", () => { d.play.textContent = "Pause"; });
  v.addEventListener("playing", () => startAudio(v.currentTime));
  v.addEventListener("pause", () => { d.play.textContent = "Play"; stopAudio(); });
  v.addEventListener("seeking", stopAudio);
  v.addEventListener("waiting", stopAudio);
  v.addEventListener("seeked", () => { ed.dirty = true; if (!v.paused) startAudio(v.currentTime); });

  // The canvas matches its on-screen size (sharp, and far cheaper than clip size).
  ed.resize = new ResizeObserver(() => {
    const r = d.canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    d.canvas.width = Math.max(1, Math.round(r.width * dpr));
    d.canvas.height = Math.max(1, Math.round(r.height * dpr));
    ed.dirty = true;
  });
  ed.resize.observe(d.canvas);

  ed.raf = requestAnimationFrame(tick);
  renderBay(); renderAudio(); renderLanes(); renderProps(); renderExport(); updateTrim();
}

function section(title, body) {
  return el("div", { class: "ed-section" }, el("h3", { text: title }), body);
}

function togglePlay() {
  const v = ed.dom.video;
  if (v.paused) v.play().catch(() => {}); else v.pause();
}

function tick(now) {
  if (!ed) return;
  const d = ed.dom;
  const v = d.video;
  const t = v.currentTime;
  // Play only the trimmed part, looping back to its start.
  if (!v.paused && !v.seeking && t >= ed.end - 0.03) v.currentTime = ed.start;
  if (t !== ed.lastT) {
    ed.lastT = t;
    const p = pct(t);
    d.head.style.left = p;
    if (d.laneHead) d.laneHead.style.left = p;
    const label = `${fmt(t - ed.start)} / ${fmt(ed.end - ed.start)}`;
    if (label !== ed.lastLabel) { ed.lastLabel = label; d.time.textContent = label; }
  }
  // Redraw only when what's on screen changes.
  const sig = ed.items.filter((it) => shownAt(it, t)).map((it) => it.id).join(",");
  if (ed.dirty || sig !== ed.sig) { ed.sig = sig; ed.dirty = false; drawPreview(t); }
  // Re-align audio if it has drifted (checked twice a second, not every frame).
  const a = ed.audio;
  if (!v.paused && a.sources.length && now - ed.driftAt > 500) {
    ed.driftAt = now;
    if (Math.abs(a.at + (a.ctx.currentTime - a.t0) - t) > 0.12) startAudio(t);
  }
  ed.raf = requestAnimationFrame(tick);
}

function drawPreview(t) {
  const c = ed.dom.canvas;
  const ctx = c.getContext("2d");
  const { width: W, height: H } = ed.info;
  const k = c.width / W;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, c.width, c.height);
  ctx.setTransform(k, 0, 0, k, 0, 0);
  for (const it of drawOrder()) {
    const shown = shownAt(it, t);
    if (!shown && it.id !== ed.selected) continue;
    // The selected item shows faintly outside its time, so it can still be placed.
    ctx.globalAlpha = shown ? 1 : 0.35;
    drawItem(ctx, it, W, H);
  }
  ctx.globalAlpha = 1;
  const sel = selectedItem();
  if (sel) {
    const [w, h] = itemSize(sel, W, H);
    const x = sel.x * W - w / 2, y = sel.y * H - h / 2;
    ctx.setLineDash([6 / k, 4 / k]);
    ctx.lineWidth = 2 / k;
    ctx.strokeStyle = "#4D8DFF";
    ctx.strokeRect(x, y, w, h);
    ctx.setLineDash([]);
    ctx.fillStyle = "#4D8DFF";
    const hs = 12 / k;
    ctx.fillRect(x + w - hs / 2, y + h - hs / 2, hs, hs);
  }
}

// ---------- moving and resizing on the video ----------
function stagePointer(canvas) {
  let drag = null;
  const toClip = (e) => {
    const r = canvas.getBoundingClientRect();
    return [(e.clientX - r.left) / r.width * ed.info.width, (e.clientY - r.top) / r.height * ed.info.height];
  };
  canvas.addEventListener("pointerdown", (e) => {
    const { width: W, height: H } = ed.info;
    const [px, py] = toClip(e);
    const k = canvas.getBoundingClientRect().width / W; // screen px per clip px
    const t = ed.dom.video.currentTime;
    const sel = selectedItem();
    const box = (it) => { const [w, h] = itemSize(it, W, H); return [it.x * W - w / 2, it.y * H - h / 2, w, h]; };
    if (sel) {
      const [x, y, w, h] = box(sel);
      if (Math.abs(px - (x + w)) < 14 / k && Math.abs(py - (y + h)) < 14 / k) {
        drag = { mode: "resize", it: sel, cx: sel.x * W, cy: sel.y * H, d0: Math.hypot(px - sel.x * W, py - sel.y * H), w0: sel.w, s0: sel.size };
      }
    }
    if (!drag) {
      const hit = drawOrder().reverse().find((it) => {
        if (!shownAt(it, t) && it.id !== ed.selected) return false;
        const [x, y, w, h] = box(it);
        return px >= x && px <= x + w && py >= y && py <= y + h;
      });
      if (hit) {
        if (ed.selected !== hit.id) select(hit.id);
        drag = { mode: "move", it: hit, dx: px - hit.x * W, dy: py - hit.y * H };
      } else drag = { mode: "click", x: e.clientX, y: e.clientY };
    }
    canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointermove", (e) => {
    if (!drag || drag.mode === "click") return;
    const { width: W, height: H } = ed.info;
    const [px, py] = toClip(e);
    const it = drag.it;
    if (drag.mode === "move") {
      it.x = clamp((px - drag.dx) / W, 0, 1);
      it.y = clamp((py - drag.dy) / H, 0, 1);
    } else {
      const f = Math.hypot(px - drag.cx, py - drag.cy) / Math.max(1, drag.d0);
      if (it.kind === "image") it.w = clamp(drag.w0 * f, 0.03, 1.5);
      else it.size = clamp(drag.s0 * f, 0.02, 0.3);
    }
    ed.dirty = true;
  });
  canvas.addEventListener("pointerup", (e) => {
    if (drag?.mode === "click" && Math.hypot(e.clientX - drag.x, e.clientY - drag.y) < 4) togglePlay();
    else if (drag?.mode === "resize") renderProps();
    drag = null;
  });
}

// ---------- timeline: trim ----------
function pct(s) { return (s / ed.info.duration) * 100 + "%"; }

function updateTrim() {
  const d = ed.dom;
  d.hStart.style.left = pct(ed.start);
  d.hEnd.style.left = pct(ed.end);
  d.range.style.left = pct(ed.start);
  d.range.style.width = pct(ed.end - ed.start);
  d.trimInfo.textContent = `${fmt(ed.start)} → ${fmt(ed.end)} · ${(ed.end - ed.start).toFixed(1)} s`;
  ed.lastLabel = "";
  ed.lastT = -1;
}

function timelineDrag(line) {
  const at = (e) => {
    const r = line.getBoundingClientRect();
    return clamp((e.clientX - r.left) / r.width, 0, 1) * ed.info.duration;
  };
  let grab = null;
  line.addEventListener("pointerdown", (e) => {
    grab = e.target === ed.dom.hStart ? "start" : e.target === ed.dom.hEnd ? "end" : "head";
    line.setPointerCapture(e.pointerId);
    move(e);
  });
  const move = (e) => {
    if (!grab) return;
    const s = at(e);
    const v = ed.dom.video;
    if (grab === "start") { ed.start = Math.min(s, ed.end - 0.5); v.currentTime = ed.start; }
    else if (grab === "end") { ed.end = Math.max(s, ed.start + 0.5); v.currentTime = Math.max(ed.start, ed.end - 1); }
    else v.currentTime = clamp(s, ed.start, ed.end - 0.05);
    if (grab !== "head") updateTrim();
  };
  line.addEventListener("pointermove", move);
  line.addEventListener("pointerup", () => { grab = null; });
  // Arrow keys nudge a focused handle by 0.1 s.
  line.addEventListener("keydown", (e) => {
    const d = e.key === "ArrowLeft" ? -0.1 : e.key === "ArrowRight" ? 0.1 : 0;
    if (!d) return;
    if (e.target === ed.dom.hStart) ed.start = clamp(ed.start + d, 0, ed.end - 0.5);
    if (e.target === ed.dom.hEnd) ed.end = clamp(ed.end + d, ed.start + 0.5, ed.info.duration);
    updateTrim();
    e.preventDefault();
  });
}

function trimPanel() {
  const here = () => ed.dom.video.currentTime;
  return el("div", {},
    ed.dom.trimInfo,
    el("div", { class: "ed-row" },
      el("button", { class: "btn", text: "Start here", onclick: () => { ed.start = Math.min(here(), ed.end - 0.5); updateTrim(); } }),
      el("button", { class: "btn", text: "End here", onclick: () => { ed.end = Math.max(here(), ed.start + 0.5); updateTrim(); } }),
      el("button", { class: "btn", text: "Reset", onclick: () => { ed.start = 0; ed.end = ed.info.duration; updateTrim(); } })),
    el("p", { class: "ed-note", text: "Drag the blue handles on the Clip track, or play to a moment and press Start here / End here." }));
}

// ---------- audio panel ----------
function trackLabel(i) {
  const name = ed.info.tracks[i];
  if (i === ed.ev) return ["Main volume", TRACK_INFO.Everything];
  if (TRACK_INFO[name]) return [name, TRACK_INFO[name]];
  return [name, "The game, Discord or your mic (from an older clip). Press Listen to hear which."];
}

function renderAudio() {
  const { tracks } = ed.info;
  if (!tracks.length) { ed.dom.audio.replaceChildren(el("p", { class: "ed-note", text: "This clip has no sound." })); return; }
  const listen = (i) => el("button", {
    class: "btn ed-listen", "aria-pressed": String(ed.solo === i), text: ed.solo === i ? "Stop" : "Listen",
    title: "Hear only this track",
    onclick: () => {
      ed.solo = ed.solo === i ? -1 : i;
      applyVolumes();
      if (ed.solo >= 0 && ed.dom.video.paused) togglePlay();
      renderAudio();
    },
  });
  const row = (i, value, enabled, set) => {
    const [name, about] = trackLabel(i);
    const out = el("span", { class: "ed-vol", text: enabled ? `${value}%` : "Off" });
    const range = el("input", { type: "range", min: "0", max: "200", step: "5", value: String(value), "aria-label": `${name} volume`, disabled: enabled ? null : "" });
    range.addEventListener("input", () => { set(Number(range.value)); out.textContent = `${range.value}%`; applyVolumes(); });
    return el("div", { class: "ed-track" + (enabled ? "" : " off") },
      el("div", { class: "ed-track-top" }, el("b", { text: name }), listen(i)),
      el("p", { class: "ed-note", text: about }),
      el("div", { class: "ed-track-slider" }, range, out));
  };
  const simple = ed.mode === "simple";
  const others = tracks.map((_, i) => i).filter((i) => i !== ed.ev);
  const kids = [];
  if (ed.ev >= 0 && others.length) {
    kids.push(el("div", { class: "seg ed-mode", role: "group", "aria-label": "How to mix" },
      [["simple", "One volume"], ["separate", "Each source"]].map(([m, label]) => el("button", {
        "aria-pressed": String(ed.mode === m),
        onclick: () => { ed.mode = m; ed.solo = -1; applyVolumes(); renderAudio(); },
      }, label))));
    kids.push(el("p", { class: "ed-note", text: simple
      ? "One volume for the whole clip. To change the game, Discord or your mic on their own, pick Each source."
      : "Each source on its own: the main volume is off, and these sources are mixed instead (other apps' sounds aren't included)." }));
  }
  if (ed.ev >= 0) kids.push(row(ed.ev, ed.main, simple, (v) => (ed.main = v)));
  if (others.length) {
    kids.push(el("div", { class: "ed-sub", text: "Per source" }));
    for (const i of others) kids.push(row(i, ed.sep[i], !simple, (v) => (ed.sep[i] = v)));
  }
  ed.dom.audio.replaceChildren(...kids);
}

// ---------- text and image tracks ----------
function selectedItem() { return ed.items.find((it) => it.id === ed.selected) || null; }

function select(id) {
  ed.selected = id;
  ed.dirty = true;
  renderLanes();
  renderProps();
}

function addText(lane) {
  const start = clamp(ed.dom.video.currentTime, ed.start, ed.end - 0.5);
  const it = { id: nextId++, kind: "text", lane, start, end: Math.min(ed.end, start + 3),
    text: "Your text", font: "Impact", size: 0.075, color: "#FFFFFF", outline: true, x: 0.5, y: 0.85 };
  ed.items.push(it);
  select(it.id);
}

function addImage(m, lane, t, pos = [0.5, 0.5]) {
  const start = clamp(t, ed.start, ed.end - 0.5);
  const it = { id: nextId++, kind: "image", lane, start, end: Math.min(ed.end, start + 3),
    img: m.img, name: m.name, aspect: m.img.naturalWidth / Math.max(1, m.img.naturalHeight),
    w: 0.3, x: clamp(pos[0], 0, 1), y: clamp(pos[1], 0, 1) };
  ed.items.push(it);
  select(it.id);
}

function deleteSelected() {
  ed.items = ed.items.filter((it) => it.id !== ed.selected);
  select(null);
}

function renderLanes() {
  const d = ed.dom;
  const rows = [];
  const lane = (kind, n) => {
    const name = `${kind === "text" ? "Text" : "Image"} ${n + 1}`;
    const track = el("div", { class: "ed-lane-track" });
    for (const it of ed.items.filter((x) => x.kind === kind && x.lane === n)) {
      const bar = el("div", {
        class: `ed-item ${kind}` + (it.id === ed.selected ? " on" : ""),
        style: `left:${pct(it.start)};width:${pct(it.end - it.start)}`,
        title: kind === "text" ? it.text : it.name,
      }, el("span", { text: kind === "text" ? it.text.split("\n")[0] || "(empty)" : it.name }));
      barDrag(bar, it, track);
      track.append(bar);
    }
    if (kind === "image") dropTarget(track, (m, e) => {
      const r = track.getBoundingClientRect();
      addImage(m, n, (e.clientX - r.left) / r.width * ed.info.duration);
    });
    const add = el("button", { class: "ed-lane-plus", text: "+", title: kind === "text" ? `Add text to ${name}` : `Add the newest image to ${name}`,
      onclick: () => kind === "text" ? addText(n) : (ed.media.length ? addImage(ed.media[ed.media.length - 1], n, ed.dom.video.currentTime) : toast("Add an image on the left first")) });
    rows.push(el("div", { class: "ed-tl" }, el("span", { class: "ed-lane-name" }, name, add), track));
  };
  for (let n = 0; n < ed.lanes.image; n++) lane("image", n);
  for (let n = 0; n < ed.lanes.text; n++) lane("text", n);
  // One playhead line across all the tracks.
  d.laneHead = el("div", { class: "ed-head ed-lane-head" });
  d.lanes.replaceChildren(el("div", { class: "ed-lanes-in" }, ...rows, el("div", { class: "ed-lane-headbox" }, d.laneHead)));
  ed.lastT = -1;
  d.laneButtons.replaceChildren(
    el("button", { class: "btn", text: "+ Text track", disabled: ed.lanes.text >= MAX_LANES ? "" : null, onclick: () => { ed.lanes.text++; renderLanes(); } }),
    el("button", { class: "btn", text: "+ Image track", disabled: ed.lanes.image >= MAX_LANES ? "" : null, onclick: () => { ed.lanes.image++; renderLanes(); } }));
}

/// Drag a bar to move it in time; drag its ends to change when it starts/stops.
function barDrag(bar, it, track) {
  bar.addEventListener("pointerdown", (e) => {
    e.stopPropagation();
    const r = track.getBoundingClientRect();
    const secPerPx = ed.info.duration / r.width;
    const br = bar.getBoundingClientRect();
    const mode = e.clientX - br.left < 8 ? "start" : br.right - e.clientX < 8 ? "end" : "move";
    const x0 = e.clientX, s0 = it.start, e0 = it.end;
    if (ed.selected !== it.id) { ed.selected = it.id; ed.dirty = true; renderProps(); bar.classList.add("on"); }
    bar.setPointerCapture(e.pointerId);
    const move = (ev) => {
      const dt = (ev.clientX - x0) * secPerPx;
      const len = e0 - s0;
      if (mode === "move") { it.start = clamp(s0 + dt, 0, ed.info.duration - len); it.end = it.start + len; }
      else if (mode === "start") it.start = clamp(s0 + dt, 0, it.end - 0.2);
      else it.end = clamp(e0 + dt, it.start + 0.2, ed.info.duration);
      bar.style.left = pct(it.start);
      bar.style.width = pct(it.end - it.start);
      ed.dirty = true;
    };
    const up = () => {
      bar.removeEventListener("pointermove", move);
      bar.removeEventListener("pointerup", up);
      renderLanes();
      renderProps();
    };
    bar.addEventListener("pointermove", move);
    bar.addEventListener("pointerup", up);
  });
}

// ---------- image bay ----------
function renderBay() {
  const input = el("input", { type: "file", accept: "image/*", multiple: true, hidden: true });
  input.addEventListener("change", () => { importImages(input.files); input.value = ""; });
  const zone = el("div", { class: "ed-drop" },
    el("b", { text: "Images" }),
    el("p", { class: "ed-note", text: "Drop images here, then drag them onto the video or an Image track." }),
    el("button", { class: "btn", text: "Browse…", onclick: () => input.click() }), input);
  zone.addEventListener("dragover", (e) => { if (e.dataTransfer.types.includes("Files")) { e.preventDefault(); zone.classList.add("over"); } });
  zone.addEventListener("dragleave", () => zone.classList.remove("over"));
  zone.addEventListener("drop", (e) => { e.preventDefault(); zone.classList.remove("over"); importImages(e.dataTransfer.files); });
  const list = el("div", { class: "ed-media" }, ed.media.map((m) => {
    const thumb = el("img", { src: m.src, alt: m.name, draggable: "true", title: `${m.name}: drag onto the video or an Image track, or click to add it now` });
    thumb.addEventListener("dragstart", (e) => e.dataTransfer.setData("text/archy-media", String(m.id)));
    thumb.addEventListener("click", () => addImage(m, 0, ed.dom.video.currentTime));
    return el("div", { class: "ed-thumb" }, thumb,
      el("button", { class: "ed-thumb-x", text: "×", "aria-label": `Remove ${m.name}`, onclick: () => { ed.media = ed.media.filter((x) => x !== m); renderBay(); } }));
  }));
  ed.dom.bay.replaceChildren(zone, list);
}

function importImages(files) {
  for (const f of files) {
    if (!f.type.startsWith("image/")) continue;
    const reader = new FileReader();
    reader.onload = () => {
      const img = new Image();
      img.onload = () => {
        if (!ed) return;
        ed.media.push({ id: nextId++, name: f.name, src: reader.result, img });
        renderBay();
      };
      img.src = reader.result;
    };
    reader.readAsDataURL(f);
  }
}

/// Lets an image from the bay (or a file from Explorer) be dropped on `target`.
function dropTarget(target, place) {
  target.addEventListener("dragover", (e) => {
    if (e.dataTransfer.types.includes("text/archy-media") || e.dataTransfer.types.includes("Files")) e.preventDefault();
  });
  target.addEventListener("drop", (e) => {
    e.preventDefault();
    const id = Number(e.dataTransfer.getData("text/archy-media"));
    const m = ed.media.find((x) => x.id === id);
    if (m) place(m, e);
    else if (e.dataTransfer.files.length) importImages(e.dataTransfer.files);
  });
}

// ---------- properties of the selected item ----------
function chipGroup(label, options, current, pick) {
  return el("div", { class: "chips ed-chips", role: "group", "aria-label": label },
    options.map(([value, text, style]) => el("button", {
      class: "chip", "aria-pressed": String(value === current), style: style || null,
      onclick: () => { pick(value); ed.dirty = true; renderProps(); },
    }, text)));
}

function timeInputs(it) {
  const changed = () => { ed.dirty = true; renderLanes(); renderProps(); };
  const num = (value, set) => {
    const input = el("input", { class: "input ed-num", type: "number", step: "0.1", min: "0", max: ed.info.duration.toFixed(1), value: value.toFixed(1) });
    input.addEventListener("change", () => { set(Number(input.value) || 0); changed(); });
    return input;
  };
  const here = () => ed.dom.video.currentTime;
  return el("div", {},
    el("div", { class: "ed-row" },
      el("span", { text: "Show from" }), num(it.start, (v) => (it.start = clamp(v, 0, it.end - 0.2))),
      el("span", { text: "to" }), num(it.end, (v) => (it.end = clamp(v, it.start + 0.2, ed.info.duration))),
      el("span", { class: "ed-note ed-inline", text: "seconds" })),
    el("div", { class: "ed-row" },
      el("button", { class: "btn", text: "Start at playhead", onclick: () => { it.start = Math.min(here(), it.end - 0.2); changed(); } }),
      el("button", { class: "btn", text: "End at playhead", onclick: () => { it.end = Math.max(here(), it.start + 0.2); changed(); } }),
      el("button", { class: "btn", text: "Whole clip", onclick: () => { it.start = ed.start; it.end = ed.end; changed(); } })));
}

function sizeSlider(label, value, min, max, set) {
  const range = el("input", { type: "range", min: String(min), max: String(max), step: "1", value: String(Math.round(value)), "aria-label": label });
  range.addEventListener("input", () => { set(Number(range.value)); ed.dirty = true; });
  return el("div", { class: "ed-slider ed-row" }, el("span", { text: label }), range);
}

function renderProps() {
  const it = selectedItem();
  const box = ed.dom.props;
  if (!it) {
    box.replaceChildren(el("p", { class: "ed-note", text: "Press + on a Text track to add text, or drag an image from the left onto the video. Click anything on the video or the tracks to change it; drag it on the video to move it, and drag its corner to resize." }));
    return;
  }
  const kids = [];
  if (it.kind === "text") {
    const input = el("textarea", { rows: "2", "aria-label": "Text" });
    input.value = it.text;
    input.addEventListener("input", () => { it.text = input.value; ed.dirty = true; });
    input.addEventListener("change", renderLanes);
    kids.push(input,
      chipGroup("Font", FONTS.map(([f, label]) => [f, label, `font-family:"${f}"`]), it.font, (v) => (it.font = v)),
      sizeSlider("Size", it.size * 100, 2, 30, (v) => (it.size = v / 100)),
      el("div", { class: "ed-row", role: "group", "aria-label": "Colour" },
        COLORS.map((col) => el("button", {
          class: "ed-swatch", style: `background:${col}`, "aria-label": col, "aria-pressed": String(it.color === col),
          onclick: () => { it.color = col; ed.dirty = true; renderProps(); },
        })),
        el("label", { class: "ed-check" },
          (() => { const b = el("input", { type: "checkbox" }); b.checked = it.outline; b.addEventListener("change", () => { it.outline = b.checked; ed.dirty = true; }); return b; })(),
          "Outline")));
  } else {
    kids.push(el("p", { class: "ed-note", text: it.name }),
      sizeSlider("Size", it.w * 100, 3, 150, (v) => (it.w = v / 100)));
  }
  kids.push(timeInputs(it),
    el("div", { class: "ed-row" },
      el("button", { class: "btn", text: "Center", onclick: () => { it.x = 0.5; it.y = 0.5; ed.dirty = true; } }),
      el("button", { class: "btn danger", text: "Delete", onclick: deleteSelected })));
  box.replaceChildren(...kids);
}

// ---------- export ----------
function renderExport() {
  const gif = ed.format === "gif";
  const len = ed.end - ed.start;
  const fmtSeg = el("div", { class: "seg", role: "group", "aria-label": "Format" },
    [["mp4", "MP4 video"], ["gif", "GIF"]].map(([v, label]) => el("button", {
      "aria-pressed": String(ed.format === v), onclick: () => { ed.format = v; renderExport(); },
    }, label)));
  const gifOpts = gif ? el("div", {},
    el("div", { class: "chips ed-chips", role: "group", "aria-label": "GIF size" },
      GIF_SIZES.map(([w, label]) => el("button", { class: "chip", "aria-pressed": String(ed.gifWidth === w), onclick: () => { ed.gifWidth = w; renderExport(); } }, `${label} (${w}px)`))),
    el("div", { class: "chips ed-chips", role: "group", "aria-label": "GIF frame rate" },
      [10, 15, 24].map((f) => el("button", { class: "chip", "aria-pressed": String(ed.gifFps === f), onclick: () => { ed.gifFps = f; renderExport(); } }, `${f} fps`))),
    el("p", { class: "ed-note", text: "GIFs have no sound. " + (len > 15 ? "This one is long: GIFs over ~15 s get very big, so trim it shorter if you can." : "Short GIFs (under 10 s) share best.") }))
    : el("p", { class: "ed-note", text: "One video with your trim, sound, text and images. The original clip is never changed." });
  ed.dom.bar = el("i");
  ed.dom.percent = el("span", { text: "Exporting… 0%" });
  ed.dom.progress = el("div", { class: "ed-progress-box", hidden: true },
    el("div", { class: "ed-progress-top" }, ed.dom.percent,
      el("button", { class: "btn danger", text: "Cancel export", onclick: () => invoke("cancel_export").catch(() => {}) })),
    el("div", { class: "ed-progress" }, ed.dom.bar),
    el("p", { class: "ed-note", text: "Keep the editor open until it's done. Your original clip is never changed." }));
  ed.dom.save = el("button", { class: "btn primary", text: "Save as…", onclick: () => runExport(false) });
  ed.dom.copy = el("button", { class: "btn", text: "Copy to clipboard", onclick: () => runExport(true) });
  ed.dom.export.replaceChildren(fmtSeg, gifOpts, el("div", { class: "ed-row" }, ed.dom.save, ed.dom.copy), ed.dom.progress);
}

function untouched() {
  const sound = ed.ev >= 0 ? ed.mode === "simple" && ed.main === 100 : ed.sep.every((v) => v === 100);
  return ed.format === "mp4" && ed.start < 0.05 && ed.end > ed.info.duration - 0.05 && !ed.items.length && sound;
}

/// One item alone on a transparent clip-sized picture, for FFmpeg to lay over the video.
function overlayImage(it) {
  const cv = document.createElement("canvas");
  cv.width = ed.info.width; cv.height = ed.info.height;
  drawItem(cv.getContext("2d"), it, cv.width, cv.height);
  return cv.toDataURL("image/png").split(",")[1];
}

async function runExport(toClipboard) {
  if (ed.busy) return;
  if (untouched()) {
    if (toClipboard) {
      try { await invoke("copy_clip", { path: ed.clip.path }); toast("Copied. Paste it into Discord or any chat"); }
      catch (e) { toast(String(e)); }
    } else toast("Nothing changed yet: the original clip is already saved");
    return;
  }
  const mine = ed;
  const d = mine.dom;
  let saveTo = null;
  if (!toClipboard) {
    try { saveTo = await invoke("choose_export_path", { path: mine.clip.path, format: mine.format }); } catch (e) { toast(String(e)); return; }
    if (!saveTo) return; // cancelled
  }
  ed.busy = true;
  d.video.pause();
  d.progress.hidden = false;
  d.bar.style.width = "0%";
  d.percent.textContent = "Exporting… 0%";
  d.save.disabled = d.copy.disabled = d.close.disabled = true;
  d.close.title = "Exporting: wait for it to finish, or press Cancel export";
  let unlisten = null;
  try {
    unlisten = await tauri.event.listen("export-progress", (e) => {
      const p = Math.round(e.payload * 100);
      d.bar.style.width = `${p}%`;
      d.percent.textContent = p >= 100 ? "Finishing…" : `Exporting… ${p}%`;
    });
  } catch {}
  try {
    const overlays = drawOrder()
      .filter((it) => it.end > mine.start && it.start < mine.end && (it.kind === "image" || it.text.trim()))
      .map((it) => ({ png: overlayImage(it), start: it.start, end: it.end }));
    const out = await invoke("export_clip", { request: {
      path: mine.clip.path, start: mine.start, end: mine.end, volumes: volumes(), overlays,
      format: mine.format, gif_width: mine.gifWidth, gif_fps: mine.gifFps, to_clipboard: toClipboard, save_to: saveTo,
    } });
    const kind = mine.format === "gif" ? "GIF" : "Video";
    if (toClipboard) toast(`${kind} copied. Paste it into Discord or any chat`);
    else { toast(`${kind} saved`); loadClips(); }
  } catch (e) {
    toast(String(e) === "cancelled" ? "Export cancelled. Nothing was saved" : String(e));
  } finally {
    if (unlisten) unlisten();
    mine.busy = false;
    d.progress.hidden = true;
    d.save.disabled = d.copy.disabled = d.close.disabled = false;
    d.close.title = "";
  }
}

document.addEventListener("keydown", (e) => {
  if (!ed) return;
  const a = document.activeElement;
  const typing = a && (a.tagName === "TEXTAREA" || (a.tagName === "INPUT" && !["range", "checkbox"].includes(a.type)));
  if (e.key === "Escape" && !typing) closeEditor();
  else if (typing) return;
  else if (e.key === " ") { e.preventDefault(); togglePlay(); }
  else if ((e.key === "Delete" || e.key === "Backspace") && ed.selected != null) { e.preventDefault(); deleteSelected(); }
});

window.openEditor = openEditor;
// Archy refused to close because an export is running (see main.rs).
tauri?.event.listen("export-busy", () => toast("Archy is still exporting. Wait for it to finish, or press Cancel export first")).catch(() => {});
})();
