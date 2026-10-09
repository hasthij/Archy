//! MP4 writing of already-encoded packets. Media Foundation's sink writer is
//! used purely as a muxer (input type == output type), so nothing is
//! re-encoded: saving a clip is a file write, not a GPU job.

use std::path::Path;

use archy_gpu::probe::Codec;
use windows::core::{Result, HSTRING};
use windows::Win32::Media::MediaFoundation::*;

mod headers;

pub use headers::codec_header;

pub struct Mp4Writer {
    writer: IMFSinkWriter,
    stream: u32,
    first_time: Option<i64>,
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

impl Mp4Writer {
    /// `first_keyframe`: the stream's first keyframe; the codec header
    /// (SPS/PPS, VPS/SPS/PPS or AV1 sequence header) is taken from it.
    pub fn create(
        path: &Path,
        codec: Codec,
        width: u32,
        height: u32,
        fps: u32,
        first_keyframe: &[u8],
    ) -> Result<Self> {
        unsafe {
            let ty = MFCreateMediaType()?;
            ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            ty.SetGUID(&MF_MT_SUBTYPE, &codec.subtype())?;
            ty.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            ty.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            ty.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            if let Some(header) = codec_header(codec, first_keyframe) {
                ty.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &header)?;
            }

            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 2)?;
            let attrs = attrs.expect("MFCreateAttributes succeeded without attributes");
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs)?;
            let stream = writer.AddStream(&ty)?;
            writer.SetInputMediaType(stream, &ty, None)?;
            writer.BeginWriting()?;
            Ok(Self {
                writer,
                stream,
                first_time: None,
            })
        }
    }

    /// Writes one compressed frame; timestamps (100 ns) are rebased so the file starts at 0.
    pub fn write(&mut self, data: &[u8], time: i64, duration: i64, keyframe: bool) -> Result<()> {
        let first = *self.first_time.get_or_insert(time);
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time - first)?;
            sample.SetSampleDuration(duration)?;
            sample.SetUINT32(&MFSampleExtension_CleanPoint, keyframe as u32)?;
            self.writer.WriteSample(self.stream, &sample)
        }
    }

    pub fn finish(self) -> Result<()> {
        unsafe { self.writer.Finalize() }
    }
}
