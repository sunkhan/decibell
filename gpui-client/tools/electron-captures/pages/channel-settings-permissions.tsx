// The GPUI settings screen `channel-settings-permissions`: general, Permissions.
import { SettingsPage, openChannel, size } from "./_server_settings";

export default SettingsPage;
export { size };
export const setup = () => openChannel("general", "Permissions");
