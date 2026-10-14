import { useEffect, useMemo, useState } from "react";
import { invoke, listen } from "../../../lib/ipc";
import { useChatStore } from "../../../stores/chatStore";
import { EMPTY_LIST } from "../../../lib/empty";
import { formatFileSize } from "../../chat/attachmentHelpers";
import { toast } from "../../../stores/toastStore";
import type { StorageInfoReceivedPayload } from "../../../types";
import { useEchoSave } from "../saveEcho";
import { useLeaveGuard } from "../leaveGuard";
import { SaveBar } from "./settingsUi";

const KIND_LABELS: Record<number, string> = {
  0: "Images",
  1: "Videos",
  2: "Documents",
  3: "Audio",
};

const GIB = 1024 * 1024 * 1024;

/// Storage (MANAGE_SERVER): host disk state for the machine running this
/// community server, this server's own footprint, and the editable upload
/// headroom. Numbers come from the server's storage_info_received event; the
/// volume figures are std::filesystem::space on the host.
export default function StorageTab({ serverId }: { serverId: string }) {
  const channels = useChatStore((s) => s.channelsByServer[serverId] ?? EMPTY_LIST);
  const channelName = (id: string) => channels.find((c) => c.id === id)?.name ?? id;

  const [info, setInfo] = useState<StorageInfoReceivedPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Headroom editor (in GB, so operators think in whole-ish numbers).
  const [minFreeGb, setMinFreeGb] = useState("");
  // The reply to a save is the echo: it carries the stored value (after
  // the server's clamp). The wait only runs out if no reply comes.
  const { saving: savingMin, start: startMin, stop: stopMin } = useEchoSave<null>(
    () => false,
    () => {},
  );

  useEffect(() => {
    setLoading(true);
    invoke("get_storage_info", { serverId }).catch((err) => {
      setLoading(false);
      setError(String(err));
    });
  }, [serverId]);

  useEffect(() => {
    const un = listen<StorageInfoReceivedPayload>("storage_info_received", (event) => {
      const p = event.payload;
      if (p.serverId !== serverId) return;
      setLoading(false);
      stopMin();
      if (!p.success) {
        setError(p.message || "Couldn't load storage info.");
        return;
      }
      setError(null);
      // i64 fields cross the native→JS bridge as BigInt once they exceed
      // int32 (disk sizes are ~1e11); coerce every numeric field to Number so
      // arithmetic (Math.max/min, division, formatFileSize) works. Byte
      // counts stay far under 2^53, so no precision is lost.
      const norm: StorageInfoReceivedPayload = {
        serverId: p.serverId,
        success: p.success,
        message: p.message,
        volumeTotalBytes: Number(p.volumeTotalBytes),
        volumeAvailableBytes: Number(p.volumeAvailableBytes),
        attachmentsBytes: Number(p.attachmentsBytes),
        thumbnailsBytes: Number(p.thumbnailsBytes),
        databaseBytes: Number(p.databaseBytes),
        attachmentCount: Number(p.attachmentCount),
        minFreeBytes: Number(p.minFreeBytes),
        byKind: p.byKind.map((k) => ({
          kind: Number(k.kind),
          bytes: Number(k.bytes),
          count: Number(k.count),
        })),
        byChannel: p.byChannel.map((c) => ({
          channelId: c.channelId,
          bytes: Number(c.bytes),
          count: Number(c.count),
        })),
        largest: p.largest.map((l) => ({
          attachmentId: Number(l.attachmentId),
          filename: l.filename,
          sizeBytes: Number(l.sizeBytes),
          channelId: l.channelId,
          kind: Number(l.kind),
        })),
      };
      setInfo(norm);
      // storage_info_received only ever arrives as a reply to this tab's own
      // fetch or save (never an unsolicited broadcast), so syncing the field
      // to the authoritative value here can't clobber in-progress typing and
      // correctly reflects a server-side clamp after a save.
      setMinFreeGb((norm.minFreeBytes / GIB).toFixed(1));
    });
    return () => {
      un.then((fn) => fn());
    };
  }, [serverId, stopMin]);

  const used = info ? Math.max(0, info.volumeTotalBytes - info.volumeAvailableBytes) : 0;
  const usedPct =
    info && info.volumeTotalBytes > 0 ? Math.min(100, (used / info.volumeTotalBytes) * 100) : 0;
  const lowSpace = !!info && info.volumeAvailableBytes <= info.minFreeBytes;
  const serverFootprint = info
    ? info.attachmentsBytes + info.thumbnailsBytes + info.databaseBytes
    : 0;

  const saveMinFree = () => {
    const gb = parseFloat(minFreeGb);
    if (!isFinite(gb) || gb < 0) {
      toast.error("Invalid value", "Enter a number of gigabytes (0 or more).");
      return;
    }
    startMin(null);
    invoke("set_storage_min_free", {
      serverId,
      minFreeBytes: Math.round(gb * GIB),
    }).catch((err) => {
      stopMin();
      toast.error("Couldn't update headroom", String(err));
    });
  };

  // Anything but the stored value is a change, a half-typed number too:
  // Save says what's wrong with it rather than the edit vanishing.
  const dirtyMinFree = useMemo(() => {
    if (!info) return false;
    const gb = parseFloat(minFreeGb);
    if (!isFinite(gb) || gb < 0) return true;
    return Math.round(gb * GIB) !== info.minFreeBytes;
  }, [minFreeGb, info]);
  // Closing the window or changing tab shakes the save bar (leaveGuard.ts).
  const leaveNudge = useLeaveGuard(dirtyMinFree);

  if (loading && !info) {
    return <p className="py-8 text-center text-[13px] text-text-muted">Loading storage…</p>;
  }
  if (error && !info) {
    return <p className="py-8 text-center text-[13px] text-error">{error}</p>;
  }
  if (!info) return null;

  return (
    <div>
      <div className="flex flex-col gap-5">
        {error && <p className="text-[12px] text-error">{error}</p>}

        {/* Host volume */}
        <section>
          <h3 className="mb-2 text-[13px] font-semibold text-text-primary">Host disk</h3>
          <div className="rounded-md border border-border p-3">
            <div className="mb-2 flex items-baseline justify-between text-[12px]">
              <span className="text-text-secondary">
                {formatFileSize(used)} / {formatFileSize(info.volumeTotalBytes)} used
              </span>
              <span className={lowSpace ? "text-error" : "text-text-muted"}>
                {formatFileSize(info.volumeAvailableBytes)} available
              </span>
            </div>
            <div className="h-2.5 w-full overflow-hidden rounded-full bg-bg-light">
              <div
                className={`h-full rounded-full ${lowSpace ? "bg-error" : "bg-accent"}`}
                style={{ width: `${usedPct}%` }}
              />
            </div>
            {lowSpace && (
              <p className="mt-2 text-[11px] text-error">
                Available space is at or below the upload headroom — new uploads are being
                refused until space is freed or the headroom is lowered.
              </p>
            )}
          </div>
        </section>

        {/* Upload headroom (editable) */}
        <section>
          <h3 className="mb-2 text-[13px] font-semibold text-text-primary">Upload headroom</h3>
          <p className="mb-2 text-[12px] text-text-muted">
            Refuse uploads that would leave the disk with less than this much free space.
          </p>
          <div className="flex items-center gap-2">
            <input
              type="number"
              min={0}
              step="0.5"
              value={minFreeGb}
              onChange={(e) => setMinFreeGb(e.target.value)}
              className="w-28 rounded-md border border-border bg-bg-light px-2.5 py-1.5 text-[13px] text-text-primary outline-none focus:border-accent"
            />
            <span className="text-[13px] text-text-secondary">GB</span>
          </div>
        </section>

        {/* This server's footprint */}
        <section>
          <h3 className="mb-2 text-[13px] font-semibold text-text-primary">
            This server is using {formatFileSize(serverFootprint)}
          </h3>
          <div className="grid grid-cols-3 gap-2">
            <Stat label="Attachments" value={formatFileSize(info.attachmentsBytes)}
                  sub={`${info.attachmentCount.toLocaleString()} files`} />
            <Stat label="Thumbnails" value={formatFileSize(info.thumbnailsBytes)} />
            <Stat label="Database" value={formatFileSize(info.databaseBytes)} />
          </div>
        </section>

        {/* By type */}
        {info.byKind.length > 0 && (
          <section>
            <h3 className="mb-2 text-[13px] font-semibold text-text-primary">By type</h3>
            <div className="flex flex-col divide-y divide-border-divider rounded-md border border-border">
              {[...info.byKind]
                .sort((a, b) => b.bytes - a.bytes)
                .map((k) => (
                  <Row
                    key={k.kind}
                    left={KIND_LABELS[k.kind] ?? `Kind ${k.kind}`}
                    right={formatFileSize(k.bytes)}
                    sub={`${k.count.toLocaleString()} files`}
                  />
                ))}
            </div>
          </section>
        )}

        {/* By channel */}
        {info.byChannel.length > 0 && (
          <section>
            <h3 className="mb-2 text-[13px] font-semibold text-text-primary">By channel</h3>
            <div className="flex flex-col divide-y divide-border-divider rounded-md border border-border">
              {info.byChannel.map((c) => (
                <Row
                  key={c.channelId}
                  left={`#${channelName(c.channelId)}`}
                  right={formatFileSize(c.bytes)}
                  sub={`${c.count.toLocaleString()} files`}
                />
              ))}
            </div>
          </section>
        )}

        {/* Largest files */}
        {info.largest.length > 0 && (
          <section>
            <h3 className="mb-2 text-[13px] font-semibold text-text-primary">Largest files</h3>
            <div className="flex flex-col divide-y divide-border-divider rounded-md border border-border">
              {info.largest.map((l) => (
                <Row
                  key={l.attachmentId}
                  left={l.filename || `#${l.attachmentId}`}
                  right={formatFileSize(l.sizeBytes)}
                  sub={`#${channelName(l.channelId)}`}
                  truncateLeft
                />
              ))}
            </div>
          </section>
        )}
      </div>
      <SaveBar
        show={dirtyMinFree}
        nudge={leaveNudge}
        saving={savingMin}
        onReset={() => setMinFreeGb((info.minFreeBytes / GIB).toFixed(1))}
        onSave={saveMinFree}
      />
    </div>
  );
}

function Stat({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="rounded-md border border-border p-2.5">
      <div className="text-[11px] text-text-muted">{label}</div>
      <div className="text-[14px] font-semibold text-text-primary">{value}</div>
      {sub && <div className="text-[11px] text-text-muted">{sub}</div>}
    </div>
  );
}

function Row({
  left,
  right,
  sub,
  truncateLeft,
}: {
  left: string;
  right: string;
  sub?: string;
  truncateLeft?: boolean;
}) {
  return (
    <div className="flex items-center justify-between gap-3 px-3 py-2">
      <div className={`min-w-0 ${truncateLeft ? "flex-1" : ""}`}>
        <div className={`text-[13px] text-text-primary ${truncateLeft ? "truncate" : ""}`}>
          {left}
        </div>
        {sub && <div className="text-[11px] text-text-muted">{sub}</div>}
      </div>
      <div className="shrink-0 text-[13px] text-text-secondary">{right}</div>
    </div>
  );
}
