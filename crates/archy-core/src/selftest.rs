//! Recording self-test: records a window that shows its own frame number as
//! black/white squares (top AND bottom rows), pure red/green/blue patches and
//! moving bars, then decodes every frame of the clip and checks for tearing
//! (top and bottom numbers differ), smudging (impure squares), frames going
//! backwards, wrong colours, an uneven frame rate and skipped frames, and
//! measures the recorder's CPU. Run from the Archy window ("Check what this PC
//! can record", `archy.exe --self-test`) and from `examples/quality_test`.
//!
//! The window presents with a flip-model swap chain, which Windows always shows
//! whole, so any tear found is the recorder's.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{Codec, ConvertMode, Recorder, RecorderConfig, Target};
use serde::Serialize;
use windows::core::{w, Interface};
use windows::Win32::Foundation::{FILETIME, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory2, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread, GetProcessTimes, GetThreadTimes};
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Options {
    pub fps: u32,
    pub seconds: u32,
    pub mbps: u32,
    /// ffmpeg.exe (ffprobe.exe must sit next to it).
    pub ffmpeg: PathBuf,
}

#[derive(Serialize, Debug, Default)]
pub struct Report {
    pub fps: u32,
    pub encoder: String,
    pub gpu: String,
    /// GPU driver version and Windows build, so a report says where it ran.
    pub driver: String,
    pub windows: String,
    /// How fast the test window itself was drawn (a busy PC draws it slower).
    pub window_fps: f64,
    pub sent: u64,
    /// Frames the encoder couldn't take in time.
    pub skipped: u64,
    /// Recorder CPU, % of one core.
    pub cpu_core_pct: f64,
    pub frames: u32,
    pub tears: u32,
    pub smudges: u32,
    pub backwards: u32,
    pub wrong_colours: u32,
    pub repeats: u32,
    /// Frames not exactly one frame apart (an uneven frame rate).
    pub uneven: u32,
    pub decode_errors: String,
    /// Clean picture: no tearing, smudging, going backwards or wrong colours.
    pub clean: bool,
    /// Clean AND smooth: an even frame rate and almost nothing skipped.
    pub pass: bool,
    /// Video held in the replay buffer just before saving (MB).
    pub buffer_mb: f64,
    /// The whole process's private memory just before saving (MB).
    pub process_mb: f64,
    /// Length of the saved clip and how long saving it took.
    pub clip_seconds: f64,
    pub save_ms: u64,
    /// With ARCHY_TEST_SESSION: the session file's length, size, uneven frames,
    /// bookmarks and any problem ("" when fine).
    pub session_seconds: f64,
    pub session_mb: f64,
    pub session_uneven: u32,
    pub session_bookmarks: usize,
    pub session_problem: String,
}

const W: u32 = 1280;
const H: u32 = 720;
const BITS: u32 = 16;
const SQ: u32 = 40;
/// Where the code squares and colour patches are drawn (in the 1280x720 source).
fn square_x(bit: u32) -> u32 {
    40 + bit * 56
}
const TOP_Y: u32 = 20;
const BOTTOM_Y: u32 = H - 20 - SQ;
const PATCH_Y: u32 = 300;
const PATCHES: [(u32, [u8; 3]); 3] = [(40, [255, 0, 0]), (160, [0, 255, 0]), (280, [0, 0, 255])];

