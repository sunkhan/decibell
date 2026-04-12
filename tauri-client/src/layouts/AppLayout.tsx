import { Outlet } from "react-router-dom";
import UpdateChecker from "../features/updater/UpdateChecker";
import Titlebar from "./Titlebar";
import ResizeHandles from "./ResizeHandles";

export default function AppLayout() {
  return (
    <div className="relative flex h-screen w-screen flex-col bg-bg-primary text-text-primary">
      <Titlebar />
      <UpdateChecker />
      <div className="flex min-h-0 flex-1">
        <Outlet />
      </div>
      <ResizeHandles />
    </div>
  );
}
