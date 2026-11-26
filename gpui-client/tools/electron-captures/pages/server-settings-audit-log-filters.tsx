// The GPUI settings screen `server-settings-audit-log-filters`: the action filter open.
import { SettingsPage, openServer, size, $, frames, sleep } from "./_server_settings";

export default SettingsPage;
export { size };
export async function setup() {
  const out = await openServer("Audit Log");
  $('[aria-label="Filter by action"]').click();
  await sleep(200);
  await frames(4);
  return out;
}
