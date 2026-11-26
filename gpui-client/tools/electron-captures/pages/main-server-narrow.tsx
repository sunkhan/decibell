// The GPUI shell's `main-server` at 900 × 700 (the narrow layout): `decibell-capture --screen
// main-server --size 900x700`, renamed main-server-narrow-<theme>.png for pairing.
import fixture from "./fixtures/shell-main-server.json";
import { ShellPage, replay, type ShellFixture } from "./_shell";

export default ShellPage;
export const size = { width: 900, height: 668 };
export const setup = () => replay(fixture as ShellFixture);
