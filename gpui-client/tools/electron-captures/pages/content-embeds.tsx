// The GPUI client's `content-embeds`: link previews, picture and GIF links, invite cards.
import fixture from "./fixtures/content-embeds.json";
import { ContentPage, replayContent, type ContentFixture } from "./_content";

export default ContentPage;
export const setup = () => replayContent(fixture as unknown as ContentFixture);
