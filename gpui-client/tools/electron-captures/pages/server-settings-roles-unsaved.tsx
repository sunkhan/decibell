// The GPUI settings screen `server-settings-roles-unsaved`: Streamer renamed and recoloured.
import { SettingsPage, openServer, selectRole, size, typeInto, $, frames, sleep } from "./_server_settings";

export default SettingsPage;
export { size };
export async function setup() {
  const out = await openServer("Roles");
  await selectRole("Streamer", "Display");
  typeInto(document.querySelector<HTMLInputElement>('input[placeholder="e.g. Moderator"]')!, "Streamers");
  $('[aria-label="#e91e63"]').click();
  await sleep(300);
  await frames(4);
  return out;
}
