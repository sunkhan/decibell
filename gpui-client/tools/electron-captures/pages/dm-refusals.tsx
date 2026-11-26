// The GPUI screen `dm-refusals` (crates/decibell-home/src/fixture.rs), on the renderer's MainLayout:
// bob's conversation ending in central's two legacy refusal strings, which DmChatPanel draws as
// ErrorCard rows.
//
// The renderer's list pins itself to the bottom before the cards' rows are final and rests about
// 35 px above its end here (dm-plain, without cards, rests at the end): the page scrolls it to the
// end, as a user would, so the pair compares the rows' geometry and not that resting place.
import fixture from "./fixtures/home-dm-refusals.json";
import { HomePage, replayHome, type HomeFixture } from "./_home";
import { frames } from "../runtime/harness";

export default HomePage;
export async function setup() {
  const out = await replayHome(fixture as unknown as HomeFixture);
  const lists = Array.from(document.querySelectorAll<HTMLElement>(".overflow-y-auto")).filter((el) => el.querySelector("[data-key]"));
  const list = lists[0];
  const before = list ? { scrollTop: list.scrollTop, scrollHeight: list.scrollHeight, clientHeight: list.clientHeight } : null;
  if (list) list.scrollTop = list.scrollHeight;
  await frames(4);
  return { ...(out ?? {}), listBefore: before, listAfter: list ? { scrollTop: list.scrollTop } : null };
}
