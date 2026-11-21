"use strict";
// Applies the theme before the page is drawn, so opening Archy never flashes
// the wrong colours. The real source is settings.json (app.js re-applies it once
// settings load); this only remembers the last look in the window's own storage.
const ARCHY_LOOK_KEY = "archy-look";

/// "dark" | "light" from the setting ("system" follows Windows).
function resolveTheme(theme) {
  if (theme === "light" || theme === "dark") return theme;
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

// Look and layout choices that are only about how the window is drawn. They live in
// the window's own storage, never in settings.json, so the recorder is untouched.
const ARCHY_UI_KEY = "archy-ui-prefs";
const UI_DEFAULTS = {
  oled: true, surface: "raised", selected: "tint", accent_hex: "", radius: 0, density: "roomy",
  icons: 1.6, motion: "subtle", font: "grotesk", library: "grid", nav: "rail", tone: "neutral", actions: "icons", scale: 100, thumb: 240, meta: "on", hover_preview: "on", one_click_play: "off",
};
let uiPrefs = (() => {
  try {
    const s = JSON.parse(localStorage.getItem(ARCHY_UI_KEY)) || {};
    // v2 (0.8.0): Grotesk + Sharp are the new defaults; older saved choices of those two reset once.
    if (s.ver !== 2) { delete s.font; delete s.radius; s.ver = 2; }
    return { ...UI_DEFAULTS, ...s };
  } catch { return { ...UI_DEFAULTS, ver: 2 }; }
})();

function applyUi() {
  const root = document.documentElement, p = uiPrefs, d = root.dataset;
  d.oled = p.oled ? "1" : "0";
  d.sl = "glow"; d.tg = "switch"; d.dd = "filled"; d.bt = "outline"; // 0.6.3 look kit
  // Hybrid Rail fixes nav/surface/tone/selected; old saved values are ignored.
  d.surface = "raised"; d.selected = "tint"; d.nav = "rail"; d.tone = "neutral";
  d.font = p.font; d.actions = p.actions; d.density = p.density;
  d.motion = (p.motion === "none" || d.rm === "1") ? "reduce" : (p.motion || "subtle");
  d.meta = p.meta;
  root.style.setProperty("--thumb-min", `${Number(p.thumb) || 240}px`);
  const z = (Number(p.scale) || 100) / 100;
  root.style.zoom = String(z); root.style.setProperty("--z", String(z));
  root.style.setProperty("--radius", `${Number(p.radius) || 0}px`);
  root.style.setProperty("--icon-stroke", String(p.icons));
  if (/^#[0-9a-f]{6}$/i.test(p.accent_hex)) {
    const n = parseInt(p.accent_hex.slice(1), 16), lum = ((n >> 16) * 299 + ((n >> 8) & 255) * 587 + (n & 255) * 114) / 1000;
    root.style.setProperty("--accent", p.accent_hex);
    root.style.setProperty("--on-accent", lum > 150 ? "#0A0C11" : "#FFFFFF");
  } else {
    root.style.removeProperty("--accent");
    root.style.removeProperty("--on-accent");
  }
}

function setUi(patch) {
  uiPrefs = { ...uiPrefs, ...patch };
  try { localStorage.setItem(ARCHY_UI_KEY, JSON.stringify(uiPrefs)); } catch {}
  applyUi();
}

function applyLook(look) {
  const root = document.documentElement;
  root.dataset.theme = resolveTheme(look.theme);
  root.dataset.accent = look.accent || "blue";
  root.dataset.density = look.density || "comfortable";
  root.dataset.rm = look.reduce_motion ? "1" : "0";
  try { localStorage.setItem(ARCHY_LOOK_KEY, JSON.stringify(look)); } catch {}
  applyUi();
}

// Slider fill: the filled part of the track follows --p (percent), set on every input.
function syncRange(r) {
  const min = Number(r.min || 0), max = Number(r.max || 100);
  r.style.setProperty("--p", `${max > min ? ((Number(r.value) - min) / (max - min)) * 100 : 0}%`);
}
document.addEventListener("input", (e) => { if (e.target.type === "range") syncRange(e.target); }, true);
new MutationObserver((muts) => {
  for (const m of muts) for (const n of m.addedNodes) if (n.nodeType === 1) { if (n.matches?.("input[type=range]")) syncRange(n); n.querySelectorAll?.("input[type=range]").forEach(syncRange); }
}).observe(document, { childList: true, subtree: true });

(function earlyLook() {
  let look = { theme: "dark" };
  try { look = JSON.parse(localStorage.getItem(ARCHY_LOOK_KEY)) || look; } catch {}
  applyLook(look);
})();
