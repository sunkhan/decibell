import type { DeviceEntry } from "../../types";

const PLATFORM_LABEL: Record<string, string> = { linux: "Linux", windows: "Windows", macos: "macOS" };

export function platformLabel(platform: string): string {
  return PLATFORM_LABEL[platform] ?? "Unknown system";
}

/// What the device list and the new sign-in notice call a device: the OS
/// name it reported, else its platform.
export function deviceTitle(d: Pick<DeviceEntry, "name" | "platform">): string {
  return d.name || platformLabel(d.platform);
}
