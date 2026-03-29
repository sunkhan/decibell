import { useState, useEffect } from "react";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

type UpdateState =
  | { status: "idle" }
  | { status: "downloading"; progress: number }
  | { status: "restarting" };

export default function UpdateChecker() {
  const [state, setState] = useState<UpdateState>({ status: "idle" });

  useEffect(() => {
    let cancelled = false;

    async function checkAndInstall() {
      try {
        const update = await check();
        if (cancelled || !update) return;

        setState({ status: "downloading", progress: 0 });

        let totalBytes = 0;
        let downloadedBytes = 0;

        await update.downloadAndInstall((event) => {
          if (event.event === "Started" && event.data.contentLength) {
            totalBytes = event.data.contentLength;
          } else if (event.event === "Progress") {
            downloadedBytes += event.data.chunkLength;
            if (totalBytes > 0) {
              setState({
                status: "downloading",
                progress: Math.round((downloadedBytes / totalBytes) * 100),
              });
            }
          }
        });

        if (!cancelled) {
          setState({ status: "restarting" });
          await relaunch();
        }
      } catch {
        // Silently ignore — don't block the app if update fails
      }
    }

    const timer = setTimeout(checkAndInstall, 3000);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, []);

  if (state.status === "idle") return null;

  return (
    <div className="flex items-center gap-3 border-b border-accent/20 bg-accent/5 px-4 py-2">
      {state.status === "downloading" && (
        <>
          <span className="text-xs text-text-secondary">
            Installing update... {state.progress > 0 ? `${state.progress}%` : ""}
          </span>
          <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-surface-active">
            <div
              className="h-full rounded-full bg-accent transition-all"
              style={{ width: `${state.progress}%` }}
            />
          </div>
        </>
      )}

      {state.status === "restarting" && (
        <span className="text-xs text-text-secondary">
          Restarting...
        </span>
      )}
    </div>
  );
}
