// The GPUI settings screen `channel-settings-overview-category`: cat-text-channels, Overview.
import { SettingsPage, openChannel, size } from "./_server_settings";

export default SettingsPage;
export { size };
export const setup = () => openChannel("cat-text-channels", "Overview");
