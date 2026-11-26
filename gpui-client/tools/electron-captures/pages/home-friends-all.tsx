// The GPUI screen `home-friends-all` (crates/decibell-home/src/fixture.rs), on the renderer's MainLayout.
import fixture from "./fixtures/home-home-friends-all.json";
import { HomePage, replayHome, type HomeFixture } from "./_home";

export default HomePage;
export const setup = () => replayHome(fixture as unknown as HomeFixture);
