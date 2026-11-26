// The GPUI kit's `kit-settings`: the settings window at 1280 wide (see _settings.tsx).
import { KitSettings } from "./_settings";

export const size = { width: 1280, height: 800 };
export default KitSettings;
export async function setup() {
  return { framed: true };
}
