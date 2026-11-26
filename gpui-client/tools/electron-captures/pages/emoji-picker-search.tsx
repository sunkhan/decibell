// The GPUI screen `emoji-picker-search`: the picker searching "smile".
import { $, fill, frames, press, start } from "./_composer";
export { default } from "./_composer";
export async function setup() {
  const shown = await start();
  await press("button[title='Emoji']");
  await fill($("input[placeholder='Search emoji...']") as HTMLInputElement, "smile");
  await frames(6);
  return shown;
}
