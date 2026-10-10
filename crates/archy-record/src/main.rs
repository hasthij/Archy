//! `archy-record`: milestone-2 test tool. Captures a window or monitor for N
//! seconds through the full GPU pipeline (WGC → video processor → hardware
//! encoder → MP4) and reports what it cost.
//!
//!   archy-record --window Roblox --seconds 20 --fps 60 --codec av1 --out test.mp4
//!   archy-record --monitor --seconds 10

mod threads;

use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use archy_capture::{find_window, primary_monitor, Capture, Target};
use archy_encode::{Backend, Config, Encoder, Input};
use archy_gpu::convert::{fit_size, AnyConverter, ConvertMode};
use archy_gpu::device::Gpu;
use archy_gpu::probe::Codec;
use archy_gpu::SendPtr;
use archy_mux::Mp4Writer;
use windows::core::Result;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Media::MediaFoundation::{MFShutdown, MFStartup, MFSTARTUP_FULL, MF_VERSION};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, GetProcessTimes, GetThreadTimes, SetThreadInformation,
    ThreadPowerThrottling, THREAD_POWER_THROTTLING_CURRENT_VERSION,
    THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

/// NV12 textures in rotation: channel depth + frames the encoder may still hold + one being written.
const NV12_POOL: usize = 8;
/// Frames allowed to wait for the encoder. Beyond this we drop instead of queueing.
const QUEUE_DEPTH: usize = 3;

struct Args {
    window: Option<String>,
    seconds: u32,
    fps: u32,
    height: u32,
    codec: Option<Codec>,
    mbps: u32,
    /// Average bitrate to aim for (Mbps); `None` = constant quality.
    target_mbps: Option<f32>,
    quality: u32,
    out: PathBuf,
    /// Profiling: run capture + convert but never hand frames to the encoder.
    convert_only: bool,
    convert: ConvertMode,
    /// Profiling: encode but throw packets away (no MP4 writing).
    discard: bool,
    gpu_priority: i32,
    eco: bool,
    backend: Option<Backend>,
}

