// The GPUI screen `sidebar-dragging-channel` (see _servers.tsx).
import fixture from "./fixtures/servers-sidebar-dragging-channel.json";
import { ServersPage, openStage, type ServersFixture } from "./_servers";

export default ServersPage;
export const size = { width: 1280, height: 800 };
export const setup = () => openStage(fixture as unknown as ServersFixture);
