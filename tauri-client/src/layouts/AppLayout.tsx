import { Outlet } from "react-router-dom";
import UpdateChecker from "../features/updater/UpdateChecker";

export default function AppLayout() {
  return (
    <div className="flex h-screen w-screen flex-col bg-bg-primary text-text-primary">
      <UpdateChecker />
      <div className="flex min-h-0 flex-1">
        <Outlet />
      </div>
    </div>
  );
}
