import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useUiStore } from "../../stores/uiStore";
import { saveSettings } from "../settings/saveSettings";

interface AudioDevice {
  name: string;
  label?: string;
}

interface Props {
  type: "input" | "output";
  anchor: { x: number; y: number };
  devices: AudioDevice[];
  onClose: () => void;
}

export default function DeviceContextMenu({ type, anchor, devices, onClose }: Props) {
  const menuRef = useRef<HTMLDivElement>(null);
  const selected = useUiStore((s) =>
    type === "input" ? s.inputDevice : s.outputDevice
  );

  // Close on outside click / escape
  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const timer = setTimeout(() => {
      document.addEventListener("mousedown", handleClick);
      document.addEventListener("keydown", handleKey);
    }, 0);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose]);

  const handleSelect = (name: string | null) => {
    if (type === "input") {
      useUiStore.getState().setInputDevice(name);
      invoke("set_input_device", { name }).catch(console.error);
    } else {
      useUiStore.getState().setOutputDevice(name);
      invoke("set_output_device", { name }).catch(console.error);
    }
    saveSettings();
    onClose();
  };

  // Position: open upward from the button since the bar is at the bottom
  const menuWidth = 240;
  const menuMaxHeight = 300;
  const x = Math.max(8, Math.min(anchor.x, window.innerWidth - menuWidth - 8));
  const y = Math.max(8, anchor.y - menuMaxHeight);

  return createPortal(
    <div
      ref={menuRef}
      className="fixed z-[100] w-[240px] rounded-xl border border-border bg-bg-secondary shadow-2xl animate-in fade-in zoom-in-95 duration-100"
      style={{ left: x, top: y, maxHeight: menuMaxHeight }}
    >
      {/* Header */}
      <div className="border-b border-border px-3 py-2.5">
        <span className="text-[11px] font-bold uppercase tracking-wider text-text-muted">
          {type === "input" ? "Input Device" : "Output Device"}
        </span>
      </div>

      {/* Device list */}
      <div className="max-h-[240px] overflow-y-auto py-1">
        <button
          onClick={() => handleSelect(null)}
          className={`flex w-full items-center gap-2 px-3 py-2 text-left text-[12px] transition-colors hover:bg-surface-hover ${
            selected === null
              ? "font-semibold text-accent-bright"
              : "text-text-secondary"
          }`}
        >
          {selected === null && (
            <svg
              width="12"
              height="12"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="3"
              strokeLinecap="round"
              strokeLinejoin="round"
              className="shrink-0 text-accent"
            >
              <polyline points="20 6 9 17 4 12" />
            </svg>
          )}
          <span className={selected === null ? "" : "pl-5"}>Default</span>
        </button>
        {devices.map((device) => (
          <button
            key={device.name}
            onClick={() => handleSelect(device.name)}
            className={`flex w-full items-center gap-2 px-3 py-2 text-left text-[12px] transition-colors hover:bg-surface-hover ${
              selected === device.name
                ? "font-semibold text-accent-bright"
                : "text-text-secondary"
            }`}
          >
            {selected === device.name && (
              <svg
                width="12"
                height="12"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="3"
                strokeLinecap="round"
                strokeLinejoin="round"
                className="shrink-0 text-accent"
              >
                <polyline points="20 6 9 17 4 12" />
              </svg>
            )}
            <span className={selected === device.name ? "" : "pl-5"}>
              {device.label ?? device.name}
            </span>
          </button>
        ))}
      </div>
    </div>,
    document.body
  );
}
