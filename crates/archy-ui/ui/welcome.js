"use strict";
// First-run welcome: what Archy does, how to save a clip, voice calibration
// first, then a short spotlight tour of the window. Shown once (settings
// `onboarded`), and again from General > "Show the tour again".
// Own scope (like editor.js); uses app.js helpers.
(() => {
  let step = 0;
  let paused = false;
  let tour = null; // the spotlight nodes, kept alive so they glide between stops

  const TOUR = [
    { target: '.rail-btn[data-view="clips"]', title: "Your clips", text: "Every clip lands here, newest first. Click one to watch it; Edit trims it and adds text, images, video, music or auto captions; GIF makes a GIF; Copy puts it on your clipboard for Discord." },
    { target: "#game-btn", title: "Which game", text: "Automatic records the game you're playing. Pick a game here to always record that one, even in a window, or choose a whole screen at the top of the menu: it records all the time, no game needed." },
    { target: '.rail-btn[data-group="settings"]', title: "Settings", text: "Quality, frame rate, games, hotkeys, sound and voice. Every option says what it's best for." },
    { target: "#theme-btn", title: "Light or dark", text: "One click switches the look. More colours are in Settings › Appearance." },
    { target: "#update-btn", title: "Updates", text: "Archy checks for new versions and asks before installing. A dot here means one is ready." },
  ];

  function close(done) {
    $("#welcome")?.remove();
    clearSpot();
    if (done) save({ onboarded: true }, "You're all set");
  }

  function clearSpot() {
    document.querySelectorAll(".spot").forEach((n) => n.remove());
    tour = null;
  }

  // One overlay that persists between steps; only the card inside it swaps (with a slide).
  function card(kids, buttons) {
    const old = $("#welcome");
    const inner = el("div", { class: `welcome-card${old ? " swap" : ""}` }, ...kids, el("div", { class: "welcome-actions" }, buttons));
    if (old) old.replaceChildren(inner);
    else document.body.append(el("div", { class: "welcome", id: "welcome", role: "dialog", "aria-modal": "true", "aria-label": "Welcome to Archy" }, inner));
    inner.querySelector(".welcome-actions .primary")?.focus();
  }

  function dots(n, of) {
    return el("div", { class: "welcome-dots", "aria-hidden": "true" }, Array.from({ length: of }, (_, i) => el("i", { class: i === n ? "on" : "" })));
  }

  // ---- little looping demos (CSS-only; same 6 s cycle so the key press, clip and chip line up) ----
  const bars = (n) => Array.from({ length: n }, (_, i) => el("i", { style: `height:${24 + Math.round(Math.abs(Math.sin(i * 1.7) * Math.cos(i * 0.6)) * 62)}%` }));
  function bufferDemo(saved) {
    return el("div", { class: "demo", "aria-hidden": "true" },
      el("div", { class: "demo-bars" }, ...bars(36), ...bars(36)),
      el("span", { class: "demo-rec" }, el("i"), "Recording"),
      saved ? el("div", { class: "demo-clip" }) : null,
      saved ? el("div", { class: "demo-chip", text: "Clip saved" }) : el("span", { class: "demo-len", text: "the last minute, always" }));
  }
  function waveDemo(phrase) {
    return el("div", { class: "demo demo-voice", "aria-hidden": "true" },
      el("div", { class: "demo-wave" }, Array.from({ length: 28 }, (_, i) => el("i", { style: `animation-delay:${(i * 0.09).toFixed(2)}s` }))),
      el("span", { class: "demo-say", text: `“${phrase}”` }),
      el("div", { class: "demo-chip", text: "Clip saved" }));
  }

  function show() {
    const phrase = settings.voice_phrases[0]?.text || "Archer clip that";
    const next = () => { step++; show(); };
    if (step === 0) {
      card([
        el("img", { class: "welcome-logo", src: "logo.svg", alt: "" }),
        el("h1", { text: "Welcome to Archy" }),
        el("p", { class: "sub", text: "Archy quietly keeps the last minute of your game in memory. When something great happens, save it, and it's a clip. It barely touches your FPS, and nothing ever leaves your PC." }),
        bufferDemo(false),
        dots(0, 4),
      ], [el("button", { class: "btn", text: "Skip", onclick: () => close(true) }), el("button", { class: "btn primary", text: "Next", onclick: next })]);
    } else if (step === 1) {
      card([
        el("h2", { text: "Saving a clip" }),
        bufferDemo(true),
        el("div", { class: "welcome-keys" },
          el("div", {}, el("kbd", { class: "press", text: settings.hotkey_save }), el("span", { text: `the last ${settings.clip_seconds} seconds` })),
          el("div", {}, el("kbd", { text: settings.hotkey_save_long }), el("span", { text: `the last ${settings.long_clip_seconds} seconds` })),
          el("div", {}, el("kbd", { text: `“${phrase}”` }), el("span", { text: "say it, once voice clipping is set up" }))),
        el("p", { class: "sub", text: "Recording starts by itself when a game is open, and stops when it closes." }),
        dots(1, 4),
      ], [el("button", { class: "btn", text: "Back", onclick: () => { step--; show(); } }), el("button", { class: "btn primary", text: "Next", onclick: next })]);
    } else if (step === 2) {
      card([
        el("h2", { text: "Set up voice clipping first" }),
        waveDemo(phrase),
        el("p", { class: "sub", text: `Say “${phrase}” instead of pressing a key. About a minute: hold a button and say it five times, then read two short sentences. Archy learns your voice and mic, entirely on this PC.` }),
        dots(2, 4),
      ], [
        el("button", { class: "btn", text: "Back", onclick: () => { step--; show(); } }),
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

  // ---- the spotlight tour: one ring + one tip that glide from stop to stop ----
  function startTour(i = 0, dir = 1) {
    $("#welcome")?.remove();
    while (i >= 0 && i < TOUR.length && !document.querySelector(TOUR[i].target)) i += dir;
    if (i >= TOUR.length) { finish(); return; }
    if (i < 0) { clearSpot(); step = 2; show(); return; }
    const t = TOUR[i];
    const target = document.querySelector(t.target);
    const r = target.getBoundingClientRect();
    if (!tour?.tip.isConnected) {
      tour = { shade: el("div", { class: "spot spot-shade", "aria-hidden": "true" }), ring: el("div", { class: "spot spot-ring", "aria-hidden": "true" }), tip: el("div", { class: "spot spot-tip", role: "dialog" }), i };
      document.body.append(tour.shade, tour.ring, tour.tip);
    }
    const first = tour.i === i && !tour.tip.firstChild;
    tour.i = i;
    Object.assign(tour.ring.style, { left: `${r.left - 6}px`, top: `${r.top - 6}px`, width: `${r.width + 12}px`, height: `${r.height + 12}px` });
    tour.ring.classList.remove("pulse"); void tour.ring.offsetWidth; tour.ring.classList.add("pulse");
    tour.tip.setAttribute("aria-label", t.title);
    const bar = el("i"); bar.style.width = `${((i + 1) / TOUR.length) * 100}%`;
    tour.tip.replaceChildren(el("div", { class: "spot-body" },
      el("div", { class: "spot-prog", "aria-hidden": "true" }, bar),
      el("div", { class: "spot-step", text: `${i + 1} of ${TOUR.length}` }),
      el("h3", { text: t.title }),
      el("p", { class: "sub", text: t.text }),
      el("div", { class: "welcome-actions" },
        el("button", { class: "btn", text: "Skip tour", onclick: () => finish() }),
        el("button", { class: "btn", text: "Back", onclick: () => startTour(i - 1, -1) }),
        el("button", { class: "btn primary", text: i === TOUR.length - 1 ? "Done" : "Next", onclick: () => startTour(i + 1) }))));
    // Beside the target, kept on screen (measured after the text is in).
    const w = 320, h = tour.tip.offsetHeight;
    const left = Math.min(innerWidth - w - 12, Math.max(12, r.right + 18 > innerWidth - w ? r.left - w - 18 : r.right + 18));
    const top = Math.min(innerHeight - h - 12, Math.max(12, r.top - 10));
    if (first) tour.tip.style.transition = "none";
    Object.assign(tour.tip.style, { left: `${left}px`, top: `${top}px` });
    if (first) { void tour.tip.offsetWidth; tour.tip.style.transition = ""; }
    tour.tip.querySelector(".primary").focus();
  }

  function finish() {
    clearSpot();
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
  window.startWelcome = () => { step = 0; paused = false; clearSpot(); show(); };
  // Whichever comes second (this file or the settings) opens it, once.
  let offered = false;
  window.maybeWelcome = () => {
    if (offered || !settings || settings.onboarded) return;
    offered = true;
    window.startWelcome();
  };
  window.maybeWelcome();
  document.addEventListener("keydown", (e) => {
    if (tour?.tip.isConnected) {
      if (e.key === "Escape") finish();
      else if (e.key === "ArrowRight" || e.key === "Enter") { e.preventDefault(); startTour(tour.i + 1); }
      else if (e.key === "ArrowLeft") startTour(tour.i - 1, -1);
    } else if (e.key === "Escape" && $("#welcome") && !paused) close(true);
  });
  let rt = 0;
  addEventListener("resize", () => { if (tour?.tip.isConnected) { clearTimeout(rt); rt = setTimeout(() => startTour(tour.i), 120); } });
})();
