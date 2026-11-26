// The GPUI shell's `main-home`: perf1 on Home (Friends page beside the conversation list).
import fixture from "./fixtures/shell-main-home.json";
import { ShellPage, replay, type ShellFixture } from "./_shell";

export default ShellPage;
export const setup = () => replay(fixture as ShellFixture);
