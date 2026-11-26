// The GPUI screen `create-channel-voice` (see _servers.tsx).
import fixture from "./fixtures/servers-create-channel-voice.json";
import { ServersPage, openStage, type ServersFixture } from "./_servers";

export default ServersPage;
export const size = { width: 1280, height: 800 };
export const setup = () => openStage(fixture as unknown as ServersFixture);
