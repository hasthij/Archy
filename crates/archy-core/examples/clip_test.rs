//! Records the primary monitor with system + mic audio for a few seconds and saves a clip.
//! cargo run --release -p archy-core --example clip_test -- <out.mp4>
use archy_audio::Source;
use archy_core::recorder::AudioSource;
use archy_core::{Codec, ConvertMode, Recorder, RecorderConfig, Target};

fn env(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).expect("output path"));
    archy_core::init_thread().unwrap();
    let cfg = RecorderConfig {
        target: Target::Monitor(archy_capture::primary_monitor()),
        fps: std::env::var("ARCHY_TEST_FPS").ok().and_then(|v| v.parse().ok()).unwrap_or(60),
        max_height: env("ARCHY_TEST_HEIGHT", 720),
        codecs: vec![Codec::H264],
        max_bitrate: env("ARCHY_TEST_MBPS", 10) * 2_000_000,
        target_bitrate: Some(env("ARCHY_TEST_MBPS", 10) * 1_000_000),
        quality: 70,
        buffer_seconds: 30,
        convert: ConvertMode::VideoEngine,
        gpu: std::env::var("ARCHY_TEST_GPU").unwrap_or_default(),
        audio: vec![
            AudioSource {
                name: "Everything".into(),
                source: Source::System { device: None },
                volumes: Default::default(),
            },
            AudioSource {
                name: "Microphone".into(),
                source: Source::Microphone { device: None },
                volumes: Default::default(),
            },
        ],
        audio_kbps: 160,
    };
    let rec = Recorder::start(cfg).unwrap();
    println!("encoder: {} on {} | audio: {:?}", rec.info.encoder, rec.info.gpu, rec.audio_report);
    let secs = env("ARCHY_TEST_SECONDS", 8);
    std::thread::sleep(std::time::Duration::from_secs(secs as u64));
    let saved = rec.save_clip(secs.saturating_sub(3).max(2), &out);
    println!("health: {:?}", rec.health());
    match saved {
        Ok(c) => println!(
            "saved {:.1} s, {:.2} MB in {} ms",
            c.seconds,
            c.bytes as f64 / 1e6,
            c.save_time.as_millis()
        ),
        Err(e) => println!("save failed: {}", e.message()),
    }
}
