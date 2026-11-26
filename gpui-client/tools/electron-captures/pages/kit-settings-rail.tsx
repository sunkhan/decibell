// The GPUI kit's `kit-settings-rail`: the settings window below 1024 px, folded to the icon rail.
import { KitSettings } from "./_settings";

export const size = { width: 900, height: 800 };
export default KitSettings;
export async function setup() {
  return { framed: true };
}
