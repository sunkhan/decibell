// The GPUI screen `join-dialog-resolving` (see _servers.tsx).
import fixture from "./fixtures/servers-join-dialog-resolving.json";
import { ServersPage, openStage, type ServersFixture } from "./_servers";

export default ServersPage;
export const size = { width: 1280, height: 800 };
export const setup = () => openStage(fixture as unknown as ServersFixture);
