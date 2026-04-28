use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex};

use super::capture::AudioFrame;

use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Com::StructuredStorage::*;
use windows::Win32::System::Variant::VT_BLOB;
use windows::Win32::Foundation::*;
use windows::core::{implement, Interface, Error, IUnknown, GUID, HRESULT, Ref};

// Constants not always exported by the windows crate
const WAVE_FORMAT_EXTENSIBLE_TAG: u16 = 0xFFFE;
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT_GUID: GUID = GUID::from_values(
    0x00000003, 0x0000, 0x0010,
    [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
);

/// Start capturing audio from a specific process by PID.
/// Uses WASAPI Process Loopback (INCLUDE_PROCESS_TREE) — requires Win10 2004+.
pub fn start_process_audio_capture(
    pid: u32,
) -> Result<std::sync::mpsc::Receiver<AudioFrame>, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<AudioFrame>(16);

    std::thread::Builder::new()
        .name("decibell-audio-capture".to_string())
        .spawn(move || {
            if let Err(e) = run_wasapi_capture(tx, pid, false) {
                eprintln!("[audio-capture] WASAPI capture error: {}", e);
            }
        })
        .map_err(|e| format!("Spawn audio capture thread: {}", e))?;

    Ok(rx)
}

/// Start capturing system audio excluding Decibell's own output.
/// Uses WASAPI Process Loopback (EXCLUDE_PROCESS_TREE) with our own PID.
pub fn start_system_audio_capture() -> Result<std::sync::mpsc::Receiver<AudioFrame>, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<AudioFrame>(16);

    let our_pid = std::process::id();

    std::thread::Builder::new()
        .name("decibell-audio-capture".to_string())
        .spawn(move || {
            if let Err(e) = run_wasapi_capture(tx, our_pid, true) {
                eprintln!("[audio-capture] WASAPI capture error: {}", e);
            }
        })
        .map_err(|e| format!("Spawn audio capture thread: {}", e))?;

    Ok(rx)
}

// ─── Activation helper ─────────────────────────────────────────────────────

/// Activate an IAudioClient for process loopback capture.
unsafe fn activate_loopback_client(
    target_pid: u32,
    exclude: bool,
) -> Result<IAudioClient, String> {
    let loopback_mode = if exclude {
        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
    } else {
        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
    };

    let activation_params = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: target_pid,
                ProcessLoopbackMode: loopback_mode,
            },
        },
    });

    // ManuallyDrop prevents PROPVARIANT's Drop (PropVariantClear) from calling
    // CoTaskMemFree on our Rust-allocated blob — that would cause heap corruption.
    let mut prop_variant: std::mem::ManuallyDrop<PROPVARIANT> =
        std::mem::ManuallyDrop::new(std::mem::zeroed());
    {
        let inner = &mut prop_variant.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob.cbSize = std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
        inner.Anonymous.blob.pBlobData = &*activation_params as *const _ as *mut u8;
    }

    let inner = Arc::new(AudioActivationInner {
        result: Mutex::new(None),
        condvar: Condvar::new(),
    });
    let handler: IActivateAudioInterfaceCompletionHandler = AudioActivationHandlerCom {
        inner: inner.clone(),
    }.into();
    let waiter = AudioActivationWaiter { inner };

    let _operation = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&*prop_variant as *const PROPVARIANT),
        &handler,
    )
    .map_err(|e| format!("ActivateAudioInterfaceAsync: {}", e))?;

    let client = waiter
        .wait_for_completion(std::time::Duration::from_secs(5))
        .map_err(|e| format!("Wait for audio client: {}", e))?;
    drop(activation_params);

    Ok(client)
}

// ─── Core capture logic ────────────────────────────────────────────────────