/// Draws frame `n` into a BGRA buffer.
fn draw(n: u32, buf: &mut [u8]) {
    let shift = (n * 9) % 128;
    // Moving bars everywhere (motion for the encoder to work on).
    for y in 0..H {
        let row = &mut buf[(y * W * 4) as usize..((y + 1) * W * 4) as usize];
        for x in 0..W {
            let band = ((x + shift + y / 3) / 64) % 4;
            let v = [52u8, 96, 140, 74][band as usize];
            let p = &mut row[(x * 4) as usize..(x * 4 + 4) as usize];
            p.copy_from_slice(&[v, v.saturating_add(10), v, 255]);
        }
    }
    let mut rect = |x0: u32, y0: u32, w: u32, h: u32, bgr: [u8; 3]| {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let i = ((y * W + x) * 4) as usize;
                buf[i..i + 4].copy_from_slice(&[bgr[0], bgr[1], bgr[2], 255]);
            }
        }
    };
    for bit in 0..BITS {
        let on = (n >> bit) & 1 == 1;
        let c = if on { [255, 255, 255] } else { [0, 0, 0] };
        // A grey frame around each square keeps its centre away from the moving bars.
        rect(square_x(bit) - 4, TOP_Y - 4, SQ + 8, SQ + 8, [128, 128, 128]);
        rect(square_x(bit) - 4, BOTTOM_Y - 4, SQ + 8, SQ + 8, [128, 128, 128]);
        rect(square_x(bit), TOP_Y, SQ, SQ, c);
        rect(square_x(bit), BOTTOM_Y, SQ, SQ, c);
    }
    for (x, rgb) in PATCHES {
        rect(x, PATCH_Y, 80, 80, [rgb[2], rgb[1], rgb[0]]);
    }
}

extern "system" fn wndproc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, wp, lp) }
}

fn filetime(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

/// CPU time of the whole process, in 100 ns units.
fn process_cpu() -> u64 {
    let (mut a, mut b, mut k, mut u) = Default::default();
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u);
    }
    filetime(k) + filetime(u)
}

fn thread_cpu() -> u64 {
    let (mut a, mut b, mut k, mut u) = Default::default();
    unsafe {
        let _ = GetThreadTimes(GetCurrentThread(), &mut a, &mut b, &mut k, &mut u);
    }
    filetime(k) + filetime(u)
}

