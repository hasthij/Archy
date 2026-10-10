//! Captures one process's audio for a few seconds (crash isolation).
//! cargo run -p archy-core --example audio_proc -- <pid>
fn main() {
    let pid: u32 = std::env::args()
        .nth(1)
        .expect("pid")
        .parse()
        .expect("number");
    archy_core::init_thread().unwrap();
    eprintln!("starting process loopback for pid {pid}");
    let mut n = 0u64;
    let cap =
        archy_audio::AudioCapture::start(archy_audio::Source::Process { pid }, 160, move |_p| {
            n += 1;
            if n % 50 == 0 {
                eprintln!("{n} packets");
            }
        });
    match cap {
        Ok(c) => {
            eprintln!("started: {:?}", c.info.bytes_per_second);
            std::thread::sleep(std::time::Duration::from_secs(4));
            drop(c);
            eprintln!("stopped cleanly");
        }
        Err(e) => eprintln!("failed: {}", e.message()),
    }
}
