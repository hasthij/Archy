//! Real clip with the track layout Archy builds: Everything + Discord (any chat app) + each playing app.
//! cargo run --release -p archy-core --example chat_clip_test -- <out.mp4>
use archy_audio::Source;
use archy_core::recorder::AudioSource;
use archy_core::{Codec, ConvertMode, Recorder, RecorderConfig, Target};

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).expect("output path"));
    archy_core::init_thread().unwrap();
    let mut audio = vec![AudioSource { name: "Everything".into(), source: Source::System { device: None }, volumes: Default::default() }];
    if let Some((pid, exe)) = archy_audio::find_chat_app() {
        println!("chat app: {exe} ({pid})");
        audio.push(AudioSource { name: "Discord".into(), source: Source::Process { pid }, volumes: Default::default() });
    }
    for (pid, exe) in archy_audio::playing_apps(false) {
        if archy_audio::is_chat_app(&exe) || exe.eq_ignore_ascii_case("archy.exe") {
            continue;
        }
        println!("app: {exe} ({pid})");
        audio.push(AudioSource { name: exe.trim_end_matches(".exe").into(), source: Source::Process { pid }, volumes: Default::default() });
    }
    let cfg = RecorderConfig {
        target: Target::Monitor(archy_capture::primary_monitor()),
        fps: 30,
        max_height: 720,
        codecs: vec![Codec::H264],
        max_bitrate: 20_000_000,
        target_bitrate: Some(10_000_000),
        quality: 70,
        buffer_seconds: 30,
        convert: ConvertMode::VideoEngine,
        gpu: String::new(),
        audio,
        audio_kbps: 160,
        avoid: Vec::new(),
        constant_rate: true,
        gpu_priority: 0,
    };
    let rec = Recorder::start(cfg).unwrap();
    println!("tracks: {:?}", rec.audio_report);
    std::thread::sleep(std::time::Duration::from_secs(8));
    match rec.save_clip(6, &out) {
        Ok(c) => println!("saved {:.1} s, {:.2} MB", c.seconds, c.bytes as f64 / 1e6),
        Err(e) => println!("save failed: {}", e.message()),
    }
}
