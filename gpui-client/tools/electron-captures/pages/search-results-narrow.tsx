// The GPUI client's `search-results` at 900 × 700 (below 1024 px the panel floats over the chat):
// `decibell-capture --screen search-results-narrow --size 900x700`.
import fixture from "./fixtures/search-results.json";
import { SearchPage, pinToday, replaySearch, type SearchFixture } from "./_search";

pinToday((fixture as unknown as SearchFixture).search.today);
export default SearchPage;
export const size = { width: 900, height: 668 };
export const setup = () => replaySearch(fixture as unknown as SearchFixture);
