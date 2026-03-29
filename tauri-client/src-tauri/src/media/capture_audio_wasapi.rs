use std::sync::mpsc::SyncSender;

use super::capture::AudioFrame;

use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::*;
use windows::Win32::Foundation::*;
use windows::core::*;

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

/// Core WASAPI capture loop using Process Loopback.
///
/// If `exclude` is true, captures all system audio EXCEPT the given PID.
/// If `exclude` is false, captures ONLY audio from the given PID.
fn run_wasapi_capture(
    tx: SyncSender<AudioFrame>,
    target_pid: u32,
    exclude: bool,
) -> Result<(), String> {
    unsafe {
        // Initialize COM for this thread
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .map_err(|e| format!("CoInitializeEx: {}", e))?;

        let mode = if exclude {
            "EXCLUDE_TARGET_PROCESS_TREE"
        } else {
            "INCLUDE_TARGET_PROCESS_TREE"
        };
        eprintln!(
            "[audio-capture] Starting WASAPI Process Loopback: pid={}, mode={}",
            target_pid, mode
        );

        // Build activation params for process loopback
        let loopback_mode = if exclude {
            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
        } else {
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
        };

        let process_params = AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
            TargetProcessId: target_pid,
            ProcessLoopbackMode: loopback_mode,
        };

        let activation_params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: process_params,
            },
        };

        // Wrap in PROPVARIANT for ActivateAudioInterfaceAsync
        let mut prop_variant: PROPVARIANT = std::mem::zeroed();
        {
            let blob = &mut prop_variant.Anonymous.Anonymous;
            blob.vt = windows::Win32::System::Variant::VT_BLOB;
            blob.Anonymous.blob.cbSize = std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
            blob.Anonymous.blob.pBlobData = &activation_params as *const _ as *mut u8;
        }

        // Create completion handler
        let handler = AudioActivationHandler::new();
        let handler_ref: IActivateAudioInterfaceCompletionHandler = handler.clone().into();

        // Activate the audio interface — keep the operation alive until completion
        let _operation = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&prop_variant),
            &handler_ref,
        )
        .map_err(|e| format!("ActivateAudioInterfaceAsync: {}", e))?;

        // Wait for activation to complete
        let audio_client: IAudioClient = handler
            .wait_for_completion(std::time::Duration::from_secs(5))
            .map_err(|e| format!("Wait for audio client: {}", e))?;

        eprintln!("[audio-capture] Audio client activated");

        // Get the mix format
        let mix_format_ptr = audio_client
            .GetMixFormat()
            .map_err(|e| format!("GetMixFormat: {}", e))?;
        let mix_format = &*mix_format_ptr;

        let channels = mix_format.nChannels as u32;
        let sample_rate = mix_format.nSamplesPerSec;
        let bits_per_sample = mix_format.wBitsPerSample;
        let block_align = mix_format.nBlockAlign;

        eprintln!(
            "[audio-capture] Mix format: {}ch, {}Hz, {}bit, block_align={}",
            channels, sample_rate, bits_per_sample, block_align
        );

        // Determine if format is float or integer
        let is_float = if mix_format.wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16 {
            let ext = &*(mix_format_ptr as *const WAVEFORMATEXTENSIBLE);
            ext.SubFormat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            mix_format.wFormatTag == 3 // WAVE_FORMAT_IEEE_FLOAT
        };

        // Initialize the audio client for loopback capture
        let buffer_duration = 200_000i64; // 20ms in 100ns units
        audio_client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                buffer_duration,
                0,
                mix_format_ptr,
                None,
            )
            .map_err(|e| format!("Initialize: {}", e))?;

        // Create and set event handle
        let event = CreateEventW(None, false, false, None)
            .map_err(|e| format!("CreateEvent: {}", e))?;
        audio_client
            .SetEventHandle(event)
            .map_err(|e| format!("SetEventHandle: {}", e))?;

        // Get capture client
        let capture_client: IAudioCaptureClient = audio_client
            .GetService()
            .map_err(|e| format!("GetService IAudioCaptureClient: {}", e))?;

        // Start capturing
        audio_client
            .Start()
            .map_err(|e| format!("Start: {}", e))?;

        eprintln!("[audio-capture] WASAPI capture started");

        let mut frame_count: u64 = 0;
        let mut channel_closed = false;

        'capture: loop {
            // Wait for data with 100ms timeout
            let wait_result = WaitForSingleObject(event, 100);
            if wait_result == WAIT_FAILED {
                break;
            }

            // Get available frames
            loop {
                let mut packet_length: u32 = 0;
                if capture_client.GetNextPacketSize(&mut packet_length).is_err() {
                    break;
                }
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

                    // Convert to interleaved stereo f32
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
                        eprintln!(
                            "[audio-capture] Frame {}: {} stereo samples",
                            frame_count,
                            stereo_f32.len() / 2
                        );
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
                            channel_closed = true;
                            break;
                        }
                    }
                }

                if capture_client.ReleaseBuffer(num_frames).is_err() {
                    break;
                }
            }

            if channel_closed {
                break 'capture;
            }
        }

        let _ = audio_client.Stop();
        let _ = CloseHandle(event);
        CoUninitialize();

        eprintln!(
            "[audio-capture] WASAPI capture stopped after {} frames",
            frame_count
        );
    }
    Ok(())
}

// ─── IActivateAudioInterfaceCompletionHandler implementation ────────────────

use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone)]
struct AudioActivationHandler {
    inner: Arc<AudioActivationInner>,
}

struct AudioActivationInner {
    result: Mutex<Option<Result<IUnknown, windows::core::Error>>>,
    condvar: Condvar,
}

impl AudioActivationHandler {
    fn new() -> Self {
        Self {
            inner: Arc::new(AudioActivationInner {
                result: Mutex::new(None),
                condvar: Condvar::new(),
            }),
        }
    }

    fn wait_for_completion(
        &self,
        timeout: std::time::Duration,
    ) -> Result<IAudioClient, String> {
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
impl AudioActivationHandler {
    fn ActivateCompleted(
        &self,
        activate_operation: Option<&IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let operation = activate_operation.ok_or(Error::from(E_POINTER))?;

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