/// Core WASAPI capture using Process Loopback.
fn run_wasapi_capture(
    tx: SyncSender<AudioFrame>,
    target_pid: u32,
    exclude: bool,
) -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| format!("CoInitializeEx: {}", e))?;

        let mode = if exclude { "EXCLUDE_TARGET_PROCESS_TREE" } else { "INCLUDE_TARGET_PROCESS_TREE" };
        eprintln!("[audio-capture] Starting WASAPI Process Loopback: pid={}, mode={}", target_pid, mode);

        // Get the mix format from the default render endpoint — the process
        // loopback virtual device mirrors this format but doesn't support
        // GetMixFormat itself.
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(
            &MMDeviceEnumerator,
            None,
            CLSCTX_ALL,
        ).map_err(|e| format!("CoCreateInstance MMDeviceEnumerator: {}", e))?;
        let default_device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| format!("GetDefaultAudioEndpoint: {}", e))?;
        let default_client: IAudioClient = default_device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| format!("Activate default device: {}", e))?;
        let mix_format_ptr = default_client
            .GetMixFormat()
            .map_err(|e| format!("GetMixFormat: {}", e))?;
        let mix_format = &*mix_format_ptr;

        let channels = mix_format.nChannels as u32;
        let sample_rate = mix_format.nSamplesPerSec;
        let bits_per_sample = mix_format.wBitsPerSample;
        let block_align = mix_format.nBlockAlign;

        eprintln!("[audio-capture] Mix format: {}ch, {}Hz, {}bit, block_align={}",
            channels, sample_rate, bits_per_sample, block_align);

        let is_float = if mix_format.wFormatTag == WAVE_FORMAT_EXTENSIBLE_TAG {
            let ext = &*(mix_format_ptr as *const WAVEFORMATEXTENSIBLE);
            let sub_format = std::ptr::addr_of!(ext.SubFormat).read_unaligned();
            sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT_GUID
        } else {
            mix_format.wFormatTag == 3
        };

        // Process-loopback Initialize requires AUDCLNT_STREAMFLAGS_LOOPBACK —
        // calling without it returns AUDCLNT_E_INVALID_STREAM_FLAG (0x88890021).
        // 20ms buffer in 100ns units.
        let buffer_duration = 200_000i64;
        let audio_client = activate_loopback_client(target_pid, exclude)?;
        match audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            buffer_duration,
            0,
            mix_format_ptr,
            None,
        ) {
            Ok(()) => {
                let result = run_capture_loop(
                    tx, &audio_client, mix_format_ptr,
                    channels, sample_rate, bits_per_sample, block_align, is_float,
                );
                CoTaskMemFree(Some(mix_format_ptr as *const _ as *mut _));
                CoUninitialize();
                result
            }
            Err(e) => {
                CoTaskMemFree(Some(mix_format_ptr as *const _ as *mut _));
                CoUninitialize();
                Err(format!("Initialize (LOOPBACK): {}", e))
            }
        }
    }
}

/// Run the capture polling loop once the client is initialized.
unsafe fn run_capture_loop(
    tx: SyncSender<AudioFrame>,
    audio_client: &IAudioClient,
    _mix_format_ptr: *const WAVEFORMATEX,
    channels: u32,
    sample_rate: u32,
    bits_per_sample: u16,
    block_align: u16,
    is_float: bool,
) -> Result<(), String> {
    let capture_client: IAudioCaptureClient = audio_client
        .GetService()
        .map_err(|e| format!("GetService IAudioCaptureClient: {}", e))?;

    audio_client
        .Start()
        .map_err(|e| format!("Start: {}", e))?;

    eprintln!("[audio-capture] WASAPI capture started");

    let mut frame_count: u64 = 0;

    'capture: loop {
        // Polling mode: sleep ~10ms between buffer checks
        std::thread::sleep(std::time::Duration::from_millis(10));

        loop {
            let packet_length = match capture_client.GetNextPacketSize() {
                Ok(len) => len,
                Err(_) => break,
            };
            if packet_length == 0 {
                break;
            }

            let mut buffer_ptr: *mut u8 = std::ptr::null_mut();
            let mut num_frames: u32 = 0;
            let mut flags: u32 = 0;

            if capture_client
                .GetBuffer(&mut buffer_ptr, &mut num_frames, &mut flags, None, None)
                .is_err()
            {
                break;
            }

            if num_frames > 0 && !buffer_ptr.is_null() {
                let buffer_bytes = (num_frames * block_align as u32) as usize;
                let buffer_slice = std::slice::from_raw_parts(buffer_ptr, buffer_bytes);

                let is_silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;

                let stereo_f32 = if is_silent {
                    vec![0.0f32; num_frames as usize * 2]
                } else if is_float && bits_per_sample == 32 {
                    convert_float_to_stereo(buffer_slice, channels as usize)
                } else if !is_float && bits_per_sample == 16 {
                    convert_s16_to_stereo(buffer_slice, channels as usize)
                } else if !is_float && bits_per_sample == 32 {
                    convert_s32_to_stereo(buffer_slice, channels as usize)
                } else {
                    vec![0.0f32; num_frames as usize * 2]
                };

                frame_count += 1;
                if frame_count == 1 || frame_count % 2400 == 0 {
                    eprintln!("[audio-capture] Frame {}: {} stereo samples",
                        frame_count, stereo_f32.len() / 2);
                }

                let frame = AudioFrame {
                    data: stereo_f32,
                    channels: 2,
                    sample_rate,
                };

                match tx.try_send(frame) {
                    Ok(()) => {}
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        let _ = capture_client.ReleaseBuffer(num_frames);
                        eprintln!("[audio-capture] Channel closed, stopping");
                        break 'capture;
                    }
                }
            }

            if capture_client.ReleaseBuffer(num_frames).is_err() {
                break;
            }
        }
    }

    let _ = audio_client.Stop();

    eprintln!("[audio-capture] WASAPI capture stopped after {} frames", frame_count);
    Ok(())
}

