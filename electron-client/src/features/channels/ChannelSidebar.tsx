import { useUiStore } from "../../stores/uiStore";
import ConversationSidebar from "./ConversationSidebar";
import ServerChannelsSidebar from "./ServerChannelsSidebar";

/// Top-level sidebar dispatcher. Routes to one of two child components
/// based on activeView so each view only subscribes to the store
/// slices its rendering actually needs:
///   - ConversationSidebar: DM conversations + friends/online users
///   - ServerChannelsSidebar: server channels + voice presence
///
/// The split avoids the "every store change re-renders the entire
/// sidebar" hot path the merged component had — a speaking event in
/// voice no longer triggers a re-render of the DM list, and a new DM
/// no longer recomputes the channel filters.
export default function ChannelSidebar() {
  const activeView = useUiStore((s) => s.activeView);
  if (activeView === "home" || activeView === "dm") {
    return <ConversationSidebar />;
  }
  return <ServerChannelsSidebar />;
}
