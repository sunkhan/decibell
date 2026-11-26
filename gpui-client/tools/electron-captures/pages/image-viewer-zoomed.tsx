// The GPUI client's `image-viewer-zoomed`: the same, at actual size (`1`), with the minimap.
import fixture from "./fixtures/content-attachments.json";
import { FramedContentPage, openViewer, type ContentFixture } from "./_content";

export default FramedContentPage;
export const size = { width: 1280, height: 800 };
export const setup = () => openViewer(fixture as unknown as ContentFixture, true);
