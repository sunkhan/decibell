// The GPUI screen `composer-attachments`: chips ready (picture), uploading 42 %, queued, failed.
import { useAttachmentsStore } from "@app/stores/attachmentsStore";
import { frames, start } from "./_composer";
export { default } from "./_composer";

function picture(width: number, height: number): string {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const g = canvas.getContext("2d")!;
  const data = g.createImageData(width, height);
  for (let y = 0; y < height; y++)
    for (let x = 0; x < width; x++) {
      const t = (x + y) / (width + height);
      const i = (y * width + x) * 4;
      data.data[i] = 200 - 120 * t;
      data.data[i + 1] = 120 + 60 * t;
      data.data[i + 2] = 230 - 40 * t;
      data.data[i + 3] = 255;
    }
  g.putImageData(data, 0, 0);
  return canvas.toDataURL("image/jpeg", 0.85);
}

export async function setup() {
  const shown = await start();
  const base = { serverId: "1", channelId: "general", mime: "image/png", attachmentId: null, errorMessage: null, cancelled: false, paused: false, abortController: new AbortController(), width: 640, height: 400, durationMs: 0, placeholder: "", source: null as never, encrypted: false };
  const preview = picture(320, 200);
  const pendings = {
    a: { ...base, pendingId: "a", status: "ready", filename: "ready-sunset.png", kind: "image", totalBytes: 2_400_000, transferredBytes: 2_400_000, previewUrl: preview },
    b: { ...base, pendingId: "b", status: "uploading", filename: "uploading-trailer.png", kind: "image", totalBytes: 8_000_000, transferredBytes: 3_360_000, previewUrl: preview },
    c: { ...base, pendingId: "c", status: "queued", filename: "release-notes.pdf", kind: "document", mime: "application/pdf", totalBytes: 310_000, transferredBytes: 0, previewUrl: null },
    d: { ...base, pendingId: "d", status: "failed", filename: "failed-raw-footage.mov", kind: "video", mime: "video/quicktime", totalBytes: 900_000_000, transferredBytes: 0, previewUrl: null, errorMessage: "the file is larger than this server allows" },
  };
  useAttachmentsStore.setState({ pendings } as never);
  await frames(6);
  return shown;
}
