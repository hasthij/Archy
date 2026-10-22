# Handoff 2026-10-22 (0.5.2, committed, not pushed)
- Mic noise suppression (nnnoiseless RNNoise) + gate slider, both off by default, live (atomics).
- Audio tab: micCleanCard = toggle, gate slider, level meter, Listen to yourself (archy.exe --mic-monitor).
- Perf: 6.35 ms per 1 s audio (~0.6% of one core), zero when off (off_leaves_audio_untouched test).
- Tray: start-up fix + manual 'start in tray' button.
- Tests: archy-audio 3, archy-core 25 green; release build OK; monitor smoke test OK (exits on stdin EOF).
- Next: user tests in target\release\archy-ui.exe; then release when asked.
