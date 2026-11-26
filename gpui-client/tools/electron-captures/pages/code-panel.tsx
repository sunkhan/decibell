// The GPUI screen `code-panel`: the code block panel with code typed, Rust chosen.
import { $, centre, click, fill, frames, press, start } from "./_composer";
export { default } from "./_composer";
export async function setup() {
  const shown = await start();
  await press("button[title='Insert code or math']");
  const code = [...document.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === "Code");
  if (code) await click(...centre(code));
  await fill($("textarea") as HTMLTextAreaElement, "fn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}");
  const lang = document.querySelector<HTMLElement>("[aria-label='Language']");
  if (lang) {
    await click(...centre(lang));
    const rust = [...document.querySelectorAll<HTMLElement>("[role='option']")].find((o) => o.textContent?.trim() === "Rust");
    if (rust) await click(...centre(rust));
  }
  await frames(6);
  return shown;
}
