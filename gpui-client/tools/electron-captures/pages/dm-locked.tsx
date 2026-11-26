// The GPUI screen `dm-locked` (crates/decibell-home/src/fixture.rs), on the renderer's MainLayout.
import fixture from "./fixtures/home-dm-locked.json";
import { HomePage, replayHome, type HomeFixture } from "./_home";

export default HomePage;
export const setup = () => replayHome(fixture as unknown as HomeFixture);
