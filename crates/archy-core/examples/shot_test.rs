//! Screenshot hotkey test: records a window that paints a known colour, takes a
//! screenshot while frames arrive (full-size path) and one after the window goes
//! still (last-recorded-frame path), and checks size, colour, time and that no
//! recording frames were lost.
//! cargo run --release -p archy-core --example shot_test

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use archy_core::{Codec, ConvertMode, Recorder, RecorderConfig, Target};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Painting moves (frames keep coming) while this is set; then the window is still.
static MOVING: AtomicBool = AtomicBool::new(true);
static mut TICK: u32 = 0;

extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                if MOVING.load(Ordering::Relaxed) {
                    TICK += 1;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                // Left half pure red (the colour checked), right half changes every tick.
                let left = RECT { right: r.right / 2, ..r };
                let right = RECT { left: r.right / 2, ..r };
                let red = CreateSolidBrush(COLORREF(0x0000FF)); // 0x00BBGGRR
                FillRect(dc, &left, red);
                let t = TICK % 256;
                let moving = CreateSolidBrush(COLORREF(t | (t << 8) | (t << 16)));
                FillRect(dc, &right, moving);
                let _ = DeleteObject(red.into());
                let _ = DeleteObject(moving.into());
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn pump(d: Duration) {
    let end = Instant::now() + d;
    let mut msg = MSG::default();
    while Instant::now() < end {
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Size and the colour at (x, y) of a PNG.
fn read_png(path: &std::path::Path, x: u32, y: u32) -> (u32, u32, [u8; 3]) {
    let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    let i = ((y * info.width + x) * 3) as usize;
    (info.width, info.height, [buf[i], buf[i + 1], buf[i + 2]])
}

fn main() {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    archy_core::init_thread().unwrap();
    let instance = unsafe { GetModuleHandleW(None) }.unwrap();
    let class = WNDCLASSW { lpfnWndProc: Some(proc_), hInstance: instance.into(), lpszClassName: w!("ArchyShotTest"), ..Default::default() };
    unsafe { RegisterClassW(&class) };
    let hwnd = unsafe {
        CreateWindowExW(WINDOW_EX_STYLE::default(), w!("ArchyShotTest"), w!("Archy screenshot test"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE, 100, 100, 800, 500, None, None, Some(instance.into()), None)
    }
    .unwrap();
    unsafe { SetTimer(Some(hwnd), 1, 15, None) };
    // `--hold N`: just keep the (changing) window open N seconds, as a stand-in
    // game for end-to-end tests of the tray.
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--hold") {
        let secs = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(60);
        pump(Duration::from_secs(secs));
        return;
    }
    pump(Duration::from_millis(500));

    let rec = Recorder::start(RecorderConfig {
        target: Target::Window(hwnd),
        fps: 60,
        max_height: 720,
        codecs: vec![Codec::H264],
        max_bitrate: 10_000_000,
        target_bitrate: Some(5_000_000),
        quality: 70,
        buffer_seconds: 30,
        convert: ConvertMode::VideoEngine,
        gpu: String::new(),
        audio: Vec::new(),
        audio_kbps: 160,
        avoid: Vec::new(),
        constant_rate: true,
    })
    .expect("start");
    pump(Duration::from_secs(3));
    let mut all = true;
    let rec = Arc::new(rec);
    for (label, moving) in [("frames arriving (full size)", true), ("still window (last frame)", false)] {
        MOVING.store(moving, Ordering::Relaxed);
        pump(Duration::from_millis(600));
        let path = std::env::temp_dir().join(format!("archy-shot-{}.png", if moving { "moving" } else { "still" }));
        let _ = std::fs::remove_file(&path);
        let (_, dropped0) = rec.frame_counts();
        let t = Instant::now();
        let r2 = rec.clone();
        let p2 = path.clone();
        let worker = std::thread::spawn(move || r2.screenshot(&p2));
        // Keep the window's messages flowing while the screenshot is taken.
        while !worker.is_finished() {
            pump(Duration::from_millis(5));
        }
        let result = worker.join().unwrap();
        let took = t.elapsed();
        pump(Duration::from_millis(500));
        let (_, dropped1) = rec.frame_counts();
        match result {
            Ok(p) => {
                let (w, h, c) = read_png(&p, 40, 100);
                let red = c[0] > 200 && c[1] < 60 && c[2] < 60;
                let ok = red && w >= 640 && dropped1 == dropped0;
                all &= ok;
                println!(
                    "{} {label}: {w}x{h} in {} ms, pixel {:?} {}, frames lost meanwhile: {}",
                    if ok { "PASS" } else { "FAIL" },
                    took.as_millis(),
                    c,
                    if red { "(red, correct)" } else { "(WRONG colour)" },
                    dropped1 - dropped0
                );
            }
            Err(e) => {
                all = false;
                println!("FAIL {label}: {e}");
            }
        }
    }
    drop(rec);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    println!("{}", if all { "ALL PASSED" } else { "SOME FAILED" });
    std::process::exit(if all { 0 } else { 1 });
}
