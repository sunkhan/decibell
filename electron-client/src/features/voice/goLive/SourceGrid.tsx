import { useEffect, useRef, useState } from "react";
import type { CaptureSource } from "../../../types";
import { Icon, SectionLabel } from "../../servers/tabs/settingsUi";

/// Tabbed grid of screens + windows, populated from Chromium's
/// desktopCapturer via the preload bridge. Thumbnails are JPEG data URLs
/// (Chromium decodes them on assignment to <img>; no extra trip through
/// canvas). Re-polled at REFRESH_MS so the previews don't go stale while
/// the modal is open — Chromium re-snapshots the surfaces server-side,
/// which is the same path the in-browser screen-share dialog uses.
///
/// Each poll is synchronous image-encoding work in the main process (it
/// stalls every window), so it is kept small: a slow cadence, thumbnails
/// only for the active tab (the other tab keeps its last ones), and app
/// icons fetched once and reused by source id — re-requested only when a
/// window id shows up that no icon fetch has seen yet.
export default function SourceGrid({
  picked,
  onPick,
  disabled,
}: {
  picked: CaptureSource | null;
  onPick: (source: CaptureSource) => void;
  disabled?: boolean;
}) {
  const REFRESH_MS = 5000;
  const [tab, setTab] = useState<"screen" | "window">("screen");
  const [sources, setSources] = useState<CaptureSource[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  // Held across re-renders so the polling interval doesn't keep
  // reading stale `tab` state and so unmount cancels in-flight calls.
  const aliveRef = useRef(true);
  const tabRef = useRef(tab);
  // Per source id: last thumbnail / app icon we got. Ids an icon fetch has
  // already covered (icon or not) are remembered so a window without an
  // icon doesn't re-trigger icon fetches on every poll.
  const thumbsRef = useRef(new Map<string, string>());
  const iconsRef = useRef(new Map<string, string>());
  const iconCheckedRef = useRef(new Set<string>());
  const needIconsRef = useRef(true);
  const fetchRef = useRef<(() => Promise<void>) | null>(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    aliveRef.current = true;
    let inFlight = false;
    // A request (tab switch) that arrived mid-fetch runs once it lands.
    let again = false;
    const fetchOnce = async (): Promise<void> => {
      if (inFlight) {
        again = true;
        return;
      }
      inFlight = true;
      const withIcons = needIconsRef.current;
      try {
        // Screens fill two wide tiles, so they get twice the pixels; there
        // are only one or two of them. Windows can number in dozens.
        const big = tabRef.current === "screen";
        const list = await window.decibell.capture.listSources({
          thumbnailWidth: big ? 640 : 320,
          thumbnailHeight: big ? 360 : 180,
          thumbnailKinds: [tabRef.current],
          fetchWindowIcons: withIcons,
        });
        if (!aliveRef.current) return;
        const thumbs = thumbsRef.current;
        const icons = iconsRef.current;
        const checked = iconCheckedRef.current;
        let unseenWindow = false;
        const merged = list.map((src) => {
          if (src.thumbnail) thumbs.set(src.id, src.thumbnail);
          if (withIcons) {
            checked.add(src.id);
            if (src.appIcon) icons.set(src.id, src.appIcon);
          } else if (src.kind === "window" && !checked.has(src.id)) {
            unseenWindow = true;
          }
          return {
            ...src,
            thumbnail: src.thumbnail || thumbs.get(src.id) || "",
            appIcon: src.appIcon || icons.get(src.id) || "",
          };
        });
        if (withIcons) needIconsRef.current = false;
        // A window appeared since the last icon fetch: get icons next poll.
        if (unseenWindow) needIconsRef.current = true;
        // Skip the re-render (and <img> re-decode) when nothing changed —
        // a static screen re-encodes to the identical JPEG.
        setSources((prev) =>
          prev.length === merged.length &&
          prev.every(
            (p, i) =>
              p.id === merged[i].id &&
              p.name === merged[i].name &&
              p.thumbnail === merged[i].thumbnail &&
              p.appIcon === merged[i].appIcon,
          )
            ? prev
            : merged,
        );
        setLoaded(true);
        setLoadError(null);
      } catch (e) {
        if (aliveRef.current) setLoadError(String(e));
      } finally {
        inFlight = false;
        if (again && aliveRef.current) {
          again = false;
          void fetchOnce();
        }
      }
    };
    fetchRef.current = fetchOnce;
    void fetchOnce();
    const id = window.setInterval(fetchOnce, REFRESH_MS);
    return () => {
      aliveRef.current = false;
      fetchRef.current = null;
      window.clearInterval(id);
    };
  }, []);

  // Switching tabs: fetch right away so the newly visible kind gets fresh
  // thumbnails instead of waiting up to REFRESH_MS.
  const firstTabRef = useRef(true);
  useEffect(() => {
    tabRef.current = tab;
    if (firstTabRef.current) {
      firstTabRef.current = false;
      return;
    }
    void fetchRef.current?.();
  }, [tab]);

  const screens = sources.filter((s) => s.kind === "screen");
  const windows = sources.filter((s) => s.kind === "window");
  const visible = tab === "screen" ? screens : windows;
  // Screens are few and wide: two big tiles. Windows come in dozens.
  const cols = tab === "screen" ? "grid-cols-2" : "grid-cols-2 lg:grid-cols-3";

  return (
    // Stacked: a capped grid above the settings. Side by side (lg): the
    // column fills the dialog's height and only the grid scrolls.
    <div className="flex shrink-0 flex-col gap-3 px-5 pb-5 lg:min-w-0 lg:flex-1 lg:pt-5">
      <div className="flex h-8 shrink-0 items-center justify-between gap-3">
        <SectionLabel>What to share</SectionLabel>
        <div role="tablist" aria-label="Source type" className="flex rounded-md bg-bg-darkest p-[3px]">
          {([
            { value: "screen" as const, label: "Screens", icon: "monitor" as const, count: screens.length },
            { value: "window" as const, label: "Windows", icon: "appWindow" as const, count: windows.length },
          ]).map((t) => (
            <button
              key={t.value}
              type="button"
              role="tab"
              aria-selected={tab === t.value}
              onClick={() => setTab(t.value)}
              className={`flex items-center gap-1.5 rounded-sm px-3 py-1 text-[12px] font-semibold ${
                tab === t.value
                  ? "bg-accent-mid text-accent-bright"
                  : "text-text-muted hover:text-text-secondary"
              }`}
            >
              <Icon name={t.icon} size={13} />
              {t.label}
              {loaded && <span className="font-meta font-medium tabular-nums opacity-70">{t.count}</span>}
            </button>
          ))}
        </div>
      </div>
      {loadError ? (
        <p className="py-6 text-center text-[12px] text-error">{loadError}</p>
      ) : !loaded ? (
        <div className={`grid ${cols} content-start gap-2`} aria-busy="true">
          {Array.from({ length: tab === "screen" ? 2 : 6 }, (_, i) => (
            <div key={i} className="flex flex-col gap-2 p-1.5">
              <div className="aspect-video animate-pulse rounded-md bg-bg-light" />
              <div className="mx-1 h-3 w-2/3 animate-pulse rounded-sm bg-bg-light" />
            </div>
          ))}
        </div>
      ) : visible.length === 0 ? (
        <div className="flex flex-col items-center gap-2 px-6 py-10 text-center">
          <Icon name={tab === "screen" ? "monitor" : "appWindow"} size={22} className="text-text-muted" />
          <p className="text-[13px] font-medium text-text-secondary">
            No {tab === "screen" ? "screens" : "windows"} to share
          </p>
          {tab === "window" && (
            <p className="text-[12px] text-text-muted">Open the app you want to show. It turns up here in a few seconds.</p>
          )}
        </div>
      ) : (
        <div
          role="radiogroup"
          aria-label={tab === "screen" ? "Screens" : "Windows"}
          className={`grid max-h-[300px] auto-rows-max ${cols} content-start gap-2 overflow-y-auto pr-1 lg:max-h-none lg:min-h-0 lg:flex-1 ${
            disabled ? "pointer-events-none opacity-60" : ""
          }`}
        >
          {visible.map((s) => {
            const on = picked?.id === s.id;
            return (
              <button
                key={s.id}
                type="button"
                role="radio"
                aria-checked={on}
                onClick={() => onPick(s)}
                className={`group flex min-w-0 flex-col gap-2 rounded-lg p-1.5 text-left ${
                  on ? "bg-accent-soft" : "hover:bg-surface-hover"
                }`}
              >
                <div
                  className={`relative aspect-video overflow-hidden rounded-md border-2 bg-bg-darkest ${
                    on ? "border-accent" : "border-border-divider group-hover:border-border"
                  }`}
                >
                  {/* draggable=false stops Chromium from initiating an HTML5
                      drag on the thumbnail when the user click-and-holds —
                      the picker should feel like buttons, not images. */}
                  {s.thumbnail && (
                    <img
                      src={s.thumbnail}
                      alt=""
                      draggable={false}
                      className="h-full w-full object-contain"
                    />
                  )}
                  {on && (
                    <span className="absolute top-1.5 right-1.5 flex h-5 w-5 items-center justify-center rounded-full bg-accent text-on-accent shadow-raised">
                      <Icon name="check" size={12} strokeWidth={3} />
                    </span>
                  )}
                </div>
                <div className="flex w-full min-w-0 items-center gap-2 px-1 pb-0.5">
                  {s.appIcon ? (
                    <img src={s.appIcon} alt="" draggable={false} className="h-4 w-4 shrink-0" />
                  ) : (
                    <Icon name="monitor" size={14} className={on ? "text-accent" : "text-text-muted"} />
                  )}
                  <span
                    className={`min-w-0 truncate text-[13px] ${
                      on ? "font-semibold text-text-bright" : "font-medium text-text-secondary group-hover:text-text-primary"
                    }`}
                    title={s.name}
                  >
                    {s.name}
                  </span>
                </div>
              </button>
            );
          })}
        </div>
      )}
      {tab === "screen" && loaded && !loadError && (
        <p className="flex shrink-0 items-center gap-2 text-[12px] text-text-muted">
          <Icon name="appWindow" size={14} />
          Sharing one window keeps notifications and other apps out of view.
        </p>
      )}
    </div>
  );
}
