// The GPUI settings screen `transfer-ownership`: bob chosen, "bo" typed.
import { SettingsPage, openServer, size, typeInto, $, frames, sleep } from "./_server_settings";

export default SettingsPage;
export { size };
export async function setup() {
  const out = await openServer("Overview");
  $('[aria-label="New owner"]').click();
  await sleep(100);
  typeInto(document.querySelector<HTMLInputElement>('input[placeholder="Find a member"]')!, "bob");
  await sleep(400);
  await frames(2);
  const bob = Array.from(document.querySelectorAll<HTMLElement>("[role=menuitem]")).find((b) => b.textContent?.includes("Bobby"));
  if (!bob) throw new Error("no Bobby in the owner picker");
  bob.click();
  await frames(2);
  typeInto(document.querySelector<HTMLInputElement>('input[placeholder="Type bob to confirm"]')!, "bo");
  await sleep(200);
  await frames(4);
  return out;
}
