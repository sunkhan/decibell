// The GPUI settings screen `server-settings-roles-members`: Moderator, Members.
import { SettingsPage, openServer, selectRole, size } from "./_server_settings";

export default SettingsPage;
export { size };
export async function setup() {
  const out = await openServer("Roles");
  await selectRole("Moderator", "Members");
  return out;
}
