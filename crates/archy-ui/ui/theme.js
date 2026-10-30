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
  oled: true, surface: "outlined", selected: "tint", accent_hex: "", radius: 8, density: "cozy",
  icons: 1.6, motion: "subtle", font: "system", library: "grid", nav: "sidebar", tone: "neutral", actions: "icons",
};
let uiPrefs = (() => {
  try { return { ...UI_DEFAULTS, ...JSON.parse(localStorage.getItem(ARCHY_UI_KEY)) }; } catch { return { ...UI_DEFAULTS }; }
})();

function applyUi() {
  const root = document.documentElement, p = uiPrefs, d = root.dataset;
  d.oled = p.oled ? "1" : "0";
  d.surface = p.surface; d.selected = p.selected; d.font = p.font; d.nav = p.nav; d.tone = p.tone; d.actions = p.actions; d.density = p.density;
  if (d.motion !== "reduce") d.motion = p.motion === "none" ? "reduce" : p.motion;
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
  root.dataset.motion = look.reduce_motion ? "reduce" : "full";
  try { localStorage.setItem(ARCHY_LOOK_KEY, JSON.stringify(look)); } catch {}
  applyUi();
}

(function earlyLook() {
  let look = { theme: "dark" };
  try { look = JSON.parse(localStorage.getItem(ARCHY_LOOK_KEY)) || look; } catch {}
  applyLook(look);
})();
