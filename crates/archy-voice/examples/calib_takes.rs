//! calib_takes <phrase> <take.wav>... -- <talk.wav>...   (16 kHz mono 16-bit WAVs)
//! Runs the button-paced calibration on recorded takes, as the Archy window does.
fn read(path: &str) -> Vec<f32> {
    let wav = std::fs::read(path).unwrap();
    let at = wav.windows(4).position(|w| w == b"data").expect("no data chunk") + 8;
    wav[at..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let split = a.iter().position(|x| x == "--").unwrap_or(a.len());
    let takes: Vec<Vec<f32>> = a[1..split].iter().map(|p| read(p)).collect();
    let talk: Vec<f32> = a.get(split + 1..).unwrap_or(&[]).iter().flat_map(|p| read(p)).collect();
    let dir = archy_voice::find_dir().unwrap();
    for (i, t) in takes.iter().enumerate() {
        let mut padded = vec![0.0; 4800];
        padded.extend(t);
        padded.extend(vec![0.0; 24000]);
        println!("take {}: heard at Balanced = {:?}", i + 1, archy_voice::heard_in(&dir, &a[0], &padded, archy_voice::Tuning::preset(50)));
    }
    let t = std::time::Instant::now();
    let c = archy_voice::calibrate_takes(&dir, &a[0], &takes, &talk);
    println!("{c:#?}
({:.1} s)", t.elapsed().as_secs_f32());
}
