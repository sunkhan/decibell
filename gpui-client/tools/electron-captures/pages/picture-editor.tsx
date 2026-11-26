// The GPUI settings screen `picture-editor`: the server picture editor on the generated test card
// (decibell_picture_editor::picture::sample_rgba(1600, 900), drawn here pixel for pixel).
import { SettingsPage, openServer, size, frames, sleep } from "./_server_settings";

export default SettingsPage;
export { size };

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

export async function setup() {
  const out = await openServer("Overview");
  const file = new File([await sample(1600, 900)], "sample.png", { type: "image/png" });
  const input = document.querySelector<HTMLInputElement>('input[type=file]')!;
  const dt = new DataTransfer();
  dt.items.add(file);
  input.files = dt.files;
  input.dispatchEvent(new Event("change", { bubbles: true }));
  await sleep(900);
  await frames(6);
  return out;
}
