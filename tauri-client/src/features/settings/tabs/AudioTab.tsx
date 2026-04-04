import { useEffect, useState, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useUiStore } from "../../../stores/uiStore";
import { useVoiceStore } from "../../../stores/voiceStore";
import { saveSettings } from "../saveSettings";

interface AudioDevice {
  name: string;
}

interface AudioDeviceList {
  inputs: AudioDevice[];
  outputs: AudioDevice[];
}

function DeviceSelector({
  label,
  icon,
  devices,
  selected,
  onChange,
}: {
  label: string;
  icon: React.ReactNode;
  devices: AudioDevice[];
  selected: string | null;
  onChange: (name: string | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [open]);

  const displayName = selected ?? "Default";

  return (
    <div className="rounded-xl bg-bg-primary p-4" ref={ref}>
      <div className="mb-2.5 flex items-center gap-2.5">
        {icon}
        <span className="text-[13px] font-semibold text-text-primary">{label}</span>
      </div>
      <div className="relative">
        <button
          onClick={() => setOpen(!open)}
          className="flex w-full items-center justify-between rounded-lg border border-border bg-bg-secondary px-3 py-2.5 text-left text-[12px] text-text-primary transition-colors hover:border-accent/40"
        >
          <span className="truncate">{displayName}</span>
          <svg
            width="12"
            height="12"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.5"
            strokeLinecap="round"
            className={`shrink-0 text-text-muted transition-transform ${open ? "rotate-180" : ""}`}
          >
            <path d="M6 9l6 6 6-6" />
          </svg>
        </button>
        {open && (
          <div className="absolute left-0 right-0 top-full z-10 mt-1 max-h-48 overflow-y-auto rounded-lg border border-border bg-bg-secondary shadow-xl">
            <button
              onClick={() => {
                onChange(null);
                setOpen(false);
              }}
              className={`flex w-full items-center px-3 py-2 text-left text-[12px] transition-colors hover:bg-surface-hover ${
                selected === null ? "text-accent-bright font-semibold" : "text-text-secondary"
              }`}
            >
              Default
            </button>
            {devices.map((device) => (
              <button
                key={device.name}
                onClick={() => {
                  onChange(device.name);
                  setOpen(false);
                }}
                className={`flex w-full items-center px-3 py-2 text-left text-[12px] transition-colors hover:bg-surface-hover ${
                  selected === device.name ? "text-accent-bright font-semibold" : "text-text-secondary"
                }`}
              >
                {device.name}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

const MIN_THRESHOLD_DB = -60;
const MAX_THRESHOLD_DB = 0;

function VoiceThresholdBar() {
  const voiceThresholdDb = useUiStore((s) => s.voiceThresholdDb);
  const inputDevice = useUiStore((s) => s.inputDevice);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const [inputLevel, setInputLevel] = useState(-96);
  const levelRef = useRef(-96);
  const animRef = useRef<number>(0);
  const barRef = useRef<HTMLDivElement>(null);

  // Start/stop mic test when not in a voice channel
  useEffect(() => {
    if (connectedChannelId) return; // Pipeline already emits levels
    invoke("start_mic_test", { deviceName: inputDevice }).catch(console.error);
    return () => {
      invoke("stop_mic_test").catch(console.error);
    };
  }, [connectedChannelId, inputDevice]);

  // Listen for input level events from the backend
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{ db: number }>("voice_input_level", (event) => {
      levelRef.current = event.payload.db;
    }).then((u) => { unlisten = u; });

    // Smooth animation loop: lerp toward the latest level
    const tick = () => {
      setInputLevel((prev) => {
        const target = levelRef.current;
        // Fast attack, slower decay
        const speed = target > prev ? 0.5 : 0.15;
        return prev + (target - prev) * speed;
      });
      animRef.current = requestAnimationFrame(tick);
    };
    animRef.current = requestAnimationFrame(tick);

    return () => {
      if (unlisten) unlisten();
      cancelAnimationFrame(animRef.current);
    };
  }, []);

  // Convert dB to percentage position on the bar
  const dbToPercent = (db: number) => {
    const clamped = Math.max(MIN_THRESHOLD_DB, Math.min(MAX_THRESHOLD_DB, db));
    return ((clamped - MIN_THRESHOLD_DB) / (MAX_THRESHOLD_DB - MIN_THRESHOLD_DB)) * 100;
  };

  const levelPercent = dbToPercent(inputLevel);
  const thresholdPercent = dbToPercent(voiceThresholdDb);
  const [dragging, setDragging] = useState(false);

  const applyFromClientX = useCallback((clientX: number) => {
    const bar = barRef.current;
    if (!bar) return;
    const rect = bar.getBoundingClientRect();
    const x = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
    // Continuous dB value, rounded to 0.5 for smooth movement
    const raw = MIN_THRESHOLD_DB + x * (MAX_THRESHOLD_DB - MIN_THRESHOLD_DB);
    const db = Math.round(raw * 2) / 2;
    useUiStore.getState().setVoiceThresholdDb(db);
    const backendDb = db <= MIN_THRESHOLD_DB ? -96 : db;
    invoke("set_voice_threshold", { thresholdDb: backendDb }).catch(console.error);
  }, []);

  const handleMouseDown = useCallback((e: React.MouseEvent<HTMLDivElement>) => {
    e.preventDefault();
    setDragging(true);
    applyFromClientX(e.clientX);
  }, [applyFromClientX]);

  // Global mousemove/mouseup while dragging
  useEffect(() => {
    if (!dragging) return;
    const onMove = (e: MouseEvent) => {
      applyFromClientX(e.clientX);
    };
    const onUp = () => {
      setDragging(false);
      saveSettings();
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, [dragging, applyFromClientX]);

  // Color the level bar: green if above threshold (or threshold off), muted otherwise
  const isOpenMic = voiceThresholdDb <= MIN_THRESHOLD_DB;
  const aboveThreshold = isOpenMic || inputLevel >= voiceThresholdDb;

  return (
    <div className="rounded-xl bg-bg-primary p-4">
      <div className="mb-2.5 flex items-center gap-2.5">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="text-text-secondary">
          <path d="M12 1a3 3 0 00-3 3v8a3 3 0 006 0V4a3 3 0 00-3-3z" />
          <path d="M19 10v2a7 7 0 01-14 0v-2" />
          <line x1="12" y1="19" x2="12" y2="23" />
        </svg>
        <span className="text-[13px] font-semibold text-text-primary">Voice Activity Threshold</span>
        <span className="ml-auto text-[11px] font-semibold text-text-muted">
          {voiceThresholdDb <= MIN_THRESHOLD_DB ? "Off" : `${Math.round(voiceThresholdDb)} dB`}
        </span>
      </div>

      {/* Bar container */}
      <div
        ref={barRef}
        className="relative h-2 w-full cursor-pointer rounded-full bg-bg-secondary select-none"
        onMouseDown={handleMouseDown}
      >
        {/* Input level fill */}
        <div
          className={`absolute inset-y-0 left-0 rounded-full transition-colors duration-75 ${
            aboveThreshold ? "bg-success/50" : "bg-text-muted/25"
          }`}
          style={{ width: `${Math.max(0, levelPercent)}%` }}
        />

        {/* Threshold handle — pill with glow */}
        <div
          className="absolute top-1/2 -translate-y-1/2 -translate-x-1/2 z-10 flex items-center justify-center"
          style={{ left: `${thresholdPercent}%` }}
        >
          <div className="h-5 w-1.5 rounded-full bg-white shadow-[0_0_6px_rgba(255,255,255,0.4)]" />
        </div>
      </div>

      <div className="mt-2 flex items-center justify-between">
        <span className="text-[10px] text-text-muted">Sensitive</span>
        <span className="text-[10px] text-text-muted">Aggressive</span>
      </div>
    </div>
  );
}

export default function AudioTab() {
  const [devices, setDevices] = useState<AudioDeviceList>({ inputs: [], outputs: [] });
  const inputDevice = useUiStore((s) => s.inputDevice);
  const outputDevice = useUiStore((s) => s.outputDevice);
  const streamStereo = useUiStore((s) => s.streamStereo);
  const separateStreamOutput = useUiStore((s) => s.separateStreamOutput);
  const streamOutputDevice = useUiStore((s) => s.streamOutputDevice);

  useEffect(() => {
    invoke<AudioDeviceList>("list_audio_devices")
      .then(setDevices)
      .catch(console.error);
  }, []);

  const handleInputChange = (name: string | null) => {
    useUiStore.getState().setInputDevice(name);
    invoke("set_input_device", { name }).catch(console.error);
    saveSettings();
  };

  const handleOutputChange = (name: string | null) => {
    useUiStore.getState().setOutputDevice(name);
    invoke("set_output_device", { name }).catch(console.error);
    saveSettings();
  };

  const handleSeparateStreamToggle = () => {
    const newEnabled = !separateStreamOutput;
    useUiStore.getState().setSeparateStreamOutput(newEnabled);
    invoke("set_separate_stream_output", {
      enabled: newEnabled,
      device: newEnabled ? streamOutputDevice : null,
    }).catch(console.error);
    saveSettings();
  };

  const handleStreamOutputChange = (name: string | null) => {
    useUiStore.getState().setStreamOutputDevice(name);
    invoke("set_stream_output_device", { name }).catch(console.error);
    saveSettings();
  };

  const handleStereoToggle = () => {
    const newValue = !streamStereo;
    useUiStore.getState().setStreamStereo(newValue);
    invoke("set_stream_stereo", { enabled: newValue }).catch(console.error);
    saveSettings();
  };

  return (
    <div>
      {/* Devices section */}
      <div className="mb-1.5 text-[10px] font-bold uppercase tracking-[0.08em] text-text-muted">
        Devices
      </div>
      <div className="mb-2 flex flex-col gap-2">
        <DeviceSelector
          label="Input Device"
          icon={
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="text-text-secondary">
              <path d="M12 1a3 3 0 00-3 3v8a3 3 0 006 0V4a3 3 0 00-3-3z" />
              <path d="M19 10v2a7 7 0 01-14 0v-2" />
              <line x1="12" y1="19" x2="12" y2="23" />
            </svg>
          }
          devices={devices.inputs}
          selected={inputDevice}
          onChange={handleInputChange}
        />
        <DeviceSelector
          label="Output Device"
          icon={
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="text-text-secondary">
              <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
              <path d="M19.07 4.93a10 10 0 010 14.14" />
              <path d="M15.54 8.46a5 5 0 010 7.07" />
            </svg>
          }
          devices={devices.outputs}
          selected={outputDevice}
          onChange={handleOutputChange}
        />
      </div>

      {/* Divider */}
      <div className="my-4 h-px bg-border" />

      {/* Voice Threshold section */}
      <div className="mb-1.5 text-[10px] font-bold uppercase tracking-[0.08em] text-text-muted">
        Input Sensitivity
      </div>
      <div className="mb-2">
        <VoiceThresholdBar />
      </div>

      {/* Divider */}
      <div className="my-4 h-px bg-border" />

      {/* Stream Audio section */}
      <div className="mb-1.5 text-[10px] font-bold uppercase tracking-[0.08em] text-text-muted">
        Stream Audio
      </div>
      <div className="flex flex-col gap-2">
        <div className="flex items-center justify-between rounded-xl bg-bg-primary px-4 py-3">
          <div>
            <div className="text-[13px] font-semibold text-text-primary">
              Separate stream output device
            </div>
            <div className="mt-0.5 text-[11px] text-text-muted">
              Route stream audio to a different output device than voice chat
            </div>
          </div>
          <button
            onClick={handleSeparateStreamToggle}
            className={`relative h-6 w-11 shrink-0 rounded-full transition-colors ${
              separateStreamOutput ? "bg-accent" : "bg-text-muted/30"
            }`}
          >
            <div
              className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-transform ${
                separateStreamOutput ? "translate-x-[22px]" : "translate-x-0.5"
              }`}
            />
          </button>
        </div>
        {separateStreamOutput && (
          <DeviceSelector
            label="Stream Output Device"
            icon={
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="text-text-secondary">
                <path d="M9 18V5l12-2v13" />
                <circle cx="6" cy="18" r="3" />
                <circle cx="18" cy="16" r="3" />
              </svg>
            }
            devices={devices.outputs}
            selected={streamOutputDevice}
            onChange={handleStreamOutputChange}
          />
        )}
        <div className="flex items-center justify-between rounded-xl bg-bg-primary px-4 py-3">
          <div>
            <div className="text-[13px] font-semibold text-text-primary">
              Stereo stream audio
            </div>
            <div className="mt-0.5 text-[11px] text-text-muted">
              Preserve left/right stereo positioning when watching streams
            </div>
          </div>
          <button
            onClick={handleStereoToggle}
            className={`relative h-6 w-11 shrink-0 rounded-full transition-colors ${
              streamStereo ? "bg-accent" : "bg-text-muted/30"
            }`}
          >
            <div
              className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-transform ${
                streamStereo ? "translate-x-[22px]" : "translate-x-0.5"
              }`}
            />
          </button>
        </div>
      </div>
    </div>
  );
}
