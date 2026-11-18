//! Session + audio/video sync test. Records the primary monitor with system audio,
//! shows tools/sync-test/index.html (flash + tone every second) fullscreen, presses
//! "start session" PRE seconds in, records RUN seconds, stops, then checks the file:
//! length ~ RUN (no pre-roll), video ~ audio length, flash/tone offset (analyze.py).
//! cargo run --release -p archy-core --example session_test
//! Env: ARCHY_TEST_PRE (6), ARCHY_TEST_RUN (15), ARCHY_BROWSER (chrome path),
//! ARCHY_TEST_TARGET = window (default) | monitor | duplicate (the window is stretched over its
//! monitor for the last two), ARCHY_TEST_STILL=1 (page stops changing after 3 s: a static window).
//! Also saves a 10 s clip at the end and checks it the same way.
use archy_audio::Source;
use archy_core::recorder::AudioSource;
use archy_core::{Codec, ConvertMode, Recorder, RecorderConfig, Target};
use std::process::Command;
use std::time::Duration;

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn probe(file: &std::path::Path, stream: &str) -> f64 {
    let o = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", stream, "-show_entries", "stream=duration", "-of", "csv=p=0"])
        .arg(file)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    o.parse().unwrap_or(-1.0)
}

fn main() {
    let still = std::env::var_os("ARCHY_TEST_STILL").is_some();
    let mode = std::env::var("ARCHY_TEST_TARGET").unwrap_or("window".into());
    let (pre, run) = (env("ARCHY_TEST_PRE", 6), env("ARCHY_TEST_RUN", 15));
    archy_core::init_thread().unwrap();
    let page = std::fs::canonicalize("tools/sync-test/index.html").expect("run from the repo root");
    let browser = std::env::var("ARCHY_BROWSER").unwrap_or(r"C:\Program Files\Google\Chrome\Application\chrome.exe".into());
    let mut b = Command::new(browser)
        .args(["--window-size=1280,720", "--autoplay-policy=no-user-gesture-required", "--disable-features=CalculateNativeWinOcclusion", "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding", "--disable-gpu"])
        .arg(format!("--user-data-dir={}", std::env::temp_dir().join("archy-sync-profile").display()))
        .arg(format!("--app=file:///{}#{}", page.display().to_string().trim_start_matches(r"\\?\").replace('\\', "/"), if still { "auto,still" } else { "auto" }))
        .spawn()
        .expect("browser");
    let mut hwnd = None;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(250));
        if let Some((h, _)) = archy_capture::find_window("Archy sync test") {
            hwnd = Some(h);
            break;
        }
    }
    let hwnd = hwnd.expect("sync test window didn't open");
    unsafe {
        // Chrome stops drawing a covered window, so bring it to the front.
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(hwnd, Some(windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST), 0, 0, 0, 0,
            windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE);
        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
    }
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    let mon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if mode != "window" {
        unsafe {
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            let r = mi.rcMonitor;
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(hwnd, Some(windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST), r.left, r.top, r.right - r.left, r.bottom - r.top, windows::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW);
        }
    }
    std::thread::sleep(Duration::from_secs(2));
    let target = match mode.as_str() {
        "monitor" => Target::Monitor(mon),
        "duplicate" => Target::Duplicate(mon, false),
        _ => Target::Window(hwnd),
    };
    println!("target: {mode}{}", if still { " (still page)" } else { "" });

    let cfg = RecorderConfig {
        target,
        fps: 60,
        max_height: 720,
        codecs: vec![Codec::H264],
        max_bitrate: 20_000_000,
        target_bitrate: Some(10_000_000),
        quality: 70,
        buffer_seconds: 30,
        convert: ConvertMode::VideoEngine,
        gpu: String::new(),
        audio: vec![AudioSource { name: "Everything".into(), source: Source::System { device: None }, volumes: Default::default() }],
        audio_kbps: 160,
        avoid: Vec::new(),
        constant_rate: false,
        gpu_priority: 0,
    };
    let rec = Recorder::start(cfg).unwrap();
    println!("encoder: {} | audio: {:?}", rec.info.encoder, rec.audio_report);
    std::thread::sleep(Duration::from_secs(pre));
    let out = std::env::temp_dir().join("archy-session-test.mp4");
    let _ = std::fs::remove_file(&out);
    let t0 = std::time::Instant::now();
    rec.start_session(&out);
    std::thread::sleep(Duration::from_secs(run));
    let clip = std::env::temp_dir().join("archy-clip-test.mp4");
    let _ = std::fs::remove_file(&clip);
    let saved = rec.save_clip(10, &clip);
    let real = t0.elapsed().as_secs_f64();
    let sum = rec.end_session();
    let _ = b.kill();
    println!("summary: {sum:?}");
    let (v, a) = (probe(&out, "v:0"), probe(&out, "a:0"));
    println!("real session time {real:.1} s | file video {v:.2} s | audio {a:.2} s");
    let mut ok = true;
    if (v - real).abs() > 1.5 {
        println!("FAIL: video length is off from real time by {:+.1} s (pre-roll or loss)", v - real);
        ok = false;
    }
    if a > 0.0 && (v - a).abs() > 0.5 {
        println!("FAIL: video and audio lengths differ by {:+.2} s", v - a);
        ok = false;
    }
    match saved {
        Ok(_) => {
            let (cv, ca) = (probe(&clip, "v:0"), probe(&clip, "a:0"));
            println!("clip: asked 10 s, got video {cv:.2} s audio {ca:.2} s");
            if cv < 8.5 || cv > 13.2 || (ca > 0.0 && (cv - ca).abs() > 0.5) {
                println!("FAIL: clip length/audio mismatch");
                ok = false;
            }
        }
        Err(e) => {
            println!("FAIL: clip save failed: {}", e.message());
            ok = false;
        }
    }
    if !still {
        for f in [&out, &clip] {
            println!("-- sync of {}", f.display());
            if let Ok(o) = Command::new("python").args(["tools/sync-test/analyze.py"]).arg(f).arg("0").output() {
                let t = String::from_utf8_lossy(&o.stdout);
                let lines: Vec<&str> = t.lines().collect();
                for l in lines.iter().rev().take(4).rev() {
                    println!("{l}");
                }
                if !t.contains("MEDIAN") {
                    println!("FAIL: no sync measurement");
                    ok = false;
                } else if let Some(ms) = t.lines().find(|l| l.contains("MEDIAN")).and_then(|l| l.split_whitespace().find(|w| w.starts_with('+') || w.starts_with('-'))).and_then(|w| w.parse::<f64>().ok()) {
                    if ms.abs() > 80.0 {
                        println!("FAIL: A/V offset {ms} ms");
                        ok = false;
                    }
                }
            }
        }
    }
    println!("{}", if ok { "PASS" } else { "FAIL" });
    std::process::exit(!ok as i32);
}
