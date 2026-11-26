// The GPUI screen `home-friends-online` (crates/decibell-home/src/fixture.rs), on the renderer's MainLayout.
import fixture from "./fixtures/home-home-friends-online.json";
import { HomePage, replayHome, type HomeFixture } from "./_home";

export default HomePage;
export const setup = () => replayHome(fixture as unknown as HomeFixture);
