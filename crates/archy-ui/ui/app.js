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
function show(name) {
  view = name;
  document.querySelectorAll(".nav").forEach((b) => b.setAttribute("aria-current", b.dataset.view === name ? "page" : "false"));
  document.querySelectorAll(".view").forEach((v) => (v.hidden = v.id !== `view-${name}`));
  if (name === "clips") loadClips();
}
document.querySelectorAll(".nav").forEach((b) => b.addEventListener("click", () => show(b.dataset.view)));

// ---------- status ----------
async function refreshStatus() {
  let s;
  try { s = await invoke("get_status"); } catch { return; }
  const box = $("#status");
  box.replaceChildren();
  const keys = settings
    ? el("div", { class: "keys" }, "Save: ", el("b", { text: settings.hotkey_save }), ` (last ${settings.clip_seconds} s) · `, el("b", { text: settings.hotkey_save_long }), ` (last ${settings.long_clip_seconds} s)`)
    : null;
  if (!s.running) {
    box.append(
      el("span", { class: "dot warn" }),
      el("div", { class: "what" }, el("b", { text: "Archy recorder is not running" }), el("span", { text: "Start it to record your games in the background." })),
      el("button", { class: "btn primary", text: "Start Archy", onclick: async () => { await invoke("start_recorder").catch((e) => toast(String(e))); setTimeout(refreshStatus, 1500); } })
    );
    return;
  }
  const states = {
    recording: ["dot on", `Recording ${s.game}`, `Keeping the last ${settings ? settings.long_clip_seconds : 60} s in memory · ${s.detail}`],
    waiting: ["dot", "Waiting for a game", "Recording starts by itself when a game from your list (or any fullscreen app) is open."],
    paused: ["dot warn", "Paused", "Nothing is being recorded. Press your pause hotkey or use the tray icon to resume."],
  };
  const [dot, title, line] = states[s.state] || ["dot", s.state, ""];
  box.append(el("span", { class: dot }), el("div", { class: "what" }, el("b", { text: title }), el("span", { text: line })), keys);
}

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

function renderClips() {
  const grid = $("#clips");
  const todayStr = new Date().toDateString();
  const shown = clips.filter((c) => filter === "all" || new Date(c.modified).toDateString() === todayStr);
  $("#clips-empty").hidden = shown.length > 0;
  // Keep existing cards (and their loaded thumbnails) when nothing changed.
  const key = shown.map((c) => c.path + c.bytes).join("|");
  if (grid.dataset.key === key) return;
  grid.dataset.key = key;
  grid.replaceChildren(...shown.map(clipCard));
}

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

function clipCard(c) {
  // The file loads only when the card scrolls into view: with every card
  // loading at once, dozens of big 1080p clips were decoded on each redraw.
  const video = el("video", { preload: "none", muted: true });
  video.dataset.src = fileSrc(c.path) + "#t=0.5";
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
  return el("article", { class: "clip" },
    el("button", { class: "thumb", "aria-label": `Play ${c.name}`, onclick: () => play(c) },
      video, el("span", { class: "play" }, playIcon()), el("span", { class: "size-tag", text: MB(c.bytes) })),
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
  const b = el("button", { class: "btn discord", text: "Make Discord copy (under 10 MB)" });
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
      b.textContent = "Make Discord copy (under 10 MB)";
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

document.querySelectorAll("[data-filter]").forEach((b) => b.addEventListener("click", () => {
  filter = b.dataset.filter;
  document.querySelectorAll("[data-filter]").forEach((x) => x.setAttribute("aria-pressed", String(x === b)));
  renderClips();
}));
$("#open-folder").addEventListener("click", () => invoke("open_clips_folder").catch((e) => toast(String(e))));

// ---------- saving settings ----------
let saveTimer;
function save(patch, message = "Saved. Applies within 2 seconds") {
  Object.assign(settings, patch);
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    try {
      settings = await invoke("save_settings", { settings });
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
        const mb = Math.round((p.mbps * settings.clip_seconds) / 8);
        const warn = mb > 10 ? " Too big for Discord's free 10 MB limit." : "";
        return { value: p.name, label: p.name, best: `${best} About ${mb} MB per ${settings.clip_seconds} s clip.${warn}`, f, q, z };
      }),
    ],
  };
}

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
      { value: 120, label: "120", best: "120 Hz monitors and slow-motion edits. Same file size per second as 60, so each frame is a little softer; pick 1080p or higher to keep it sharp.", f: 3, q: 3, z: 3 },
      { value: 144, label: "144", best: "144 Hz monitors: clips look exactly as smooth as you played. The most work for your graphics chip; skip it on laptops that struggle at 60.", f: 3, q: 3, z: 3 },
    ] },
  { key: "codec", name: "Video format", rec: "h264",
    what: "How clips are compressed. All formats cost the same FPS (the GPU's video chip does the work); newer ones look better at the same size but play in fewer places.",
    options: [
      { value: "h264", label: "H.264", best: "Sending clips. Plays everywhere: Discord previews, phones, every editor and site.", f: 1, q: 1, z: 2 },
      { value: "hevc", label: "HEVC", best: "Better-looking clips at the same size. Some apps and older PCs won't preview it.", f: 1, q: 2, z: 1 },
      { value: "av1", label: "AV1", best: "Best quality per MB on new GPUs (RTX 40+, Radeon RX 7000+, Intel Arc). Many apps can't preview it yet.", f: 1, q: 3, z: 0 },
      { value: "auto", label: "Auto", best: "Archy uses the best format your graphics chip encodes in hardware.", f: 1, q: 3, z: 1 },
    ] },
  { key: "capture", name: "What to capture", rec: "window",
    what: "Both are anti-cheat safe: Archy never injects anything into your game.",
    options: [
      { value: "window", label: "Game window", best: "Records only the game. Discord popups and notifications never end up in your clips." },
      { value: "screen", label: "Whole screen", best: "Games that don't record properly as a window, or when you want overlays in the clip." },
    ] },
  { key: "efficiency_mode", name: "Efficiency mode", rec: true,
    what: "Keeps Archy away from your game's CPU time: low priority, power-saving scheduling, and on newer Intel/AMD chips only the efficiency cores.",
    options: [
      { value: true, label: "On", best: "Everyone. In Roblox this cut Archy's FPS cost to about 2–3%.", f: 0 },
      { value: false, label: "Off", best: "Only if clips drop frames on a PC with very few cores.", f: 2 },
    ] },
  { key: "clip_seconds", name: "Clip length", rec: 30,
    what: "How far back a clip goes when you press your save hotkey. Longer clips use more RAM while you play, not more FPS.",
    options: [15, 30, 45, 60].map((s) => ({ value: s, label: `${s} s`, best: { 15: "Quick reactions and single kills.", 30: "Most moments.", 45: "Plays with a longer build-up.", 60: "Whole fights or rounds." }[s] })) },
  { key: "long_clip_seconds", name: "Long clip length", rec: 60,
    what: "What your long-clip hotkey saves. Archy keeps this much in memory.",
    options: [30, 45, 60].map((s) => ({ value: s, label: `${s} s`, best: s === 60 ? "Catch everything that just happened. About 2× the RAM of a 30 s buffer." : "Less RAM if your PC is short on memory." })) },
];

