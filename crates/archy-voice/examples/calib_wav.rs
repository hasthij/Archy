//! calib_wav <16k mono wav> <phrase> <talk_start> <talk_end> <prompt_s>...
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let wav = std::fs::read(&a[0]).unwrap();
    let audio: Vec<f32> = wav[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    let talk = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let prompts: Vec<f32> = a[4..].iter().map(|p| p.parse().unwrap()).collect();
    let dir = archy_voice::find_dir().unwrap();
    let t = std::time::Instant::now();
    let c = archy_voice::calibrate(&dir, &a[1], &audio, &prompts, 4.5, talk);
    println!("{c:#?}\n({:.1} s)", t.elapsed().as_secs_f32());
}
