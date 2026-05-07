import { Outlet } from "react-router-dom";
import Titlebar from "./Titlebar";

// Always-on chrome wrapper. Both /login and / sit inside this layout
// so the custom Titlebar (with min/max/close) stays present from the
// moment the window opens. The outlet renders the active route's
// content beneath the titlebar.
//
// UpdateChecker (electron-updater) and ResizeHandles (edge-pull
// resize affordances on frameless windows) port with their own PRs.
export default function AppLayout() {
  return (
    <div className="relative flex h-screen w-screen flex-col bg-bg-primary text-text-primary">
      <Titlebar />
      <div className="flex min-h-0 flex-1">
        <Outlet />
      </div>
    </div>
  );
}
