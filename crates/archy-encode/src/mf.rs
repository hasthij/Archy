//! Media Foundation encoder MFTs (NVENC / AMD AMF / Intel Quick Sync behind one interface).

use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use archy_gpu::device::Gpu;
use archy_gpu::probe::{encoder_activates, friendly_name};
use archy_gpu::SendPtr;
use archy_gpu::probe::Codec;
use windows::core::{implement, Error, Interface, Ref, Result, GUID};
use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_TEXTURE2D_DESC,
};

use crate::{random_access, Config, Input, Packet, Stats};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4,
};

pub struct MfEncoder {
    transform: IMFTransform,
    _manager: IMFDXGIDeviceManager,
    input_id: u32,
    output_id: u32,
    provides_samples: bool,
    output_buffer_size: u32,
    frame_duration: i64,
    codec: Codec,
    /// Frames between keyframes that were asked for.
    gop: u32,
    /// For forcing keyframes when the driver ignores the interval.
    codec_api: Option<ICodecAPI>,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// Encoder-owned copies of the input frames (see `input_sample`).
    pool: Vec<Slot>,
    pub name: String,
    pub rate_control: &'static str,
}

/// One input texture and whether the encoder still holds it.
struct Slot {
    texture: ID3D11Texture2D,
    busy: Arc<AtomicBool>,
}

/// More than any encoder should ever hold; past this the encoder is stuck.
const MAX_POOL: usize = 32;

/// Called by Media Foundation when the encoder lets go of an input sample.
#[implement(IMFAsyncCallback)]
struct Released(Arc<AtomicBool>);

