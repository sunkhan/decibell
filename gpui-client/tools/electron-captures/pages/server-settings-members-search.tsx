// The GPUI settings screen `server-settings-members-search`: "er" typed and searched.
import { SettingsPage, openServer, size, typeInto, sleep, frames } from "./_server_settings";

export default SettingsPage;
export { size };
export async function setup() {
  const out = await openServer("Members");
  const input = document.querySelector<HTMLInputElement>('input[placeholder="Search members"]')!;
  typeInto(input, "er");
  await sleep(400);
  await frames(4);
  return out;
}
