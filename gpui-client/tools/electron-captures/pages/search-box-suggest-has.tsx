// The GPUI client's `search-box-suggest-has` (crates/decibell-search/src/screens.rs): the search fixture replayed
// through the renderer's hooks (see _search.tsx).
import fixture from "./fixtures/search-box-suggest-has.json";
import { SearchPage, pinToday, replaySearch, type SearchFixture } from "./_search";

pinToday((fixture as unknown as SearchFixture).search.today);
export default SearchPage;
export const setup = () => replaySearch(fixture as unknown as SearchFixture);
