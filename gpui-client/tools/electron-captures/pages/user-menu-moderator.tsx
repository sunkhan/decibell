// The GPUI screen `user-menu-moderator` (crates/decibell-home/src/fixture.rs), on the renderer's MainLayout drawn
// as the whole window (its overlay places itself against the window).
import fixture from "./fixtures/home-user-menu-moderator.json";
import { FramedHomePage, replayHome, type HomeFixture } from "./_home";

export default FramedHomePage;
export const size = { width: 1280, height: 800 };
export const setup = () => replayHome(fixture as unknown as HomeFixture, true);
