// The GPUI client's `chat-features`: a hand-written channel with every row type and body feature.
import fixture from "./fixtures/chat-features.json";
import { ChatPage, replayChat } from "./_chat";
import type { ShellFixture } from "./_shell";

export default ChatPage;
export const setup = () => replayChat(fixture as unknown as ShellFixture);
