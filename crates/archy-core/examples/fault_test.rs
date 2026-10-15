//! Simulated driver failures, to prove Archy survives a misbehaving NVIDIA/AMD
//! encoder without the hardware. Each scenario runs in its own process (a hung
//! thread dies with it) with ARCHY_FAULT set, recording the primary screen.
//! cargo run --release -p archy-core --example fault_test
//! (ARCHY_ENCODER=mf for the NVIDIA/AMD code path; the default here.)

use std::time::{Duration, Instant};

use archy_core::{Codec, ConvertMode, Recorder, RecorderConfig, RecorderStatus, Target};

fn config() -> RecorderConfig {
    RecorderConfig {
        target: Target::Monitor(archy_capture::primary_monitor()),
        fps: 60,
        max_height: 720,
        codecs: vec![Codec::H264],
        max_bitrate: 10_000_000,
        target_bitrate: Some(5_000_000),
        quality: 70,
        buffer_seconds: 30,
        convert: ConvertMode::VideoEngine,
        gpu: String::new(),
        audio: Vec::new(),
        audio_kbps: 160,
        avoid: Vec::new(),
        constant_rate: true,
    }
}

/// Waits until the recording ends on its own (or `limit` passes).
fn wait_end(rec: &Recorder, limit: Duration) -> Option<Duration> {
    let t = Instant::now();
    while t.elapsed() < limit {
        if rec.status() != RecorderStatus::Recording {
            return Some(t.elapsed());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// One scenario; prints "PASS|FAIL <details>" on the last line.
fn scenario(name: &str) -> (bool, String) {
    let clip = std::env::temp_dir().join(format!("archy-fault-{name}.mp4"));
    let _ = std::fs::remove_file(&clip);
    match name {
        // The encoder works: record, save, check the file.
        "normal" => match Recorder::start(config()) {
            Ok(rec) => {
                std::thread::sleep(Duration::from_secs(6));
                let saved = rec.save_clip(5, &clip);
                let ok = saved.as_ref().is_ok_and(|c| c.bytes > 10_000);
                (ok, format!("encoder: {}; saved: {:?}", rec.info.encoder, saved.map(|c| c.bytes)))
            }
            Err(e) => (false, format!("couldn't start: {}", e.message())),
        },
        // The driver hangs while the encoder is created: start must give up (15 s),
        // not freeze.
        "start_hang" => {
            let t = Instant::now();
            let r = Recorder::start(config());
            let took = t.elapsed();
            let ok = r.is_err() && took < Duration::from_secs(20);
            (ok, format!("start returned {} after {:.1} s", if r.is_ok() { "OK (wrong)" } else { "an error" }, took.as_secs_f64()))
        }
        // The encoder stops producing output mid-recording (thread blocked), or goes
        // silent (no events): the recording must end with an encoder reason, and
        // what was recorded before must still save.
        "output_stall" | "mf_silent" => match Recorder::start(config()) {
            Ok(rec) => match wait_end(&rec, Duration::from_secs(40)) {
                Some(after) => {
                    let reason = rec.stop_reason();
                    let failed = rec.status() == RecorderStatus::Failed;
                    let saved = rec.save_clip(30, &clip);
                    let ok = failed && reason.to_ascii_lowercase().contains("encoder") && saved.as_ref().is_ok_and(|c| c.bytes > 1_000);
                    let t = Instant::now();
                    drop(rec);
                    (ok, format!(
                        "ended after {:.1} s: \"{reason}\"; earlier footage saved: {}; stopping took {:.1} s",
                        after.as_secs_f64(),
                        saved.map(|c| format!("{:.1} s", c.seconds)).unwrap_or_else(|e| e.message()),
                        t.elapsed().as_secs_f64()
                    ))
                }
                None => (false, "the recording never noticed the dead encoder (40 s)".into()),
            },
            Err(e) => (false, format!("couldn't start: {}", e.message())),
        },
        _ => (false, "unknown scenario".into()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(name) = args.first() {
        archy_core::init_thread().unwrap();
        let (ok, details) = scenario(name);
        println!("{} {details}", if ok { "PASS" } else { "FAIL" });
        std::process::exit(if ok { 0 } else { 1 });
    }
    // Parent: each scenario in its own process.
    let me = std::env::current_exe().unwrap();
    let encoder = std::env::var("ARCHY_ENCODER").unwrap_or_else(|_| "mf".into());
    let mut all = true;
    for (name, fault) in [
        ("normal", ""),
        ("start_hang", "start_hang"),
        ("output_stall", "output_stall"),
        ("mf_silent", "mf_silent"),
    ] {
        if name == "mf_silent" && encoder != "mf" {
            continue;
        }
        let out = std::process::Command::new(&me)
            .arg(name)
            .env("ARCHY_ENCODER", &encoder)
            .env("ARCHY_FAULT", fault)
            .output()
            .expect("run scenario");
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().last().unwrap_or("FAIL no output (crashed?)").to_string();
        all &= line.starts_with("PASS");
        println!("[{encoder}] {name:<13} {line}");
    }
    println!("{}", if all { "ALL PASSED" } else { "SOME FAILED" });
    std::process::exit(if all { 0 } else { 1 });
}
