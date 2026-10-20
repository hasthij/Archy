"use strict";

const tauri = window.__TAURI__;
const invoke = tauri ? tauri.core.invoke : async () => { throw new Error("not running inside Archy"); };
const fileSrc = (p) => (tauri ? tauri.core.convertFileSrc(p) : "");

let settings = null;
let presets = [];
let clips = [];
let filter = "all";
let view = "clips";

// ---------- small DOM helpers (never innerHTML with data) ----------
function el(tag, attrs = {}, ...kids) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") n.className = v;
    else if (k === "text") n.textContent = v;
    else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
    else if (v === true) n.setAttribute(k, "");
    else if (v !== false && v != null) n.setAttribute(k, v);
  }
  for (const k of kids.flat()) if (k != null) n.append(k);
  return n;
}
const $ = (s) => document.querySelector(s);

let toastTimer;
function toast(msg) {
  const t = $("#toast");
  t.textContent = msg;
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.hidden = true), 2600);
}

// ---------- navigation ----------
// Pages under Settings (the rail's gear); Games and Voice also have rail buttons.
const SETTINGS_PAGES = ["recording", "games", "audio", "hotkeys", "storage", "appearance", "general"];

function show(name) {
  view = name;
  const inSettings = SETTINGS_PAGES.includes(name);
  document.querySelectorAll(".settings-nav .nav").forEach((b) => b.setAttribute("aria-current", b.dataset.view === name ? "page" : "false"));
  // The rail lights its own button, or the gear for any other settings page.
  const railHit = [...document.querySelectorAll(".rail-btn")].some((b) => b.dataset.view === name);
  document.querySelectorAll(".rail-btn").forEach((b) => b.setAttribute("aria-current",
    b.dataset.view === name || (!railHit && inSettings && b.dataset.group === "settings") ? "page" : "false"));
  $("#settings-nav").hidden = !inSettings;
  document.querySelectorAll(".view").forEach((v) => (v.hidden = v.id !== `view-${name}`));
  if (name === "clips") loadClips();
}
document.querySelectorAll(".nav").forEach((b) => b.addEventListener("click", () => show(b.dataset.view)));
window.showView = show; // for welcome.js

// ---------- status ----------
async function refreshStatus() {
  let s;
  try { s = await invoke("get_status"); } catch { return; }
  const box = $("#status");
  box.className = "status";
  box.replaceChildren();
  $("#game-btn").textContent = gameLabel();
  const keys = $("#keys");
  if (settings) {
    const phrase = settings.voice_enabled && settings.voice_phrases[0] ? [el("span", { class: "sep", text: "·" }), `or say "${settings.voice_phrases[0].text}"`] : [];
    keys.replaceChildren(
      el("kbd", { text: settings.hotkey_save }), `last ${settings.clip_seconds} s`,
      el("span", { class: "sep", text: "·" }),
      el("kbd", { text: settings.hotkey_save_long }), `last ${settings.long_clip_seconds} s`, ...phrase);
  }
  if (!s.running) {
    box.classList.add("warn");
    box.append(
      el("span", { class: "dot warn" }),
      el("b", { text: "Recorder isn't running" }),
      el("button", { class: "btn primary", text: "Start Archy", onclick: async () => { await invoke("start_recorder").catch((e) => toast(String(e))); setTimeout(refreshStatus, 1500); } }));
    return;
  }
  const states = {
    recording: ["dot on", `Recording ${s.game}`, s.detail, "recording"],
    waiting: ["dot", "Waiting for a game", "Recording starts by itself", ""],
    paused: ["dot warn", "Paused", "Resume from the tray icon or your pause hotkey", "warn"],
  };
  const [dot, title, detail, kind] = states[s.state] || ["dot", s.state, "", ""];
  if (kind) box.classList.add(kind);
  box.append(el("span", { class: dot }), el("b", { text: title }), detail ? el("span", { class: "detail", text: detail }) : null);
}

// ---------- which game to record ----------
// Automatic = the game being played; or a game the user picked from the open
// apps, recorded whether or not it's in front (settings.record_target).
const gameLabel = () => (settings?.record_target ? `Game: ${settings.record_target.replace(/\.exe$/i, "")}` : "Game: Automatic")
  + (settings?.capture === "screen" ? " · whole screen" : "");

function closeGameMenu() {
  $("#game-menu").hidden = true;
  $("#game-btn").setAttribute("aria-expanded", "false");
}

async function openGameMenu() {
  const menu = $("#game-menu");
  if (!menu.hidden) { closeGameMenu(); return; }
  menu.replaceChildren(el("p", { class: "sub", text: "Looking for open apps…" }));
  menu.hidden = false;
  $("#game-btn").setAttribute("aria-expanded", "true");
  let apps = [];
  try { apps = await invoke("running_apps"); } catch {}
  const listed = (exe) => settings.games.some((g) => g.toLowerCase() === exe.toLowerCase());
  // Your listed games first, then everything else that's open.
  apps.sort((a, b) => Number(listed(b.exe)) - Number(listed(a.exe)) || a.title.localeCompare(b.title));
  const current = (settings.record_target || "").toLowerCase();
  const item = (label, sub, exe) => el("button", {
    class: "game-item", role: "menuitemradio", "aria-checked": String(current === exe.toLowerCase()),
    onclick: () => { closeGameMenu(); save({ record_target: exe }, exe ? `Recording ${label}` : "Back to automatic"); },
  }, el("b", { text: label }), el("span", { text: sub }));
  // The one place to switch between the game's window and the whole screen.
  const whole = settings.capture === "screen";
  const screenSwitch = el("button", {
    class: "screen-switch", role: "switch", "aria-checked": String(whole),
    onclick: () => {
      const next = whole ? "window" : "screen";
      closeGameMenu();
      save({ capture: next }, next === "screen" ? "Recording the whole screen" : "Recording just the game window");
    },
  }, el("span", { class: "track" }, el("i")),
    el("span", {}, el("b", { text: "Record the whole screen" }),
      el("span", { class: "sub", text: whole ? "On: overlays and popups show up in clips too." : "Off: just the game window, no popups." })));
  menu.replaceChildren(
    screenSwitch,
    el("div", { class: "game-menu-head", text: "Record which game?" }),
    item("Automatic", "The game you're playing (from your list, or anything fullscreen)", ""),
    ...apps.map((a) => item(a.title, `${a.exe}${listed(a.exe) ? " · on your games list" : ""}`, a.exe)),
    ...(apps.length ? [] : [el("p", { class: "sub", text: "No other apps are open." })]));
}
$("#game-btn").addEventListener("click", openGameMenu);
document.addEventListener("click", (e) => { if (!e.target.closest(".pick-game")) closeGameMenu(); });
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !$("#game-menu").hidden) closeGameMenu(); });

// ---------- clips ----------
const MB = (b) => (b / 1e6).toFixed(b < 10e6 ? 1 : 0) + " MB";
function when(ms) {
  const d = new Date(ms);
  const today = new Date();
  const time = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const days = Math.round((new Date(today.toDateString()) - new Date(d.toDateString())) / 864e5);
  if (days === 0) return `Today ${time}`;
  if (days === 1) return `Yesterday ${time}`;
  return d.toLocaleDateString([], { month: "short", day: "numeric" }) + ` ${time}`;
}

async function loadClips() {
  try { clips = await invoke("list_clips"); } catch { clips = []; }
  renderClips();
}

/// "Roblox 2026-10-11 09-29-33 (edit)" -> "Roblox".
const gameOf = (c) => c.name.replace(/\s+\d{4}-\d\d-\d\d.*$/, "") || "Other";
let search = "";

function renderFilters() {
  const games = [...new Set(clips.map(gameOf))].slice(0, 6);
  const options = [["all", "All"], ["today", "Today"], ...games.map((g) => [`game:${g}`, g])];
  if (!options.some(([v]) => v === filter)) filter = "all";
  const key = options.map(([v]) => v).join("|") + "#" + filter;
  const box = $("#clip-filters");
  if (box.dataset.key === key) return;
  box.dataset.key = key;
  box.replaceChildren(...options.map(([v, label]) => el("button", {
    "aria-pressed": String(v === filter), text: label,
    onclick: () => { filter = v; renderClips(); },
  })));
}

/// Clip cards by clip + size + featured, reused across renders.
let cardCache = new Map();

function renderClips() {
  const grid = $("#clips");
  const todayStr = new Date().toDateString();
  const q = search.trim().toLowerCase();
  const shown = clips.filter((c) =>
    (filter === "all" || (filter === "today" ? new Date(c.modified).toDateString() === todayStr : gameOf(c) === filter.slice(5)))
    && (!q || c.name.toLowerCase().includes(q)));
  const total = clips.reduce((n, c) => n + c.bytes, 0);
  $("#clips-meta").textContent = `${clips.length} clip${clips.length === 1 ? "" : "s"} · ${(total / 1e9).toFixed(1)} GB · everything stays on this PC`;
  renderFilters();
  $("#clips-empty").hidden = shown.length > 0;
  $("#clips-empty").textContent = clips.length
    ? "No clips match. Try another filter or search."
    : "No clips yet. While a game is open, press your save hotkey and the last seconds land here.";
  // Keep existing cards (and their loaded thumbnails) when nothing changed.
  const key = shown.map((c) => c.path + c.bytes).join("|") + (q || filter);
  if (grid.dataset.key === key) return;
  grid.dataset.key = key;
  // The newest clip gets the big card when you're looking at everything. Cards
  // that are still the same clip are reused (moving a node keeps its loaded
  // thumbnail), so a new clip adds one card instead of rebuilding them all.
  const next = new Map();
  grid.replaceChildren(...shown.map((c, i) => {
    const featured = i === 0 && filter === "all" && !q && c.kind !== "shot";
    const id = `${c.path}|${c.bytes}|${featured}`;
    const card = cardCache.get(id) || clipCard(c, featured);
    next.set(id, card);
    return card;
  }));
  cardCache = next;
}
$("#clip-search").addEventListener("input", (e) => { search = e.target.value; renderClips(); });

const playIcon = () => {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  const c = document.createElementNS(svg.namespaceURI, "circle");
  c.setAttribute("cx", "12"); c.setAttribute("cy", "12"); c.setAttribute("r", "11"); c.setAttribute("fill", "rgba(13,14,16,.8)");
  const p = document.createElementNS(svg.namespaceURI, "path");
  p.setAttribute("d", "M10 8l6 4-6 4z"); p.setAttribute("fill", "#fff");
  svg.append(c, p);
  return svg;
};

/// A screenshot in the library: the picture is its own thumbnail.
function shotCard(c) {
  const img = el("img", { alt: "", class: "thumb-img", decoding: "async", loading: "lazy", src: fileSrc(c.path) });
  img.onload = () => img.classList.add("ready");
  let confirming = false;
  const del = el("button", { class: "btn danger", text: "Delete", "aria-label": `Move ${c.name} to Recycle Bin` });
  del.addEventListener("click", async () => {
    if (!confirming) {
      confirming = true;
      del.textContent = "Sure?";
      setTimeout(() => { confirming = false; del.textContent = "Delete"; }, 3000);
      return;
    }
    try { await invoke("recycle_clip", { path: c.path }); toast("Moved to Recycle Bin"); loadClips(); }
    catch (e) { toast(String(e)); }
  });
  return el("article", { class: "clip shot" },
    el("button", { class: "thumb", "aria-label": `View ${c.name}`, onclick: () => viewShot(c) },
      img, el("span", { class: "size-tag", text: "Screenshot" })),
    el("div", { class: "clip-body" },
      el("div", { class: "clip-name", title: c.name, text: c.name }),
      el("div", { class: "clip-meta", text: when(c.modified) }),
      el("div", { class: "clip-actions" },
        el("button", { class: "btn primary", text: "Copy", onclick: async () => {
          try { await invoke("copy_clip", { path: c.path }); toast("Copied. Paste it into Discord or any chat"); }
          catch (e) { toast(String(e)); }
        } }),
        el("button", { class: "btn", text: "Folder", onclick: () => invoke("reveal_clip", { path: c.path }).catch((e) => toast(String(e))) }),
        del)));
}

