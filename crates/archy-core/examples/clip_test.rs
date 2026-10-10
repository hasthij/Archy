//! Records the primary monitor with system + mic audio for a few seconds and saves a clip.
//! cargo run --release -p archy-core --example clip_test -- <out.mp4>
use archy_audio::Source;
use archy_core::recorder::AudioSource;
use archy_core::{Codec, ConvertMode, Recorder, RecorderConfig, Target};

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).expect("output path"));
    archy_core::init_thread().unwrap();
    let cfg = RecorderConfig {
        target: Target::Monitor(archy_capture::primary_monitor()),
        fps: 60,
        max_height: 720,
        codecs: vec![Codec::H264],
        max_bitrate: 10_000_000,
        target_bitrate: Some(5_000_000),
        quality: 70,
        buffer_seconds: 30,
        convert: ConvertMode::VideoEngine,
        audio: vec![
            AudioSource {
                name: "Everything".into(),
                source: Source::System { device: None },
            },
            AudioSource {
                name: "Microphone".into(),
                source: Source::Microphone { device: None },
            },
        ],
        audio_kbps: 160,
    };
    let rec = Recorder::start(cfg).unwrap();
    println!("audio: {:?}", rec.audio_report);
    std::thread::sleep(std::time::Duration::from_secs(8));
    match rec.save_clip(5, &out) {
        Ok(c) => println!(
            "saved {:.1} s, {:.2} MB in {} ms",
            c.seconds,
            c.bytes as f64 / 1e6,
            c.save_time.as_millis()
        ),
        Err(e) => println!("save failed: {}", e.message()),
    }
}
