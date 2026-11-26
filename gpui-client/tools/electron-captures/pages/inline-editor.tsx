// The GPUI screen `inline-editor`: ArrowUp in the empty box edits the latest own message.
import { $, frames, start } from "./_composer";
export { default } from "./_composer";
export async function setup() {
  const shown = await start();
  const box = $(".rich-input");
  box.focus();
  box.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true, cancelable: true }));
  await frames(8);
  return shown;
}
