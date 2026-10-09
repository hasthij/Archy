//! Media Foundation encoder MFTs (NVENC / AMD AMF / Intel Quick Sync behind one interface).

use std::mem::ManuallyDrop;
use std::sync::mpsc::Receiver;

use archy_gpu::device::Gpu;
use archy_gpu::probe::{encoder_activates, friendly_name};
use archy_gpu::SendPtr;
use windows::core::{Interface, Result, GUID};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use crate::{Config, Input, Packet, Stats};
use windows::Win32::Foundation::VARIANT_TRUE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4,
};

pub struct MfEncoder {
    transform: IMFTransform,
    _manager: IMFDXGIDeviceManager,
    input_id: u32,
    output_id: u32,
    provides_samples: bool,
    output_buffer_size: u32,
    frame_duration: i64,
    pub name: String,
    pub rate_control: &'static str,
}

// The MFT is created free-threaded (async, unlocked) and the device it uses is
// multithread-protected, so it can be driven from the encoder thread.
unsafe impl Send for MfEncoder {}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

impl MfEncoder {
    /// Media Foundation must be started on the calling thread's process.
    pub fn new(gpu: &Gpu, cfg: &Config) -> Result<Self> {
        let activate = encoder_activates(gpu.luid, cfg.codec)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                windows::core::Error::new(
                    MF_E_TOPO_CODEC_NOT_FOUND,
                    "no hardware encoder for this codec",
                )
            })?;
        let name = friendly_name(&activate);
        let transform: IMFTransform = unsafe { activate.ActivateObject()? };

        unsafe {
            let attrs = transform.GetAttributes()?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
        }

        let mut ins = [0u32; 1];
        let mut outs = [0u32; 1];
        let (input_id, output_id) = match unsafe { transform.GetStreamIDs(&mut ins, &mut outs) } {
            Ok(()) => (ins[0], outs[0]),
            Err(_) => (0, 0), // E_NOTIMPL: fixed stream ids
        };

        let mut token = 0u32;
        let mut manager = None;
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager)? };
        let manager = manager.expect("MFCreateDXGIDeviceManager returned no manager");
        unsafe {
            manager.ResetDevice(&gpu.device, token)?;
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
        }

        // Rate control must be chosen before the media types are set.
        let rate_control = configure_codec_api(&transform, cfg);

        unsafe {
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &cfg.codec.subtype())?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, cfg.max_bitrate)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(cfg.width, cfg.height))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(cfg.fps, 1))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            transform.SetOutputType(output_id, &out, 0)?;

            let input = find_input_type(&transform, input_id, &MFVideoFormat_NV12)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, pack(cfg.width, cfg.height))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, pack(cfg.fps, 1))?;
            transform.SetInputType(input_id, &input, 0)?;
        }

        let info = unsafe { transform.GetOutputStreamInfo(output_id)? };
        let provides =
            (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;

        Ok(Self {
            transform,
            _manager: manager,
            input_id,
            output_id,
            provides_samples: info.dwFlags & provides != 0,
            output_buffer_size: info.cbSize.max(1 << 20),
            frame_duration: 10_000_000 / cfg.fps as i64,
            name,
            rate_control,
        })
    }

    /// Encodes until `inputs` disconnects, then drains. `on_sample(sample, is_keyframe)`
    /// receives every compressed sample in order.
    pub fn run(
        &self,
        inputs: Receiver<SendPtr<Input>>,
        mut on_packet: impl FnMut(Packet) -> Result<()>,
    ) -> Result<Stats> {
        let events: IMFMediaEventGenerator = self.transform.cast()?;
        let mut stats = Stats::default();
        let mut draining = false;
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        loop {
            let event = unsafe { events.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0))? };
            let kind = unsafe { event.GetType()? };
            if kind == METransformNeedInput.0 as u32 {
                if draining {
                    continue;
                }
                match inputs.recv() {
                    Ok(SendPtr(input)) => {
                        let sample = self.input_sample(&input)?;
                        unsafe { self.transform.ProcessInput(self.input_id, &sample, 0)? };
                        stats.frames_in += 1;
                    }
                    Err(_) => unsafe {
                        self.transform
                            .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
                        self.transform
                            .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
                        draining = true;
                    },
                }
            } else if kind == METransformHaveOutput.0 as u32 {
                self.pull_output(&mut on_packet, &mut stats)?;
            } else if kind == METransformDrainComplete.0 as u32 {
                break;
            }
        }
        Ok(stats)
    }

    fn input_sample(&self, input: &Input) -> Result<IMFSample> {
        unsafe {
            let buffer =
                MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &input.texture, 0, false)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(input.time)?;
            sample.SetSampleDuration(self.frame_duration)?;
            Ok(sample)
        }
    }

    fn pull_output(
        &self,
        on_packet: &mut impl FnMut(Packet) -> Result<()>,
        stats: &mut Stats,
    ) -> Result<()> {
        let own_sample = if self.provides_samples {
            None
        } else {
            unsafe {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.output_buffer_size)?)?;
                Some(s)
            }
        };
        let mut buffer = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: self.output_id,
            pSample: ManuallyDrop::new(own_sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        };
        let mut status = 0u32;
        let result = unsafe {
            self.transform
                .ProcessOutput(0, std::slice::from_mut(&mut buffer), &mut status)
        };
        let sample = ManuallyDrop::into_inner(buffer.pSample);
        drop(ManuallyDrop::into_inner(buffer.pEvents));

        match result {
            Ok(()) => {
                if let Some(sample) = sample {
                    emit(&sample, on_packet, stats)?;
                }
                Ok(())
            }
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => unsafe {
                let t = self.transform.GetOutputAvailableType(self.output_id, 0)?;
                self.transform.SetOutputType(self.output_id, &t, 0)
            },
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Hands a compressed MF sample to `on_packet` as a borrowed packet.
fn emit(
    sample: &IMFSample,
    on_packet: &mut impl FnMut(Packet) -> Result<()>,
    stats: &mut Stats,
) -> Result<()> {
    unsafe {
        let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
        let time = sample.GetSampleTime()?;
        let duration = sample.GetSampleDuration().unwrap_or(0);
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len))?;
        let data = strip_ivf(std::slice::from_raw_parts(ptr, len as usize));
        let packet = Packet {
            data,
            time,
            duration,
            keyframe,
        };
        stats.count(&packet);
        let result = on_packet(packet);
        buffer.Unlock()?;
        result
    }
}

