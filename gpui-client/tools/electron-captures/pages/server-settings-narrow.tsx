// The GPUI settings screen `server-settings-narrow` (`decibell-capture --size 900x700`): the rail.
import { SettingsPage, openServer } from "./_server_settings";

export default SettingsPage;
export const size = { width: 900, height: 700 };
export const setup = () => openServer("Overview");