pub fn run(opts: &Options) -> Result<Report, String> {
    crate::init_thread().map_err(|e| e.message())?;
    let fps = opts.fps;
    let secs = opts.seconds.max(3);

    // ---- the test window ----
    let instance = unsafe { GetModuleHandleW(None) }.map_err(|e| e.message())?;
    let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: w!("ArchyQualityTest"), ..Default::default() };
    unsafe { RegisterClassW(&class) };
    let hwnd = unsafe {
        CreateWindowExW(WINDOW_EX_STYLE::default(), w!("ArchyQualityTest"), w!("Archy quality test"), WS_POPUP | WS_VISIBLE,
            80, 80, W as i32, H as i32, None, None, Some(instance.into()), None)
    }
    .map_err(|e| e.message())?;

    // ---- the renderer: one new picture per screen refresh, presented whole ----
    let stop = Arc::new(AtomicBool::new(false));
    let shown = Arc::new(AtomicU64::new(0));
    let render_cpu = Arc::new(AtomicU64::new(0));
    let hwnd_raw = hwnd.0 as isize;
    let renderer = {
        let (stop, shown, render_cpu) = (stop.clone(), shown.clone(), render_cpu.clone());
        std::thread::spawn(move || unsafe {
            crate::eco::boost_current_thread();
            let hwnd = HWND(hwnd_raw as *mut _);
            let gpu = archy_gpu::device::Gpu::new(None, 0).unwrap();
            let factory: IDXGIFactory2 = CreateDXGIFactory1().unwrap();
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: W, Height: H, Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT, BufferCount: 2,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD, ..Default::default()
            };
            let swap = factory.CreateSwapChainForHwnd(&gpu.device, hwnd, &desc, None, None).unwrap();
            let back: ID3D11Texture2D = swap.GetBuffer(0).unwrap();
            let mut buf = vec![0u8; (W * H * 4) as usize];
            let mut n = 0u32;
            while !stop.load(Ordering::Acquire) {
                draw(n, &mut buf);
                gpu.context.UpdateSubresource(&back.cast::<windows::Win32::Graphics::Direct3D11::ID3D11Resource>().unwrap(), 0, None, buf.as_ptr() as *const _, W * 4, 0);
                let _ = swap.Present(1, DXGI_PRESENT(0)); // waits for the next refresh
                n = n.wrapping_add(1) & 0xFFFF;
                shown.store(n as u64, Ordering::Release);
            }
            render_cpu.store(thread_cpu(), Ordering::Release);
        })
    };
    let pump = |d: Duration| {
        let until = Instant::now() + d;
        while Instant::now() < until {
            let mut msg = MSG::default();
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    pump(Duration::from_millis(800));

    // ---- record it ----
    let mbps = opts.mbps;
    let cfg = RecorderConfig {
        target: Target::Window(hwnd),
        fps,
        max_height: 720,
        codecs: vec![match std::env::var("ARCHY_TEST_CODEC").as_deref() {
            Ok("hevc") => Codec::Hevc,
            Ok("av1") => Codec::Av1,
            _ => Codec::H264,
        }],
        max_bitrate: mbps * 2_000_000,
        target_bitrate: Some(mbps * 1_000_000),
        quality: 70,
        // Long tests hold their whole length (to measure long clips' RAM and save time).
        buffer_seconds: secs.max(30),
        convert: ConvertMode::VideoEngine,
        gpu: String::new(),
        audio: Vec::new(),
        audio_kbps: 160,
        avoid: Vec::new(),
        constant_rate: true,
    };
    let cpu0 = process_cpu();
    let wall0 = Instant::now();
    let out = std::env::temp_dir().join(format!("archy-selftest-{fps}.mp4"));
    #[allow(clippy::type_complexity)]
    let recorded = (|| -> Result<(String, String, u64, u64, u64, (usize, u64, u64, f64), Option<crate::session::Summary>), String> {
        let rec = Recorder::start(cfg).map_err(|e| e.message())?;
        // ARCHY_TEST_SESSION=1: also record the whole session to a file (feature test).
        let session_path = std::env::temp_dir().join(format!("archy-selftest-{fps}-session.mp4"));
        let with_session = std::env::var_os("ARCHY_TEST_SESSION").is_some();
        if with_session {
            let _ = std::fs::remove_file(&session_path);
            rec.start_session(&session_path);
        }
        let frames0 = shown.load(Ordering::Acquire);
        pump(Duration::from_secs(secs as u64));
        let rendered = shown.load(Ordering::Acquire).wrapping_sub(frames0) & 0xFFFF;
        if with_session {
            rec.bookmark();
        }
        let ring = rec.buffer_bytes();
        let memory = process_memory();
        let clip = rec.save_clip(secs.saturating_sub(2).max(2), &out).map_err(|e| e.message())?;
        let (sent, skipped) = rec.frame_counts();
        let session = if with_session { rec.end_session() } else { None };
        Ok((rec.info.encoder.clone(), rec.info.gpu.clone(), rendered, sent, skipped, (ring, memory, clip.save_time.as_millis() as u64, clip.seconds), session))
    })();
    stop.store(true, Ordering::Release);
    let joined = renderer.join();
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    joined.map_err(|_| "the test window failed".to_string())?;
    let (encoder, gpu, rendered, sent, skipped, (ring, memory, save_ms, clip_seconds), session) = recorded?;
    let wall = wall0.elapsed().as_secs_f64();
    let recorder_cpu = (process_cpu() - cpu0).saturating_sub(render_cpu.load(Ordering::Acquire)) as f64 / 1e7;

    // ---- decode every frame and check it ----
    let mut child = no_window(std::process::Command::new(&opts.ffmpeg))
        .args(["-v", "error", "-i"]).arg(&out)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
    // Frames are checked one at a time as they stream out (a whole 144 fps clip
    // decoded at once is gigabytes).
    let mut stdout = child.stdout.take().ok_or("no output from ffmpeg")?;
    let stderr = child.stderr.take().ok_or("no output from ffmpeg")?;
    let errors = std::thread::spawn(move || {
        let mut e = String::new();
        let mut stderr = stderr;
        let _ = stderr.read_to_string(&mut e);
        e
    });
    let (fw, fh) = (probe_num(&opts.ffmpeg, &out, "stream=width")?, probe_num(&opts.ffmpeg, &out, "stream=height")?);
    let frame_len = (fw * fh * 3) as usize;
    let sx = fw as f64 / W as f64;
    let sy = fh as f64 / H as f64;
    let avg = |f: &[u8], x0: u32, y0: u32, size: u32| -> [f64; 3] {
        // The middle half of the area, scaled to the clip's size.
        let (cx, cy) = (((x0 + size / 4) as f64 * sx) as u32, ((y0 + size / 4) as f64 * sy) as u32);
        let (cw, ch) = (((size / 2) as f64 * sx).max(1.0) as u32, ((size / 2) as f64 * sy).max(1.0) as u32);
        let mut s = [0f64; 3];
        for y in cy..cy + ch {
            for x in cx..cx + cw {
                let i = ((y * fw + x) * 3) as usize;
                for c in 0..3 {
                    s[c] += f[i + c] as f64;
                }
            }
        }
        let n = (cw * ch) as f64;
        [s[0] / n, s[1] / n, s[2] / n]
    };
    let code = |f: &[u8], y: u32, smudged: &mut u32| -> u32 {
        (0..BITS).fold(0, |acc, bit| {
            let [r, g, b] = avg(f, square_x(bit), y, SQ);
            let l = (r + g + b) / 3.0;
            if l > 70.0 && l < 185.0 {
                *smudged += 1;
            }
            acc | (((l > 128.0) as u32) << bit)
        })
    };
    let (mut frames, mut tears, mut smudges, mut backwards, mut repeats, mut colour_bad) = (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
    let mut prev: Option<u32> = None;
    let mut max_jump = 0u32;
    let mut jump_at = 0u32;
    let mut repeat_runs: Vec<(u32, u32)> = Vec::new(); // (first frame, length)
    let mut f = vec![0u8; frame_len];
    while stdout.read_exact(&mut f).is_ok() {
        let f = &f[..];
        frames += 1;
        let mut smudge = 0;
        let top = code(f, TOP_Y, &mut smudge);
        let bottom = code(f, BOTTOM_Y, &mut smudge);
        if smudge > 0 {
            smudges += 1;
        }
        if top != bottom {
            tears += 1;
        }
        if let Some(p) = prev {
            let step = top.wrapping_sub(p) & 0xFFFF;
            if step == 0 {
                repeats += 1;
                match repeat_runs.last_mut() {
                    Some((start, len)) if *start + *len == frames - 1 => *len += 1,
                    _ => repeat_runs.push((frames - 1, 1)),
                }
            } else if step > 0x8000 {
                backwards += 1;
            } else if step > max_jump {
                max_jump = step;
                jump_at = frames - 1;
            }
        }
        prev = Some(top);
        for (x, want) in PATCHES {
            let got = avg(f, x, PATCH_Y, 80);
            if (0..3).any(|c| (got[c] - want[c] as f64).abs() > 45.0) {
                colour_bad += 1;
                break;
            }
        }
    }
    let _ = child.wait();
    let err = errors.join().unwrap_or_default();
    let uneven = pts_gaps(&opts.ffmpeg, &out, fps);
    if std::env::var_os("ARCHY_SELFTEST_KEEP").is_none() {
        let _ = std::fs::remove_file(&out);
    }
    let _ = (max_jump, jump_at, repeat_runs);
    let decode_errors = err.trim().to_string();
    let clean = decode_errors.is_empty() && frames > 0 && tears == 0 && smudges == 0 && backwards == 0 && colour_bad == 0;
    Ok(Report {
        fps,
        encoder,
        driver: archy_gpu::device::driver_version(&gpu),
        gpu,
        windows: windows_build(),
        window_fps: rendered as f64 / secs as f64,
        sent,
        skipped,
        cpu_core_pct: recorder_cpu / wall * 100.0,
        frames,
        tears,
        smudges,
        backwards,
        wrong_colours: colour_bad,
        repeats,
        uneven,
        decode_errors,
        clean,
        pass: clean && uneven == 0 && skipped * 100 <= (sent + skipped).max(1),
        buffer_mb: ring as f64 / 1e6,
        process_mb: memory as f64 / 1e6,
        clip_seconds,
        save_ms,
        session_seconds: session.as_ref().map_or(0.0, |s| probe_num_f(&opts.ffmpeg, &s.path)),
        session_mb: session.as_ref().map_or(0.0, |s| s.bytes as f64 / 1e6),
        session_uneven: session.as_ref().map_or(0, |s| pts_gaps(&opts.ffmpeg, &s.path, fps)),
        session_bookmarks: session.as_ref().map_or(0, |s| crate::session::read_bookmarks(&s.path).len()),
        session_problem: session.as_ref().and_then(|s| s.problem.clone()).unwrap_or_default(),
    })
}

fn no_window(mut c: std::process::Command) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}

fn probe(ffmpeg: &Path, out: &Path, entries: &str, select: &str) -> String {
    let r = no_window(std::process::Command::new(ffmpeg.with_file_name("ffprobe.exe")))
        .args(["-v", "error", "-select_streams", select, "-show_entries", entries, "-of", "csv=p=0"])
        .arg(out)
        .output();
    r.map(|r| String::from_utf8_lossy(&r.stdout).into_owned()).unwrap_or_default()
}

fn probe_num(ffmpeg: &Path, out: &Path, entry: &str) -> Result<u32, String> {
    probe(ffmpeg, out, entry, "v:0").trim().parse().map_err(|_| "couldn't read the test clip".to_string())
}

/// Frames whose spacing isn't one frame interval (variable frame rate).
fn pts_gaps(ffmpeg: &Path, out: &Path, fps: u32) -> u32 {
    let mut pts: Vec<f64> = probe(ffmpeg, out, "packet=pts_time", "v:0").lines().filter_map(|l| l.trim().parse().ok()).collect();
    pts.sort_by(|a, b| a.total_cmp(b));
    let step = 1.0 / fps as f64;
    pts.windows(2).filter(|w| ((w[1] - w[0]) - step).abs() > step * 0.05).count() as u32
}

/// "Windows 11 24H2 (26100.4061)", from the registry.
pub fn windows_build() -> String {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    let key = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let text = |name: windows::core::PCWSTR| -> String {
        let mut buf = [0u16; 128];
        let mut len = (buf.len() * 2) as u32;
        let ok = unsafe {
            RegGetValueW(HKEY_LOCAL_MACHINE, key, name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut len))
        };
        if ok.is_err() {
            return String::new();
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        String::from_utf16_lossy(&buf[..n])
    };
    let build = text(w!("CurrentBuildNumber"));
    let release = text(w!("DisplayVersion"));
    let mut ubr = 0u32;
    let mut len = 4u32;
    unsafe {
        let _ = RegGetValueW(HKEY_LOCAL_MACHINE, key, w!("UBR"), RRF_RT_REG_DWORD, None, Some(&mut ubr as *mut u32 as *mut _), Some(&mut len));
    }
    let major = if build.parse::<u32>().unwrap_or(0) >= 22000 { "Windows 11" } else { "Windows 10" };
    format!("{major} {release} ({build}.{ubr})")
}

/// This process's private memory in bytes.
fn process_memory() -> u64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    if ok.is_ok() { c.PrivateUsage as u64 } else { 0 }
}

/// A file's duration in seconds (0 if unknown).
fn probe_num_f(ffmpeg: &Path, out: &Path) -> f64 {
    probe(ffmpeg, out, "format=duration", "v:0").trim().parse().unwrap_or(0.0)
}