/// Some AV1 MFTs (Intel's) wrap output in IVF: a 32-byte file header on the
/// first packet and a 12-byte frame header (size + pts) on every packet.
fn strip_ivf(data: &[u8]) -> &[u8] {
    let data = if data.starts_with(b"DKIF") && data.len() >= 32 {
        &data[32..]
    } else {
        data
    };
    if data.len() >= 12 {
        let size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        if size == data.len() - 12 {
            return &data[12..];
        }
    }
    data
}

fn find_input_type(transform: &IMFTransform, id: u32, subtype: &GUID) -> Result<IMFMediaType> {
    for i in 0.. {
        let t = unsafe { transform.GetInputAvailableType(id, i)? };
        if unsafe { t.GetGUID(&MF_MT_SUBTYPE)? } == *subtype {
            return Ok(t);
        }
    }
    unreachable!()
}

/// Best-effort encoder tuning. Returns the rate-control mode that was accepted.
fn configure_codec_api(transform: &IMFTransform, cfg: &Config) -> &'static str {
    let Ok(api) = transform.cast::<ICodecAPI>() else {
        return "driver default";
    };
    let set = |key: &GUID, value: VARIANT| unsafe { api.SetValue(key, &value).is_ok() };

    set(&CODECAPI_AVEncMPVGOPSize, var_u32(cfg.gop));
    // Low-latency mode turns off lookahead and B-frame reordering: less GPU work,
    // packets out sooner. Quality is recovered with bitrate.
    set(&CODECAPI_AVLowLatencyMode, var_bool());
    set(&CODECAPI_AVEncMPVDefaultBPictureCount, var_u32(0u32));
    set(&CODECAPI_AVEncCommonMaxBitRate, var_u32(cfg.max_bitrate));

    if set(
        &CODECAPI_AVEncCommonRateControlMode,
        var_u32(eAVEncCommonRateControlMode_Quality.0 as u32),
    ) {
        set(&CODECAPI_AVEncCommonQuality, var_u32(cfg.quality));
        return "constant quality";
    }
    if set(
        &CODECAPI_AVEncCommonRateControlMode,
        var_u32(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32),
    ) {
        set(
            &CODECAPI_AVEncCommonMeanBitRate,
            var_u32(cfg.max_bitrate / 10 * 7),
        );
        return "peak-constrained VBR";
    }
    "driver default"
}

fn variant(vt: VARENUM, value: VARIANT_0_0_0) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt,
                Anonymous: value,
                ..Default::default()
            }),
        },
    }
}

fn var_u32(v: u32) -> VARIANT {
    variant(VT_UI4, VARIANT_0_0_0 { ulVal: v })
}

fn var_bool() -> VARIANT {
    variant(
        VT_BOOL,
        VARIANT_0_0_0 {
            boolVal: VARIANT_TRUE,
        },
    )
}
