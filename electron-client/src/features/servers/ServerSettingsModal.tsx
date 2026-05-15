import { createPortal } from "react-dom";
import { useEffect, useState, useCallback, useRef } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { toast } from "../../stores/toastStore";
import { stringToGradient } from "../../utils/colors";
import { ServerPictureCropperModal } from "./ServerPictureCropperModal";

interface Props {
  serverId: string;
}

const TABS = [
  {
    id: "overview",
    label: "Overview",
    icon: (
      <svg
        width="16"
        height="16"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
      >
        <rect x="3" y="3" width="18" height="18" rx="2" />
        <path d="M3 9h18M9 3v18" />
      </svg>
    ),
  },
];

const MAX_BYTES = 1024 * 1024;

/// Mirrors SettingsModal chrome 1:1 (820×560, tabbed sidebar, fade-in
/// scale-95→1, Esc closes, backdrop click closes, portal to body).
/// v1 has one Overview tab containing server-picture management.
export default function ServerSettingsModal({ serverId }: Props) {
  const isOpen = useUiStore((s) => s.activeModal === "server-settings");
  const closeModal = useUiStore((s) => s.closeModal);
  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);
  const [activeTab, setActiveTab] = useState<string>("overview");

  const server = useChatStore((s) =>
    s.servers.find((x) => x.id === serverId),
  );
  const pictureVersion = useChatStore(
    (s) => s.serverPictureVersions[serverId] ?? "",
  );
  const pictureDataUrl = useChatStore((s) => s.serverPictures[serverId]);
  const hasPicture = pictureVersion !== "";

  useEffect(() => {
    if (isOpen) {
      setMounted(true);
      requestAnimationFrame(() => setVisible(true));
    } else {
      setVisible(false);
    }
  }, [isOpen]);

  const handleTransitionEnd = useCallback(() => {
    if (!visible) setMounted(false);
  }, [visible]);

  useEffect(() => {
    if (!mounted) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeModal();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mounted, closeModal]);

  const fileInputRef = useRef<HTMLInputElement>(null);
  const [cropperFile, setCropperFile] = useState<File | null>(null);

  const onUploadClick = () => {
    fileInputRef.current?.click();
  };

  const onFileSelected = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = ""; // allow re-select of the same file
    if (!file) return;
    if (file.size > MAX_BYTES) {
      toast.error("Image too large", "Maximum size is 1 MB.");
      return;
    }
    // Sniff JPEG/PNG magic bytes.
    const head = new Uint8Array(await file.slice(0, 4).arrayBuffer());
    const isPng =
      head[0] === 0x89 &&
      head[1] === 0x50 &&
      head[2] === 0x4e &&
      head[3] === 0x47;
    const isJpeg = head[0] === 0xff && head[1] === 0xd8 && head[2] === 0xff;
    if (!isPng && !isJpeg) {
      toast.error("Unsupported format", "Only JPEG and PNG are supported.");
      return;
    }
    // Hand off to the cropper — it owns the file-to-bytes conversion
    // and the actual upload via update_server_picture. We just open
    // the modal with the picked file.
    setCropperFile(file);
  };

  const onRemove = () => {
    if (
      !window.confirm(
        "Remove the server picture? The default gradient and letter will be used instead.",
      )
    ) {
      return;
    }
    invoke("update_server_picture", {
      serverId,
      data: new Uint8Array(0),
    }).catch((err) => {
      console.error("update_server_picture:", err);
      toast.error("Failed to remove", "Please try again.");
    });
  };

  if (!mounted || !server) return null;

  const cropperModal = cropperFile ? (
    <ServerPictureCropperModal
      serverId={serverId}
      file={cropperFile}
      onSave={() => setCropperFile(null)}
      onCancel={() => setCropperFile(null)}
    />
  ) : null;

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
      style={{
        backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)",
      }}
      onClick={closeModal}
      onTransitionEnd={handleTransitionEnd}
    >
      <div
        className="flex h-[560px] w-[820px] overflow-hidden rounded-2xl border border-border bg-bg-dark shadow-[0_24px_80px_rgba(0,0,0,0.6),0_0_0_1px_rgba(255,255,255,0.02)] transition-all duration-300"
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Sidebar */}
        <div className="flex w-[210px] shrink-0 flex-col gap-0.5 border-r border-border-divider bg-bg-darkest px-3 py-6">
          <div className="mb-2 px-3 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
            Server settings
          </div>
          {TABS.map((tab) => (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`flex items-center gap-2.5 rounded-[10px] px-3 py-[9px] text-[14px] transition-colors ${
                activeTab === tab.id
                  ? "bg-accent-soft font-medium text-text-primary"
                  : "font-normal text-text-secondary hover:bg-surface-hover hover:text-text-primary"
              }`}
            >
              <span
                className={
                  activeTab === tab.id
                    ? "text-accent-bright"
                    : "text-text-muted"
                }
              >
                {tab.icon}
              </span>
              {tab.label}
            </button>
          ))}
        </div>

        {/* Content */}
        <div className="flex flex-1 flex-col overflow-y-auto scrollbar-thin">
          <div className="flex items-center justify-between px-8 pt-7 pb-5">
            <h2 className="font-display text-xl font-semibold text-text-primary">
              Overview
            </h2>
            <button
              onClick={closeModal}
              className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
            >
              <svg
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2.5"
                strokeLinecap="round"
              >
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>
          <div className="flex flex-col gap-4 px-8 pb-7">
            <h3 className="text-[14px] font-semibold text-text-primary">
              Server picture
            </h3>
            <p className="text-[13px] text-text-secondary">
              Shown in the server bar in place of the default gradient and
              letter. Square images work best; JPEG or PNG, max 1 MB.
            </p>
            <div className="flex items-center gap-6">
              {hasPicture && pictureDataUrl ? (
                <img
                  src={pictureDataUrl}
                  alt={server.name}
                  className="h-[120px] w-[120px] rounded-xl object-cover"
                />
              ) : (
                <div
                  className="flex h-[120px] w-[120px] items-center justify-center rounded-xl text-[44px] font-bold text-white"
                  style={{ background: stringToGradient(server.name) }}
                >
                  {server.name.charAt(0).toUpperCase()}
                </div>
              )}
              <div className="flex flex-col gap-2">
                <button
                  onClick={onUploadClick}
                  className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white transition-colors hover:bg-accent-hover"
                >
                  Upload picture
                </button>
                {hasPicture && (
                  <button
                    onClick={onRemove}
                    className="rounded-lg border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
                  >
                    Remove picture
                  </button>
                )}
                <input
                  ref={fileInputRef}
                  type="file"
                  accept="image/jpeg,image/png"
                  className="hidden"
                  onChange={onFileSelected}
                />
              </div>
            </div>
          </div>
        </div>
      </div>
      {cropperModal}
    </div>,
    document.body,
  );
}
