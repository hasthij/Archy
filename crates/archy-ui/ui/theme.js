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

function applyLook(look) {
  const root = document.documentElement;
  root.dataset.theme = resolveTheme(look.theme);
  root.dataset.accent = look.accent || "blue";
  root.dataset.density = look.density || "comfortable";
  root.dataset.motion = look.reduce_motion ? "reduce" : "full";
  try { localStorage.setItem(ARCHY_LOOK_KEY, JSON.stringify(look)); } catch {}
}

(function earlyLook() {
  let look = { theme: "dark" };
  try { look = JSON.parse(localStorage.getItem(ARCHY_LOOK_KEY)) || look; } catch {}
  applyLook(look);
})();
