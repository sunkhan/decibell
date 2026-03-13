import { Outlet } from "react-router-dom";

export default function AppLayout() {
  return (
    <div className="flex h-screen w-screen bg-bg-primary text-text-primary">
      <Outlet />
    </div>
  );
}
