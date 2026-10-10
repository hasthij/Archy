//! Windows' built-in AAC encoder (Media Foundation MFT), fed 48 kHz stereo PCM.

use std::mem::ManuallyDrop;

use windows::core::{Error, Result};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;

use crate::{CHANNELS, SAMPLE_RATE};

/// What a muxer needs to write the track.
#[derive(Clone, Debug)]
pub struct AacInfo {
    pub sample_rate: u32,
    pub channels: u32,
    pub bytes_per_second: u32,
    /// MF_MT_USER_DATA of the encoder's output type (carries AudioSpecificConfig).
    pub user_data: Vec<u8>,
}

pub struct AacEncoder {
    transform: IMFTransform,
    out_size: u32,
    pub info: AacInfo,
}

// Used only from its capture thread.
unsafe impl Send for AacEncoder {}

/// The encoder accepts 96/128/160/192 kbps (12000..24000 bytes/s).
fn bytes_per_second(kbps: u32) -> u32 {
    match kbps {
        0..=111 => 12_000,
        112..=143 => 16_000,
        144..=175 => 20_000,
        _ => 24_000,
    }
}

fn pcm_type() -> Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType()?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS)?;
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, CHANNELS * 2)?;
        t.SetUINT32(
            &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            SAMPLE_RATE * CHANNELS * 2,
        )?;
        Ok(t)
    }
}

impl AacEncoder {
    pub fn new(kbps: u32) -> Result<Self> {
        unsafe {
            let input = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Audio,
                guidSubtype: MFAudioFormat_PCM,
            };
            let output = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Audio,
                guidSubtype: MFAudioFormat_AAC,
            };
            let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFTEnumEx(
                MFT_CATEGORY_AUDIO_ENCODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut list,
                &mut count,
            )?;
            let mut activates = Vec::new();
            for i in 0..count as usize {
                if let Some(a) = std::ptr::read(list.add(i)) {
                    activates.push(a);
                }
            }
            if !list.is_null() {
                CoTaskMemFree(Some(list as *const _));
            }
            let activate = activates
                .into_iter()
                .next()
                .ok_or_else(|| Error::new(E_FAIL, "no AAC encoder"))?;
            let transform: IMFTransform = activate.ActivateObject()?;

            let bps = bytes_per_second(kbps);
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, SAMPLE_RATE)?;
            out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, CHANNELS)?;
            out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bps)?;
            // Raw AAC frames (no ADTS headers): what MP4 stores.
            out.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            // The AAC encoder wants its input type first.
            transform.SetInputType(0, &pcm_type()?, 0)?;
            transform.SetOutputType(0, &out, 0)?;

            let current = transform.GetOutputCurrentType(0)?;
            let mut user_data = Vec::new();
            let size = current.GetBlobSize(&MF_MT_USER_DATA).unwrap_or(0);
            if size > 0 {
                user_data.resize(size as usize, 0);
                current.GetBlob(&MF_MT_USER_DATA, &mut user_data, None)?;
            }
            let info = transform.GetOutputStreamInfo(0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(Self {
                transform,
                out_size: info.cbSize.max(8192),
                info: AacInfo {
                    sample_rate: SAMPLE_RATE,
                    channels: CHANNELS,
                    bytes_per_second: bps,
                    user_data,
                },
            })
        }
    }

    /// Encodes interleaved 16-bit PCM starting at `time` (100 ns); emits finished
    /// AAC frames as (data, time, duration).
    pub fn encode(
        &mut self,
        pcm: &[i16],
        time: i64,
        mut emit: impl FnMut(&[u8], i64, i64),
    ) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        unsafe {
            let bytes = pcm.len() * 2;
            let buffer = MFCreateMemoryBuffer(bytes as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr() as *const u8, ptr, bytes);
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            let frames = (pcm.len() as u32 / CHANNELS) as i64;
            sample.SetSampleDuration(frames * 10_000_000 / SAMPLE_RATE as i64)?;
            self.transform.ProcessInput(0, &sample, 0)?;
        }
        self.drain(&mut emit)
    }

    fn drain(&mut self, emit: &mut impl FnMut(&[u8], i64, i64)) -> Result<()> {
        loop {
            unsafe {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(self.out_size)?)?;
                let mut out = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(Some(sample)),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                };
                let mut status = 0u32;
                let r =
                    self.transform
                        .ProcessOutput(0, std::slice::from_mut(&mut out), &mut status);
                let sample = ManuallyDrop::into_inner(out.pSample);
                drop(ManuallyDrop::into_inner(out.pEvents));
                match r {
                    Ok(()) => {
                        let Some(sample) = sample else { continue };
                        let time = sample.GetSampleTime().unwrap_or(0);
                        let duration = sample.GetSampleDuration().unwrap_or(213_333);
                        let buf = sample.ConvertToContiguousBuffer()?;
                        let mut p = std::ptr::null_mut();
                        let mut len = 0u32;
                        buf.Lock(&mut p, None, Some(&mut len))?;
                        emit(std::slice::from_raw_parts(p, len as usize), time, duration);
                        buf.Unlock()?;
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) => return Err(e),
                }
            }
        }
    }
}
