// The GPUI client's `chat-fixture`: the 10,000-message fixture channel at its newest rows.
import fixture from "./fixtures/chat-fixture.json";
import { ChatPage, replayChat } from "./_chat";
import type { ShellFixture } from "./_shell";

export default ChatPage;
export const setup = () => replayChat(fixture as unknown as ShellFixture);
