//! The short tones offered in Settings > Sounds & notifications, synthesised as
//! 16-bit mono WAVs so the recorder can play exactly what the window previews.
//! Keep the table in step with `TONES` in archy-ui/ui/app.js.

const RATE: u32 = 44_100;

/// (frequency Hz, start s, length s) per note.
type Notes = &'static [(f32, f32, f32)];

const TABLE: &[(&str, Notes)] = &[
    ("chime", &[(660.0, 0.0, 0.12), (880.0, 0.12, 0.22)]),
    ("rise", &[(440.0, 0.0, 0.1), (554.0, 0.1, 0.1), (659.0, 0.2, 0.18)]),
    ("fall", &[(659.0, 0.0, 0.1), (554.0, 0.1, 0.1), (440.0, 0.2, 0.18)]),
    ("shutter", &[(1800.0, 0.0, 0.03), (900.0, 0.05, 0.05)]),
    ("blip", &[(880.0, 0.0, 0.07)]),
    ("double", &[(740.0, 0.0, 0.06), (740.0, 0.1, 0.06)]),
    ("tick", &[(1200.0, 0.0, 0.03)]),
    ("alert", &[(520.0, 0.0, 0.14), (390.0, 0.18, 0.2)]),
    ("sparkle", &[(988.0, 0.0, 0.06), (1319.0, 0.07, 0.06), (1568.0, 0.14, 0.14)]),
];

pub fn is_tone(name: &str) -> bool {
    TABLE.iter().any(|(n, _)| *n == name)
}

/// The named tone as a WAV file at `volume` percent (0-200), or None if unknown.
pub fn wav(name: &str, volume: u32) -> Option<Vec<u8>> {
    let notes = TABLE.iter().find(|(n, _)| *n == name)?.1;
    // Same level as the window preview (0.25 at 100%), capped so 200% can't clip.
    let peak = (0.25 * volume as f32 / 100.0).min(0.9);
    let end = notes.iter().map(|(_, at, d)| at + d).fold(0.0, f32::max) + 0.02;
    let mut pcm = vec![0f32; (end * RATE as f32) as usize + 1];
    for &(freq, at, dur) in notes {
        let first = (at * RATE as f32) as usize;
        let n = (dur * RATE as f32) as usize;
        for i in 0..n.min(pcm.len().saturating_sub(first)) {
            let t = i as f32 / RATE as f32;
            // 10 ms attack, then an exponential fall to near silence.
            let env = if t < 0.01 { t / 0.01 } else { 0.0001f32.powf((t - 0.01) / (dur - 0.01).max(0.001)) };
            pcm[first + i] += (std::f32::consts::TAU * freq * t).sin() * env * peak;
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(w: &[u8]) -> i16 {
        w[44..].chunks_exact(2).map(|s| i16::from_le_bytes([s[0], s[1]]).abs()).max().unwrap()
    }

    #[test]
    fn every_tone_is_a_valid_nonsilent_wav() {
        for (name, _) in TABLE {
            let w = wav(name, 100).unwrap();
            assert_eq!(&w[..4], b"RIFF");
            assert_eq!(&w[8..16], b"WAVEfmt ");
            assert_eq!(&w[36..40], b"data");
            let data = u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize;
            assert_eq!(data, w.len() - 44, "{name}: header length matches");
            assert!(peak(&w) > 1000, "{name}: audible");
        }
    }

    #[test]
    fn volume_scales_and_never_clips() {
        let quiet = peak(&wav("chime", 50).unwrap());
        let normal = peak(&wav("chime", 100).unwrap());
        let loud = peak(&wav("chime", 200).unwrap());
        assert!(quiet < normal && normal < loud);
        assert!(loud < 32767);
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert!(wav("nope", 100).is_none());
        assert!(!is_tone(""));
        assert!(is_tone("sparkle"));
    }
}
