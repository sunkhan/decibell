// `content-embeds` in a window tall enough for the whole channel (the GPUI capture's
// `--size 1280x2400`: its content area is 1280 × 2368).
import fixture from "./fixtures/content-embeds.json";
import { ContentPage, replayContent, type ContentFixture } from "./_content";

export default ContentPage;
export const size = { width: 1280, height: 2368 };
export const setup = () => replayContent(fixture as unknown as ContentFixture);
