//! Live test: listens on a mic for N seconds, prints the level each second and
//! any phrase heard. `kws_mic <seconds> <mic id or -> <sensitivity 0-100> <phrase>...`
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let secs: u64 = a[0].parse().unwrap();
    let mic = (a[1] != "-").then_some(a[1].as_str());
    let sens = archy_voice::Tuning::preset(a[2].parse().unwrap());
    let phrases: Vec<String> = a[3..].to_vec();
    let dir = archy_voice::find_dir().expect("voice dir");
    let mut spotter = archy_voice::Spotter::new(&dir, &phrases, &[], sens).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(secs));
        s2.store(true, Ordering::Release);
    });
    let start = std::time::Instant::now();
    let (mut n, mut peak, mut sq) = (0usize, 0f32, 0f64);
    let mut buf = Vec::new();
    let mut rec: Vec<i16> = Vec::new();
    archy_audio::listen_microphone(mic, 16000, &stop, &mut |pcm| {
        rec.extend_from_slice(pcm);
        buf.clear();
        buf.extend(pcm.iter().map(|&s| s as f32 / 32768.0));
        for &x in &buf {
            peak = peak.max(x.abs());
            sq += (x * x) as f64;
        }
        n += buf.len();
        if n >= 16000 {
            println!("{:5.1}s  peak {:.3}  rms {:.4}", start.elapsed().as_secs_f32(), peak, (sq / n as f64).sqrt());
            n = 0; peak = 0.0; sq = 0.0;
        }
        if let Some(i) = spotter.accept(&buf) {
            println!(">>> HEARD \"{}\"", phrases[i]);
        }
    })
    .unwrap();
    // ARCHY_REC=<path.wav> keeps what was heard (16 kHz mono) for offline tests.
    if let Some(path) = std::env::var_os("ARCHY_REC") {
        let data: Vec<u8> = rec.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        for v in [16u32] { wav.extend_from_slice(&v.to_le_bytes()); }
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16000u32.to_le_bytes());
        wav.extend_from_slice(&32000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        std::fs::write(path, wav).unwrap();
    }
}
