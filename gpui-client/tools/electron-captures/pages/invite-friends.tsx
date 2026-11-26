// The GPUI screen `invite-friends` (see _servers.tsx).
import fixture from "./fixtures/servers-invite-friends.json";
import { ServersPage, openStage, type ServersFixture } from "./_servers";

export default ServersPage;
export const size = { width: 1280, height: 800 };
export const setup = () => openStage(fixture as unknown as ServersFixture);