/// Shows a screenshot full size; a click or Escape closes it.
function viewShot(c) {
  const close = () => { box.remove(); document.removeEventListener("keydown", onKey); };
  const onKey = (e) => { if (e.key === "Escape") close(); };
  const box = el("div", { class: "shot-view", role: "dialog", "aria-label": c.name, onclick: close },
    el("img", { src: fileSrc(c.path), alt: c.name }));
  document.addEventListener("keydown", onKey);
  document.body.append(box);
}

function clipCard(c, hero = false) {
  if (c.kind === "shot") return shotCard(c);
  // A small cached JPEG, fetched when the card scrolls into view (decoding a
  // video per card to show one frame made the library lag).
  const video = el("img", { alt: "", class: "thumb-img", decoding: "async" });
  video.dataset.clip = c.path;
  thumbObserver.observe(video);
  let confirming = false;
  const del = el("button", { class: "btn danger", text: "Delete", "aria-label": `Move ${c.name} to Recycle Bin` });
  del.addEventListener("click", async () => {
    if (!confirming) {
      confirming = true;
      del.textContent = "Sure?";
      setTimeout(() => { confirming = false; del.textContent = "Delete"; }, 3000);
      return;
    }
    try { await invoke("recycle_clip", { path: c.path }); toast("Moved to Recycle Bin"); loadClips(); }
    catch (e) { toast(String(e)); }
  });
  const fresh = Date.now() - c.modified < 10 * 60 * 1000;
  return el("article", { class: "clip" + (hero ? " hero" : "") },
    el("button", { class: "thumb", "aria-label": `Play ${c.name}`, onclick: () => play(c) },
      video, el("span", { class: "play" }, playIcon()), el("span", { class: "size-tag", text: MB(c.bytes) }),
      hero && fresh ? el("span", { class: "new-tag", text: "Just saved" }) : null),
    el("div", { class: "clip-body" },
      el("div", { class: "clip-name", title: c.name, text: c.name }),
      el("div", { class: "clip-meta", text: when(c.modified) }),
      c.bytes > 10e6 && !c.name.endsWith("(Discord)") ? discordButton(c) : null,
      el("div", { class: "clip-actions" },
        el("button", { class: "btn primary", text: "Copy", onclick: async () => {
          try { await invoke("copy_clip", { path: c.path }); toast("Copied. Paste it into Discord or any chat"); }
          catch (e) { toast(String(e)); }
        } }),
        el("button", { class: "btn", text: "Edit", onclick: () => openEditor(c) }),
        el("button", { class: "btn", text: "GIF", title: "Make a GIF from this clip", onclick: () => openEditor(c, { format: "gif" }) }),
        el("button", { class: "btn", text: "Folder", onclick: () => invoke("reveal_clip", { path: c.path }).catch((e) => toast(String(e))) }),
        del)));
}

function discordButton(c) {
  const b = el("button", { class: "btn discord", text: "Discord copy", title: "Makes a copy under 10 MB that Discord accepts, and copies it" });
  b.addEventListener("click", async () => {
    b.disabled = true;
    b.textContent = "Making Discord copy…";
    try {
      await invoke("discord_copy", { path: c.path });
      toast("Discord copy ready and copied. Paste it into Discord");
      loadClips();
    } catch (e) {
      toast("Discord copy failed: " + e);
      b.disabled = false;
      b.textContent = "Discord copy";
    }
  });
  return b;
}

let playing = null;
function play(c) {
  playing = c;
  $("#player-title").textContent = c.name;
  const v = $("#player-video");
  v.src = fileSrc(c.path);
  $("#player").hidden = false;
  v.play().catch(() => {});
}
function closePlayer() {
  const v = $("#player-video");
  v.pause();
  v.removeAttribute("src");
  v.load();
  $("#player").hidden = true;
}
$("#player-close").addEventListener("click", closePlayer);
$("#player-edit").addEventListener("click", () => { if (playing) openEditor(playing); });
$("#player").addEventListener("click", (e) => { if (e.target.id === "player") closePlayer(); });
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && !$("#player").hidden) closePlayer(); });

$("#open-folder").addEventListener("click", () => invoke("open_clips_folder").catch((e) => toast(String(e))));

// ---------- saving settings ----------
let saveTimer;
function save(patch, message = "Saved. Applies within 2 seconds") {
  Object.assign(settings, patch);
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    try {
      settings = await invoke("save_settings", { settings });
      applySettingsLook();
      toast(message);
      renderAll();
      refreshStatus();
    } catch (e) { toast("Not saved: " + e); }
  }, 150);
}

// ---------- setting cards ----------
const FPS_WORD = ["None", "Low", "Medium", "High"];
const Q_WORD = ["Low", "OK", "Good", "Best"];
const Z_WORD = ["Small", "Medium", "Large", "Huge"];

function meter(label, level, words, fps) {
  const none = level == null;
  return el("div", {},
    el("div", { class: "meter-top" }, el("span", { text: label }), el("span", { text: none ? "No effect" : words[level] })),
    el("div", { class: "segs" + (fps ? " fps" : "") }, [0, 1, 2, 3].map((i) => el("i", { class: !none && i <= level ? `on l${level}` : "" }))));
}

// def: { key, name, what, rec, options: [{ value, label, best, f, q, z }] }
function settingCard(def) {
  const current = settings[def.key];
  const sel = def.options.find((o) => o.value === current) || def.options.find((o) => o.value === def.rec) || def.options[0];
  const recOpt = def.options.find((o) => o.value === def.rec);
  const hasMeters = def.options.some((o) => o.f != null || o.q != null || o.z != null);
  return el("div", { class: "card setting" },
    el("div", { class: "about" },
      el("h3", { text: def.name }),
      el("p", { text: def.what }),
      recOpt ? el("div", { class: "rec" }, "Recommended: ", el("span", { text: recOpt.label })) : null),
    el("div", { class: "pick" },
      el("div", { class: "chips", role: "group", "aria-label": def.name },
        def.options.map((o) => el("button", {
          class: "chip", "aria-pressed": String(o === sel),
          onclick: () => save({ [def.key]: o.value }),
        }, o.label, o.value === def.rec ? el("span", { class: "star", text: "★", "aria-label": "recommended" }) : null))),
      el("div", { class: "explain" },
        el("div", { class: "best" }, el("b", { text: "Best for: " }), sel.best),
        hasMeters ? el("div", { class: "meters" }, meter("FPS cost", sel.f, FPS_WORD, true), meter("Quality", sel.q, Q_WORD), meter("File size", sel.z, Z_WORD)) : null)));
}

/// About how much RAM `seconds` of replay buffer takes at the current quality,
/// frame rate and audio tracks (MB). Video at its average bitrate, as the tray sets it.
function bufferMB(seconds) {
  const p = presets.find((x) => x.name === settings.quality);
  const videoMbps = p
    ? p.mbps * Math.pow(Math.max(60, settings.fps) / 60, 0.6)
    : 80 / Math.max(1, settings.clip_seconds); // Discord preset: ~10 MB per clip
  const tracks = settings.audio_mode === "separate"
    ? 1 + [settings.volume_game, settings.volume_discord, settings.volume_mic].filter((v) => v > 0).length
    : (settings.record_stems ? 4 : 1);
  return Math.round(((videoMbps + (tracks * settings.audio_kbps) / 1000) * seconds) / 8);
}
const ramNote = (s) => `Uses about ${bufferMB(s)} MB of RAM while you play (not FPS).`;
const lengthLabel = (s) => (s < 60 ? `${s} s` : s % 60 ? `${Math.floor(s / 60)} min ${s % 60} s` : `${s / 60} min`);

function qualityDef() {
  const info = {
    "360p": ["Smallest files. Fine on phones and for quick memes; text and far-away details get soft.", 0, 0, 0],
    "480p": ["A 30 s clip fits Discord's free 10 MB upload limit. Good on phones.", 0, 1, 0],
    "720p": ["The best balance: sharp on any phone and in Discord, small files, light on weak PCs.", 1, 2, 1],
    "1080p": ["Full HD for YouTube or big screens. Bigger files, a bit more work for the GPU.", 1, 3, 2],
    "1440p": ["Very sharp on 1440p+ monitors. Large files; best with a strong graphics card.", 2, 3, 3],
  };
  return {
    key: "quality", name: "Quality", rec: "720p",
    what: "Resolution and file size of your clips. Lower means smaller files that are quicker to send, and less work for your PC.",
    options: [
      { value: "discord", label: "Discord (10 MB)", best: `Every ${settings.clip_seconds} s clip stays under Discord's free 10 MB upload limit. Archy sizes the video to fit (720p when there's room, else 480p). Long clips can still go over.`, f: 0, q: 1, z: 0 },
      ...presets.map((p) => {
        const [best, f, q, z] = info[p.name] || ["", 1, 2, 1];
        const mb = Math.round((p.mbps * Math.pow(Math.max(60, settings.fps) / 60, 0.6) * settings.clip_seconds) / 8); // same scaling as the tray
        const warn = mb > 10 ? " Too big for Discord's free 10 MB limit." : "";
        return { value: p.name, label: p.name, best: `${best} About ${mb} MB per ${settings.clip_seconds} s clip.${warn}`, f, q, z };
      }),
    ],
  };
}

const RECORDING_TIMING = {
  key: "frame_timing", name: "Frame timing", rec: "smooth",
  what: "How each frame's time is written. Only matters if you edit clips in After Effects or Premiere.",
  options: [
    { value: "smooth", label: "Smoothest", best: "Watching and sharing. Every frame keeps the exact moment it was on your screen, so clips play as smoothly as you played." },
    { value: "constant", label: "For video editors", best: "After Effects, Premiere and Resolve: every frame exactly evenly spaced, so editors read the clip perfectly. If your screen's refresh rate isn't a multiple of the clip's (60 fps on a 144 Hz screen), motion looks slightly uneven." },
  ],
};

const RECORDING_SESSION = () => ({
  key: "session_mode", name: "Record whole sessions", rec: "off",
  what: `Also writes everything you play to one long video in your clips folder, besides the replay buffer. No extra encoding (it's the same video), so no FPS cost; it uses disk space: about ${(bufferMB(3600) / 1000).toFixed(1)} GB per hour at your quality. While it records, your clip key also adds a bookmark you can jump to in the editor.`,
  options: [
    { value: "off", label: "Off", best: "Most people: save the moments with your clip key." },
    { value: "hotkey", label: "With a hotkey", best: "Recording a match or a stream on purpose: press your session key (Hotkeys page) to start and stop." },
    { value: "always", label: "Always", best: "Never missing anything: every game is recorded start to finish. Watch your disk space (the clips folder limit applies)." },
  ],
});

