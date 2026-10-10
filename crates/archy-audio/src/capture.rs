//! WASAPI capture loop for one source.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::core::{implement, Error, Interface, Ref, Result, HSTRING};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, E_FAIL, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IAgileObject, IAgileObject_Impl, BLOB, CLSCTX_ALL,
    COINIT_MULTITHREADED,
};
use windows::Win32::System::Com::{CoTaskMemFree, STGM_READ};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

use crate::aac::{AacEncoder, AacInfo};
use crate::{AudioPacket, Source, CHANNELS, SAMPLE_RATE};

/// A gap longer than this in the device timeline (an idle output device sends
/// nothing) is filled with silence so the track stays continuous and in sync.
const GAP: i64 = 200_000; // 20 ms in 100 ns units
const MAX_FILL: i64 = 20_000_000; // never invent more than 2 s at once

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Activated {
    done: Mutex<Option<SyncSender<()>>>,
}

// Marker interface: the handler may be called from any thread.
impl IAgileObject_Impl for Activated_Impl {}

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _op: Ref<IActivateAudioInterfaceAsyncOperation>) -> Result<()> {
        if let Some(tx) = self.done.lock().ok().and_then(|mut d| d.take()) {
            let _ = tx.send(());
        }
        Ok(())
    }
}

/// An IAudioClient that captures one process tree (Windows 10 2004+).
fn process_client(pid: u32) -> Result<IAudioClient> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let prop = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_BLOB,
                Anonymous: PROPVARIANT_0_0_0 {
                    blob: BLOB {
                        cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                        pBlobData: &mut params as *mut _ as *mut u8,
                    },
                },
                ..Default::default()
            }),
        },
    };
    let (tx, rx) = sync_channel(1);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated {
        done: Mutex::new(Some(tx)),
    }
    .into();
    let op = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&prop),
            &handler,
        )?
    };
    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| Error::new(E_FAIL, "audio activation timed out"))?;
    let mut hr = windows::core::HRESULT(0);
    let mut unknown = None;
    unsafe { op.GetActivateResult(&mut hr, &mut unknown)? };
    hr.ok()?;
    unknown
        .ok_or_else(|| Error::new(E_FAIL, "no audio client"))?
        .cast()
}

/// A specific device by id, falling back to Windows' default if it's gone
/// (unplugged headset), so a missing device never stops recording.
fn device_client(flow: EDataFlow, id: Option<&str>) -> Result<IAudioClient> {
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let chosen = id
            .filter(|i| !i.is_empty())
            .and_then(|i| enumerator.GetDevice(&HSTRING::from(i)).ok());
        let device = match chosen {
            Some(d) => d,
            None => enumerator.GetDefaultAudioEndpoint(flow, eConsole)?,
        };
        device.Activate(CLSCTX_ALL, None)
    }
}

/// A microphone or output device Windows has, for the device pickers.
#[derive(Clone, Debug)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

/// Active microphones (`input`) or output devices, by their Windows names.
pub fn list_devices(input: bool) -> Vec<AudioDevice> {
    let flow = if input { eCapture } else { eRender };
    let list = || -> Result<Vec<AudioDevice>> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let devices = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
            let mut out = Vec::new();
            for i in 0..devices.GetCount()? {
                let device = devices.Item(i)?;
                let id_ptr = device.GetId()?;
                let id = id_ptr.to_string().unwrap_or_default();
                CoTaskMemFree(Some(id_ptr.0 as *const _));
                let name = device
                    .OpenPropertyStore(STGM_READ)
                    .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
                    .ok()
                    .and_then(|v| {
                        let p = v.Anonymous.Anonymous.Anonymous.pwszVal;
                        (!p.is_null()).then(|| p.to_string().unwrap_or_default())
                    })
                    .unwrap_or_else(|| id.clone());
                out.push(AudioDevice { id, name });
            }
            Ok(out)
        }
    };
    list().unwrap_or_default()
}

pub fn run(
    source: Source,
    kbps: u32,
    stop: &Arc<AtomicBool>,
    ready: &SyncSender<Result<AacInfo>>,
    on_packet: &mut dyn FnMut(AudioPacket),
) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let (client, loopback) = match source {
        Source::System { device } => (device_client(eRender, device.as_deref())?, true),
        Source::Process { pid } => (process_client(pid)?, true),
        Source::Microphone { device } => (device_client(eCapture, device.as_deref())?, false),
    };
    // Ask for exactly what the AAC encoder takes; Windows converts and resamples.
    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * CHANNELS * 2,
        nBlockAlign: (CHANNELS * 2) as u16,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    if loopback {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }
    let event = unsafe { CreateEventW(None, false, false, None)? };
    let result = (|| -> Result<()> {
        unsafe {
            // 200 ms buffer: generous, so a busy PC never loses audio.
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &format, None)?;
            client.SetEventHandle(event)?;
        }
        let capture: IAudioCaptureClient = unsafe { client.GetService()? };
        let mut encoder = AacEncoder::new(kbps)?;
        let _ = ready.send(Ok(encoder.info.clone()));
        unsafe { client.Start()? };

        let frame_time = |frames: i64| frames * 10_000_000 / SAMPLE_RATE as i64;
        let mut next_time: Option<i64> = None;
        let mut silence: Vec<i16> = Vec::new();
        let mut emit = |data: &[u8], time: i64, duration: i64| {
            on_packet(AudioPacket {
                data,
                time,
                duration,
            })
        };
        while !stop.load(Ordering::Acquire) {
            // System loopback goes quiet (no events) when nothing plays; the timeout keeps us responsive.
            let _ = unsafe { WaitForSingleObject(event, 100) } == WAIT_OBJECT_0;
            loop {
                let pending = unsafe { capture.GetNextPacketSize()? };
                if pending == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut bflags = 0u32;
                let mut qpc = 0u64;
                unsafe {
                    capture.GetBuffer(&mut data, &mut frames, &mut bflags, None, Some(&mut qpc))?
                };
                let time = qpc as i64;
                // Fill timeline gaps with silence so the track stays continuous.
                if let Some(expected) = next_time {
                    let gap = time - expected;
                    if gap > GAP {
                        let fill = (gap.min(MAX_FILL) * SAMPLE_RATE as i64 / 10_000_000) as usize
                            * CHANNELS as usize;
                        silence.clear();
                        silence.resize(fill, 0);
                        encoder.encode(&silence, expected, &mut emit)?;
                    }
                }
                let samples = frames as usize * CHANNELS as usize;
                if bflags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    silence.clear();
                    silence.resize(samples, 0);
                    encoder.encode(&silence, time, &mut emit)?;
                } else {
                    let pcm = unsafe { std::slice::from_raw_parts(data as *const i16, samples) };
                    encoder.encode(pcm, time, &mut emit)?;
                }
                unsafe { capture.ReleaseBuffer(frames)? };
                next_time = Some(time + frame_time(frames as i64));
            }
        }
        unsafe {
            let _ = client.Stop();
        }
        Ok(())
    })();
    unsafe {
        let _ = CloseHandle(event);
    }
    result
}
