//! Prints the loudness of every audio track in a clip.
//! cargo run -p archy-core --example track_levels -- <clip.mp4>
use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::*;

fn main() {
    let path = std::env::args().nth(1).expect("clip path");
    archy_core::init_thread().unwrap();
    unsafe {
        let mut track = 0;
        for stream in 0..8u32 {
            let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_str()), None).unwrap();
            let Ok(native) = reader.GetNativeMediaType(stream, 0) else {
                break;
            };
            if native.GetGUID(&MF_MT_MAJOR_TYPE).unwrap() != MFMediaType_Audio {
                continue;
            }
            track += 1;
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .unwrap();
            reader.SetStreamSelection(stream, true).unwrap();
            let pcm = MFCreateMediaType().unwrap();
            pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).unwrap();
            pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).unwrap();
            pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).unwrap();
            reader.SetCurrentMediaType(stream, None, &pcm).unwrap();
            let (mut peak, mut sum, mut n) = (0i32, 0f64, 0u64);
            loop {
                let mut flags = 0u32;
                let mut sample = None;
                reader
                    .ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
                    .unwrap();
                if let Some(s) = sample {
                    let buf = s.ConvertToContiguousBuffer().unwrap();
                    let mut p = std::ptr::null_mut();
                    let mut len = 0u32;
                    buf.Lock(&mut p, None, Some(&mut len)).unwrap();
                    for &v in std::slice::from_raw_parts(p as *const i16, len as usize / 2) {
                        peak = peak.max((v as i32).abs());
                        sum += (v as f64) * (v as f64);
                        n += 1;
                    }
                    buf.Unlock().unwrap();
                }
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
            }
            let rms = (sum / n.max(1) as f64).sqrt();
            let db = |x: f64| {
                if x <= 0.0 {
                    -99.0
                } else {
                    20.0 * (x / 32768.0).log10()
                }
            };
            println!(
                "audio track {track}: peak {:6.1} dB, average {:6.1} dB  {}",
                db(peak as f64),
                db(rms),
                if peak < 30 {
                    "(silent)"
                } else if rms < 30.0 {
                    "(very quiet)"
                } else {
                    ""
                }
            );
        }
    }
}
