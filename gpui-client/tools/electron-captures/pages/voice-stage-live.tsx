// The GPUI voice screen `voice-stage-live` (crates/decibell-voice/src/screens.rs), from the same fixture
// (fixtures/voice-stage-live.json), drawn as the whole window: see _voice.tsx.
import fixture from "./fixtures/voice-stage-live.json";
import { VoicePage, replayVoice, size, type VoiceFixture } from "./_voice";

export default VoicePage;
export { size };
export const setup = () => replayVoice(fixture as unknown as VoiceFixture);
