import { useEffect } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";
import { useUiStore } from "../../stores/uiStore";
import { useMenuPosition } from "../../hooks/useMenuPosition";
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
  // Opens upward from the user-panel button it was summoned on, sized to
  // its real height (it used to float a fixed 300px above the anchor).
  const { ref: menuRef, style: menuStyle } = useMenuPosition(anchor, { prefer: "above" });
  const selected = useUiStore((s) =>
    type === "input" ? s.inputDevice : s.outputDevice,
  );

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
    // null → undefined: napi's Option<String> rejects null (a "Default" pick),
    // accepting only undefined/absent.
    if (type === "input") {
      useUiStore.getState().setInputDevice(name);
      invoke("set_input_device", { name: name ?? undefined }).catch(console.error);
    } else {
      useUiStore.getState().setOutputDevice(name);
      invoke("set_output_device", { name: name ?? undefined }).catch(console.error);
    }
    saveSettings();
    onClose();
  };

  return createPortal(
    <div
      ref={menuRef}
      className="z-[100] w-[240px] rounded-lg border border-border bg-bg-secondary shadow-modal"
      style={menuStyle}
    >
      <div className="border-b border-border px-3 py-2.5">
        <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
          {type === "input" ? "Input Device" : "Output Device"}
        </span>
      </div>
      <div className="max-h-[240px] overflow-y-auto py-1">
        <button
          onClick={() => handleSelect(null)}
          className={`flex w-full items-center gap-2 px-3 py-2 text-left text-[12px] transition-colors hover:bg-surface-hover ${
            selected === null ? "font-semibold text-accent-bright" : "text-text-secondary"
          }`}
        >
          {selected === null && (
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" className="shrink-0 text-accent">
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
              selected === device.name ? "font-semibold text-accent-bright" : "text-text-secondary"
            }`}
          >
            {selected === device.name && (
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" className="shrink-0 text-accent">
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
    document.body,
  );
}
