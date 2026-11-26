// The GPUI screen `picture-editor-server`: the server picture editor on the generated test card
// (decibell_picture_editor::picture::sample_rgba(1600, 900)), over bg-primary.
import { useEffect, useState } from "react";
import { ServerPictureCropperModal } from "@app/features/servers/ServerPictureCropperModal";
import { useChatStore } from "@app/stores/chatStore";
import { Framed } from "./_frame";
import { frames, sleep } from "../runtime/harness";

export const size = { width: 1280, height: 800 };

function sample(w: number, h: number): Promise<Blob> {
  const c = new OffscreenCanvas(w, h);
  const ctx = c.getContext("2d")!;
  const img = ctx.createImageData(w, h);
  const cx = w * 0.38, cy = h * 0.45, r = Math.min(w, h) * 0.22;
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const t = (x / w + y / h) / 2;
      const lerp = (a: number, b: number) => Math.round(a + (b - a) * t);
      const d = Math.hypot(x - cx, y - cy);
      const p = (y * w + x) * 4;
      const rgb = d < r ? [36, 28, 52] : Math.abs(y / h - 0.72) < 0.04 ? [246, 236, 214] : [lerp(232, 58), lerp(120, 74), lerp(84, 168)];
      img.data.set([...rgb, 255], p);
    }
  }
  ctx.putImageData(img, 0, 0);
  return c.convertToBlob({ type: "image/png" });
}


export default function Page() {
  const [file, setFile] = useState<File | null>(null);
  useEffect(() => {
    useChatStore.setState({ servers: [{ id: "1", name: "Daily Gaming", description: "", hostIp: "", port: 0, memberCount: 298 }] } as never);
    sample(1600, 900).then((b) => setFile(new File([b], "sample.png", { type: "image/png" })));
  }, []);
  return <Framed>{file && <ServerPictureCropperModal serverId="1" file={file} onSaved={() => {}} onClose={() => {}} />}</Framed>;
}

export async function setup() {
  await sleep(900);
  await frames(6);
  return { framed: true };
}