// ─── IActivateAudioInterfaceCompletionHandler implementation ────────────────

struct AudioActivationInner {
    result: Mutex<Option<std::result::Result<IUnknown, Error>>>,
    condvar: Condvar,
}

struct AudioActivationWaiter {
    inner: Arc<AudioActivationInner>,
}

impl AudioActivationWaiter {
    fn wait_for_completion(
        &self,
        timeout: std::time::Duration,
    ) -> std::result::Result<IAudioClient, String> {
        let mut guard = self.inner.result.lock().unwrap();
        let start = std::time::Instant::now();
        while guard.is_none() {
            let remaining = timeout.checked_sub(start.elapsed()).unwrap_or_default();
            if remaining.is_zero() {
                return Err("Timeout waiting for audio activation".to_string());
            }
            let (new_guard, _) = self.inner.condvar.wait_timeout(guard, remaining).unwrap();
            guard = new_guard;
        }

        match guard.take().unwrap() {
            Ok(unknown) => unsafe {
                unknown
                    .cast::<IAudioClient>()
                    .map_err(|e| format!("Cast to IAudioClient: {}", e))
            },
            Err(e) => Err(format!("Activation failed: {}", e)),
        }
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct AudioActivationHandlerCom {
    inner: Arc<AudioActivationInner>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for AudioActivationHandlerCom_Impl {
    fn ActivateCompleted(
        &self,
        activateoperation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let operation: &IActivateAudioInterfaceAsyncOperation =
            activateoperation.ok().map_err(|_| Error::from(E_POINTER))?;

        let mut activate_result = HRESULT(0);
        let mut activated_interface: Option<IUnknown> = None;

        unsafe {
            operation.GetActivateResult(&mut activate_result, &mut activated_interface)?;
        }

        let result = if activate_result.is_ok() {
            match activated_interface {
                Some(iface) => Ok(iface),
                None => Err(Error::from(E_POINTER)),
            }
        } else {
            Err(Error::from(activate_result))
        };

        let mut guard = self.inner.result.lock().unwrap();
        *guard = Some(result);
        self.inner.condvar.notify_all();

        Ok(())
    }
}

// ─── Format conversion helpers ──────────────────────────────────────────────

fn convert_float_to_stereo(raw: &[u8], channels: usize) -> Vec<f32> {
    let samples: Vec<f32> = raw
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    let frame_count = samples.len() / channels;
    let mut stereo = Vec::with_capacity(frame_count * 2);

    for frame in samples.chunks_exact(channels) {
        if channels == 1 {
            stereo.push(frame[0]);
            stereo.push(frame[0]);
        } else {
            stereo.push(frame[0]);
            stereo.push(frame[1]);
        }
    }

    stereo
}

fn convert_s16_to_stereo(raw: &[u8], channels: usize) -> Vec<f32> {
    let samples: Vec<i16> = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();

    let frame_count = samples.len() / channels;
    let mut stereo = Vec::with_capacity(frame_count * 2);

    for frame in samples.chunks_exact(channels) {
        let l = frame[0] as f32 / 32768.0;
        let r = if channels > 1 {
            frame[1] as f32 / 32768.0
        } else {
            l
        };
        stereo.push(l);
        stereo.push(r);
    }

    stereo
}

fn convert_s32_to_stereo(raw: &[u8], channels: usize) -> Vec<f32> {
    let samples: Vec<i32> = raw
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    let frame_count = samples.len() / channels;
    let mut stereo = Vec::with_capacity(frame_count * 2);

    for frame in samples.chunks_exact(channels) {
        let l = frame[0] as f32 / 2147483648.0;
        let r = if channels > 1 {
            frame[1] as f32 / 2147483648.0
        } else {
            l
        };
        stereo.push(l);
        stereo.push(r);
    }

    stereo
}
