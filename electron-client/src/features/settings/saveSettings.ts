// Settings persistence — call this after a user-facing setting
// changes so the value sticks across restarts. PR5 ships this as a
// no-op because the load_config / save_settings native commands port
// with the settings-modal PR (the AppSettings struct needs napi
// (object) annotations across all its fields, which is settings-PR
// scope). Until then preferences live in `useUiStore` for the
// session but reset on app restart. The function exists now so call
// sites (DeviceContextMenu, UserContextMenu, settings tabs once
// they port) don't need to change shape later.
export function saveSettings(): void {
  // Intentional no-op until the settings-modal PR.
}
