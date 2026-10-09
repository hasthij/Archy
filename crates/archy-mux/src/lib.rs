//! MP4 writing of already-encoded samples. Media Foundation's sink writer is
//! used purely as a muxer (input type == output type), so nothing is re-encoded:
//! saving a clip is a file write, not a GPU job.

use std::path::Path;

use windows::core::{Result, HSTRING};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSample, IMFSinkWriter, MFCreateAttributes,
    MFCreateSinkWriterFromURL, MFTranscodeContainerType_MPEG4, MF_SINK_WRITER_DISABLE_THROTTLING,
    MF_TRANSCODE_CONTAINERTYPE,
};

pub struct Mp4Writer {
    writer: IMFSinkWriter,
    stream: u32,
    first_time: Option<i64>,
}

impl Mp4Writer {
    /// `encoded_type`: the encoder's current output type (codec, size, rate, sequence header).
    pub fn create(path: &Path, encoded_type: &IMFMediaType) -> Result<Self> {
        let mut attrs: Option<IMFAttributes> = None;
        unsafe { MFCreateAttributes(&mut attrs, 2)? };
        let attrs = attrs.expect("MFCreateAttributes succeeded without attributes");
        unsafe {
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs)?;
            let stream = writer.AddStream(encoded_type)?;
            writer.SetInputMediaType(stream, encoded_type, None)?;
            writer.BeginWriting()?;
            Ok(Self { writer, stream, first_time: None })
        }
    }

    /// Writes one compressed sample; timestamps are rebased so the file starts at 0.
    pub fn write(&mut self, sample: &IMFSample) -> Result<()> {
        unsafe {
            let t = sample.GetSampleTime()?;
            let first = *self.first_time.get_or_insert(t);
            sample.SetSampleTime(t - first)?;
            self.writer.WriteSample(self.stream, sample)
        }
    }

    pub fn finish(self) -> Result<()> {
        unsafe { self.writer.Finalize() }
    }
}