fn parse_args() -> std::result::Result<Args, String> {
    let mut a = Args {
        window: None,
        seconds: 10,
        fps: 60,
        height: 1080,
        codec: None,
        mbps: 40,
        target_mbps: None,
        quality: 70,
        out: PathBuf::from("archy-test.mp4"),
        convert_only: false,
        convert: ConvertMode::VideoEngine,
        discard: false,
        gpu_priority: archy_gpu::device::RECORDER_GPU_PRIORITY,
        eco: false,
        backend: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().ok_or(format!("{flag} needs a value"));
        let num = |s: String| s.parse::<u32>().map_err(|_| format!("bad number: {s}"));
        match flag.as_str() {
            "--window" => a.window = Some(val()?),
            "--monitor" => a.window = None,
            "--seconds" => a.seconds = num(val()?)?,
            "--fps" => a.fps = num(val()?)?.clamp(24, 90),
            "--height" => a.height = num(val()?)?,
            "--mbps" => a.mbps = num(val()?)?,
            "--quality" => a.quality = num(val()?)?.min(100),
            "--out" => a.out = PathBuf::from(val()?),
            // Resolution + bitrate presets: predictable size, ~MB per 30 s in comments.
            "--preset" => {
                let (height, mbps) = match val()?.to_lowercase().as_str() {
                    "360p" => (360, 1.5),    // ~6 MB
                    "480p" => (480, 2.5),    // ~9 MB, fits Discord's 10 MB
                    "720p" => (720, 5.0),    // ~19 MB
                    "1080p" => (1080, 10.0), // ~38 MB
                    "1440p" => (1440, 18.0), // ~68 MB
                    other => return Err(format!("unknown preset {other}")),
                };
                a.height = height;
                a.target_mbps = Some(mbps);
                a.mbps = (mbps * 1.5).ceil() as u32;
            }
            "--target-mbps" => {
                let v = val()?;
                a.target_mbps = Some(v.parse().map_err(|_| format!("bad number: {v}"))?);
            }
            "--convert-only" => a.convert_only = true,
            "--discard" => a.discard = true,
            "--eco" => a.eco = true,
            "--encoder" => {
                a.backend = Some(match val()?.to_lowercase().as_str() {
                    "vpl" | "qsv" => Backend::Vpl,
                    "mf" => Backend::MediaFoundation,
                    other => return Err(format!("unknown encoder {other}")),
                })
            }
            "--gpu-priority" => {
                let v = val()?;
                a.gpu_priority = v
                    .parse::<i32>()
                    .map_err(|_| format!("bad number: {v}"))?
                    .clamp(-7, 7)
            }
            "--convert" => {
                a.convert = match val()?.to_lowercase().as_str() {
                    "video" | "vp" => ConvertMode::VideoEngine,
                    "shader" => ConvertMode::Shader,
                    other => return Err(format!("unknown convert mode {other}")),
                }
            }
            "--codec" => {
                a.codec = Some(match val()?.to_lowercase().as_str() {
                    "av1" => Codec::Av1,
                    "hevc" | "h265" => Codec::Hevc,
                    "h264" | "avc" => Codec::H264,
                    other => return Err(format!("unknown codec {other}")),
                })
            }
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(a)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("{msg}");
            }
            eprintln!("usage: archy-record [--window TITLE | --monitor] [--seconds N] [--fps 24-90] [--height PX] [--codec av1|hevc|h264] [--mbps N] [--quality 0-100] [--out FILE] [--preset 360p|480p|720p|1080p|1440p] [--target-mbps N] [--convert video|shader] [--convert-only] [--discard] [--eco] [--gpu-priority -7..7] [--encoder vpl|mf]");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// EcoQoS for the calling thread: on hybrid CPUs (Intel 12th gen+) Windows
/// prefers efficiency cores for it, leaving performance cores to the game.
fn efficiency_mode() {
    let state = THREAD_POWER_THROTTLING_STATE {
        Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
    };
    unsafe {
        let _ = SetThreadInformation(
            GetCurrentThread(),
            ThreadPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        );
    }
}

/// Labels an error with the pipeline step that produced it.
fn step<T>(what: &str, r: Result<T>) -> Result<T> {
    r.map_err(|e| windows::core::Error::new(e.code(), format!("{what}: {}", e.message())))
}

fn ticks(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// Kernel + user CPU time of the whole process.
fn cpu_time() -> Duration {
    let (mut c, mut e, mut k, mut u) = Default::default();
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    Duration::from_nanos((ticks(k) + ticks(u)) * 100)
}

/// Kernel + user CPU time of the calling thread.
fn thread_cpu_time() -> Duration {
    let (mut c, mut e, mut k, mut u) = Default::default();
    unsafe {
        let _ = GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u);
    }
    Duration::from_nanos((ticks(k) + ticks(u)) * 100)
}

fn run(args: Args) -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }

    let target = match &args.window {
        Some(title) => match find_window(title) {
            Some((hwnd, name)) => {
                println!("Capturing window: {name}");
                Target::Window(hwnd)
            }
            None => {
                eprintln!("No visible window with \"{title}\" in its title.");
                std::process::exit(1);
            }
        },
        None => {
            println!("Capturing primary monitor");
            Target::Monitor(primary_monitor())
        }
    };

    let gpu = step("create GPU device", Gpu::new(None, args.gpu_priority))?;
    if args.eco {
        efficiency_mode();
    }
    let interval = Duration::from_secs_f64(1.0 / args.fps as f64);
    let capture = step(
        "start capture",
        // Half the interval: the compositor wakes us at most twice per wanted frame,
        // and frames arriving a hair early are never pushed to the next refresh.
        Capture::start(&gpu.device, target, interval / 2),
    )?;
    let (out_w, out_h) = fit_size(capture.width, capture.height, args.height);
    let mut converter = step(
        "create video processor",
        AnyConverter::new(
            &gpu,
            args.convert,
            (capture.width, capture.height),
            (out_w, out_h),
            args.fps,
            NV12_POOL,
        ),
    )?;

    let codecs: Vec<Codec> = match args.codec {
        Some(c) => vec![c],
        None => Codec::ALL.to_vec(),
    };
    // oneVPL first (Intel, far less CPU), Media Foundation for everything else.
    let backends: Vec<Backend> = match args.backend {
        Some(b) => vec![b],
        None => vec![Backend::Vpl, Backend::MediaFoundation],
    };
    let mut encoder = None;
    'search: for codec in codecs {
        for &backend in &backends {
            let cfg = Config {
                codec,
                width: out_w,
                height: out_h,
                fps: args.fps,
                max_bitrate: args.mbps * 1_000_000,
                quality: args.quality,
                target_bitrate: args.target_mbps.map(|m| (m * 1_000_000.0) as u32),
                gop: args.fps,
            };
            match Encoder::new(backend, &gpu, &cfg) {
                Ok(e) => {
                    println!(
                        "Encoder: {} ({codec}, {}), {}x{} @ {} fps, convert: {:?}",
                        e.name(),
                        e.rate_control(),
                        out_w,
                        out_h,
                        args.fps,
                        args.convert
                    );
                    encoder = Some((e, codec));
                    break 'search;
                }
                Err(e) => println!("  {codec} via {backend:?} unavailable: {}", e.message()),
            }
        }
    }
    let Some((encoder, codec)) = encoder else {
        eprintln!("No hardware encoder could be started.");
        std::process::exit(1);
    };

    let (tx, rx) = sync_channel::<SendPtr<Input>>(QUEUE_DEPTH);
    let out_path = args.out.clone();
    let discard = args.discard;
    let fps = args.fps;
    let eco = args.eco;
    let enc_thread = thread::spawn(move || -> Result<(archy_encode::Stats, Duration)> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        if eco {
            efficiency_mode();
        }
        let cpu0 = thread_cpu_time();
        let mut encoder = encoder;
        let mut writer: Option<Mp4Writer> = None;
        let stats = step(
            "encode",
            encoder.run(rx, |p| {
                if discard {
                    return Ok(());
                }
                if writer.is_none() {
                    if !p.keyframe {
                        return Ok(()); // a file must start on a keyframe
                    }
                    writer = Some(step(
                        "create mp4",
                        Mp4Writer::create(&out_path, codec, out_w, out_h, fps, p.data),
                    )?);
                }
                let w = writer.as_mut().expect("writer just created");
                step("write mp4", w.write(p.data, p.time, p.duration, p.keyframe))
            }),
        )?;
        if let Some(w) = writer {
            step("finish mp4", w.finish())?;
        }
        Ok((stats, thread_cpu_time() - cpu0))
    });

    let duration = Duration::from_secs(args.seconds as u64);
    // Cadence: frames are due on a fixed grid (one per interval). Each due slot takes
    // the first frame within half an interval of it, so a 90 or 144 Hz game averages
    // exactly the target rate; after a short hiccup it catches up. After a real pause
    // (static screen) the grid restarts instead of bursting to catch up.
    const GRID_RESET: i64 = 2_500_000; // 250 ms
    let frame_interval = 10_000_000 / args.fps as i64;
    let tolerance = frame_interval / 2;
    let mut next_due = i64::MIN / 2;
    let mut cadence_trace = std::env::var_os("ARCHY_CADENCE_TRACE").map(|_| Vec::<i64>::new());
    let (mut taken, mut thinned, mut dropped) = (0u64, 0u64, 0u64);
    let (mut first_drop, mut last_drop) = (None, 0u64);
    let mut convert_time = Duration::ZERO;
    let mut first_time: Option<i64> = None;
    let mut last_taken = 0i64;
    let cpu_start = cpu_time();
    let threads_before = threads::snapshot();
    let main_cpu_start = thread_cpu_time();
    let start = Instant::now();
    println!("Recording {} s...", args.seconds);

    while start.elapsed() < duration {
        // Sleeps until the compositor has a new frame. A static screen produces
        // none, so idle moments cost nothing (MP4 timestamps keep timing right).
        let Some(frame) = step("get frame", capture.next(Duration::from_millis(100)))? else {
            continue;
        };
        if let Some(gaps) = cadence_trace.as_mut() {
            gaps.push(frame.time);
        }
        if frame.time < next_due - tolerance {
            thinned += 1;
            continue;
        }
        // Restart the grid only after a real pause (static screen); restarting on
        // the 20-30 ms delivery hiccups the compositor has cost ~6% of frames.
        next_due = if frame.time > next_due + GRID_RESET {
            frame.time + frame_interval
        } else {
            next_due + frame_interval
        };
        let time = frame.time - *first_time.get_or_insert(frame.time);
        last_taken = time;
        let t1 = Instant::now();
        let texture = step("convert frame", converter.convert(&frame.texture))?;
        drop(frame); // hand the buffer back to the compositor right away
        convert_time += t1.elapsed();
        taken += 1;
        if args.convert_only {
            continue;
        }
        match tx.try_send(SendPtr(Input { texture, time })) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                dropped += 1;
                first_drop.get_or_insert(taken);
                last_drop = taken;
            }
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
    let wall = start.elapsed();
    if let Some(times) = &cadence_trace {
        let mut hist = std::collections::BTreeMap::<i64, u32>::new();
        for w in times.windows(2) {
            *hist.entry((w[1] - w[0]) / 10_000).or_default() += 1; // ms buckets
        }
        eprintln!("  delivered frame gaps (ms: count): {hist:?}");
    }
    let main_cpu = thread_cpu_time() - main_cpu_start;
    // Before the encoder thread exits, so its time is still attributable.
    let threads_after = threads::snapshot();
    drop(tx);
    let (stats, enc_cpu) = enc_thread.join().expect("encoder thread panicked")?;
    let cpu = cpu_time() - cpu_start;
    drop(capture);
    unsafe {
        let _ = MFShutdown();
    }

    let secs = wall.as_secs_f64();
    println!();
    println!("Wrote {}", args.out.display());
    println!(
        "  Frames: {taken} captured ({:.1} fps while the game was drawing), {thinned} above target rate skipped, {dropped} dropped (encoder busy)",
        (taken.saturating_sub(1)) as f64 / (last_taken.max(1) as f64 / 1e7)
    );
    if let Some(first) = first_drop {
        println!("    drops between frame {first} and {last_drop}");
    }
    println!(
        "  Encoded: {} frames -> {} packets, {} keyframes",
        stats.frames_in, stats.packets, stats.keyframes
    );
    println!(
        "  Size: {:.1} MB, avg {:.1} Mbps",
        stats.bytes as f64 / 1e6,
        stats.bytes as f64 * 8.0 / secs / 1e6
    );
    println!(
        "  Recorder CPU: {:.1}% of one core, {:.2} ms per frame  (convert submit {:.2} ms)",
        cpu.as_secs_f64() / secs * 100.0,
        cpu.as_secs_f64() * 1000.0 / taken.max(1) as f64,
        convert_time.as_secs_f64() * 1000.0 / taken.max(1) as f64
    );
    let pct = |d: Duration| d.as_secs_f64() / secs * 100.0;
    println!(
        "    capture thread {:.1}%, encoder loop {:.1}%, driver/OS threads {:.1}%",
        pct(main_cpu),
        pct(enc_cpu),
        pct(cpu.saturating_sub(main_cpu + enc_cpu))
    );
    threads::report(&threads_before, &threads_after, wall);
    Ok(())
}
