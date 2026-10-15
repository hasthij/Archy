"use strict";
// First-run welcome: what Archy does, how to save a clip, voice calibration
// first, then a short spotlight tour of the window. Shown once (settings
// `onboarded`), and again from General > "Show the tour again".
// Own scope (like editor.js); uses app.js helpers.
(() => {
  let step = 0;
  let paused = false;

  const TOUR = [
    { target: '.rail-btn[data-view="clips"]', title: "Your clips", text: "Every clip lands here, newest first. Click one to watch it; Edit trims it and adds text, images, video or music; GIF makes a GIF; Copy puts it on your clipboard for Discord." },
    { target: "#game-btn", title: "Which game", text: "Automatic records the game you're playing. Pick a game here to always record that one, even in a window, and switch on “Record the whole screen” to include overlays." },
    { target: '.rail-btn[data-group="settings"]', title: "Settings", text: "Quality, frame rate, games, hotkeys, sound and voice. Every option says what it's best for." },
    { target: "#theme-btn", title: "Light or dark", text: "One click switches the look. More colours are in Settings › Appearance." },
    { target: "#update-btn", title: "Updates", text: "Archy checks for new versions and asks before installing. A dot here means one is ready." },
  ];

  function close(done) {
    $("#welcome")?.remove();
    document.querySelectorAll(".spot").forEach((n) => n.remove());
    if (done) save({ onboarded: true }, "You're all set");
  }

  function card(kids, buttons) {
    $("#welcome")?.remove();
    const box = el("div", { class: "welcome", id: "welcome", role: "dialog", "aria-modal": "true", "aria-label": "Welcome to Archy" },
      el("div", { class: "welcome-card" }, ...kids, el("div", { class: "welcome-actions" }, buttons)));
    document.body.append(box);
    box.querySelector(".welcome-actions .primary")?.focus();
  }

  function dots(n, of) {
    return el("div", { class: "welcome-dots", "aria-hidden": "true" }, Array.from({ length: of }, (_, i) => el("i", { class: i === n ? "on" : "" })));
  }

  function show() {
    const phrase = settings.voice_phrases[0]?.text || "Archer clip that";
    const next = () => { step++; show(); };
    if (step === 0) {
      card([
        el("div", { class: "welcome-logo", "aria-hidden": "true" }),
        el("h1", { text: "Welcome to Archy" }),
        el("p", { class: "sub", text: "Archy quietly keeps the last minute of your game in memory. When something great happens, save it, and it's a clip. It barely touches your FPS, and nothing ever leaves your PC." }),
        dots(0, 4),
      ], [el("button", { class: "btn", text: "Skip", onclick: () => close(true) }), el("button", { class: "btn primary", text: "Next", onclick: next })]);
    } else if (step === 1) {
      card([
        el("h2", { text: "Saving a clip" }),
        el("div", { class: "welcome-keys" },
          el("div", {}, el("kbd", { text: settings.hotkey_save }), el("span", { text: `the last ${settings.clip_seconds} seconds` })),
          el("div", {}, el("kbd", { text: settings.hotkey_save_long }), el("span", { text: `the last ${settings.long_clip_seconds} seconds` })),
          el("div", {}, el("kbd", { text: `“${phrase}”` }), el("span", { text: "say it, once voice clipping is set up" }))),
        el("p", { class: "sub", text: "Recording starts by itself when a game is open, and stops when it closes." }),
        dots(1, 4),
      ], [el("button", { class: "btn", text: "Back", onclick: () => { step--; show(); } }), el("button", { class: "btn primary", text: "Next", onclick: next })]);
    } else if (step === 2) {
      card([
        el("h2", { text: "Set up voice clipping first" }),
        el("p", { class: "sub", text: `Say “${phrase}” instead of pressing a key. About a minute: hold a button and say it five times, then read two short sentences. Archy learns your voice and mic, entirely on this PC.` }),
        dots(2, 4),
      ], [
        el("button", { class: "btn", text: "Later, I'll use the hotkey", onclick: () => { step = 3; startTour(); } }),
        el("button", { class: "btn primary", text: "Calibrate now", onclick: () => {
          // Turn voice on, open the calibration, and come back to the tour after.
          paused = true;
          $("#welcome")?.remove();
          if (!settings.voice_enabled) save({ voice_enabled: true }, "Voice clipping on");
          show_("audio");
          setTimeout(() => {
            document.querySelector(".calib-hold, .card .btn.primary")?.scrollIntoView({ block: "center" });
            runCalibration(phrase);
          }, 300);
        } }),
      ]);
    }
  }

  // app.js's view switcher (named show there).
  const show_ = (name) => window.showView(name);

  function startTour(i = 0) {
    document.querySelectorAll(".spot").forEach((n) => n.remove());
    $("#welcome")?.remove();
    if (i >= TOUR.length) { finish(); return; }
    const t = TOUR[i];
    const target = document.querySelector(t.target);
    if (!target) { startTour(i + 1); return; }
    if (i === 0) show_("clips");
    const r = target.getBoundingClientRect();
    const ring = el("div", { class: "spot spot-ring", "aria-hidden": "true",
      style: `left:${r.left - 6}px;top:${r.top - 6}px;width:${r.width + 12}px;height:${r.height + 12}px` });
    // Beside the target, kept on screen.
    const left = Math.min(window.innerWidth - 340, Math.max(12, r.right + 18 > window.innerWidth - 340 ? r.left - 340 : r.right + 18));
    const top = Math.min(window.innerHeight - 220, Math.max(12, r.top - 10));
    const tip = el("div", { class: "spot spot-tip", role: "dialog", "aria-label": t.title, style: `left:${left}px;top:${top}px` },
      el("div", { class: "spot-step", text: `${i + 1} of ${TOUR.length}` }),
      el("h3", { text: t.title }),
      el("p", { class: "sub", text: t.text }),
      el("div", { class: "welcome-actions" },
        el("button", { class: "btn", text: "Skip tour", onclick: () => finish() }),
        el("button", { class: "btn primary", text: i === TOUR.length - 1 ? "Done" : "Next", onclick: () => startTour(i + 1) })));
    document.body.append(el("div", { class: "spot spot-shade", "aria-hidden": "true" }), ring, tip);
    tip.querySelector(".primary").focus();
  }

  function finish() {
    document.querySelectorAll(".spot").forEach((n) => n.remove());
    card([
      el("h2", { text: "You're all set" }),
      el("p", { class: "sub", text: "Open a game and Archy starts recording by itself. Settings are on the left whenever you want to change something, and the tour is in General if you want it again." }),
      dots(3, 4),
    ], [el("button", { class: "btn primary", text: "Start using Archy", onclick: () => close(true) })]);
  }

  // Called by the calibration when it ends (done or not): carry on with the tour.
  window.welcomeAfterCalibration = () => {
    if (!paused) return;
    paused = false;
    setTimeout(() => startTour(0), 1500);
  };

  // First run, or "Show the tour again".
  window.startWelcome = () => { step = 0; paused = false; show(); };
  // Whichever comes second (this file or the settings) opens it, once.
  let offered = false;
  window.maybeWelcome = () => {
    if (offered || !settings || settings.onboarded) return;
    offered = true;
    window.startWelcome();
  };
  window.maybeWelcome();
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && $("#welcome") && !paused) close(true); });
})();
