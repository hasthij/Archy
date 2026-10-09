# Archy — Plan

Archy is a local-first game clipper for Windows. The whole point: **best-looking clips with
near-zero FPS impact, including on integrated GPUs.** No account, no servers, no uploads.

## Locked decisions (v1)

| Topic | Decision |
|---|---|
| Language | Rust |
| Minimum OS | Windows 10 2004+ (per-app audio). Capture rate limiter uses Win11 24H2 API when present, self-paced otherwise |
| Capture | Windows.Graphics.Capture (WGC), window **and** monitor. Must work with fullscreen games. **Never** inject into or hook games |
| Hardware target | iGPU is first-class. If it runs great on an Intel/AMD iGPU, it runs great everywhere |
| Benchmark game | Roblox first (Hyperion anti-cheat, often CPU-bound, huge low-end/iGPU player base), then "everything else" |
| Clips | Replay buffer in RAM, 15–60 s, saved by hotkey or voice phrase |
| Voice | Default "Archy, clip that" (30 s), "Archy, clip the last minute" (60 s). Phrases + per-phrase length are user-editable. On-device only |
| Hotkeys | All configurable. Defaults F8 / Shift+F8 / Ctrl+F8 |
| Editor | Trim only (keyframe-snapped, instant) + "fit to N MB" send copy |
| Settings | Every setting user-adjustable, each with a plain "Best for" explanation (see UI mockups) |
| Not in v1 | Full-session recording, game-event auto-clipping, effects/text/music in editor |

## Performance targets (must be measured, not assumed)

| Metric | Target |
|---|---|
| Avg FPS + 1% lows while recording | within 1–2% of not recording, on iGPU and dGPU |
| Recorder CPU | < 2% of one core at 1080p60 (Roblox is often CPU-bound) |
| Idle RAM (recorder, excl. buffer) | < 50 MB |
| Hotkey → clip file on disk | < 150 ms |
| Voice phrase → clip saved | < 500 ms after phrase ends |
| Disk writes during gameplay | zero (RAM buffer mode) |

## The pipeline (where the FPS is won)

```
Game frame (already on GPU)
  │  WGC frame pool (2 buffers), capture paced to target fps (24–90), never every game frame
  ▼
1 GPU copy into a pre-allocated texture pool, release WGC frame immediately
  ▼
ID3D11VideoProcessor: scale + RGB→NV12 in one pass (video engine, not the game's shader cores)
  ▼
Hardware encoder (Media Foundation MFT; NVENC / AMF / QuickSync) — AQ, lookahead, UHQ OFF
  ▼
Encoded packets → RAM ring buffer (pre-allocated, keyframe every 1 s)
  ▼  hotkey / voice
Remux packets → MP4 (no re-encode) → temp file → atomic rename → clipboard
```

Rules that every change is judged by:
1. Frames never leave the GPU until they are compressed packets.
2. Nothing in the recorder runs on the game's shader cores if a fixed-function block can do it.
3. Recorder never makes the game wait: low GPU priority, no `Flush`/`Map` stalls, drop a frame instead of queueing.
4. No allocations per frame. No disk I/O during gameplay.
5. Adaptive mode: if the game becomes GPU-bound or the encoder queue grows, step down resolution/fps, then recover.

### Fullscreen handling
- Default: window capture of the game. Most modern "fullscreen" is flip-model / fullscreen-optimized and captures fine.
- Detect black or stalled frames (true exclusive fullscreen, mode switches) → automatically fall back to **monitor capture** of the game's display.
- Recreate the capture session + texture pool on resolution/display-mode change (alt-tab, fullscreen toggle) without losing the existing buffer.
- Verify specifically: Roblox windowed, Roblox fullscreen (F11 / in-game toggle), alt-tab in and out.

### iGPU specifics
- Count every full-resolution copy — iGPUs share RAM bandwidth with the CPU and the game. Target: one RGBA copy, then NV12 only.
- Downscale in the video-processor pass, before encode.
- Intel: prefer low-power (VDEnc) encode where the driver exposes it. AMD APU: VCN with pre-analysis off.
- On hybrid laptops, test encoding on the iGPU so the dGPU is untouched by recording.
- First-launch auto-tune picks a tier per machine; user can override everything.

### Encoder backend
- v1: **Media Foundation hardware MFTs** for all vendors (one code path, D3D11 texture input, H.264/HEVC/AV1).
- Later, only if benchmarks show a gap: native NVENC / AMF / oneVPL backends for finer control (e.g. Intel low-power flag).

## Repository layout (planned)

```
crates/
  archy-capture/   WGC window/monitor capture, frame pacing, fullscreen fallback
  archy-gpu/       D3D11 device, texture pool, video processor (scale + NV12)
  archy-encode/    MF hardware encoder wrapper, codec/encoder detection
  archy-buffer/    encoded packet ring buffer (+ audio rings)
  archy-mux/       MP4 remux of packets, atomic save
  archy-audio/     WASAPI per-process loopback + mic tracks
  archy-voice/     VAD + keyword spotting (sherpa-onnx KWS)
  archy-daemon/    the tray recorder process: hotkeys, settings, adaptive mode
  archy-bench/     benchmark harness (PresentMon on/off comparisons)
app/               Tauri UI (library, trim, settings) — only runs when opened
```

## Milestones

1. **Probe** — list GPUs, which one drives which display, and which hardware encoders (H.264/HEVC/AV1) each exposes. Decides tiers and the iGPU-vs-dGPU choice.
2. **Capture → encode → file** — WGC (window + monitor) → video processor → MF encoder → raw stream to disk. Measure Roblox FPS on/off on an iGPU machine.
3. **Replay buffer + save** — RAM ring, hotkey, remux to MP4, < 150 ms save.
4. **Fullscreen + resilience** — black-frame detection, monitor fallback, mode-change recovery.
5. **Adaptive mode + auto-tune.**
6. **Audio tracks.**
7. **Voice clipping.**
8. **Tauri UI** — from the approved mockups.
9. **Send copies** — fit-to-size re-encode on the GPU, clipboard, drag-out.

Every milestone from 2 on ends with a benchmark: same Roblox scene, recording off vs on, avg FPS and 1% lows, on at least one iGPU and one dGPU.