const RECORDING_LIMIT = {
  key: "clips_max_gb", name: "Clips folder size limit", rec: 0,
  what: "When the clips folder grows past this, Archy moves the oldest clips to the Recycle Bin after each save (you can still restore them there). The clip you just saved is always kept.",
  options: [
    { value: 0, label: "No limit", best: "Keeping every clip. Archy warns when the drive gets nearly full." },
    { value: 10, label: "10 GB", best: "Small drives and laptops." },
    { value: 25, label: "25 GB", best: "Most people: hundreds of clips, and old ones clear themselves." },
    { value: 50, label: "50 GB", best: "Recording at 1440p or 144 fps, which makes bigger clips." },
    { value: 100, label: "100 GB", best: "Big drives, if you rarely clean up." },
  ],
};

const RECORDING = () => [
  qualityDef(),
  { key: "fps", name: "Frame rate", rec: 60,
    what: "Frames per second in your clips (24–144). Archy only takes this many even if your game runs at 240, so lower saves real work. A clip can't be smoother than your monitor: 120 and 144 need a monitor that runs that fast.",
    options: [
      { value: 24, label: "24", best: "A film-like look with the lowest load. Fast games look choppy.", f: 0, q: 1, z: 0 },
      { value: 30, label: "30", best: "Weak laptops or slow-paced games. Half the work of 60.", f: 0, q: 1, z: 0 },
      { value: 45, label: "45", best: "A middle ground for integrated graphics that struggle with 60.", f: 1, q: 2, z: 1 },
      { value: 60, label: "60", best: "Most people. Smooth motion; what YouTube and Discord play best.", f: 1, q: 2, z: 2 },
      { value: 75, label: "75", best: "High-refresh players who want slightly smoother clips.", f: 2, q: 3, z: 2 },
      { value: 90, label: "90", best: "Very smooth clips for fast shooters, on a high-refresh monitor.", f: 2, q: 3, z: 3 },
      { value: 120, label: "120", best: "120 Hz monitors and slow-motion edits. Clips are about 1.5x the size of 60 fps, so every frame stays as sharp.", f: 3, q: 3, z: 3 },
      { value: 144, label: "144", best: "144 Hz monitors: clips look exactly as smooth as you played, at about 1.7x the size of 60 fps. The most work for your graphics chip: run “Check what this PC can record” first.", f: 3, q: 3, z: 3 },
    ] },
  { key: "codec", name: "Video format", rec: "h264",
    what: "How clips are compressed. All formats cost the same FPS (the GPU's video chip does the work); newer ones look better at the same size but play in fewer places.",
    options: [
      { value: "h264", label: "H.264", best: "Sending clips. Plays everywhere: Discord previews, phones, every editor and site.", f: 1, q: 1, z: 2 },
      { value: "hevc", label: "HEVC", best: "Better-looking clips at the same size. Some apps and older PCs won't preview it.", f: 1, q: 2, z: 1 },
      { value: "av1", label: "AV1", best: "Best quality per MB on new GPUs (RTX 40+, Radeon RX 7000+, Intel Arc). Many apps can't preview it yet.", f: 1, q: 3, z: 0 },
      { value: "auto", label: "Auto", best: "Archy uses the best format your graphics chip encodes in hardware.", f: 1, q: 3, z: 1 },
    ] },
  { key: "efficiency_mode", name: "Efficiency mode", rec: true,
    what: "Keeps Archy away from your game's CPU time: low priority, power-saving scheduling, and on newer Intel/AMD chips only the efficiency cores.",
    options: [
      { value: true, label: "On", best: "Everyone. In Roblox this cut Archy's FPS cost to about 2–3%.", f: 0 },
      { value: false, label: "Off", best: "Only if clips drop frames on a PC with very few cores.", f: 2 },
    ] },
  { key: "clip_seconds", name: "Clip length", rec: 30,
    what: "How far back a clip goes when you press your save hotkey. Longer clips use more RAM while you play, not more FPS.",
    options: [15, 30, 45, 60, 90, 120, 180, 300].map((s) => ({ value: s, label: lengthLabel(s),
      best: `${{ 15: "Quick reactions and single kills.", 30: "Most moments.", 45: "Plays with a longer build-up.", 60: "Whole fights or rounds.", 90: "Long fights.", 120: "Full rounds and comebacks.", 180: "Whole matches in short games.", 300: "Everything in the last 5 minutes." }[s]} ${ramNote(s)}` })) },
  { key: "long_clip_seconds", name: "Long clip length", rec: 60,
    what: "What your long-clip hotkey saves. Archy keeps this much in memory.",
    options: [30, 60, 90, 120, 180, 300].map((s) => ({ value: s, label: lengthLabel(s),
      best: `${s <= 60 ? "Catch what just happened." : "Catch a whole round or match."} ${ramNote(s)}` })) },
];

function renderRecording() {
  $("#recording-settings").replaceChildren(...RECORDING().map(settingCard), selfTestCard(), screenPicker(), gpuPicker(), settingCard(RECORDING_TIMING), settingCard(RECORDING_SESSION()), settingCard(RECORDING_LIMIT));
  loadMonitors();
  loadGpus();
}

// ---------- "Check what this PC can record" ----------
// Records a test window at 60, 120 and 144 fps and checks every frame for
// tearing, smudging, skipped frames and an uneven frame rate.
const selfTest = { running: false, results: [], error: "" };

function selfTestCard() {
  const rows = selfTest.results.map((r) => {
    const verdict = r.error ? `Couldn't test: ${r.error}`
      : r.pass ? "Smooth and clean"
      : r.clean ? `Clean, but ${r.skipped} frames were missed: the PC was too busy (a game or app running?)`
      : `Problems: ${[r.tears && "tearing", r.smudges && "smudging", r.backwards && "frames out of order", r.wrong_colours && "wrong colours", r.decode_errors && "broken video"].filter(Boolean).join(", ") || "not smooth"}`;
    return el("div", { class: "selftest-row" + (r.pass ? " ok" : " bad") },
      el("b", { text: `${r.fps} fps` }),
      el("span", { text: verdict }),
      el("span", { class: "sub", text: r.error ? "" : `${r.encoder} · ${Math.round(r.cpu_core_pct)}% CPU` }));
  });
  const best = selfTest.running ? null : Math.max(0, ...selfTest.results.filter((r) => r.pass).map((r) => r.fps));
  const done = !selfTest.running && selfTest.results.length > 0;
  const actions = [el("button", { class: "btn" + (done ? "" : " primary"), text: selfTest.running ? "Testing…" : done ? "Test again" : "Run the check", disabled: selfTest.running ? "" : null, onclick: runSelfTest })];
  if (done) actions.unshift(el("button", { class: "btn", text: "Copy report", title: "Copies the results with your GPU, driver and Windows version, to paste when asking for help", onclick: copySelfTestReport }));
  if (done && best && best !== settings.fps) {
    actions.push(el("button", { class: "btn primary", text: `Use ${best} fps`, onclick: () => { save({ fps: best }, `Frame rate: ${best}`); renderRecording(); } }));
  }
  return el("div", { class: "card setting", id: "self-test" },
    el("div", { class: "about" }, el("h3", { text: "Check what this PC can record" }),
      el("p", { text: "Records a test pattern at 60, 120 and 144 fps and checks every frame for tearing, smudging and skipped frames. Takes about a minute; a test window pops up while it runs. Close your game first for a fair result." })),
    el("div", { class: "pick" },
      rows.length ? el("div", { class: "selftest-rows" }, rows) : null,
      done ? el("p", { class: "sub", text: best ? `Your PC records ${best} fps cleanly.` : "Even 60 fps missed frames. Close games and heavy apps and test again; if it still misses, try 30 or 45 fps or a lower quality." } ) : null,
      el("div", { class: "welcome-actions" }, actions)));
}

/// Everything someone helping needs, as text to paste (nothing is sent anywhere).
async function copySelfTestReport() {
  let version = "";
  try { version = await invoke("app_version"); } catch { /* older build */ }
  const report = { archy: version, settings: { quality: settings.quality, fps: settings.fps, codec: settings.codec, encoder_gpu: settings.encoder_gpu }, results: selfTest.results };
  const text = "Archy self-test report\n" + JSON.stringify(report, null, 2);
  try { await navigator.clipboard.writeText(text); toast("Report copied. Paste it where you're asking for help"); }
  catch { toast("Couldn't copy the report"); }
}

async function runSelfTest() {
  if (selfTest.running) return;
  selfTest.running = true;
  selfTest.results = [];
  const redraw = () => $("#self-test")?.replaceWith(selfTestCard());
  redraw();
  for (const fps of [60, 120, 144]) {
    let r;
    try { r = await invoke("self_test", { fps }); } catch (e) { r = { fps, error: String(e) }; }
    selfTest.results.push(r);
    redraw();
    if (r.error && !r.fps) break;
  }
  selfTest.running = false;
  redraw();
}

// ---------- GPU picker ----------
let gpus = null;
async function loadGpus() {
  if (gpus === null) {
    try { gpus = await invoke("gpus"); } catch { gpus = []; }
  }
  const box = $("#gpu-picker");
  if (box) box.replaceWith(gpuPicker());
}

function gpuPicker() {
  const list = gpus || [];
  const select = el("select", { class: "input", "aria-label": "GPU that records" },
    el("option", { value: "", text: "Automatic (recommended)" }),
    list.map((g) => el("option", {
      value: g.id,
      text: g.encoders.length ? `${g.name} (${g.encoders.join(", ")})` : `${g.name} (no video encoder)`,
      disabled: g.encoders.length ? null : "",
    })));
  select.value = list.some((g) => g.id === settings.encoder_gpu) ? settings.encoder_gpu : "";
  select.addEventListener("change", () => save({ encoder_gpu: select.value }, `Recording GPU: ${select.selectedOptions[0].text}`));
  const chosen = list.find((g) => g.id === select.value);
  const best = !chosen
    ? "Archy picks a GPU that can record, and switches to another if one can't. Right for almost everyone."
    : chosen.integrated
      ? "Built-in graphics: your graphics card stays fully free for the game (best FPS, better battery on laptops). Clips look very slightly softer."
      : "Graphics card: often slightly sharper clips, but recording uses a little of the GPU your game runs on.";
  return el("div", { class: "card setting", id: "gpu-picker" },
    el("div", { class: "about" }, el("h3", { text: "GPU that records" }),
      el("p", { text: "Which graphics chip encodes your clips. Only matters on PCs with two (e.g. Intel + NVIDIA laptops)." })),
    el("div", { class: "pick" },
      el("div", { class: "field" }, select),
      el("div", { class: "explain" }, el("div", { class: "best" }, el("b", { text: "Best for: " }), best)),
      el("p", { class: "sub", text: gpus === null ? "Looking for GPUs…" : "If the chosen GPU can't record (driver problem, unplugged), Archy falls back to Automatic so you never miss a clip." })));
}

// ---------- screen picker (Whole screen mode) ----------
let monitors = [];
async function loadMonitors() {
  try { monitors = await invoke("monitors"); } catch { monitors = []; }
  const box = $("#screen-picker");
  if (box) box.replaceWith(screenPicker());
}

function screenPicker() {
  if (settings.capture !== "screen") return el("div", { id: "screen-picker" });
  const select = el("select", { class: "input", "aria-label": "Screen to record" },
    el("option", { value: "", text: "The screen the game is on" }),
    monitors.map((m) => el("option", { value: m.id, text: m.label })));
  select.value = monitors.some((m) => m.id === settings.capture_monitor) ? settings.capture_monitor : "";
  select.addEventListener("change", () => save({ capture_monitor: select.value }, `Recording: ${select.selectedOptions[0].text}`));
  return el("div", { class: "card setting", id: "screen-picker" },
    el("div", { class: "about" }, el("h3", { text: "Which screen" }),
      el("p", { text: "With \u201cRecord the whole screen\u201d on (the Game button at the top), Archy records this screen while a game is open." })),
    el("div", { class: "pick" },
      el("div", { class: "field" }, select, el("button", { class: "btn", text: "Refresh", onclick: loadMonitors })),
      el("p", { class: "sub", text: "If the screen is unplugged, Archy records the screen the game is on instead." })));
}

