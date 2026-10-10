//! cargo run -p archy-voice --example kws_wav -- <16k mono wav> "PHRASE ONE" "PHRASE TWO"
fn main() {
    let mut args = std::env::args().skip(1);
    let wav = std::fs::read(args.next().expect("wav")).expect("read wav");
    let phrases: Vec<String> = args.collect();
    let dir = archy_voice::find_dir().expect("voice folder not found");
    let model = dir.join(archy_voice::MODEL_DIR);
    let vocab = archy_voice::Vocab::from_model(&std::fs::read(model.join("bpe.model")).unwrap()).unwrap();
    for p in &phrases {
        println!("{p:?} -> {}", vocab.encode(p));
    }
    let mut spotter = archy_voice::Spotter::new(&dir, &phrases, &[], archy_voice::Tuning::preset(std::env::var("ARCHY_KWS_SENS").ok().and_then(|v| v.parse().ok()).unwrap_or(50))).expect("spotter");
    // Skip the 44-byte header of a plain PCM WAV.
    let samples: Vec<f32> = wav[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    for (n, chunk) in samples.chunks(1600).enumerate() {
        if let Some(i) = spotter.accept(chunk) {
            println!("detected {:?} at {:.1} s", phrases[i], n as f32 * 0.1);
        }
    }
    // Trailing silence flushes the last words.
    if let Some(i) = spotter.accept(&vec![0.0; 16000]) {
        println!("detected {:?} at end", phrases[i]);
    }
}
