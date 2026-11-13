//! Streaming stereo resampler (linear), so a device is captured at the rate it
//! already runs at (no Windows rate conversion, which added hiss on some USB
//! mics) and brought to the encoder's rate here.

use windows::Win32::Media::Audio::IAudioClient;
use windows::Win32::System::Com::CoTaskMemFree;

use crate::{CHANNELS, SAMPLE_RATE};

/// The rate the device's shared-mode mix runs at (what Windows uses for it).
pub(crate) fn device_rate(client: &IAudioClient) -> u32 {
    unsafe {
        let Ok(fmt) = client.GetMixFormat() else { return SAMPLE_RATE };
        let rate = std::ptr::addr_of!((*fmt).nSamplesPerSec).read_unaligned();
        CoTaskMemFree(Some(fmt as *const _));
        if (8_000..=384_000).contains(&rate) { rate } else { SAMPLE_RATE }
    }
}

pub(crate) struct Resampler {
    step: f64,
    pos: f64,
    prev: Option<[i16; 2]>,
}

impl Resampler {
    /// `None` when the device already runs at the encoder's rate.
    pub fn new(from: u32) -> Option<Self> {
        (from != SAMPLE_RATE).then(|| Self { step: from as f64 / SAMPLE_RATE as f64, pos: 0.0, prev: None })
    }

    /// Appends the resampled `input` (interleaved stereo) to `out`.
    pub fn process(&mut self, input: &[i16], out: &mut Vec<i16>) {
        let ch = CHANNELS as usize;
        let n = input.len() / ch;
        if n == 0 {
            return;
        }
        let first = [input[0], input[1]];
        let prev = *self.prev.get_or_insert(first);
        // Index 0 is the frame before this block, index k is input[k - 1].
        let at = |i: usize, c: usize| if i == 0 { prev[c] } else { input[(i - 1) * ch + c] } as f64;
        while self.pos < n as f64 {
            let i = self.pos as usize;
            let f = self.pos - i as f64;
            for c in 0..ch {
                out.push((at(i, c) * (1.0 - f) + at(i + 1, c) * f) as i16);
            }
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = Some([input[(n - 1) * ch], input[(n - 1) * ch + 1]]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_follows_the_rate_ratio() {
        let mut r = Resampler::new(44_100).unwrap();
        let mut out = Vec::new();
        for _ in 0..100 {
            r.process(&vec![1000i16; 441 * 2], &mut out);
        }
        let frames = out.len() / 2;
        assert!((frames as i64 - 48_000).abs() <= 2, "{frames}");
        assert!(out.iter().all(|&s| s == 1000));
        assert!(Resampler::new(SAMPLE_RATE).is_none());
    }
}