impl IMFAsyncCallback_Impl for Released_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, _result: Ref<IMFAsyncResult>) -> Result<()> {
        self.0.store(false, Ordering::Release);
        Ok(())
    }
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
            // No MF_LOW_LATENCY: on AMD/NVIDIA it also shrinks the rate-control
            // buffer to about one frame, which crushes keyframes into blocks.
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
            // The average to aim for (the max is only a ceiling): with the max here,
            // drivers aimed for it and files came out about twice the chosen size.
            let average = cfg.target_bitrate.unwrap_or(cfg.max_bitrate / 10 * 7).min(cfg.max_bitrate);
            out.SetUINT32(&MF_MT_AVG_BITRATE, average)?;
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
            codec_api: transform.cast::<ICodecAPI>().ok(),
            transform,
            _manager: manager,
            input_id,
            output_id,
            provides_samples: info.dwFlags & provides != 0,
            // At least one raw frame: a big keyframe can exceed a fixed 1 MB.
            output_buffer_size: info.cbSize.max(1 << 20).max(cfg.width * cfg.height * 3 / 2),
            frame_duration: 10_000_000 / cfg.fps as i64,
            codec: cfg.codec,
            gop: cfg.gop.max(1),
            device: gpu.device.clone(),
            context: gpu.context.clone(),
            pool: Vec::new(),
            name,
            rate_control,
        })
    }

    /// Encodes until `inputs` disconnects, then drains. `on_sample(sample, is_keyframe)`
    /// receives every compressed sample in order.
    pub fn run(
        &mut self,
        inputs: Receiver<SendPtr<Input>>,
        mut on_packet: impl FnMut(Packet) -> Result<()>,
    ) -> Result<Stats> {
        let events: IMFMediaEventGenerator = self.transform.cast()?;
        let mut stats = Stats::default();
        let mut draining = false;
        // Packets since the last real keyframe, and whether one was requested.
        let mut since_key = 0u32;
        let mut force_pending = false;
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
                        // Some drivers ignore the keyframe interval; clips can only start
                        // on a keyframe, so ask for one when it's overdue.
                        if !force_pending && since_key >= self.gop + self.gop / 4 {
                            if let Some(api) = &self.codec_api {
                                unsafe {
                                    let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &var_u32(1));
                                }
                            }
                            force_pending = true;
                            stats.forced_keyframes += 1;
                        }
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
                match self.pull_output(&mut on_packet, &mut stats)? {
                    Some(true) => {
                        since_key = 0;
                        force_pending = false;
                    }
                    Some(false) => since_key += 1,
                    None => {}
                }
            } else if kind == METransformDrainComplete.0 as u32 {
                break;
            }
        }
        Ok(stats)
    }

    /// Wraps a copy of the frame in a sample. The recorder reuses its textures in
    /// rotation and an async encoder can hold several frames, so handing the
    /// recorder's texture straight over let a held frame be overwritten by a later
    /// one (mixed-up frames, then blocks until the next keyframe). The copy lives
    /// in a slot that is only reused once Media Foundation reports the encoder
    /// has released it.
    fn input_sample(&mut self, input: &Input) -> Result<IMFSample> {
        let slot = self.free_slot(&input.texture)?;
        let Slot { texture, busy } = &self.pool[slot];
        unsafe {
            self.context.CopyResource(texture, &input.texture);
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)?;
            let tracked = MFCreateTrackedSample()?;
            let sample: IMFSample = tracked.cast()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(input.time)?;
            sample.SetSampleDuration(self.frame_duration)?;
            busy.store(true, Ordering::Release);
            let released: IMFAsyncCallback = Released(busy.clone()).into();
            tracked.SetAllocator(&released, None)?;
            Ok(sample)
        }
    }

    /// A slot the encoder isn't holding; grows the pool when all are held.
    fn free_slot(&mut self, like: &ID3D11Texture2D) -> Result<usize> {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            if let Some(i) = self.pool.iter().position(|s| !s.busy.load(Ordering::Acquire)) {
                return Ok(i);
            }
            if self.pool.len() < MAX_POOL {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                let mut texture = None;
                unsafe {
                    like.GetDesc(&mut desc);
                    desc.MiscFlags = 0;
                    self.device.CreateTexture2D(&desc, None, Some(&mut texture))?;
                }
                let texture = texture.ok_or_else(|| Error::new(E_FAIL, "no texture"))?;
                self.pool.push(Slot { texture, busy: Arc::new(AtomicBool::new(false)) });
                return Ok(self.pool.len() - 1);
            }
            if Instant::now() > deadline {
                return Err(Error::new(E_FAIL, "the encoder stopped releasing frames"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Collects one output sample; returns whether it was a keyframe (None: no packet).
    fn pull_output(
        &self,
        on_packet: &mut impl FnMut(Packet) -> Result<()>,
        stats: &mut Stats,
    ) -> Result<Option<bool>> {
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
            Ok(()) => match sample {
                Some(sample) => emit(&sample, self.codec, on_packet, stats).map(Some),
                None => Ok(None),
            },
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => unsafe {
                let t = self.transform.GetOutputAvailableType(self.output_id, 0)?;
                self.transform.SetOutputType(self.output_id, &t, 0)?;
                Ok(None)
            },
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Hands a compressed MF sample to `on_packet` as a borrowed packet; returns
/// whether it is a keyframe.
fn emit(
    sample: &IMFSample,
    codec: Codec,
    on_packet: &mut impl FnMut(Packet) -> Result<()>,
    stats: &mut Stats,
) -> Result<bool> {
    unsafe {
        let clean_point = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
        let time = sample.GetSampleTime()?;
        let duration = sample.GetSampleDuration().unwrap_or(0);
        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len))?;
        let data = strip_ivf(std::slice::from_raw_parts(ptr, len as usize));
        // The bitstream decides, not the driver's label.
        let keyframe = random_access(codec, data).unwrap_or(clean_point);
        let packet = Packet {
            data,
            time,
            duration,
            keyframe,
        };
        stats.count(&packet);
        let result = on_packet(packet);
        buffer.Unlock()?;
        result.map(|()| keyframe)
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
    // No low-latency mode: on AMD/NVIDIA it also squeezes the rate-control buffer
    // to about one frame, so keyframes come out as blocks. B-frames stay off
    // explicitly instead (no reordering, less GPU work).
    set(&CODECAPI_AVEncMPVDefaultBPictureCount, var_u32(0u32));
    set(&CODECAPI_AVEncCommonMaxBitRate, var_u32(cfg.max_bitrate));

    if cfg.target_bitrate.is_none()
        && set(
            &CODECAPI_AVEncCommonRateControlMode,
            var_u32(eAVEncCommonRateControlMode_Quality.0 as u32),
        )
    {
        set(&CODECAPI_AVEncCommonQuality, var_u32(cfg.quality));
        return "constant quality";
    }
    if set(
        &CODECAPI_AVEncCommonRateControlMode,
        var_u32(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32),
    ) {
        let mean = cfg
            .target_bitrate
            .unwrap_or(cfg.max_bitrate / 10 * 7)
            .min(cfg.max_bitrate);
        set(&CODECAPI_AVEncCommonMeanBitRate, var_u32(mean));
        // A 2-second rate-control buffer (in bits), as on the Intel path: a keyframe
        // can take what it needs and the next frames pay it back. Same file size.
        set(&CODECAPI_AVEncCommonBufferSize, var_u32(mean.saturating_mul(2)));
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

