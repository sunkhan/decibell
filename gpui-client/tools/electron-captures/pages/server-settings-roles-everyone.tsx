// The GPUI settings screen `server-settings-roles-everyone`: @everyone, Permissions.
import { SettingsPage, openServer, selectRole, size } from "./_server_settings";

export default SettingsPage;
export { size };
export async function setup() {
  const out = await openServer("Roles");
  await selectRole("@everyone", "Permissions");
  return out;
}
