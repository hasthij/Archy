//! Recording quality test (see archy_core::selftest). Exit code 1 on failure.
//! cargo run --release -p archy-core --example quality_test
//! (ARCHY_TEST_FPS=144, ARCHY_ENCODER=mf|vpl, ARCHY_TEST_SECONDS=8, ARCHY_TEST_MBPS=5,
//! ARCHY_FFMPEG=path; default: tools\ffmpeg\ffmpeg.exe above the current folder)

fn env(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn main() {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let ffmpeg = std::env::var_os("ARCHY_FFMPEG").map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::env::current_dir().unwrap().ancestors()
            .map(|a| a.join("tools").join("ffmpeg").join("ffmpeg.exe"))
            .find(|p| p.exists())
            .expect("FFmpeg not found: put it in tools\\ffmpeg or set ARCHY_FFMPEG")
    });
    let opts = archy_core::selftest::Options {
        fps: env("ARCHY_TEST_FPS", 60),
        seconds: env("ARCHY_TEST_SECONDS", 8),
        mbps: env("ARCHY_TEST_MBPS", 5),
        ffmpeg,
    };
    match archy_core::selftest::run(&opts) {
        Ok(r) => {
            println!("encoder: {} on {} · {} fps target", r.encoder, r.gpu, r.fps);
            println!("window showed {:.0} fps · recorder sent {} frames, {} skipped · CPU {:.1}% of one core", r.window_fps, r.sent, r.skipped, r.cpu_core_pct);
            println!("decoded {} frames · tearing {} · smudged {} · backwards {} · wrong colours {} · repeats {} · uneven {} · decode errors: {}",
                r.frames, r.tears, r.smudges, r.backwards, r.wrong_colours, r.repeats, r.uneven, if r.decode_errors.is_empty() { "none" } else { &r.decode_errors });
            println!("{}", if r.pass { "PASS" } else if r.clean { "CLEAN BUT NOT SMOOTH (the GPU can't keep up at this frame rate)" } else { "FAIL" });
            std::process::exit(if r.pass { 0 } else { 1 });
        }
        Err(e) => {
            println!("test couldn't run: {e}");
            std::process::exit(2);
        }
    }
}
