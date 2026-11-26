// The GPUI screen `settings-narrow` (see _settings_window.tsx).
import fixture from "./fixtures/settings-tab-privacy.json";
import { SettingsPage, openSettings, type SettingsFixture } from "./_settings_window";

export default SettingsPage;
export const size = { width: 900, height: 800 };
export const setup = () => openSettings(fixture as unknown as SettingsFixture);
