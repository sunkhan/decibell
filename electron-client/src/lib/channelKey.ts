/// Composite cache key for per-channel client state.
///
/// Channel ids are slugs that are only unique WITHIN a server — every
/// server ships a "general" — and the client holds multiple live server
/// connections at once, so any map keyed by the bare channel id mixes
/// different servers' data (messages, history flags, scroll positions,
/// drafts). All per-channel maps key on serverId + channelId instead.
///
/// The brand makes TypeScript reject un-namespaced strings at every
/// index site, so a future call site can't quietly reintroduce the
/// collision. Keys are never parsed back — always carry serverId and
/// channelId separately and compose at the boundary.
export type ChannelKey = string & { readonly __channelKey: "ChannelKey" };

export function channelKey(serverId: string, channelId: string): ChannelKey {
  return `${serverId}:${channelId}` as ChannelKey;
}
