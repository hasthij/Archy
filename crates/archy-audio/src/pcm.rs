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
