// The GPUI screen `composer-slowmode`: #slow-lane, 24 s of the window left, a held draft.
import { channelKey } from "@app/lib/channelKey";
import { useSlowmodeStore } from "@app/features/chat/slowmode";
import { frames, start, typeDraft } from "./_composer";
export { default } from "./_composer";
export async function setup() {
  const shown = await start("slow-lane");
  useSlowmodeStore.setState({ until: { [channelKey("1", "slow-lane")]: Date.now() + 23_700 } });
  await typeDraft("slow-lane", "held until the window ends");
  await frames(4);
  return shown;
}
