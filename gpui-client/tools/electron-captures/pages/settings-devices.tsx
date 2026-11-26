// The GPUI screen `settings-devices` (see _settings_window.tsx).
import fixture from "./fixtures/settings-tab-devices.json";
import { SettingsPage, openSettings, type SettingsFixture } from "./_settings_window";

export default SettingsPage;
export const size = { width: 1280, height: 800 };
export const setup = () => openSettings(fixture as unknown as SettingsFixture);
