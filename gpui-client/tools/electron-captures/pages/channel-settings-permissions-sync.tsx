// The GPUI settings screen `channel-settings-permissions-sync`: cat-text-channels, Permissions.
import { SettingsPage, openChannel, size } from "./_server_settings";

export default SettingsPage;
export { size };
export const setup = () => openChannel("cat-text-channels", "Permissions");
