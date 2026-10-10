//! cargo run --release -p archy-core --example discord_copy -- <clip.mp4> [limit MB]
fn main() {
    let mut args = std::env::args().skip(1);
    let src = std::path::PathBuf::from(args.next().expect("clip path"));
    let mb: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(10.0);
    let dst = archy_core::discord::copy_path(&src);
    let t = std::time::Instant::now();
    match archy_core::discord::make_copy(&src, &dst, (mb * 1_000_000.0) as u64) {
        Ok(c) => println!(
            "wrote {} ({:.2} MB, {}x{}) in {:.1} s",
            c.path.display(),
            c.bytes as f64 / 1e6,
            c.width,
            c.height,
            t.elapsed().as_secs_f64()
        ),
        Err(e) => println!("failed: {}", e.message()),
    }
}
