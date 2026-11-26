// The GPUI settings screen `color-picker`: Event Host's custom colour open in the picker.
import { SettingsPage, openServer, selectRole, size, frames, sleep } from "./_server_settings";

export default SettingsPage;
export { size };
export async function setup() {
  const out = await openServer("Roles");
  await selectRole("Event Host", "Display");
  document.querySelector<HTMLElement>('[aria-label^="Custom color"]')!.click();
  await sleep(200);
  await frames(4);
  return out;
}
