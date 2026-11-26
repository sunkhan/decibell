// The GPUI screen `composer-timed-out`: the member's timeout runs about two hours more.
import { useChatStore } from "@app/stores/chatStore";
import { useAuthStore } from "@app/stores/authStore";
import { frames, start } from "./_composer";
export { default } from "./_composer";
export async function setup() {
  const shown = await start();
  const me = useAuthStore.getState().uid;
  const until = Math.floor(Date.now() / 1000) + 7_000;
  useChatStore.setState((s) => ({
    membersByServer: {
      ...s.membersByServer,
      "1": (s.membersByServer["1"] ?? []).map((m) => (m.uid === me ? { ...m, timedOutUntil: until } : m)),
    },
  }));
  await frames(6);
  return { ...shown, me, member: (useChatStore.getState().membersByServer["1"] ?? []).some((m) => m.uid === me) };
}
