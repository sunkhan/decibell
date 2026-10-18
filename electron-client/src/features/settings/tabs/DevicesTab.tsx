import { useCallback, useEffect, useState } from "react";
import { invoke } from "../../../lib/ipc";
import ConfirmModal from "../../../components/ConfirmModal";
import { useDevicesStore } from "../../../stores/devicesStore";
import { toast } from "../../../stores/toastStore";
import { formatAgo } from "../../transfers/format";
import { deviceTitle, platformLabel } from "../deviceLabels";
import type { DeviceEntry } from "../../../types";

// The devices signed in to this account (docs/superpowers/specs/2026-10-19-
// devices-and-password-design.md): every install that signed in by password
// and hasn't been signed out since. Signing one out closes its connections,
// stops its saved password and reconnect token from working, and drops its
// community sessions within a minute; it needs the password to sign in
// again. Reloads when central says the list changed (`devices_changed`).

type Confirming = { kind: "one"; device: DeviceEntry } | { kind: "all"; count: number } | null;

export default function DevicesTab() {
  const revision = useDevicesStore((s) => s.revision);
  const [devices, setDevices] = useState<DeviceEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<Confirming>(null);
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  const load = useCallback(async () => {
    try {
      const list = await invoke<DeviceEntry[]>("list_devices");
      setDevices(list);
      setError(null);
    } catch (err) {
      setError(String(err).replace(/^Error:\s*/, ""));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load, revision]);

  // "Last active 5 min ago" moves on while the tab is open.
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(t);
  }, []);

  const signOut = async (ids: number[], allOthers: boolean) => {
    setBusy(true);
    try {
      const res = await invoke<{ signedOut: number }>("sign_out_devices", { ids, allOthers });
      const n = res.signedOut;
      if (allOthers) {
        toast.success("Signed out", n === 1 ? "1 other device was signed out." : `${n} other devices were signed out.`);
      } else {
        toast.success("Device signed out");
      }
    } catch (err) {
      toast.error("Couldn't sign out", String(err).replace(/^Error:\s*/, ""));
    } finally {
      setBusy(false);
      setConfirming(null);
      void load();
    }
  };

  const current = devices?.find((d) => d.current) ?? null;
  const others = devices?.filter((d) => !d.current) ?? [];

  return (
    <div className="flex flex-col gap-5">
      <p className="text-[12px] leading-[1.55] text-text-muted">
        These devices are signed in to your account. Sign out any you don't use or don't recognize: a device you sign
        out needs your password to sign in again. If you don't recognize one, change your password too (Account).
      </p>

      {error && (
        <div className="flex items-center justify-between gap-4 rounded-md border border-error/20 bg-error/10 px-4 py-3 text-[12px] text-error">
          <span>{error}</span>
          <button onClick={() => void load()} className="shrink-0 font-semibold underline-offset-2 hover:underline">
            Try again
          </button>
        </div>
      )}

      {!devices && !error && <div className="text-[12px] text-text-muted">Loading your devices…</div>}

      {current && (
        <section>
          <SectionLabel>This device</SectionLabel>
          <DeviceRow device={current} now={now} />
        </section>
      )}

      {devices && (
        <section>
          <SectionLabel>Other devices</SectionLabel>
          {others.length === 0 ? (
            <div className="rounded-md border border-border-divider bg-bg-light px-4 py-3.5 text-[12px] text-text-muted">
              No other devices are signed in.
            </div>
          ) : (
            <div className="flex flex-col gap-2">
              {others.map((d) => (
                <DeviceRow
                  key={d.id}
                  device={d}
                  now={now}
                  onSignOut={() => setConfirming({ kind: "one", device: d })}
                  disabled={busy}
                />
              ))}
            </div>
          )}
          {others.length > 1 && (
            <button
              onClick={() => setConfirming({ kind: "all", count: others.length })}
              disabled={busy}
              className="mt-3 rounded-md border border-error/20 bg-error/10 px-4 py-2 text-[13px] font-medium text-error transition-colors hover:bg-error/20 disabled:opacity-50"
            >
              Sign out of all other devices
            </button>
          )}
        </section>
      )}

      <ConfirmModal
        open={confirming !== null}
        title={confirming?.kind === "all" ? "Sign out of all other devices?" : "Sign out this device?"}
        confirmLabel={confirming?.kind === "all" ? "Sign out all" : "Sign out"}
        confirmDisabled={busy}
        onCancel={() => !busy && setConfirming(null)}
        onConfirm={() => {
          if (!confirming) return;
          if (confirming.kind === "all") void signOut([], true);
          else void signOut([confirming.device.id], false);
        }}
      >
        {confirming?.kind === "all"
          ? `Every device but this one (${confirming.count}) will be signed out and need your password to sign in again.`
          : confirming
            ? `${deviceTitle(confirming.device)}${confirming.device.address ? ` (${confirming.device.address})` : ""} will be signed out and need your password to sign in again. If you don't recognize it, change your password too.`
            : ""}
      </ConfirmModal>
    </div>
  );
}

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="mb-2.5 pl-0.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">{children}</div>
  );
}

function DeviceRow({
  device,
  now,
  onSignOut,
  disabled,
}: {
  device: DeviceEntry;
  now: number;
  onSignOut?: () => void;
  disabled?: boolean;
}) {
  // The platform only when the title doesn't already say it ("CachyOS", not "Windows").
  const platform = platformLabel(device.platform);
  const details = [device.appVersion && `Decibell ${device.appVersion}`, deviceTitle(device) !== platform && platform]
    .filter(Boolean)
    .join(" · ");
  const signedIn = new Date(Number(device.signedInAt) * 1000).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
  return (
    <div className="flex items-center gap-3.5 rounded-md border border-border-divider bg-bg-light px-4 py-3.5">
      <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-md bg-bg-darkest text-text-secondary">
        <MonitorGlyph />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-[14px] font-medium text-text-primary">{deviceTitle(device)}</span>
          {device.current && (
            <span className="shrink-0 rounded-sm bg-accent-soft px-1.5 py-px font-channel text-[10px] font-semibold uppercase tracking-[0.06em] text-accent-bright">
              This device
            </span>
          )}
        </div>
        {details && <div className="mt-0.5 truncate text-[12px] text-text-secondary">{details}</div>}
        <div className="mt-0.5 flex flex-wrap items-center gap-x-1.5 text-[12px] text-text-muted">
          {device.online ? (
            <span className="flex items-center gap-1.5 text-success">
              <span className="h-2 w-2 rounded-full bg-success" aria-hidden />
              Active now
            </span>
          ) : (
            <span>Last active {formatAgo(Number(device.lastActiveAt) * 1000, now)}</span>
          )}
          {device.address && (
            <>
              <span aria-hidden>·</span>
              <span className="select-text">{device.address}</span>
            </>
          )}
          <span aria-hidden>·</span>
          <span>Signed in {signedIn}</span>
        </div>
      </div>
      {onSignOut && (
        <button
          onClick={onSignOut}
          disabled={disabled}
          className="shrink-0 rounded-sm border border-error/20 bg-error/10 px-3 py-1.5 text-[12px] font-medium text-error transition-colors hover:bg-error/20 disabled:opacity-50"
        >
          Sign out
        </button>
      )}
    </div>
  );
}

function MonitorGlyph() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <rect x="2" y="4" width="20" height="13" rx="2" />
      <path d="M8 21h8M12 17v4" />
    </svg>
  );
}
