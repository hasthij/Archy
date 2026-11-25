//! Real-PC check of voice-chat detection: which chat app Archy picks, whether it has an
//! audio session, and how much AAC it captures from it in 5 s (silence = tiny packets).
//! cargo run --release -p archy-core --example chat_probe
fn main() {
    archy_core::init_thread().unwrap();
    println!("playing apps (incl. idle): {:?}", archy_audio::playing_apps(true));
    let Some((pid, exe)) = archy_audio::find_chat_app() else { return println!("no chat app running") };
    println!("picked chat app: {exe} root pid {pid}");
    let dir = std::env::temp_dir().join("archy_chat_probe_icons");
    archy_audio::save_icon(&dir, "Discord", pid);
    println!("icon saved: {} bytes", archy_audio::cached_icon(&dir, "Discord").map_or(0, |b| b.len()));
    let bytes = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let b = bytes.clone();
    let cap = archy_audio::AudioCapture::start(archy_audio::Source::Process { pid }, 160, Default::default(), move |p| {
        b.fetch_add(p.data.len() as u64, std::sync::atomic::Ordering::Relaxed);
    });
    match cap {
        Ok(c) => {
            std::thread::sleep(std::time::Duration::from_secs(5));
            drop(c);
            println!("captured {} AAC bytes in 5 s", bytes.load(std::sync::atomic::Ordering::Relaxed));
        }
        Err(e) => println!("capture failed: {}", e.message()),
    }
}
