// The GPUI screen `math-panel`: the math panel with a formula and its rendering.
import { $, centre, click, fill, frames, press, start } from "./_composer";
export { default } from "./_composer";
export async function setup() {
  const shown = await start();
  await press("button[title='Insert code or math']");
  const math = [...document.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === "ΣMath" || b.textContent?.trim().endsWith("Math"));
  if (math) await click(...centre(math));
  await fill($("textarea") as HTMLTextAreaElement, "\\int_0^1 x^2\\,dx = \\frac{1}{3}");
  await frames(6);
  return shown;
}
