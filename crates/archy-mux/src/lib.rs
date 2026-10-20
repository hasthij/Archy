//! MP4 writing of already-encoded packets. Media Foundation's sink writer is
//! used purely as a muxer (input type == output type), so nothing is
//! re-encoded: saving a clip is a file write, not a GPU job.

use std::path::Path;

use archy_gpu::probe::Codec;
use windows::core::{Result, HSTRING};
use windows::Win32::Media::MediaFoundation::*;

mod headers;

pub use headers::codec_header;

/// An AAC audio track as produced by the encoder.
pub struct AudioTrack<'a> {
    pub sample_rate: u32,
    pub channels: u32,
    pub bytes_per_second: u32,
    /// The encoder's MF_MT_USER_DATA (carries the AudioSpecificConfig).
    pub user_data: &'a [u8],
}

pub struct Mp4Writer {
    writer: IMFSinkWriter,
    video: u32,
    audio: Vec<u32>,
    /// Packet time that becomes 0 in the file (100 ns). `None`: the first video packet.
    base: Option<i64>,
    /// Last time written per stream (video first, then audio tracks): times must
    /// rise, or the sink writer fails the whole save ("The parameter is incorrect").
    last: Vec<Option<i64>>,
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

impl Mp4Writer {
    /// Video only, starting at the first packet written.
    pub fn create(
        path: &Path,
        codec: Codec,
        width: u32,
        height: u32,
        fps: u32,
        first_keyframe: &[u8],
    ) -> Result<Self> {
        Self::create_with_audio(path, codec, width, height, fps, first_keyframe, &[], None)
    }

    /// `first_keyframe`: the stream's first keyframe; the codec header (SPS/PPS,
    /// VPS/SPS/PPS or AV1 sequence header) is taken from it. `base`: the packet
    /// time that becomes 0 in the file, shared by video and audio so they stay in sync.
    #[allow(clippy::too_many_arguments)]
    pub fn create_with_audio(
        path: &Path,
        codec: Codec,
        width: u32,
        height: u32,
        fps: u32,
        first_keyframe: &[u8],
        audio: &[AudioTrack],
        base: Option<i64>,
    ) -> Result<Self> {
        Self::open(path, codec, width, height, fps, first_keyframe, audio, base, false)
    }

    /// Like `create_with_audio`, but fragmented MP4: written in small pieces as it
    /// goes, so the file plays even if writing stops without `finish` (a crash
    /// mid-session). For long recordings.
    #[allow(clippy::too_many_arguments)]
    pub fn create_fragmented(
        path: &Path,
        codec: Codec,
        width: u32,
        height: u32,
        fps: u32,
        first_keyframe: &[u8],
        audio: &[AudioTrack],
        base: Option<i64>,
    ) -> Result<Self> {
        Self::open(path, codec, width, height, fps, first_keyframe, audio, base, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        path: &Path,
        codec: Codec,
        width: u32,
        height: u32,
        fps: u32,
        first_keyframe: &[u8],
        audio: &[AudioTrack],
        base: Option<i64>,
        fragmented: bool,
    ) -> Result<Self> {
        unsafe {
            let ty = MFCreateMediaType()?;
            ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            ty.SetGUID(&MF_MT_SUBTYPE, &codec.subtype())?;
            ty.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            ty.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            // BT.709 limited range, as recorded (players guessed otherwise).
            let _ = ty.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32);
            let _ = ty.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32);
            let _ = ty.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32);
            let _ = ty.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
            ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            ty.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            match codec_header(codec, first_keyframe) {
                Some(header) => ty.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &header)?,
                None => {
                    return Err(windows::core::Error::new(
                        windows::Win32::Foundation::E_FAIL,
                        "the clip's first keyframe has no codec header",
                    ))
                }
            }

            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 2)?;
            let attrs = attrs.expect("MFCreateAttributes succeeded without attributes");
            let container = if fragmented { MFTranscodeContainerType_FMPEG4 } else { MFTranscodeContainerType_MPEG4 };
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &container)?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            // No "faststart" (MF_MPEG4SINK_MOOV_BEFORE_MDAT): Media Foundation then
            // re-reads the file it opened write-only, and every save failed with
            // "Access is denied" (2026-10-15). Fast saves matter more.
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs)?;
            let video = writer.AddStream(&ty)?;
            writer.SetInputMediaType(video, &ty, None)?;

            let mut audio_streams = Vec::with_capacity(audio.len());
            for track in audio {
                let a = MFCreateMediaType()?;
                a.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                a.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                a.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, track.sample_rate)?;
                a.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, track.channels)?;
                a.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
                a.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, track.bytes_per_second)?;
                a.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
                if !track.user_data.is_empty() {
                    a.SetBlob(&MF_MT_USER_DATA, track.user_data)?;
                }
                let stream = writer.AddStream(&a)?;
                writer.SetInputMediaType(stream, &a, None)?;
                audio_streams.push(stream);
            }
            writer.BeginWriting()?;
            let streams = 1 + audio_streams.len();
            Ok(Self {
                writer,
                video,
                audio: audio_streams,
                base,
                last: vec![None; streams],
            })
        }
    }

    fn sample(data: &[u8], time: i64, duration: i64, keyframe: bool) -> Result<IMFSample> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            sample.SetUINT32(&MFSampleExtension_CleanPoint, keyframe as u32)?;
            Ok(sample)
        }
    }

    /// Makes `time` (relative to the base) strictly rise on stream `index`.
    fn next_time(&mut self, index: usize, time: i64) -> i64 {
        let time = match self.last[index] {
            Some(last) if time <= last => last + 1,
            _ => time,
        };
        self.last[index] = Some(time);
        time
    }

    /// Writes one compressed video frame (time in 100 ns).
    pub fn write(&mut self, data: &[u8], time: i64, duration: i64, keyframe: bool) -> Result<()> {
        if data.is_empty() {
            return Ok(()); // an empty packet (skipped frame) is rejected by the writer
        }
        let base = *self.base.get_or_insert(time);
        let t = self.next_time(0, time - base);
        let sample = Self::sample(data, t, duration.max(1), keyframe)?;
        unsafe { self.writer.WriteSample(self.video, &sample) }
    }

    /// Writes one AAC frame to audio track `track` (index into the tracks given at
    /// creation). Frames from before the clip start are skipped.
    pub fn write_audio(
        &mut self,
        track: usize,
        data: &[u8],
        time: i64,
        duration: i64,
    ) -> Result<()> {
        let Some(base) = self.base else { return Ok(()) };
        if time < base || data.is_empty() {
            return Ok(());
        }
        let t = self.next_time(1 + track, time - base);
        let sample = Self::sample(data, t, duration.max(1), true)?;
        unsafe { self.writer.WriteSample(self.audio[track], &sample) }
    }

    pub fn finish(self) -> Result<()> {
        unsafe { self.writer.Finalize() }
    }
}