/// A list of program file names with "pick from open apps" and "type a name".
function appListCard(key, title, about, placeholder) {
  const list = settings[key];
  const has = (exe) => list.some((g) => g.toLowerCase() === exe.toLowerCase());
  const addName = (name) => { if (name && !has(name)) save({ [key]: [...list, name] }, `Added ${name}`); };
  const input = el("input", { class: "input", placeholder, "aria-label": `Add to ${title}` });
  const add = () => { addName(input.value.trim()); input.value = ""; };
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") add(); });
  const running = el("select", { class: "input", "aria-label": `Running apps for ${title}` }, el("option", { value: "", text: "Pick from apps that are open now…" }));
  const loadRunning = async () => {
    const apps = await invoke("running_apps").catch(() => []);
    running.replaceChildren(el("option", { value: "", text: apps.length ? "Pick from apps that are open now…" : "No apps found" }),
      ...apps.map((a) => el("option", { value: a.exe, text: `${a.title}  (${a.exe})` })));
  };
  running.addEventListener("focus", loadRunning);
  running.addEventListener("change", () => addName(running.value));
  loadRunning();
  return el("div", { class: "card" },
    el("h3", { text: title }),
    el("p", { class: "sub", text: about }),
    el("div", { class: "chips", style: "margin:14px 0" }, list.length ? list.map((g) =>
      el("span", { class: "tag" }, g, el("button", { "aria-label": `Remove ${g}`, text: "×", onclick: () => save({ [key]: list.filter((x) => x !== g) }, `Removed ${g}`) })))
      : el("span", { class: "sub", text: "Nothing here yet." })),
    el("div", { class: "field" }, running, el("button", { class: "btn", text: "Refresh", onclick: loadRunning })),
    el("p", { class: "sub", style: "margin:12px 0 6px", text: "Or type the program's file name:" }),
    el("div", { class: "field" }, input, el("button", { class: "btn", text: "Add", onclick: add })));
}

function renderGames() {
  $("#games-settings").replaceChildren(
    appListCard("games", "Always record these", "The program file of each game. Archy starts recording when one is in front.",
      "Game file name, e.g. VALORANT-Win64-Shipping.exe"),
    settingCard({ key: "record_fullscreen_apps", name: "Other fullscreen apps", rec: true,
      what: "Also record anything else running fullscreen (browsers, Discord and video players are ignored).",
      options: [
        { value: true, label: "On", best: "Catching games that aren't on your list without setting anything up." },
        { value: false, label: "Off", best: "Only ever recording the games listed above." },
      ] }),
    appListCard("never_record", "Never record these",
      "Apps Archy should leave alone, even when they're fullscreen or in front: a video player, a launcher, a work app. This list wins over the one above.",
      "Program file name, e.g. Netflix.exe"));
}

function renderStorage() {
  const folder = el("input", { class: "input", value: settings.save_folder, "aria-label": "Clips folder" });
  $("#storage-settings").replaceChildren(
    el("div", { class: "card" },
      el("h3", { text: "Save clips to" }),
      el("div", { class: "field", style: "margin-top:10px" }, folder,
        el("button", { class: "btn primary", text: "Browse…", onclick: async () => {
          const picked = await invoke("pick_folder").catch(() => null);
          if (picked) save({ save_folder: picked }, "Clips folder changed");
        } }),
        el("button", { class: "btn", text: "Save typed path", onclick: () => save({ save_folder: folder.value.trim() }, "Clips folder changed") }),
        el("button", { class: "btn", text: "Open", onclick: () => invoke("open_clips_folder").catch((e) => toast(String(e))) })),
      el("p", { class: "sub", style: "margin-top:10px", text: "Any folder on this PC works. Optional: pick your OneDrive or Google Drive folder and their app backs clips up for you. Archy itself never touches the internet." })),
    settingCard({ key: "copy_after_save", name: "Copy every new clip", rec: true,
      what: "Right after a clip saves, it's on your clipboard. Alt-tab to Discord and press Ctrl+V.",
      options: [
        { value: true, label: "On", best: "Sending clips to friends right away. Nothing to click." },
        { value: false, label: "Off", best: "Keeping your clipboard as it was. You can still copy any clip from the library." },
      ] }));
}

