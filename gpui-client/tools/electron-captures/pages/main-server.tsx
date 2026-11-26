// The GPUI shell's `main-server`: perf1 in Daily Gaming #general, in voice in Squad (the shared
// fixture, replayed through the renderer's hooks; see _shell.tsx).
import fixture from "./fixtures/shell-main-server.json";
import { ShellPage, replay, type ShellFixture } from "./_shell";

export default ShellPage;
export const setup = () => replay(fixture as ShellFixture);
