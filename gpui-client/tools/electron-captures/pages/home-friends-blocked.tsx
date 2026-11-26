// The GPUI screen `home-friends-blocked` (crates/decibell-home/src/fixture.rs), on the renderer's MainLayout.
import fixture from "./fixtures/home-home-friends-blocked.json";
import { HomePage, replayHome, type HomeFixture } from "./_home";

export default HomePage;
export const setup = () => replayHome(fixture as unknown as HomeFixture);
