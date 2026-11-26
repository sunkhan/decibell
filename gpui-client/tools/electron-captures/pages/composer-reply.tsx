// The GPUI screen `composer-reply`: replying to the third row from the bottom (its hover bar's
// Reply button).
import { centre, click, frames, hover, start } from "./_composer";
export { default } from "./_composer";
export async function setup() {
  const shown = await start();
  const rows = [...document.querySelectorAll<HTMLElement>("[data-key]")];
  const row = rows[rows.length - 3];
  await hover(...centre(row));
  const reply = row.querySelector<HTMLElement>("button[title='Reply']");
  if (reply) await click(...centre(reply));
  // Leave the pointer where nothing reacts to it.
  await hover(640, 20);
  await frames(6);
  return { ...shown, replied: row.dataset.key };
}
