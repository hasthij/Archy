//! "Discord copy": re-encodes a clip to fit an upload limit (Discord's free 10 MB).
//!
//! Media Foundation does the whole job on the GPU where it can: hardware decode,
//! scaling in the video processor, hardware H.264 encode. The bitrate is chosen
//! from the clip's length so the file lands just under the limit; the original
//! is never touched. Only the first audio track (everything you heard) is kept.

use std::path::{Path, PathBuf};

use windows::core::{Result, HSTRING};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Media::MediaFoundation::*;

use crate::init_thread;

const AUDIO_BPS: u32 = 96_000;

#[derive(Debug)]
pub struct CopyInfo {
    pub path: PathBuf,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

/// `<name> (Discord).mp4` next to the original.
pub fn copy_path(src: &Path) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    src.with_file_name(format!("{stem} (Discord).mp4"))
}

/// Encodes `src` into `dst` so it stays under `limit_bytes`.
pub fn make_copy(src: &Path, dst: &Path, limit_bytes: u64) -> Result<CopyInfo> {
    init_thread()?;
    let duration = duration_seconds(src)?.max(1.0);
    // Leave room for the container and encoder overshoot.
    let mut budget = limit_bytes as f64 * 0.90;
    for _ in 0..3 {
        let total_bps = budget * 8.0 / duration;
        let video_bps = (total_bps - AUDIO_BPS as f64).max(250_000.0) as u32;
        let info = transcode(src, dst, video_bps)?;
        if info.bytes <= limit_bytes {
            return Ok(info);
        }
        // Overshot (very busy footage): try again a bit smaller.
        budget *= 0.85;
    }
    Err(windows::core::Error::new(
        E_FAIL,
        "couldn't get the clip under the size limit",
    ))
}

fn reader_for(src: &Path, attrs: Option<&IMFAttributes>) -> Result<IMFSourceReader> {
    unsafe { MFCreateSourceReaderFromURL(&HSTRING::from(src.as_os_str()), attrs) }
}

fn duration_seconds(src: &Path) -> Result<f64> {
    let reader = reader_for(src, None)?;
    let value = unsafe {
        reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)?
    };
    // PROPVARIANT VT_UI8, 100 ns units.
    let hns = unsafe { value.Anonymous.Anonymous.Anonymous.uhVal };
    Ok(hns as f64 / 1e7)
}

/// Index of the first stream of `major` type.
fn first_stream(reader: &IMFSourceReader, major: &windows::core::GUID) -> Option<u32> {
    (0..16u32).find(|&i| unsafe {
        reader
            .GetNativeMediaType(i, 0)
            .and_then(|t| t.GetGUID(&MF_MT_MAJOR_TYPE))
            .is_ok_and(|g| g == *major)
    })
}

fn transcode(src: &Path, dst: &Path, video_bps: u32) -> Result<CopyInfo> {
    unsafe {
        // One GPU device shared by decoder, scaler and encoder: frames stay on the GPU.
        let gpu = archy_gpu::device::Gpu::new(None, 0)?;
        let mut token = 0u32;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.expect("no DXGI manager");
        manager.ResetDevice(&gpu.device, token)?;

        let mut ra: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut ra, 3)?;
        let ra = ra.expect("attributes");
        ra.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &manager)?;
        ra.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        // Lets the reader scale and convert (on the GPU's video processor).
        ra.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;
        let reader = reader_for(src, Some(&ra))?;

        let video = first_stream(&reader, &MFMediaType_Video)
            .ok_or_else(|| windows::core::Error::new(E_FAIL, "no video"))?;
        let audio = first_stream(&reader, &MFMediaType_Audio);
        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
        reader.SetStreamSelection(video, true)?;
        if let Some(a) = audio {
            reader.SetStreamSelection(a, true)?;
        }

        let native = reader.GetNativeMediaType(video, 0)?;
        let (src_w, src_h) = unpack(native.GetUINT64(&MF_MT_FRAME_SIZE)?);
        let rate = native.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(pack(60, 1));
        // Resolution the bitrate can carry cleanly.
        let target_h = if video_bps >= 1_800_000 {
            720
        } else if video_bps >= 700_000 {
            480
        } else {
            360
        };
        let h = src_h.min(target_h) & !1;
        let w = ((src_w as u64 * h as u64 / src_h.max(1) as u64) as u32) & !1;

        let raw = MFCreateMediaType()?;
        raw.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        raw.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        raw.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        raw.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
        raw.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        raw.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        reader.SetCurrentMediaType(video, None, &raw)?;
        if let Some(a) = audio {
            let pcm = MFCreateMediaType()?;
            pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            reader.SetCurrentMediaType(a, None, &pcm)?;
        }

        let mut wa: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut wa, 4)?;
        let wa = wa.expect("attributes");
        wa.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;
        wa.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        wa.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        wa.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        let _ = std::fs::remove_file(dst);
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(dst.as_os_str()), None, &wa)?;

        let vout = MFCreateMediaType()?;
        vout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        vout.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        vout.SetUINT32(&MF_MT_AVG_BITRATE, video_bps)?;
        vout.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        vout.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
        vout.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        vout.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        let vstream = writer.AddStream(&vout)?;
        writer.SetInputMediaType(vstream, &reader.GetCurrentMediaType(video)?, None)?;

        let astream = match audio {
            Some(a) => {
                let input = reader.GetCurrentMediaType(a)?;
                let aout = MFCreateMediaType()?;
                aout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                aout.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                aout.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                aout.SetUINT32(
                    &MF_MT_AUDIO_SAMPLES_PER_SECOND,
                    input.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?,
                )?;
                aout.SetUINT32(
                    &MF_MT_AUDIO_NUM_CHANNELS,
                    input.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?,
                )?;
                aout.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AUDIO_BPS / 8)?;
                let s = writer.AddStream(&aout)?;
                writer.SetInputMediaType(s, &input, None)?;
                Some((a, s))
            }
            None => None,
        };

        writer.BeginWriting()?;
        let mut open = 1 + audio.is_some() as u32;
        while open > 0 {
            let mut index = 0u32;
            let mut flags = 0u32;
            let mut time = 0i64;
            let mut sample = None;
            reader.ReadSample(
                MF_SOURCE_READER_ANY_STREAM.0 as u32,
                0,
                Some(&mut index),
                Some(&mut flags),
                Some(&mut time),
                Some(&mut sample),
            )?;
            let target = if index == video {
                Some(vstream)
            } else {
                astream.filter(|(a, _)| *a == index).map(|(_, s)| s)
            };
            if let (Some(sample), Some(stream)) = (sample, target) {
                writer.WriteSample(stream, &sample)?;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                open -= 1;
            }
        }
        writer.Finalize()?;
        drop(writer);
        Ok(CopyInfo {
            path: dst.to_path_buf(),
            bytes: std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0),
            width: w,
            height: h,
        })
    }
}
