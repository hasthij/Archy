//! transcribe <16k mono wav> [start_s end_s]...: what the model hears in each span.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let wav = std::fs::read(&a[0]).unwrap();
    let audio: Vec<f32> = wav[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    let dir = archy_voice::find_dir().unwrap();
    for span in a[1..].chunks(2) {
        let (s, e): (f32, f32) = (span[0].parse().unwrap(), span[1].parse().unwrap());
        let part = &audio[(s * 16000.0) as usize..((e * 16000.0) as usize).min(audio.len())];
        println!("{s:6.1}-{e:6.1}: {}", archy_voice::transcribe(&dir, &[part]).unwrap()[0].join(" "));
    }
}
