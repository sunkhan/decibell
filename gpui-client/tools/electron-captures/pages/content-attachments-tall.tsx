// `content-attachments` in a window tall enough for the whole channel (the GPUI capture's
// `--size 1280x3400`: its content area is 1280 × 3368).
import fixture from "./fixtures/content-attachments.json";
import { ContentPage, replayContent, type ContentFixture } from "./_content";

export default ContentPage;
export const size = { width: 1280, height: 3368 };
export const setup = () => replayContent(fixture as unknown as ContentFixture);
