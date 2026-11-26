// The GPUI client's `image-viewer`: the viewer at fit over the attachments channel, on the
// "Two side by side" message's first picture (drawn as the whole window: the viewer sits under the
// title bar).
import fixture from "./fixtures/content-attachments.json";
import { FramedContentPage, openViewer, type ContentFixture } from "./_content";

export default FramedContentPage;
export const size = { width: 1280, height: 800 };
export const setup = () => openViewer(fixture as unknown as ContentFixture, false);