function renderRecording() {
  $("#recording-settings").replaceChildren(...RECORDING().map(settingCard), screenPicker(), gpuPicker());
  loadMonitors();
  loadGpus();
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
      el("p", { text: "With Whole screen on, Archy records this screen while a game is open, like picking a screen to share." })),
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

function hotkeyRow(key, label, desc) {
  const shown = () => settings[key] || "Off";
  const box = el("button", { class: "input keybox", text: shown(), "aria-label": `Change hotkey for ${label}` });
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
      if (value) save({ [key]: value }, `Hotkey set to ${value}`);
      else box.textContent = shown();
    };
    window.addEventListener("keydown", onKey, true);
  });
  const clear = settings[key]
    ? el("button", { class: "btn", text: "Turn off", onclick: () => save({ [key]: "" }, `${label} hotkey turned off`) })
    : null;
  return el("div", { class: "row" }, el("div", { class: "label" }, el("b", { text: label }), el("span", { text: desc })), el("div", { class: "field" }, box, clear));
}

function renderHotkeys() {
  $("#hotkey-settings").replaceChildren(el("div", { class: "card" },
    hotkeyRow("hotkey_save", "Save clip", `Saves the last ${settings.clip_seconds} s`),
    hotkeyRow("hotkey_save_long", "Save long clip", `Saves the last ${settings.long_clip_seconds} s`),
    hotkeyRow("hotkey_pause", "Pause / resume", "Stops recording completely: zero load. Off by default so it can't be hit by accident; the tray menu can pause too.")));
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
    onOff("audio_everything", "Track 1: everything you heard", "All PC audio mixed together: game, friends, music, plus your microphone when it's on below. Discord, phones and most players only play this track.",
      "Clips that sound right everywhere. Keep this on.", "Silent clips in apps that only play the first track."),
    volumeRow("volume_everything", "Track 1 volume", "How loud everything the PC plays is in track 1 (your mic has its own slider below)."),
    onOff("audio_game", "Game only", "A separate track with just the game's sound.",
      "Editing: turn the game up or down on its own.", "Smaller files if you never edit."),
    volumeRow("volume_game", "Game track volume", "How loud the game-only track is."),
    onOff("audio_discord", "Discord only", "A separate track with just your friends in voice chat.",
      "Muting or lowering friends later without losing game sound.", "Keeping friends' voices out of the separate tracks (they're still in track 1)."),
    volumeRow("volume_discord", "Discord track volume", "How loud the Discord-only track is."),
    onOff("audio_mic", "Your microphone", "Your voice, mixed into track 1 (so every player hears you) and also kept as its own track.",
      "Commentary and reactions; turn yourself up or down later.", "Keeping your mic out of clips entirely."),
    volumeRow("volume_mic", "Microphone volume", "How loud your voice is, in track 1 and in your own track."),
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
    el("button", { class: "btn", text: "Check now", onclick: async () => {
      try { const u = await invoke("check_update"); toast(u ? `Archy ${u.version} is available` : "You have the latest version"); if (u) checkUpdate(); }
      catch (e) { toast(String(e)); }
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
      [15, 30, 45, 60].map((sec) => el("button", {
        class: "chip", "aria-pressed": String(p.seconds === sec), text: `${sec} s`,
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
    volumeRow("voice_sound_volume", "Voice ping volume", "How loud the blip is when Archy hears your phrase."));
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
    el("div", { class: "calib-dots" }, Array.from({ length: CAL_TAKES }, (_, i) => el("span", {
      class: i < done ? (calib.takes[i] ? "ok" : "miss") : "",
      text: i < done ? (calib.takes[i] ? "✓" : "✗") : String(i + 1),
      title: i < done ? (calib.takes[i] ? "Heard" : "Not heard at the Balanced setting") : "",
    })))));
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
      calib.takes[m.n - 1] = m.heard;
      calib.takes.length = m.n;
      if (m.n >= CAL_TAKES) { calib.stage = "talk"; showCalib("", "Now two sentences that shouldn't set it off. Read each one normally."); }
      else showCalib(m.heard ? `Heard it (${m.n} of ${CAL_TAKES})` : `Take ${m.n} saved`,
        m.heard ? "" : "Not heard at the Balanced setting yet. That's fine: calibration learns how you say it. Keep going.");
    } else if (m.step === "undone") { calib.takes.length = m.n; showCalib("Last take removed", ""); }
    else if (m.step === "talked") {
      calib.talked += 1;
      if (calib.talked >= CAL_SENTENCES.length) {
        calib.stage = "analyzing";
        invoke("calibration_input", { command: "analyze" }).catch(() => {});
      }
      renderVoice();
    } else if (m.step === "analyzing") showCalib("Finding your setting…", "Testing settings on your recording. About 30 seconds; you can play meanwhile.");
    else if (m.step === "error") { calib.running = false; showCalib("Calibration didn't finish", m.message); }
    else if (m.step === "done") {
      calib.running = false;
      calib.result = m;
      if (m.hits >= Math.min(CAL_MIN, m.prompts)) {
        const learned = m.learned?.length ? ` Learned ${m.learned.length} way${m.learned.length === 1 ? "" : "s"} you say it.` : "";
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

function renderAll() {
  renderRecording();
  renderGames();
  renderStorage();
  renderHotkeys();
  renderAudio();
  renderGeneral();
}

// ---------- updates ----------
async function checkUpdate() {
  let u = null;
  try { u = await invoke("check_update"); } catch { return; }
  if (!u) return;
  const old = $("#update-banner");
  if (old) old.remove();
  const status = el("span", { class: "sub" });
  const banner = el("section", { class: "status", id: "update-banner", style: "margin-bottom:12px;border-color:var(--accent)" },
    el("div", { class: "what" },
      el("b", { text: `Archy ${u.version} is available` }),
      el("span", { text: "Fixes and improvements. Your clips and settings are kept. " },
        el("a", { href: "#", text: "What's new", onclick: (e) => { e.preventDefault(); invoke("open_release_page", { url: u.page }).catch((err) => toast(String(err))); } }))),
    status,
    el("button", { class: "btn primary", text: "Update", onclick: async (e) => {
      e.target.disabled = true;
      status.textContent = "Downloading…";
      try {
        await invoke("install_update");
        status.textContent = "The installer is open: finish there.";
      } catch (err) {
        status.textContent = "Update failed: " + err;
        e.target.disabled = false;
      }
    } }),
    el("button", { class: "btn", text: "Not now", onclick: () => banner.remove() }),
    el("button", { class: "btn", text: "Skip this version", onclick: () => { save({ skipped_version: u.version }, `Won't ask about ${u.version} again`); banner.remove(); } }));
  $("#status").before(banner);
}

// Loads a clip's thumbnail once its card is (nearly) visible.
const thumbObserver = new IntersectionObserver((entries) => {
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    const v = e.target;
    thumbObserver.unobserve(v);
    v.preload = "metadata";
    v.src = v.dataset.src;
  }
}, { rootMargin: "200px" });

// ---------- start ----------
(async function start() {
  try {
    [settings, presets] = await Promise.all([invoke("get_settings"), invoke("presets")]);
  } catch (e) {
    document.body.prepend(el("p", { class: "empty", text: "Open this page from the Archy app. " + e }));
    return;
  }
  try { appVersion = await invoke("app_version"); } catch {}
  $("#app-version").textContent = appVersion ? `Archy v${appVersion}` : "";
  renderAll();
  refreshStatus();
  loadClips();
  loadDevices();
  checkUpdate();
  setInterval(refreshStatus, 2000);
  setInterval(() => { if (view === "clips" && !document.hidden) loadClips(); }, 4000);
})();
