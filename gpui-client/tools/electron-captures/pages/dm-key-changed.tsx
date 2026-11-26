// The GPUI screen `dm-key-changed` (crates/decibell-home/src/fixture.rs), on the renderer's MainLayout.
import fixture from "./fixtures/home-dm-key-changed.json";
import { HomePage, replayHome, type HomeFixture } from "./_home";

export default HomePage;
export const setup = () => replayHome(fixture as unknown as HomeFixture);
