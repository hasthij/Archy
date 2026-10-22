//! Track 1: everything the PC plays *plus your microphone*, mixed in sync.
//!
//! Most players (Discord, phones, Windows' own) only play a clip's first audio
//! track, and system loopback never contains the mic, so without this your own
//! voice was missing when clips were watched. Both devices are read here,
//! lined up by their QPC timestamps, added together and encoded once.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Duration;

use windows::core::Result;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::aac::{AacEncoder, AacInfo};
use crate::capture::{device_client, pcm_format};
use crate::{AudioPacket, CHANNELS, SAMPLE_RATE};

const HNS: i64 = 10_000_000;
/// Frames per mixed block (one AAC frame).
const BLOCK: usize = 1024;
/// Gaps longer than this in a device's timeline are filled with silence.
const GAP: i64 = 200_000;
/// Stop waiting for the slower device after this much lag.
const MAX_WAIT: i64 = 1_500_000;

fn frames_to_hns(frames: i64) -> i64 {
    frames * HNS / SAMPLE_RATE as i64
}

fn hns_to_frames(hns: i64) -> i64 {
    hns * SAMPLE_RATE as i64 / HNS
}

/// PCM from one device on the shared QPC clock.
#[derive(Default)]
struct Timeline {
    /// Time of `buf[0]` (100 ns).
    start: Option<i64>,
    /// Interleaved stereo i16.
    buf: VecDeque<i16>,
}

impl Timeline {
    fn end(&self) -> Option<i64> {
        self.start
            .map(|s| s + frames_to_hns((self.buf.len() / CHANNELS as usize) as i64))
    }

    fn push(&mut self, time: i64, pcm: &[i16]) {
        if let Some(end) = self.end() {
            let gap = time - end;
            if gap > GAP {
                let fill = hns_to_frames(gap.min(20_000_000)) as usize * CHANNELS as usize;
                self.buf.extend(std::iter::repeat_n(0, fill));
            }
        } else {
            self.start = Some(time);
        }
        self.buf.extend(pcm.iter().copied());
    }

    /// Adds this device's samples for `[from, from + BLOCK)` into `out`, and
    /// drops everything before the block's end.
    fn mix_into(&mut self, from: i64, out: &mut [i32], percent: u32) {
        let percent = percent as i32;
        let Some(start) = self.start else { return };
        let offset = hns_to_frames(from - start);
        for (i, slot) in out.chunks_mut(CHANNELS as usize).enumerate() {
            let frame = offset + i as i64;
            if frame < 0 {
                continue;
            }
            let at = frame as usize * CHANNELS as usize;
            for (c, v) in slot.iter_mut().enumerate() {
                if let Some(&s) = self.buf.get(at + c) {
                    *v += s as i32 * percent / 100;
                }
            }
        }
        let consumed =
            (offset + BLOCK as i64).clamp(0, (self.buf.len() / CHANNELS as usize) as i64);
        self.buf.drain(..consumed as usize * CHANNELS as usize);
        self.start = Some(start + frames_to_hns(consumed));
    }
}

fn open(client: &IAudioClient, loopback: bool) -> Result<IAudioCaptureClient> {
    let mut flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    if loopback {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }
    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            flags,
            2_000_000,
            0,
            &pcm_format(),
            None,
        )?;
        let capture = client.GetService()?;
        client.Start()?;
        Ok(capture)
    }
}

fn drain(
    capture: &IAudioCaptureClient,
    into: &mut Timeline,
    mut filter: Option<&mut crate::denoise::Filter>,
) -> Result<()> {
    let mut filtered: Vec<i16> = Vec::new();
    loop {
        let pending = unsafe { capture.GetNextPacketSize()? };
        if pending == 0 {
            return Ok(());
        }
        let mut data = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        let mut qpc = 0u64;
        unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))? };
        let samples = frames as usize * CHANNELS as usize;
        if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
            into.push(qpc as i64, &vec![0i16; samples]);
        } else {
            let pcm = unsafe { std::slice::from_raw_parts(data as *const i16, samples) };
            match filter.as_deref_mut().filter(|f| f.active()) {
                Some(f) => {
                    filtered.clear();
                    filtered.extend_from_slice(pcm);
                    f.process(&mut filtered, CHANNELS as usize);
                    into.push(qpc as i64, &filtered);
                }
                None => into.push(qpc as i64, pcm),
            }
        }
        unsafe { capture.ReleaseBuffer(frames)? };
    }
}

pub fn run(
    output: Option<String>,
    mic: Option<String>,
    kbps: u32,
    volumes: &crate::Volumes,
    stop: &Arc<AtomicBool>,
    ready: &SyncSender<Result<AacInfo>>,
    on_packet: &mut dyn FnMut(AudioPacket),
) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    crate::audio_thread();
    let sys_client = device_client(eRender, output.as_deref())?;
    let mic_client = device_client(eCapture, mic.as_deref())?;
    let sys = open(&sys_client, true)?;
    let mic_cap = open(&mic_client, false)?;
    let mut encoder = AacEncoder::new(kbps)?;
    let _ = ready.send(Ok(encoder.info.clone()));

    let (mut a, mut b) = (Timeline::default(), Timeline::default());
    let mut mic_filter = crate::denoise::Filter::new(volumes.mic_filter.clone());
    let mut out_start: Option<i64> = None;
    let mut out_frames: i64 = 0;
    let mut mixed = vec![0i32; BLOCK * CHANNELS as usize];
    let mut pcm = vec![0i16; BLOCK * CHANNELS as usize];
    let mut emit = |data: &[u8], time: i64, duration: i64| {
        on_packet(AudioPacket {
            data,
            time,
            duration,
        })
    };

    while !stop.load(Ordering::Acquire) {
        // Two devices without a shared event: a 10 ms poll is cheap and keeps latency low.
        std::thread::sleep(Duration::from_millis(10));
        drain(&sys, &mut a, None)?;
        drain(&mic_cap, &mut b, Some(&mut mic_filter))?;
        let ends = [a.end(), b.end()];
        let Some(latest) = ends.iter().flatten().copied().max() else {
            continue;
        };
        let base = *out_start.get_or_insert_with(|| {
            [a.start, b.start]
                .iter()
                .flatten()
                .copied()
                .min()
                .unwrap_or(latest)
        });
        loop {
            let from = base + frames_to_hns(out_frames);
            let to = from + frames_to_hns(BLOCK as i64);
            // Wait until both devices have covered this block, unless one has gone
            // quiet (an idle output sends nothing) for longer than MAX_WAIT.
            let covered = ends.iter().all(|e| e.is_some_and(|e| e >= to));
            if !(covered || latest - to > MAX_WAIT) || latest < to {
                break;
            }
            mixed.iter_mut().for_each(|v| *v = 0);
            a.mix_into(from, &mut mixed, volumes.main.load(Ordering::Relaxed));
            b.mix_into(from, &mut mixed, volumes.mic.load(Ordering::Relaxed));
            // Two loud sources (or >100% volume) can add up past full scale:
            // round the peaks off rather than clip.
            for (o, m) in pcm.iter_mut().zip(&mixed) {
                *o = crate::soft_limit(*m as f32);
            }
            encoder.encode(&pcm, from, &mut emit)?;
            out_frames += BLOCK as i64;
        }
    }
    unsafe {
        let _ = sys_client.Stop();
        let _ = mic_client.Stop();
    }
    Ok(())
}
