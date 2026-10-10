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
  const video = el("video", { preload: "metadata", muted: true, src: fileSrc(c.path) + "#t=0.5" });
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

function play(c) {
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
    what: "Frames per second in your clips (24–90). Archy only takes this many even if your game runs at 240, so lower saves real work.",
    options: [
      { value: 24, label: "24", best: "A film-like look with the lowest load. Fast games look choppy.", f: 0, q: 1, z: 0 },
      { value: 30, label: "30", best: "Weak laptops or slow-paced games. Half the work of 60.", f: 0, q: 1, z: 0 },
      { value: 45, label: "45", best: "A middle ground for integrated graphics that struggle with 60.", f: 1, q: 2, z: 1 },
      { value: 60, label: "60", best: "Most people. Smooth motion; what YouTube and Discord play best.", f: 1, q: 2, z: 2 },
      { value: 75, label: "75", best: "High-refresh players who want slightly smoother clips.", f: 2, q: 3, z: 2 },
      { value: 90, label: "90", best: "Maximum smoothness for fast shooters. The most work and biggest files.", f: 2, q: 3, z: 3 },
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

function renderGames() {
  const box = $("#games-settings");
  const input = el("input", { class: "input", placeholder: "Game file name, e.g. VALORANT-Win64-Shipping.exe", "aria-label": "Add a game" });
  const add = () => {
    const name = input.value.trim();
    if (!name) return;
    if (!settings.games.some((g) => g.toLowerCase() === name.toLowerCase())) save({ games: [...settings.games, name] }, `Added ${name}`);
    input.value = "";
  };
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") add(); });
  const running = el("select", { class: "input", "aria-label": "Running apps" }, el("option", { value: "", text: "Pick from apps that are open now…" }));
  const loadRunning = async () => {
    const apps = await invoke("running_apps").catch(() => []);
    running.replaceChildren(el("option", { value: "", text: apps.length ? "Pick from apps that are open now…" : "No apps found" }),
      ...apps.map((a) => el("option", { value: a.exe, text: `${a.title}  (${a.exe})` })));
  };
  running.addEventListener("focus", loadRunning);
  running.addEventListener("change", () => {
    const exe = running.value;
    if (exe && !settings.games.some((g) => g.toLowerCase() === exe.toLowerCase())) save({ games: [...settings.games, exe] }, `Added ${exe}`);
  });
  loadRunning();
  box.replaceChildren(
    el("div", { class: "card" },
      el("h3", { text: "Always record these" }),
      el("p", { class: "sub", text: "The program file of each game. Archy starts recording when one is in front." }),
      el("div", { class: "chips", style: "margin:14px 0" }, settings.games.map((g) =>
        el("span", { class: "tag" }, g, el("button", { "aria-label": `Remove ${g}`, text: "×", onclick: () => save({ games: settings.games.filter((x) => x !== g) }, `Removed ${g}`) })))),
      el("div", { class: "field" }, running, el("button", { class: "btn", text: "Refresh", onclick: loadRunning })),
      el("p", { class: "sub", style: "margin:12px 0 6px", text: "Or type the program's file name:" }),
      el("div", { class: "field" }, input, el("button", { class: "btn", text: "Add", onclick: add }))),
    settingCard({ key: "record_fullscreen_apps", name: "Other fullscreen apps", rec: true,
      what: "Also record anything else running fullscreen (browsers, Discord and video players are ignored).",
      options: [
        { value: true, label: "On", best: "Catching games that aren't on your list without setting anything up." },
        { value: false, label: "Off", best: "Only ever recording the games listed above." },
      ] }));
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

function renderGeneral() {
  $("#general-settings").replaceChildren(
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
    onOff("sound_on_voice", "Sound when a phrase is heard", "A soft blip the moment Archy hears you, before the clip finishes saving.",
      "Knowing Archy heard you.", "Silent voice clipping."),
    volumeRow("voice_sound_volume", "Voice ping volume", "How loud the blip is when Archy hears your phrase."));
}

// ---------- voice calibration ----------
// Lives outside the settings so a redraw mid-run keeps showing progress.
let calib = { running: false, line: "", sub: "", result: null };

function calibrationCard() {
  const phrase = settings.voice_phrases[0]?.text || "";
  const big = el("div", { style: "font-size:20px;font-weight:600;margin:10px 0 4px", text: calib.line });
  const sub = el("p", { class: "sub", text: calib.sub });
  const start = el("button", { class: "btn primary", text: calib.result ? "Calibrate again" : "Calibrate",
    disabled: !phrase ? "" : null, onclick: () => runCalibration(phrase) });
  const stop = el("button", { class: "btn", text: "Stop", onclick: stopCalibration });
  const restart = el("button", { class: "btn", text: "Restart", onclick: async () => { await stopCalibration(); runCalibration(phrase); } });
  const t = settings.voice_tuning;
  const learnedNote = t && t.phrase === phrase && t.learned?.length
    ? el("p", { class: "sub", text: `Archy has learned ${t.learned.length} way${t.learned.length === 1 ? "" : "s"} you say "${phrase}".` })
    : null;
  return el("div", { class: "card" },
    el("h3", { text: "Calibrate to your voice" }),
    el("p", { class: "sub", text: `About a minute. Archy asks you to say "${phrase}" 5 times (say it all in one go, the way you would in a game), then to talk normally for 15 s. It learns how you pronounce it and picks the setting that hears you best without clipping by accident. Uses the microphone chosen above; the recording never leaves this PC and is thrown away afterwards.` }),
    learnedNote,
    calib.line ? big : null, calib.sub ? sub : null,
    el("div", { class: "field", style: "margin-top:12px" }, calib.running ? [stop, restart] : start));
}

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
  calib = { running: true, cancelled: false, line: "Starting the microphone…", sub: "", result: null };
  renderVoice();
  let countdown;
  const progress = new tauri.core.Channel();
  progress.onmessage = (m) => {
    clearInterval(countdown);
    if (m.step === "ready") showCalib("Get ready…", `Say "${phrase}" each time Archy asks.`);
    else if (m.step === "say") showCalib(`Say: "${phrase}"`, `${m.n} of ${m.of}. Say it the way you would in a game.`);
    else if (m.step === "talk") {
      let left = m.secs;
      const tick = () => showCalib("Now just talk normally (or play)", `Don't say the phrase. ${left} s left…`);
      tick();
      countdown = setInterval(() => { left = Math.max(0, left - 1); tick(); }, 1000);
    } else if (m.step === "analyzing") showCalib("Learning how you say it…", "Testing settings on your recording. About 30 seconds; you can play meanwhile.");
    else if (m.step === "error") { calib.running = false; showCalib("Calibration didn't finish", m.message); }
    else if (m.step === "done") {
      calib.running = false;
      calib.result = m;
      if (m.hits >= 3) {
        const learned = m.learned?.length ? ` Learned ${m.learned.length} way${m.learned.length === 1 ? "" : "s"} you say it.` : "";
        showCalib(`Done: caught ${m.hits} of ${m.prompts}`, `No false alarms while you talked.${learned} Archy uses this now (Sensitivity: Calibrated).`);
        save({ voice_sensitivity: 0, voice_tuning: { score: m.score, paths: m.paths, threshold: m.threshold, hits: m.hits, prompts: m.prompts, phrase, learned: m.learned || [] } }, "Voice calibrated");
      } else {
        showCalib(`Only caught ${m.hits} of ${m.prompts}`, "Nothing was changed. Try again closer to the mic, check the microphone picked above, or use a phrase with clearer, longer words.");
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
  clearInterval(countdown);
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

// ---------- start ----------
(async function start() {
  try {
    [settings, presets] = await Promise.all([invoke("get_settings"), invoke("presets")]);
  } catch (e) {
    document.body.prepend(el("p", { class: "empty", text: "Open this page from the Archy app. " + e }));
    return;
  }
  renderAll();
  refreshStatus();
  loadClips();
  loadDevices();
  checkUpdate();
  setInterval(refreshStatus, 2000);
  setInterval(() => { if (view === "clips" && !document.hidden) loadClips(); }, 4000);
})();
