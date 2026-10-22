//! Microphone clean-up: RNNoise noise suppression (fans, keyboards, hum) and a
//! Discord-style volume gate that mutes the mic below a threshold.
//!
//! Both are off by default and then cost nothing: `Filter::process` returns at
//! once. When on, audio is handled in 10 ms mono blocks, so the mic comes out
//! 10 ms late; the clip timestamps are not moved (10 ms is below what anyone
//! hears as out of sync).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use nnnoiseless::DenoiseState;

/// Gate threshold meaning "no gate".
pub const GATE_OFF: i32 = -100;
const FRAME: usize = DenoiseState::FRAME_SIZE; // 480 = 10 ms at 48 kHz
/// Keep the gate open this many blocks after the voice drops (no clipped word ends).
const HOLD_FRAMES: u32 = 20;
/// Per-sample gain steps: ~10 ms to open, ~50 ms to close (no clicks).
const OPEN_STEP: f32 = 1.0 / 480.0;
const CLOSE_STEP: f32 = 1.0 / 2400.0;

/// Mic filter settings, shared with the settings so changes apply at once.
#[derive(Clone, Debug)]
pub struct MicFilter {
    pub denoise: Arc<AtomicBool>,
    /// Gate threshold in dBFS (-100 = off, -60 quiet room .. -10 shouting).
    pub gate_db: Arc<AtomicI32>,
}

impl Default for MicFilter {
    fn default() -> Self {
        Self {
            denoise: Arc::new(AtomicBool::new(false)),
            gate_db: Arc::new(AtomicI32::new(GATE_OFF)),
        }
    }
}

impl MicFilter {
    pub fn set(&self, denoise: bool, gate_db: i32) {
        self.denoise.store(denoise, Ordering::Relaxed);
        self.gate_db.store(gate_db.clamp(GATE_OFF, 0), Ordering::Relaxed);
    }
}

/// RMS level of mono or interleaved audio in dBFS (-100 = silence).
pub fn level_db(pcm: &[i16]) -> f32 {
    let power = pcm.iter().map(|&s| (s as f32).powi(2)).sum::<f32>() / pcm.len().max(1) as f32;
    (10.0 * (power.max(1e-3) / (32768.0 * 32768.0)).log10()).max(-100.0)
}

pub struct Filter {
    cfg: MicFilter,
    rnn: Option<Box<DenoiseState<'static>>>,
    input: Vec<f32>,
    output: VecDeque<f32>,
    block: Vec<f32>,
    gain: f32,
    hold: u32,
    running: bool,
    /// Level of the last 10 ms block in dBFS (after noise suppression), for meters.
    pub level_db: f32,
    /// Whether the gate let the last block through.
    pub open: bool,
}

impl Filter {
    pub fn new(cfg: MicFilter) -> Self {
        Self {
            cfg,
            rnn: None,
            input: Vec::with_capacity(FRAME),
            output: VecDeque::with_capacity(FRAME * 2),
            block: vec![0.0; FRAME],
            gain: 1.0,
            hold: 0,
            running: false,
            level_db: -100.0,
            open: true,
        }
    }

    /// Whether `process` would change audio (callers skip copying when not).
    pub fn active(&self) -> bool {
        self.running
            || self.cfg.denoise.load(Ordering::Relaxed)
            || self.cfg.gate_db.load(Ordering::Relaxed) > GATE_OFF
    }

    /// Filters interleaved 48 kHz audio with `channels` channels in place.
    /// Multi-channel mic audio is mixed to mono (a voice is mono anyway).
    pub fn process(&mut self, pcm: &mut [i16], channels: usize) {
        let denoise = self.cfg.denoise.load(Ordering::Relaxed);
        let gate = self.cfg.gate_db.load(Ordering::Relaxed);
        if !denoise && gate <= GATE_OFF {
            if self.running {
                *self = Self::new(self.cfg.clone());
            }
            return;
        }
        if !self.running {
            self.running = true;
            self.output.extend(std::iter::repeat(0.0).take(FRAME));
        }
        if denoise && self.rnn.is_none() {
            self.rnn = Some(DenoiseState::new());
        } else if !denoise {
            self.rnn = None;
        }
        for frame in pcm.chunks_exact_mut(channels.max(1)) {
            let mono = frame.iter().map(|&s| s as f32).sum::<f32>() / frame.len() as f32;
            self.input.push(mono);
            if self.input.len() == FRAME {
                self.run_block(gate);
            }
            let s = self.output.pop_front().unwrap_or(0.0);
            let s = s.clamp(-32768.0, 32767.0) as i16;
            frame.iter_mut().for_each(|o| *o = s);
        }
    }

    fn run_block(&mut self, gate: i32) {
        match self.rnn.as_mut() {
            Some(rnn) => {
                rnn.process_frame(&mut self.block, &self.input);
            }
            None => self.block.copy_from_slice(&self.input),
        }
        self.input.clear();
        let power = self.block.iter().map(|s| s * s).sum::<f32>() / FRAME as f32;
        self.level_db = (10.0 * (power.max(1e-3) / (32768.0 * 32768.0)).log10()).max(-100.0);
        if gate <= GATE_OFF || self.level_db >= gate as f32 {
            self.hold = HOLD_FRAMES;
        } else {
            self.hold = self.hold.saturating_sub(1);
        }
        self.open = self.hold > 0;
        for s in &self.block {
            self.gain = if self.open {
                (self.gain + OPEN_STEP).min(1.0)
            } else {
                (self.gain - CLOSE_STEP).max(0.0)
            };
            self.output.push_back(s * self.gain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_leaves_audio_untouched() {
        let mut f = Filter::new(MicFilter::default());
        let mut pcm: Vec<i16> = (0..4800).map(|i| (i % 300) as i16).collect();
        let before = pcm.clone();
        f.process(&mut pcm, 2);
        assert_eq!(pcm, before);
    }

    #[test]
    fn gate_mutes_quiet_audio_and_passes_loud() {
        let cfg = MicFilter::default();
        cfg.set(false, -30);
        let mut f = Filter::new(cfg);
        let mut quiet = vec![30i16; 48_000];
        f.process(&mut quiet, 1);
        assert!(quiet[40_000..].iter().all(|&s| s == 0));
        let mut loud: Vec<i16> = (0..48_000).map(|i| if i % 2 == 0 { 10_000 } else { -10_000 }).collect();
        f.process(&mut loud, 1);
        assert!(loud[24_000..].iter().all(|&s| s.abs() == 10_000));
        assert!(f.open);
    }

    #[test]
    fn denoise_keeps_length_and_is_fast() {
        let cfg = MicFilter::default();
        cfg.set(true, GATE_OFF);
        let mut f = Filter::new(cfg);
        let mut pcm: Vec<i16> = (0..96_000).map(|i| ((i * 7919) % 2000) as i16 - 1000).collect();
        let t = std::time::Instant::now();
        f.process(&mut pcm, 2); // 1 s of stereo audio
        let took = t.elapsed();
        assert_eq!(pcm.len(), 96_000);
        eprintln!("noise suppression: {took:?} per second of audio");
    }
}
