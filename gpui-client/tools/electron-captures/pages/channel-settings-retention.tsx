// The GPUI settings screen `channel-settings-retention`: clips, Retention.
import { SettingsPage, openChannel, size } from "./_server_settings";

export default SettingsPage;
export { size };
export const setup = () => openChannel("clips", "Retention");
