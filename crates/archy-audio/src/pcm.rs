//! Raw PCM from a microphone, for listeners (voice clipping) rather than clips.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::Result;

use crate::capture::device_client;

/// Captures microphone `device` (`None` or unknown = Windows' default) as mono
/// 16-bit PCM at `rate` Hz on the calling thread, handing each buffer to
/// `on_pcm`, until `stop` is set. Windows does the downmix and resampling.
pub fn listen_microphone(
    device: Option<&str>,
    rate: u32,
    stop: &AtomicBool,
    on_pcm: &mut dyn FnMut(&[i16]),
) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    // No audio-thread boost here: the voice engine runs inside this callback,
    // and Windows demotes audio-class threads that do heavy work.
    let client = device_client(eCapture, device)?;
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: 1,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * 2,
        nBlockAlign: 2,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    let event = unsafe { CreateEventW(None, false, false, None)? };
    let result = (|| -> Result<()> {
        unsafe {
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &format, None)?;
            client.SetEventHandle(event)?;
        }
        let capture: IAudioCaptureClient = unsafe { client.GetService()? };
        unsafe { client.Start()? };
        let mut silence: Vec<i16> = Vec::new();
        while !stop.load(Ordering::Acquire) {
            let _ = unsafe { WaitForSingleObject(event, 100) } == WAIT_OBJECT_0;
            loop {
                let pending = unsafe { capture.GetNextPacketSize()? };
                if pending == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut bflags = 0u32;
                unsafe { capture.GetBuffer(&mut data, &mut frames, &mut bflags, None, None)? };
                if bflags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    silence.clear();
                    silence.resize(frames as usize, 0);
                    on_pcm(&silence);
                } else {
                    on_pcm(unsafe { std::slice::from_raw_parts(data as *const i16, frames as usize) });
                }
                unsafe { capture.ReleaseBuffer(frames)? };
            }
        }
        unsafe {
            let _ = client.Stop();
        }
        Ok(())
    })();
    unsafe {
        let _ = CloseHandle(event);
    }
    result
}

/// Plays microphone `mic` through output device `output` (`None` = defaults)
/// with `filter` applied, so the user hears what clips will get, until `stop`.
/// `on_level` gets the mic level (dBFS) and whether the gate is open, about
/// every 10 ms.
pub fn monitor_microphone(
    mic: Option<&str>,
    output: Option<&str>,
    filter: crate::MicFilter,
    stop: &AtomicBool,
    on_level: &mut dyn FnMut(f32, bool),
) -> Result<()> {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    const RATE: u32 = crate::SAMPLE_RATE;
    // Never let more than 60 ms queue up, or the user hears themselves late.
    const MAX_QUEUE: usize = RATE as usize * 60 / 1000;
    let queue = Arc::new(Mutex::new(VecDeque::<i16>::new()));
    let render_stop = Arc::new(AtomicBool::new(false));
    let player = {
        let (queue, render_stop) = (queue.clone(), render_stop.clone());
        let output = output.map(str::to_owned);
        std::thread::spawn(move || play(output.as_deref(), RATE, &queue, &render_stop))
    };
    let mut f = crate::Filter::new(filter);
    let mut buf: Vec<i16> = Vec::new();
    let result = listen_microphone(mic, RATE, stop, &mut |pcm| {
        buf.clear();
        buf.extend_from_slice(pcm);
        f.process(&mut buf, 1);
        if f.active() {
            on_level(f.level_db, f.open);
        } else {
            on_level(crate::denoise::level_db(pcm), true);
        }
        let mut q = queue.lock().unwrap();
        q.extend(buf.iter().copied());
        let extra = q.len().saturating_sub(MAX_QUEUE);
        q.drain(..extra);
    });
    render_stop.store(true, Ordering::Release);
    let played = player.join().unwrap_or(Ok(()));
    result.and(played)
}

/// Feeds mono 16-bit audio from `queue` to an output device until `stop`.
fn play(
    device: Option<&str>,
    rate: u32,
    queue: &std::sync::Mutex<std::collections::VecDeque<i16>>,
    stop: &AtomicBool,
) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let client = device_client(eRender, device)?;
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: 1,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * 2,
        nBlockAlign: 2,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, &format, None)? };
    let render: IAudioRenderClient = unsafe { client.GetService()? };
    let size = unsafe { client.GetBufferSize()? };
    unsafe { client.Start()? };
    while !stop.load(Ordering::Acquire) {
        std::thread::sleep(std::time::Duration::from_millis(5));
        let free = size - unsafe { client.GetCurrentPadding()? };
        let mut q = queue.lock().unwrap();
        let n = (free as usize).min(q.len());
        if n == 0 {
            continue;
        }
        let data = unsafe { render.GetBuffer(n as u32)? } as *mut i16;
        let out = unsafe { std::slice::from_raw_parts_mut(data, n) };
        for (o, s) in out.iter_mut().zip(q.drain(..n)) {
            *o = s;
        }
        unsafe { render.ReleaseBuffer(n as u32, 0)? };
    }
    unsafe {
        let _ = client.Stop();
    }
    Ok(())
}
