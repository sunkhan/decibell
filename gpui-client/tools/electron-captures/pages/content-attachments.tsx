// The GPUI client's `content-attachments`: every attachment kind and grid shape and their states.
import fixture from "./fixtures/content-attachments.json";
import { ContentPage, replayContent, type ContentFixture } from "./_content";

export default ContentPage;
export const setup = () => replayContent(fixture as unknown as ContentFixture);
