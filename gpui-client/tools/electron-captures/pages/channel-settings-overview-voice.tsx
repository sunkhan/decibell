// The GPUI settings screen `channel-settings-overview-voice`: lounge, Overview.
import { SettingsPage, openChannel, size } from "./_server_settings";

export default SettingsPage;
export { size };
export const setup = () => openChannel("lounge", "Overview");
