// The GPUI screen `settings-about-error`: About with the updater's "error" state (see _settings_window.tsx).
import fixture from "./fixtures/settings-about-error.json";
import { SettingsPage, openSettings, type SettingsFixture } from "./_settings_window";

export default SettingsPage;
export const size = { width: 1280, height: 800 };
export const setup = () => openSettings(fixture as unknown as SettingsFixture);