// Physical keys (e.code), so Shift+1 is "Shift+1" not "!", and layouts don't matter.
const CODE_NAMES = {
  BracketLeft: "[", BracketRight: "]", Semicolon: ";", Quote: "'", Comma: ",", Period: ".",
  Slash: "/", Backslash: "\\", Minus: "-", Equal: "=", Backquote: "`",
  NumpadAdd: "NumPlus", NumpadSubtract: "NumMinus", NumpadMultiply: "NumMultiply",
  NumpadDivide: "NumDivide", NumpadDecimal: "NumDecimal",
  ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right",
  Space: "Space", Tab: "Tab", Enter: "Enter", Backspace: "Backspace",
  Insert: "Insert", Delete: "Delete", Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown",
  Pause: "Pause", ScrollLock: "ScrollLock",
};
function keyName(e) {
  const c = e.code;
  if (/^(Control|Shift|Alt|Meta|OS)(Left|Right)?$/.test(c)) return null;
  if (/^F\d{1,2}$/.test(c)) return c;
  let m = c.match(/^Key([A-Z])$/) || c.match(/^Digit(\d)$/);
  if (m) return m[1];
  m = c.match(/^Numpad(\d)$/);
  if (m) return "Num" + m[1];
  return CODE_NAMES[c] || null;
}
// Keys you type with: bound alone, Windows would swallow them everywhere (chat, Discord).
const TYPING_KEY = /^([A-Z0-9]|[\[\];',./\\=`-]|Space|Tab|Enter|Backspace)$/;

/// Turns `box` into a "press keys" button; `onValue(text)` gets the new key.
function captureKey(box, shown, onValue) {
  box.addEventListener("click", () => {
    box.classList.add("listening");
    box.textContent = "Press keys…";
    const onKey = (e) => {
      e.preventDefault();
      if (e.key === "Escape") return done(null);
      const name = keyName(e);
      if (!name) return;
      const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Win"].filter(Boolean);
      if (!mods.length && TYPING_KEY.test(name)) {
        toast(`Add Ctrl, Alt or Shift: "${name}" alone would stop working for typing everywhere.`);
        return;
      }
      done([...mods, name].join("+"));
    };
    const done = (value) => {
      window.removeEventListener("keydown", onKey, true);
      box.classList.remove("listening");
      if (value) onValue(value);
      else box.textContent = shown();
    };
    window.addEventListener("keydown", onKey, true);
  });
}

function hotkeyRow(key, label, desc) {
  const shown = () => settings[key] || "Off";
  const box = el("button", { class: "input keybox", text: shown(), "aria-label": `Change hotkey for ${label}` });
  captureKey(box, shown, (value) => save({ [key]: value }, `Hotkey set to ${value}`));
  const clear = settings[key]
    ? el("button", { class: "btn", text: "Turn off", onclick: () => save({ [key]: "" }, `${label} hotkey turned off`) })
    : null;
  return el("div", { class: "row" }, el("div", { class: "label" }, el("b", { text: label }), el("span", { text: desc })), el("div", { class: "field" }, box, clear));
}

function renderHotkeys() {
  $("#hotkey-settings").replaceChildren(el("div", { class: "card" },
    hotkeyRow("hotkey_save", "Save clip", `Saves the last ${lengthLabel(settings.clip_seconds)}`),
    hotkeyRow("hotkey_save_long", "Save long clip", `Saves the last ${lengthLabel(settings.long_clip_seconds)}`),
    hotkeyRow("hotkey_pause", "Pause / resume", "Stops recording completely: zero load. Off by default so it can't be hit by accident; the tray menu can pause too."),
    hotkeyRow("hotkey_screenshot", "Screenshot", "Saves a full-size picture (PNG) of the game into your clips. Off until you set a key; costs nothing until you press it."),
    hotkeyRow("hotkey_session", "Start / stop session recording", "Records the whole session to one video. Only when Record whole sessions (Recording page) is set to With a hotkey.")),
    extraHotkeysCard());
}

const EXTRA_MAX = 8;
/// More clip hotkeys, each with its own length. Empty by default: no change.
function extraHotkeysCard() {
  const list = settings.extra_hotkeys || [];
  const put = (next, msg) => save({ extra_hotkeys: next }, msg);
  const rows = list.map((h, i) => {
    const shown = () => h.key || "Press to set";
    const box = el("button", { class: "input keybox", text: shown(), "aria-label": `Key for extra clip hotkey ${i + 1}` });
    captureKey(box, shown, (value) => put(list.map((x, j) => (j === i ? { ...x, key: value } : x)), `Hotkey set to ${value}`));
    const length = el("select", { class: "input", "aria-label": `Clip length for extra hotkey ${i + 1}` },
      [15, 30, 60, 90, 120, 180, 300].map((s) => el("option", { value: s, text: lengthLabel(s), selected: s === h.seconds ? "" : null })));
    length.addEventListener("change", () => put(list.map((x, j) => (j === i ? { ...x, seconds: Number(length.value) } : x)), `Saves the last ${lengthLabel(Number(length.value))}`));
    const remove = el("button", { class: "btn", text: "Remove", onclick: () => put(list.filter((_, j) => j !== i), "Hotkey removed") });
    return el("div", { class: "row" },
      el("div", { class: "label" }, el("b", { text: `Clip key ${i + 1}` }), el("span", { text: h.key ? `Saves the last ${lengthLabel(h.seconds)}` : "Not set yet" })),
      el("div", { class: "field" }, box, length, remove));
  });
  const longest = Math.max(settings.clip_seconds, settings.long_clip_seconds,
    ...settings.voice_phrases.map((p) => p.seconds), ...list.filter((h) => h.key).map((h) => h.seconds));
  const add = el("button", { class: "btn", text: "+ Add a clip hotkey", disabled: list.length >= EXTRA_MAX ? "" : null,
    onclick: () => put([...list, { key: "", seconds: 120 }], "Added: now press its key") });
  return el("div", { class: "card" },
    el("h3", { text: "More clip hotkeys" }),
    el("p", { class: "sub", text: `Each key saves its own length, e.g. Ctrl+F8 for the last 2 minutes. Archy keeps the longest of all your clip lengths in memory: now ${lengthLabel(longest)}, about ${bufferMB(longest)} MB of RAM. It never costs FPS.` }),
    ...rows,
    el("div", { class: "field", style: "margin-top:10px" }, add));
}

const onOff = (key, name, what, onBest, offBest, rec = true) => settingCard({ key, name, what, rec,
  options: [{ value: true, label: "On", best: onBest }, { value: false, label: "Off", best: offBest }] });

let devices = { mics: [], outputs: [] };
async function loadDevices() {
  try {
    const [mics, outputs] = await Promise.all([invoke("audio_devices", { input: true }), invoke("audio_devices", { input: false })]);
    devices = { mics, outputs };
  } catch { /* keep the last list */ }
  renderAudio();
}

function devicePicker(key, list, label, what, note) {
  const select = el("select", { class: "input", "aria-label": label },
    el("option", { value: "", text: "Windows default" }),
    list.map((d) => el("option", { value: d.id, text: d.name })));
  select.value = list.some((d) => d.id === settings[key]) ? settings[key] : "";
  select.addEventListener("change", () => save({ [key]: select.value }, `${label}: ${select.value ? select.selectedOptions[0].text : "Windows default"}`));
  return el("div", { class: "card setting" },
    el("div", { class: "about" }, el("h3", { text: label }), el("p", { text: what })),
    el("div", { class: "pick" },
      el("div", { class: "field" }, select, el("button", { class: "btn", text: "Refresh", onclick: loadDevices })),
      note ? el("p", { class: "sub", text: note }) : null));
}

function renderAudio() {
  $("#audio-settings").replaceChildren(
    devicePicker("mic_device", devices.mics, "Microphone", "Which mic Archy records for your voice track.",
      "If it's unplugged, Archy uses Windows' default mic instead. Voice clipping listens on this mic too."),
    devicePicker("output_device", devices.outputs, "Speakers / headphones", "Which output track 1 (everything you heard) records from. Pick the one you actually listen on.",
      "If it's unplugged, Archy uses Windows' default output instead."),
    soundMixCard(),
    pushToTalkCard(),
    micTestCard(),
    ...(settings.audio_mode === "separate" ? [] : [onOff("record_stems", "Keep each source as its own track", "In One volume mode, also record the game, Discord and your mic as separate tracks, so the editor can rebalance them after the clip is saved.",
      "Fixing the mix later in the editor.", "The lightest recording: one audio track (one audio encoder instead of four).")]),
    settingCard({ key: "audio_kbps", name: "Audio quality", rec: 160,
      what: "Bitrate per track. Audio is tiny next to video; higher is clearer music and voices.",
      options: [
        { value: 96, label: "96 kbps", best: "Smallest files. Fine for voice; music sounds a bit dull.", z: 0 },
        { value: 128, label: "128 kbps", best: "Good for most clips.", z: 1 },
        { value: 160, label: "160 kbps", best: "Clear game sound and music. The default.", z: 1 },
        { value: 192, label: "192 kbps", best: "Best quality; files about 1 MB bigger per minute per track.", z: 2 },
      ] }),
    el("h2", { text: "Voice clipping" }),
    el("div", { id: "voice-settings" }));
  // The voice section lives inside this page, so redraw it whenever the page is.
  renderVoice();
}

// What's in a clip's sound, like the editor: one main volume, or each source on
// its own. Only sliders; the mode decides which ones count (the others are off).
// ---------- push-to-talk ----------
function pushToTalkCard() {
  const on = settings.push_to_talk;
  const key = settings.push_to_talk_key;
  const box = el("button", { class: "input keybox", text: key || "Press to set", "aria-label": "Push-to-talk key" });
  // Any single key works here: push-to-talk only watches the key, it never takes
  // it away from the game or chat.
  box.addEventListener("click", () => {
    box.classList.add("listening");
    box.textContent = "Press a key…";
    const onKey = (e) => {
      e.preventDefault();
      window.removeEventListener("keydown", onKey, true);
      box.classList.remove("listening");
      if (e.key === "Escape") { box.textContent = key || "Press to set"; return; }
      const name = keyName(e);
      if (name) save({ push_to_talk_key: name }, `Push-to-talk key: ${name}`);
    };
    window.addEventListener("keydown", onKey, true);
  });
  return el("div", { class: "card setting" },
    el("div", { class: "about" }, el("h3", { text: "Push to talk" }),
      el("p", { text: "Your mic is only heard in clips while you hold a key (or a mouse side button). Useful when your keyboard, fan or room is loud. Voice clipping still hears you." })),
    el("div", { class: "pick" },
      el("div", { class: "seg", role: "group", "aria-label": "Push to talk" },
        [[false, "Off"], [true, "On"]].map(([v, label]) => el("button", {
          "aria-pressed": String(on === v), onclick: () => save({ push_to_talk: v }, v ? "Push-to-talk on" : "Push-to-talk off"),
        }, label))),
      on ? el("div", { class: "field", style: "margin-top:8px" }, box,
        el("button", { class: "btn", text: "Mouse 4", onclick: () => save({ push_to_talk_key: "Mouse4" }, "Push-to-talk: Mouse 4") }),
        el("button", { class: "btn", text: "Mouse 5", onclick: () => save({ push_to_talk_key: "Mouse5" }, "Push-to-talk: Mouse 5") })) : null,
      el("p", { class: "sub", text: on && !key ? "Pick a key: until then your mic is heard as normal." : "Costs nothing: Archy just checks the key 60 times a second." })));
}

// ---------- mic test ----------
function micTestCard() {
  const status = el("p", { class: "sub", text: "Records 5 seconds from the microphone picked above, then plays it back, so you hear what your clips will sound like." });
  const player = el("audio", { controls: true, hidden: true, style: "width:100%;margin-top:8px" });
  const btn = el("button", { class: "btn primary", text: "Test your mic" });
  btn.addEventListener("click", async () => {
    btn.disabled = true;
    btn.textContent = "Recording… talk now";
    player.hidden = true;
    try {
      const wav = await invoke("mic_test", { mic: settings.mic_device || "" });
      player.src = fileSrc(wav) + "?t=" + Date.now();
      player.hidden = false;
      player.play().catch(() => {});
      status.textContent = "Here's how you sound. Too quiet or noisy? Check the mic picked above and its level in Windows sound settings.";
    } catch (e) {
      status.textContent = "Couldn't test the mic: " + e;
    }
    btn.disabled = false;
    btn.textContent = "Test again";
  });
  return el("div", { class: "card setting" },
    el("div", { class: "about" }, el("h3", { text: "Mic test" }), status),
    el("div", { class: "pick" }, el("div", { class: "field" }, btn), player));
}

function soundMixCard() {
  const separate = settings.audio_mode === "separate";
  const slider = (key, name, about, enabled, offAt0) => {
    const v = settings[key];
    const out = el("span", { class: "mix-val", text: !enabled ? "—" : offAt0 && v === 0 ? "Off" : `${v}%` });
    const range = el("input", { type: "range", min: 0, max: 200, step: 5, value: v, "aria-label": `${name} volume`, disabled: enabled ? null : "" });
    range.addEventListener("input", () => { out.textContent = offAt0 && Number(range.value) === 0 ? "Off" : `${range.value}%`; });
    range.addEventListener("change", () => save({ [key]: Number(range.value) }, `${name}: ${offAt0 && Number(range.value) === 0 ? "not recorded" : range.value + "%"}`));
    range.addEventListener("dblclick", () => { if (enabled) save({ [key]: 100 }, `${name}: 100%`); });
    return el("div", { class: "mix-row" + (enabled ? "" : " off") },
      el("div", {}, el("b", { text: name }), el("span", { class: "sub", text: about })), range, out);
  };
  return el("div", { class: "card" },
    el("div", { class: "mix-head" },
      el("div", {}, el("h3", { text: "Sound in your clips" }),
        el("p", { class: "sub", style: "margin:0", text: separate
          ? "Each source at its own volume. The main volume is off. A source at 0% isn't recorded at all, so 0% on your mic keeps it out of clips."
          : "One volume for everything: game, friends, your mic and other apps. To change one of them on its own, pick Each source." })),
      el("div", { class: "seg", role: "group", "aria-label": "How to set the sound" },
        [["main", "One volume"], ["separate", "Each source"]].map(([m, label]) => el("button", {
          "aria-pressed": String((settings.audio_mode || "main") === m),
          onclick: () => save({ audio_mode: m }, m === "separate" ? "Each source at its own volume" : "One volume for everything"),
        }, label)))),
    slider("volume_everything", "Main volume", "Everything you heard, plus your mic", !separate, false),
    el("div", { class: "mix-sub", text: "Each source" }),
    slider("volume_game", "Game", "Only the game's sound", separate, true),
    slider("volume_discord", "Discord", "Your friends in voice chat", separate, true),
    slider("volume_mic", "Microphone", "Your voice", separate, true),
    el("p", { class: "sub", style: "margin-top:12px", text: (separate || settings.record_stems
      ? "Clips always have one mixed track that every player plays, plus the game, Discord and your mic as separate tracks you can rebalance later in the editor."
      : "Clips have one mixed track (separate tracks are off below).") + " Double-click a slider to reset it to 100%." }));
}

// A 0-200% volume slider; saves when released.
function volumeRow(key, label, desc) {
  const value = el("b", { text: `${settings[key]}%` });
  const range = el("input", { type: "range", min: 0, max: 200, step: 5, value: settings[key], "aria-label": label, style: "width:220px" });
  range.addEventListener("input", () => { value.textContent = `${range.value}%`; });
  range.addEventListener("change", () => save({ [key]: Number(range.value) }, `${label}: ${range.value}%`));
  range.addEventListener("dblclick", () => { range.value = 100; value.textContent = "100%"; save({ [key]: 100 }, `${label}: 100%`); });
  return el("div", { class: "card setting" },
    el("div", { class: "about" }, el("h3", { text: label }), el("p", { text: desc })),
    el("div", { class: "pick" }, el("div", { class: "field" }, range, value),
      el("p", { class: "sub", text: "0% is silent, 100% is normal, up to 200%. Double-click to reset." })));
}

let appVersion = "";
function aboutCard() {
  return el("div", { class: "card about-card" },
    el("div", {}, el("h3", { text: `Archy v${appVersion}` }),
      el("p", { class: "sub", text: "The version installed on this PC. Archy checks GitHub for a newer one when it starts, if \"Check for updates\" is on." })),
    el("button", { class: "btn", text: "Show the tour again", onclick: () => window.startWelcome?.() }),
    el("button", { class: "btn", text: "Check now", onclick: async () => {
      updateDialog();
    } }));
}

function renderGeneral() {
  $("#general-settings").replaceChildren(
    aboutCard(),
    onOff("notify_on_save", "Notification when a clip saves", "A small Windows notification with the clip's length and size.",
      "Knowing the clip saved without leaving the game.", "No pop-ups at all; check the library instead."),
    onOff("sound_on_save", "Sound when a clip saves", "Archy's short two-note chime.",
      "Hearing it worked while you keep playing, even in fullscreen.", "Silence: for streams or quiet rooms."),
    volumeRow("clip_sound_volume", "Clip sound volume", "How loud the clip-saved chime is."),
    onOff("sound_on_hotkey", "Ping when you press the clip key", "The same soft blip as voice clipping, the moment you press it, before the clip finishes saving.",
      "Knowing the key press registered, even in fullscreen.", "Only the saved chime."),
    onOff("notify_on_start", "\"Archy is running\" notification", "A reminder when Archy starts that it's ready and what your hotkey is.",
      "Remembering your hotkey.", "A quieter start-up."),
    onOff("start_with_windows", "Start with Windows", "Archy starts in the tray when you sign in, so clips are always ready.",
      "Never forgetting to open Archy before playing.", "Starting Archy yourself only when you want it.", false),
    onOff("tray_recording_dot", "Recording dot on the tray icon", "A small red dot on Archy's icon while a game is being recorded.",
      "Seeing at a glance that Archy is recording.", "A plain icon at all times."),
    onOff("check_updates", "Check for updates", "When Archy starts, it asks GitHub whether a newer version exists. This is the only time Archy goes online. Updating always asks you first.",
      "Getting fixes as soon as they're out.", "Archy never goes online; you update by downloading the installer yourself."),
    logCard());
  loadLog();
}

// ---------- log viewer ----------
function logCard() {
  const box = el("pre", { id: "log-box", style: "max-height:320px;overflow:auto;white-space:pre-wrap;word-break:break-word;font:12px/1.5 ui-monospace,Consolas,monospace;background:var(--bg,#0d0f14);border:1px solid var(--line-soft,#2a2f3a);border-radius:8px;padding:10px;margin:10px 0 0;user-select:text" });
  return el("div", { class: "card" },
    el("h3", { text: "Log" }),
    el("p", { class: "sub", text: "What Archy did: games it recorded, clips it saved, voice phrases it heard, and any errors. If something goes wrong, copy this and send it along when asking for help. It stays on this PC." }),
    box,
    el("div", { class: "field", style: "margin-top:10px" },
      el("button", { class: "btn", text: "Refresh", onclick: loadLog }),
      el("button", { class: "btn", text: "Copy", onclick: async () => {
        try { await navigator.clipboard.writeText($("#log-box").textContent); toast("Log copied"); }
        catch { toast("Couldn't copy; use Open folder instead"); }
      } }),
      el("button", { class: "btn", text: "Open folder", onclick: () => invoke("open_log_folder").catch((e) => toast(String(e))) })));
}

async function loadLog() {
  const box = $("#log-box");
  if (!box) return;
  try {
    const text = await invoke("read_log", { lines: 300 });
    box.textContent = text || "Nothing logged yet.";
    box.scrollTop = box.scrollHeight;
  } catch (e) { box.textContent = "Couldn't read the log: " + e; }
}

function renderVoice() {
  const rows = settings.voice_phrases.map((p, i) => {
    const input = el("input", { class: "input", value: p.text, "aria-label": `Phrase ${i + 1}` });
    input.addEventListener("change", () => {
      const phrases = settings.voice_phrases.map((x, j) => (j === i ? { ...x, text: input.value.trim() } : x));
      save({ voice_phrases: phrases }, "Phrase saved");
    });
    const lengths = el("div", { class: "chips", role: "group", "aria-label": `Clip length for phrase ${i + 1}` },
      [15, 30, 60, 120, 300].map((sec) => el("button", {
        class: "chip", "aria-pressed": String(p.seconds === sec), text: lengthLabel(sec), title: ramNote(sec),
        onclick: () => save({ voice_phrases: settings.voice_phrases.map((x, j) => (j === i ? { ...x, seconds: sec } : x)) }),
      })));
    const remove = el("button", { class: "btn", text: "Remove", "aria-label": `Remove phrase ${i + 1}`,
      onclick: () => save({ voice_phrases: settings.voice_phrases.filter((_, j) => j !== i) }, "Phrase removed") });
    return el("div", { class: "row" }, el("div", { class: "field", style: "flex:1" }, input, lengths, remove));
  });
  const add = el("button", { class: "btn", text: "+ Add a phrase",
    onclick: () => save({ voice_phrases: [...settings.voice_phrases, { text: "Archy save that", seconds: 30 }] }, "Phrase added") });
  $("#voice-settings").replaceChildren(
    settingCard({ key: "voice_enabled", name: "Voice clipping", rec: true,
      what: "Say a phrase instead of pressing a key. A tiny keyword model listens to your Archy microphone for only these phrases, entirely on this PC. Nothing is sent anywhere.",
      options: [
        { value: true, label: "On", best: "Clipping without taking your hands off the game. Uses a little CPU on one efficiency core. Needs Windows' microphone access for desktop apps turned on.", f: 0 },
        { value: false, label: "Off", best: "Hotkeys only, and the microphone is never listened to.", f: 0 },
      ] }),
    el("div", { class: "card" },
      el("h3", { text: "Your phrases" }),
      el("p", { class: "sub", text: "Use 3–4 real, everyday words you wouldn't say by accident, like \"Archer clip that\". Made-up names and gamer tags can't be recognized: the voice model only knows real English words." }),
      el("div", { style: "margin-top:8px" }, rows), el("div", { style: "margin-top:12px" }, add)),
    calibrationCard(),
    settingCard({ key: "voice_sensitivity", name: "Sensitivity", rec: settings.voice_tuning ? 0 : 50,
      what: "How easily your phrase triggers a clip. Calibrating (above) finds the best setting for your voice and mic.",
      options: [
        ...(settings.voice_tuning ? [{ value: 0, label: "Calibrated",
          best: `Tuned to your voice: caught ${settings.voice_tuning.hits} of ${settings.voice_tuning.prompts} phrases and never fired while you were just talking. Recalibrate if you change mic or phrase.` }] : []),
        { value: 25, label: "Strict", best: "Loud games, music or friends talking a lot: almost never clips by accident, but you may need to repeat the phrase." },
        { value: 50, label: "Balanced", best: "Most setups, if you haven't calibrated." },
        { value: 80, label: "Loose", best: "Quiet rooms or a soft voice, if Balanced misses you. May clip on similar-sounding words." },
      ] }),
    fineTuneCard(),
    voiceTestCard(),
    onOff("sound_on_voice", "Sound when a phrase is heard", "A soft blip the moment Archy hears you, before the clip finishes saving.",
      "Knowing Archy heard you.", "Silent voice clipping."),
    volumeRow("voice_sound_volume", "Ping volume", "How loud the blip is when Archy hears your phrase or you press the clip key."));
}

// ---------- voice fine-tuning and test ----------
function fineTuneLabel(v) {
  if (v === 0) return "No change";
  const how = Math.abs(v) <= 15 ? "A little" : Math.abs(v) <= 35 ? "" : "Much";
  const text = `${how} ${v < 0 ? "stricter" : "looser"}`.trim();
  return text[0].toUpperCase() + text.slice(1) + ` (${v > 0 ? "+" : ""}${v})`;
}

function fineTuneSlider() {
  const out = el("span", { class: "ed-vol", text: fineTuneLabel(settings.voice_fine_tune) });
  const range = el("input", { type: "range", min: "-50", max: "50", step: "5", value: String(settings.voice_fine_tune), "aria-label": "Voice fine-tune" });
  range.addEventListener("input", () => (out.textContent = fineTuneLabel(Number(range.value))));
  range.addEventListener("change", () => save({ voice_fine_tune: Number(range.value) }, "Sensitivity saved"));
  return el("div", { class: "ed-slider voice-slider" },
    el("div", { class: "voice-ends" },
      el("span", { text: "Stricter: fewer accidental clips" }), el("span", { text: "Looser: hears you more easily" })),
    range, el("div", { class: "voice-value" }, out));
}

function fineTuneCard() {
  return el("div", { class: "card" },
    el("h3", { text: "Fine-tune sensitivity" }),
    el("p", { class: "sub", text: "Adjusts the choice above in small steps. If Archy clips when you didn't ask, slide toward Stricter; if it misses you, toward Looser. The test below shows the effect right away." }),
    fineTuneSlider(),
    savedTestNote());
}

// The practice paragraph, one sentence at a time: the phrase 3 times, between
// sentences full of words that sound a bit like it, which shouldn't trigger.
function testSteps(phrase) {
  return [
    ["Okay, let's start this round. Watch the left side, they always rush there.", false],
    [phrase + ".", true],
    ["That was a clean shot. The archer on the hill keeps clipping the wall, so stay low.", false],
    [phrase + ".", true],
    ["Nice, I'll archive that later. Remember to reload before you peek, and keep talking.", false],
    [phrase + "!", true],
  ];
}

// step: sentence being read; hits[i]: times it went off during sentence i.
let vtest = { running: false, step: 0, hits: [], stepAt: 0, line: "", note: "", done: false };

function testScore() {
  const steps = testSteps("");
  let caught = 0, expected = 0, falseAlarms = 0;
  steps.forEach(([, isPhrase], i) => {
    const h = vtest.hits[i] || 0;
    if (isPhrase) { expected++; if (h > 0) caught++; } else falseAlarms += h;
  });
  return { caught, expected, false_alarms: falseAlarms };
}

/// How much to trust a test result: [level, word, advice].
function confidence(r) {
  if (r.false_alarms === 0 && r.caught === r.expected) return ["good", "High", "Every phrase caught and nothing set it off by accident. You're set."];
  if (r.false_alarms === 0 && r.caught >= r.expected - 1) return ["ok", "OK", "It missed one. Slide a little toward Looser and test again, or keep it if accidental clips worry you more."];
  if (r.false_alarms > 0) return ["bad", "Low", "It went off on sentences without your phrase. Slide toward Stricter and test again."];
  return ["bad", "Low", "It missed most of your phrases. Slide toward Looser (or calibrate again) and test again."];
}

function savedTestNote() {
  const r = settings.voice_test;
  if (!r) return el("p", { class: "sub", text: "No test saved yet. Run the test below to see how much you can trust this setting." });
  const [level, word, advice] = confidence(r);
  const stale = r.sensitivity !== settings.voice_sensitivity || r.fine_tune !== settings.voice_fine_tune;
  return el("div", { class: `voice-confidence ${stale ? "stale" : level}` },
    el("b", { text: `Confidence: ${word}` }),
    el("span", { text: ` · caught ${r.caught} of ${r.expected}, ${r.false_alarms} false alarm${r.false_alarms === 1 ? "" : "s"} · tested ${when(r.when)}` }),
    el("p", { class: "sub", text: stale ? "You've changed the sensitivity since this test. Test again to check the new setting." : advice }));
}

function voiceTestCard() {
  const phrase = settings.voice_phrases[0]?.text || "";
  const calibrated = settings.voice_sensitivity === 0 && settings.voice_tuning;
  const steps = testSteps(phrase);
  const kids = [
    el("h3", { text: "Test it" }),
    el("p", { class: "sub", text: (calibrated ? "" : "Calibrate first (above) for the best starting point. ") +
      "Read each sentence out loud at your normal gaming volume, then press Next. Three of them are your phrase; the others have similar-sounding words that shouldn't set it off. No clips are saved while testing." }),
  ];
  if (vtest.running || vtest.done) {
    kids.push(el("ol", { class: "voice-steps" }, steps.map(([text, isPhrase], i) => {
      const h = vtest.hits[i] || 0;
      const past = vtest.done || i < vtest.step;
      const mark = !past ? "" : isPhrase ? (h ? "✓ caught" : "✗ missed") : (h ? `✗ went off${h > 1 ? ` ${h}×` : ""}` : "✓ quiet");
      const ok = isPhrase ? h > 0 : h === 0;
      return el("li", { class: (i === vtest.step && !vtest.done ? "now " : "") + (isPhrase ? "phrase " : "") + (past ? (ok ? "ok" : "bad") : "") },
        el("span", { text }), mark ? el("small", { text: mark }) : null);
    })));
  }
  if (vtest.line) kids.push(el("div", { class: "voice-line", text: vtest.line }));
  if (vtest.done) {
    const r = testScore();
    const [level, word, advice] = confidence(r);
    kids.push(el("div", { class: `voice-confidence ${level}` },
      el("b", { text: `Confidence: ${word}` }),
      el("span", { text: ` · caught ${r.caught} of ${r.expected}, ${r.false_alarms} false alarm${r.false_alarms === 1 ? "" : "s"}` }),
      el("p", { class: "sub", text: advice })));
  }
  if (vtest.note) kids.push(el("p", { class: "sub", text: vtest.note }));
  if (vtest.running || vtest.done) kids.push(fineTuneSlider());
  const buttons = [];
  if (vtest.running && !vtest.done) {
    buttons.push(el("button", { class: "btn primary", text: vtest.step === steps.length - 1 ? "Finish" : "Next", onclick: nextTestStep }),
      el("button", { class: "btn", text: "Start over", onclick: restartTest }),
      el("button", { class: "btn", text: "Stop", onclick: stopVoiceTest }));
  } else if (vtest.done) {
    buttons.push(el("button", { class: "btn primary", text: "Save result", onclick: saveTestResult }),
      el("button", { class: "btn", text: "Test again", onclick: () => (vtest.running ? restartTest() : runVoiceTest()) }));
    if (vtest.running) buttons.push(el("button", { class: "btn", text: "Stop", onclick: stopVoiceTest }));
  } else {
    buttons.push(el("button", { class: "btn primary", text: "Start test", disabled: !phrase || calib.running ? "" : null, onclick: runVoiceTest }));
  }
  kids.push(el("div", { class: "field", style: "margin-top:12px" }, buttons));
  return el("div", { class: "card" }, ...kids);
}

function nextTestStep() {
  const last = testSteps("").length - 1;
  if (vtest.step >= last) { finishTest(); return; }
  vtest.step++;
  vtest.stepAt = performance.now();
  vtest.line = "";
  renderVoice();
}

function finishTest() {
  vtest.done = true;
  vtest.line = "";
  renderVoice();
}

function restartTest() {
  vtest = { ...vtest, step: 0, hits: [], stepAt: performance.now(), line: "Listening. Read the first sentence.", note: "", done: false };
  renderVoice();
}

function saveTestResult() {
  const r = testScore();
  save({ voice_test: { ...r, sensitivity: settings.voice_sensitivity, fine_tune: settings.voice_fine_tune, when: Date.now() } }, "Test result saved");
  vtest.note = "Saved. It shows next to the sensitivity slider above.";
}

async function runVoiceTest() {
  if (vtest.running || calib.running) return;
  vtest = { running: true, step: 0, hits: [], stepAt: performance.now(), line: "Starting the microphone…", note: "", done: false };
  renderVoice();
  const progress = new tauri.core.Channel();
  progress.onmessage = (m) => {
    if (m.step === "ready") { vtest.line = "Listening. Read the first sentence."; renderVoice(); }
    else if (m.step === "heard") {
      if (vtest.done) return;
      // A phrase is heard a moment after it's said: right after Next, it belongs
      // to the sentence just finished if that was the phrase and it had no hit yet.
      let i = vtest.step;
      const steps = testSteps("");
      if (i > 0 && performance.now() - vtest.stepAt < 1500 && steps[i - 1][1] && !vtest.hits[i - 1]) i -= 1;
      vtest.hits[i] = (vtest.hits[i] || 0) + 1;
      vtest.line = steps[i][1] ? "Heard it ✓" : "It went off, but that sentence didn't have your phrase";
      renderVoice();
    } else if (m.step === "retuned") {
      vtest = { ...vtest, step: 0, hits: [], stepAt: performance.now(), done: false, note: "", line: "New sensitivity in use. Read from the first sentence again." };
      renderVoice();
    } else if (m.step === "error") { vtest.running = false; vtest.line = "The test couldn't run"; vtest.note = m.message; renderVoice(); }
  };
  try { await invoke("test_voice", { progress }); }
  catch (e) { if (vtest.running && String(e) !== "stopped") { vtest.line = "The test stopped"; vtest.note = String(e); } }
  vtest.running = false;
  renderVoice();
}

async function stopVoiceTest() {
  vtest.running = false;
  try { await invoke("stop_calibration"); } catch {}
  if (!vtest.done) vtest = { running: false, step: 0, hits: [], stepAt: 0, line: "", note: "", done: false };
  renderVoice();
}

// ---------- voice calibration ----------
// User-paced: hold a button (or Space) while saying the phrase, so there's no
// timer to race. Each take is checked at once; then two look-alike sentences
// show what must NOT trigger; then Archy picks the best setting.
// Lives outside the settings so a redraw mid-run keeps its progress.
const CAL_TAKES = 5;
const CAL_MIN = 3;
const CAL_SENTENCES = [
  "The archer on the hill keeps clipping the wall, so stay low.",
  "Nice, I'll archive that later. Remember to reload before you peek.",
];
let calib = { running: false, ready: false, takes: [], talked: 0, holding: false, stage: "", line: "", sub: "", result: null };

function calibrationCard() {
  const phrase = settings.voice_phrases[0]?.text || "";
  const t = settings.voice_tuning;
  const learnedNote = t && t.phrase === phrase && t.learned?.length
    ? el("p", { class: "sub", text: `Archy has learned ${t.learned.length} way${t.learned.length === 1 ? "" : "s"} you say "${phrase}".` })
    : null;
  const kids = [
    el("h3", { text: "Calibrate to your voice" }),
    el("p", { class: "sub", text: `About a minute, at your own pace. 1) Hold the button and say "${phrase}", 5 times. 2) Hold it and read two sentences that sound a bit like it. Archy then picks the setting that hears you best without going off by accident. Uses the microphone chosen above; nothing leaves this PC.` }),
    learnedNote,
  ];
  if (calib.running) kids.push(...calibrationSteps(phrase));
  if (calib.line) kids.push(el("div", { class: "calib-line", text: calib.line }));
  if (calib.sub) kids.push(el("p", { class: "sub", text: calib.sub }));
  kids.push(el("div", { class: "field", style: "margin-top:12px" }, calib.running
    ? el("button", { class: "btn", text: "Stop", onclick: stopCalibration })
    : el("button", { class: "btn primary", text: calib.result ? "Calibrate again" : "Start calibrating", disabled: !phrase || vtest.running ? "" : null, onclick: () => runCalibration(phrase) })));
  return el("div", { class: "card" }, ...kids);
}

function calibrationSteps(phrase) {
  const out = [];
  // Live mic meter, updated in place (not by redrawing the page).
  calib.meter = el("i");
  out.push(el("div", { class: "calib-meter-row" }, el("span", { text: "Mic" }), el("div", { class: "calib-meter" }, calib.meter)));
  if (!calib.ready) { out.push(el("p", { class: "sub", text: "Starting the microphone…" })); return out; }
  if (calib.stage === "analyzing") return out;
  const talking = calib.stage === "talk";
  const done = calib.takes.length;
  out.push(el("div", { class: "calib-step" + (talking ? " past" : "") },
    el("b", { text: `1. Say "${phrase}"` }),
    el("div", { class: "calib-dots" }, Array.from({ length: CAL_TAKES }, (_, i) => {
      const t = calib.takes[i];
      const state = i >= done ? "" : t == null ? "wait" : t ? "ok" : "miss";
      return el("span", {
        class: state,
        text: { "": String(i + 1), wait: "…", ok: "✓", miss: "✗" }[state],
        title: { "": "", wait: "Checking…", ok: "Heard", miss: "Not heard at the Balanced setting (calibration still learns from it)" }[state],
      });
    }))));
  if (talking) {
    const n = Math.min(calib.talked, CAL_SENTENCES.length - 1);
    out.push(el("div", { class: "calib-step" },
      el("b", { text: `2. Read this sentence (${calib.talked + 1} of ${CAL_SENTENCES.length})` }),
      el("p", { class: "calib-sentence", text: CAL_SENTENCES[n] })));
  }
  const hold = el("button", { class: "btn primary calib-hold" + (calib.holding ? " on" : ""),
    text: calib.holding ? "Listening… let go when you're done" : talking ? "Hold to read it (or hold Space)" : "Hold to say it (or hold Space)" });
  hold.addEventListener("pointerdown", (e) => { hold.setPointerCapture(e.pointerId); holdStart(hold); });
  hold.addEventListener("pointerup", () => holdEnd(hold));
  hold.addEventListener("pointercancel", () => holdEnd(hold));
  const row = [hold];
  if (!talking && done) row.push(el("button", { class: "btn", text: "Redo last", onclick: () => invoke("calibration_input", { command: "undo" }).catch(() => {}) }));
  if (!talking && done >= CAL_MIN && done < CAL_TAKES) row.push(el("button", { class: "btn", text: "That's enough, next", onclick: () => { calib.stage = "talk"; calib.line = ""; calib.sub = ""; renderVoice(); } }));
  out.push(el("div", { class: "field calib-actions" }, row));
  return out;
}

function holdStart(button) {
  if (!calib.running || !calib.ready || calib.holding || calib.stage === "analyzing") return;
  calib.holding = true;
  button.classList.add("on");
  button.textContent = "Listening… let go when you're done";
  invoke("calibration_input", { command: calib.stage === "talk" ? "talk" : "take" }).catch(() => {});
}

function holdEnd(button) {
  if (!calib.holding) return;
  calib.holding = false;
  button?.classList.remove("on");
  invoke("calibration_input", { command: "stop" }).catch(() => {});
}

// Space works like the button while calibrating (not while typing).
// Every Space press is swallowed while calibrating: a held key repeats, and each
// repeat would otherwise scroll the page (or "click" whatever button has focus).
const calibSpace = (e) => e.code === "Space" && calib.running && view === "audio"
  && !/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName);
document.addEventListener("keydown", (e) => {
  if (!calibSpace(e)) return;
  e.preventDefault();
  if (!e.repeat) holdStart(document.querySelector(".calib-hold") || { classList: { add() {} }, set textContent(_) {} });
});
document.addEventListener("keyup", (e) => {
  if (!calibSpace(e)) return;
  e.preventDefault();
  holdEnd(document.querySelector(".calib-hold"));
});

function showCalib(line, sub = "") {
  calib.line = line;
  calib.sub = sub;
  renderVoice();
}

async function stopCalibration() {
  calib.cancelled = true;
  try { await invoke("stop_calibration"); } catch { /* already finished */ }
}

async function runCalibration(phrase) {
  if (calib.running) return;
  calib = { running: true, cancelled: false, ready: false, takes: [], talked: 0, holding: false, stage: "takes", line: "", sub: "", result: null };
  renderVoice();
  const progress = new tauri.core.Channel();
  progress.onmessage = (m) => {
    if (m.step === "level") {
      if (calib.meter) calib.meter.style.width = `${Math.round(Math.min(1, m.v * 3) * 100)}%`;
      return;
    }
    if (m.step === "ready") { calib.ready = true; showCalib("", "Hold the button, say your phrase the way you would in a game, then let go."); }
    else if (m.step === "take") {
      // Listed at once; whether it was heard arrives a moment later ("checked").
      calib.takes[m.n - 1] = m.heard;
      calib.takes.length = m.n;
      if (m.n >= CAL_TAKES) { calib.stage = "talk"; showCalib("", "Now two sentences that shouldn't set it off. Read each one normally."); }
      else showCalib(`Take ${m.n} saved`, "");
    } else if (m.step === "checked") {
      if (m.n <= calib.takes.length) calib.takes[m.n - 1] = m.heard;
      if (calib.stage === "takes" && m.n === calib.takes.length) {
        showCalib(m.heard ? `Heard it (${m.n} of ${CAL_TAKES})` : `Take ${m.n} saved`,
          m.heard ? "" : "Not heard at the Balanced setting yet. That's fine: calibration learns how you say it. Keep going.");
      } else renderVoice();
    } else if (m.step === "undone") { calib.takes.length = m.n; showCalib("Last take removed", ""); }
    else if (m.step === "talked") {
      calib.talked += 1;
      if (calib.talked >= CAL_SENTENCES.length) {
        calib.stage = "analyzing";
        invoke("calibration_input", { command: "analyze" }).catch(() => {});
      }
      renderVoice();
    } else if (m.step === "analyzing") showCalib("Finding your setting…", "Testing settings on your recording. About 30 seconds; you can play meanwhile.");
    else if (m.step === "error") { calib.running = false; showCalib("Calibration didn't finish", m.message); window.welcomeAfterCalibration?.(); }
    else if (m.step === "done") {
      calib.running = false;
      calib.result = m;
      if (m.hits >= Math.min(CAL_MIN, m.prompts)) {
        const learned = m.learned?.length ? ` Learned ${m.learned.length} way${m.learned.length === 1 ? "" : "s"} you say it.` : "";
        window.welcomeAfterCalibration?.();
        showCalib(`Done: caught ${m.hits} of ${m.prompts}`, `No false alarms on the look-alike sentences.${learned} Archy uses this now (Sensitivity: Calibrated). Run the test below to check it.`);
        save({ voice_sensitivity: 0, voice_fine_tune: 0, voice_tuning: { score: m.score, paths: m.paths, threshold: m.threshold, hits: m.hits, prompts: m.prompts, phrase, learned: m.learned || [] } }, "Voice calibrated");
      } else {
        showCalib(`Only caught ${m.hits} of ${m.prompts}`, "Nothing was changed. Check the mic meter moves when you talk, hold the button for the whole phrase, and say it clearly. A phrase with longer, clearer words also helps.");
      }
    }
  };
  try {
    await invoke("calibrate_voice", { phrase, mic: settings.mic_device || "", progress });
  } catch (e) {
    calib.running = false;
    if (calib.cancelled) showCalib("", "");
    else showCalib("Calibration didn't finish", String(e));
  }
}

// ---------- appearance ----------
const lookOf = (s) => ({ theme: s.theme, accent: s.accent, density: s.density, reduce_motion: s.reduce_motion });
function applySettingsLook() { if (settings) applyLook(lookOf(settings)); }
window.matchMedia("(prefers-color-scheme: light)").addEventListener("change", applySettingsLook);

/// Sun/moon on the rail: flips between light and dark (from "system" too).
$("#theme-btn").addEventListener("click", () => {
  const now = document.documentElement.dataset.theme;
  const next = now === "dark" ? "light" : "dark";
  settings.theme = next;
  applySettingsLook(); // at once, before the save round-trip
  save({ theme: next }, next === "dark" ? "Dark mode" : "Light mode");
});

function renderAppearance() {
  const pick = (patch, msg) => { Object.assign(settings, patch); applySettingsLook(); save(patch, msg); };
  const themeCard = (value, title, about) => el("button", {
    class: "theme-card", role: "radio", "aria-checked": String(settings.theme === value),
    onclick: () => pick({ theme: value }, `${title} theme`),
  }, el("div", { class: `mini ${value}` }, value === "system" ? null : [el("div", { class: "r" }), el("div", { class: "c" }, el("i"), el("i"), el("i"), el("u"))]),
    el("b", { text: title }), el("span", { text: about }));
  const ACCENTS = [["blue", "Blue", "#4D8DFF"], ["violet", "Violet", "#8B7CFF"], ["teal", "Teal", "#2EC4A6"], ["orange", "Orange", "#FF9F43"]];
  $("#appearance-settings").replaceChildren(
    el("div", { class: "card" },
      el("h3", { text: "Theme" }),
      el("p", { class: "sub", style: "margin:0 0 16px", text: "System follows Windows: light or dark, whatever Windows uses. The sun/moon button on the left switches at any time." }),
      el("div", { class: "themes", role: "radiogroup", "aria-label": "Theme" },
        themeCard("dark", "Dark", "Easy on the eyes next to a game"),
        themeCard("light", "Light", "Bright rooms and daytime"),
        themeCard("system", "System", "Follows your Windows setting"))),
    el("div", { class: "card setting" },
      el("div", { class: "about" }, el("h3", { text: "Accent colour" }), el("p", { text: "Buttons, highlights and progress bars." })),
      el("div", { class: "pick" }, el("div", { class: "swatches", role: "radiogroup", "aria-label": "Accent colour" },
        ACCENTS.map(([v, name, hex]) => el("button", { class: "swatch", role: "radio", "aria-checked": String(settings.accent === v), "aria-label": name, title: name, style: `background:${hex}`,
          onclick: () => pick({ accent: v }, `${name} accent`) }))))),
    el("div", { class: "card setting" },
      el("div", { class: "about" }, el("h3", { text: "Density" }), el("p", { text: "How much fits on screen at once." })),
      el("div", { class: "pick" }, el("div", { class: "seg", role: "group", "aria-label": "Density" },
        [["comfortable", "Comfortable"], ["compact", "Compact"]].map(([v, label]) => el("button", { "aria-pressed": String(settings.density === v), text: label, onclick: () => pick({ density: v }, `${label} spacing`) }))))),
    el("div", { class: "card setting" },
      el("div", { class: "about" }, el("h3", { text: "Reduce motion" }), el("p", { text: "No animations in the window. Recording is never affected." })),
      el("div", { class: "pick" }, el("div", { class: "seg", role: "group", "aria-label": "Reduce motion" },
        [[false, "Off"], [true, "On"]].map(([v, label]) => el("button", { "aria-pressed": String(settings.reduce_motion === v), text: label, onclick: () => pick({ reduce_motion: v }, v ? "Animations off" : "Animations on") }))))));
}

function renderAll() {
  renderAppearance();
  renderRecording();
  renderGames();
  renderStorage();
  renderHotkeys();
  renderAudio();
  renderGeneral();
}

// ---------- updates ----------
// ---------- updates ----------
let knownUpdate = null;

/// Light Markdown for release notes: headings, bold and bullet lists, as plain text nodes.
function notesView(md) {
  const box = el("div", { class: "upd-notes" });
  let list = null;
  for (const raw of md.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) { list = null; continue; }
    const strong = (t) => t.split(/\*\*(.+?)\*\*/).map((part, i) => (i % 2 ? el("b", { text: part }) : part));
    if (line.startsWith("#")) { list = null; box.append(el("h4", { text: line.replace(/^#+\s*/, "") })); }
    else if (/^[-*]\s/.test(line)) { if (!list) box.append(list = el("ul")); list.append(el("li", {}, ...strong(line.slice(2)))); }
    else { list = null; box.append(el("p", {}, ...strong(line))); }
  }
  return box;
}

function setUpdateDot(on) { $("#update-btn")?.classList.toggle("has-update", !!on); }

/// The update dialog. `u`: an update already found (opens straight to it);
/// otherwise it checks first.
async function updateDialog(u = null) {
  $("#upd-back")?.remove();
  const body = el("div", { class: "upd-body" });
  const actions = el("div", { class: "upd-actions" });
  const close = () => back.remove();
  const box = el("div", { class: "upd-box", role: "dialog", "aria-modal": "true", "aria-label": "Updates" },
    el("div", { class: "upd-top" }, el("h3", { text: "Updates" }), el("span", { class: "sub", text: `You have Archy v${appVersion}` })),
    body, actions);
  const back = el("div", { class: "upd-back", id: "upd-back" }, box);
  back.addEventListener("click", (e) => { if (e.target === back) close(); });
  back.addEventListener("keydown", (e) => { if (e.key === "Escape") close(); });
  document.body.append(back);
  const show = (kids, buttons) => { body.replaceChildren(...kids); actions.replaceChildren(...buttons); };
  const closeBtn = (label = "Close") => el("button", { class: "btn", text: label, onclick: close });
  if (!u) {
    show([el("div", { class: "upd-spin" }), el("p", { text: "Checking GitHub for a newer Archy…" })], [closeBtn("Cancel")]);
    try { u = await invoke("check_update", { manual: true }); }
    catch (e) { show([el("p", { class: "upd-big", text: "Couldn't check right now" }), el("p", { class: "sub", text: `${e}. Are you online?` })], [el("button", { class: "btn", text: "Try again", onclick: () => updateDialog() }), closeBtn()]); return; }
    if (!back.isConnected) return;
    if (!u) {
      knownUpdate = null; setUpdateDot(false);
      show([el("p", { class: "upd-big", text: "You're up to date" }), el("p", { class: "sub", text: `Archy v${appVersion} is the newest version.` })], [closeBtn()]);
      return;
    }
  }
  knownUpdate = u; setUpdateDot(true);
  const status = el("p", { class: "sub" });
  const install = el("button", { class: "btn primary", text: "Install now", onclick: async () => {
    install.disabled = true;
    status.textContent = "Downloading the installer… (checked against GitHub's fingerprint before it runs)";
    try { await invoke("install_update"); status.textContent = "The installer is open: click Yes there. Archy restarts on the new version; your clips and settings are kept."; }
    catch (err) { status.textContent = "Update failed: " + err; install.disabled = false; }
  } });
  show([
    el("p", { class: "upd-big", text: `Archy ${u.version} is available` }),
    u.notes ? notesView(u.notes) : el("p", { class: "sub", text: "Fixes and improvements." }),
    el("a", { href: "#", class: "sub", text: "Open the release page", onclick: (e) => { e.preventDefault(); invoke("open_release_page", { url: u.page }).catch((err) => toast(String(err))); } }),
    status,
  ], [
    el("button", { class: "btn", text: "Skip this version", onclick: () => { save({ skipped_version: u.version }, `Won't ask about ${u.version} again`); knownUpdate = null; setUpdateDot(false); close(); } }),
    closeBtn("Later"),
    install,
  ]);
}

/// Quiet check: on opening Archy (shows the dialog when there's something new)
/// and every 6 hours while it stays open (just the dot on the sidebar button).
async function checkUpdate(openDialog = true) {
  let u = null;
  try { u = await invoke("check_update", { manual: false }); } catch { return; }
  if (!u) return;
  const fresh = !knownUpdate || knownUpdate.version !== u.version;
  knownUpdate = u;
  setUpdateDot(true);
  if (openDialog) updateDialog(u);
  else if (fresh) toast(`Archy ${u.version} is available: click Check for updates`);
}
setInterval(() => checkUpdate(false), 6 * 60 * 60 * 1000);

// Loads a clip's thumbnail once its card is (nearly) visible, a few at a time
// so making new ones never holds the window up. Cached ones come back at once.
const thumbQueue = [];
let thumbsBusy = 0;
function pumpThumbs() {
  while (thumbsBusy < 3 && thumbQueue.length) {
    const img = thumbQueue.shift();
    if (!img.isConnected) continue;
    thumbsBusy++;
    invoke("clip_thumb", { path: img.dataset.clip })
      .then((p) => { img.src = fileSrc(p); img.onload = () => img.classList.add("ready"); })
      .catch(() => img.classList.add("none"))
      .finally(() => { thumbsBusy--; pumpThumbs(); });
  }
}
const thumbObserver = new IntersectionObserver((entries) => {
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    thumbObserver.unobserve(e.target);
    thumbQueue.push(e.target);
  }
  pumpThumbs();
}, { rootMargin: "300px" });

// ---------- start ----------
(async function start() {
  try {
    [settings, presets] = await Promise.all([invoke("get_settings"), invoke("presets")]);
  } catch (e) {
    document.body.prepend(el("p", { class: "empty", text: "Open this page from the Archy app. " + e }));
    return;
  }
  applySettingsLook();
  try { appVersion = await invoke("app_version"); } catch {}
  $("#app-version").textContent = appVersion ? `v${appVersion}` : "";
  $("#update-btn").addEventListener("click", () => updateDialog(knownUpdate));
  renderAll();
  refreshStatus();
  loadClips();
  window.maybeWelcome?.();
  loadDevices();
  checkUpdate();
  setInterval(refreshStatus, 2000);
  setInterval(() => { if (view === "clips" && !document.hidden) loadClips(); }, 4000);
})();
