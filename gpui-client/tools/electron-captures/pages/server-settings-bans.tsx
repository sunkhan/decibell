// The GPUI settings screen `server-settings-bans` (see _server_settings.tsx).
import { SettingsPage, openServer, size } from "./_server_settings";

export default SettingsPage;
export { size };
export const setup = () => openServer("Bans");
