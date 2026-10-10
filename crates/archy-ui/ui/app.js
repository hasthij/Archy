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
  $("#recording-settings").replaceChildren(...RECORDING().map(settingCard));
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
  box.replaceChildren(
    el("div", { class: "card" },
      el("h3", { text: "Always record these" }),
      el("p", { class: "sub", text: "The program file of each game. Archy starts recording when one is in front." }),
      el("div", { class: "chips", style: "margin:14px 0" }, settings.games.map((g) =>
        el("span", { class: "tag" }, g, el("button", { "aria-label": `Remove ${g}`, text: "×", onclick: () => save({ games: settings.games.filter((x) => x !== g) }, `Removed ${g}`) })))),
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

function keyName(e) {
  const k = e.key;
  if (["Control", "Shift", "Alt", "Meta"].includes(k)) return null;
  if (/^F\d{1,2}$/.test(k)) return k;
  if (k.length === 1 && /[a-z0-9]/i.test(k)) return k.toUpperCase();
  return { Insert: "Insert", Delete: "Delete", Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown", Pause: "Pause", ScrollLock: "ScrollLock" }[k] || null;
}

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
      "If it's unplugged, Archy uses Windows' default mic instead. Voice clipping always listens on Windows' default mic (a Windows limit)."),
    devicePicker("output_device", devices.outputs, "Speakers / headphones", "Which output track 1 (everything you heard) records from. Pick the one you actually listen on.",
      "If it's unplugged, Archy uses Windows' default output instead."),
    onOff("audio_everything", "Track 1: everything you heard", "All PC audio mixed together: game, friends, music. Discord, phones and most players only play this track.",
      "Clips that sound right everywhere. Keep this on.", "Silent clips in apps that only play the first track."),
    onOff("audio_game", "Game only", "A separate track with just the game's sound.",
      "Editing: turn the game up or down on its own.", "Smaller files if you never edit."),
    onOff("audio_discord", "Discord only", "A separate track with just your friends in voice chat.",
      "Muting or lowering friends later without losing game sound.", "Keeping friends' voices out of the separate tracks (they're still in track 1)."),
    onOff("audio_mic", "Your microphone", "A separate track with your voice.",
      "Commentary and reactions; turn yourself up or down later.", "Keeping your mic out of clips entirely."),
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
}

function renderGeneral() {
  $("#general-settings").replaceChildren(
    onOff("notify_on_save", "Notification when a clip saves", "A small Windows notification with the clip's length and size.",
      "Knowing the clip saved without leaving the game.", "No pop-ups at all; check the library instead."),
    onOff("sound_on_save", "Sound when a clip saves", "Archy's short two-note chime.",
      "Hearing it worked while you keep playing, even in fullscreen.", "Silence: for streams or quiet rooms."),
    onOff("notify_on_start", "\"Archy is running\" notification", "A reminder when Archy starts that it's ready and what your hotkey is.",
      "Remembering your hotkey.", "A quieter start-up."),
    onOff("start_with_windows", "Start with Windows", "Archy starts in the tray when you sign in, so clips are always ready.",
      "Never forgetting to open Archy before playing.", "Starting Archy yourself only when you want it.", false),
    onOff("tray_recording_dot", "Recording dot on the tray icon", "A small red dot on Archy's icon while a game is being recorded.",
      "Seeing at a glance that Archy is recording.", "A plain icon at all times."));
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
      what: "Say a phrase instead of pressing a key. Windows' own speech recognizer listens for only these phrases, entirely on this PC. Nothing is sent anywhere.",
      options: [
        { value: true, label: "On", best: "Clipping without taking your hands off the game. Needs Windows' microphone access for desktop apps turned on.", f: 0 },
        { value: false, label: "Off", best: "Hotkeys only, and the microphone is never listened to.", f: 0 },
      ] }),
    el("div", { class: "card" },
      el("h3", { text: "Your phrases" }),
      el("p", { class: "sub", text: "2–4 words you wouldn't say by accident work best. Starting with \"Archy\" makes accidental clips very rare." }),
      el("div", { style: "margin-top:8px" }, rows), el("div", { style: "margin-top:12px" }, add)),
    onOff("sound_on_voice", "Sound when a phrase is heard", "A soft blip the moment Archy hears you, before the clip finishes saving.",
      "Knowing Archy heard you.", "Silent voice clipping."));
}

function renderAll() {
  renderRecording();
  renderGames();
  renderStorage();
  renderHotkeys();
  renderAudio();
  renderGeneral();
  renderVoice();
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
  setInterval(refreshStatus, 2000);
  setInterval(() => { if (view === "clips" && !document.hidden) loadClips(); }, 4000);
})();
