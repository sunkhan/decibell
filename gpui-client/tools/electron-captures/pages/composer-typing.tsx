// The GPUI screen `composer-typing`: a formatted draft, the preview strip.
import { FORMATTED, start, typeDraft } from "./_composer";
export { default } from "./_composer";
export async function setup() {
  const shown = await start();
  await typeDraft("general", FORMATTED);
  return shown;
}
