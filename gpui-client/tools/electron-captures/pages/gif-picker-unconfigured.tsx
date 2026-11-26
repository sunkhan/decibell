// The GPUI screen `gif-picker-unconfigured`: the GIFs tab without a key.
import { frames, gifs, press, sleep, start } from "./_composer";
export { default } from "./_composer";
export async function setup() {
  gifs(false);
  const shown = await start();
  localStorage.setItem("decibell.picker.tab", "gifs");
  await press("button[title='Emoji']");
  await sleep(400);
  await frames(6);
  return shown;
}
