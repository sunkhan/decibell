// The GPUI screen `emoji-picker`: the picker open, with frequently used emoji.
import { press, start } from "./_composer";
export { default } from "./_composer";
export async function setup() {
  const shown = await start();
  localStorage.setItem("decibell.emoji.recent", JSON.stringify(["joy", "fire", "thumbsup", "heart", "eyes", "tada", "sob", "pray", "100", "rocket", "sparkles"]));
  await press("button[title='Emoji']");
  return shown;
}
