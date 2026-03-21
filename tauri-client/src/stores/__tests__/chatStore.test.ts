import { describe, it, expect, beforeEach } from "vitest";
import { useChatStore } from "../chatStore";

describe("chatStore", () => {
  beforeEach(() => {
    useChatStore.setState({ servers: [], activeServerId: null, activeChannelId: null, channelsByServer: {}, messagesByChannel: {}, channelMembers: {}, onlineUsers: [], connectedServers: new Set() });
  });
  it("setServers replaces server list", () => {
    const servers = [{ id: "1", name: "Test", description: "desc", hostIp: "127.0.0.1", port: 9090, memberCount: 5 }];
    useChatStore.getState().setServers(servers);
    expect(useChatStore.getState().servers).toEqual(servers);
  });
  it("setActiveServer and setActiveChannel update selection", () => {
    useChatStore.getState().setActiveServer("1");
    expect(useChatStore.getState().activeServerId).toBe("1");
    useChatStore.getState().setActiveChannel("ch1");
    expect(useChatStore.getState().activeChannelId).toBe("ch1");
  });
  it("setChannelsForServer stores channels keyed by server", () => {
    const channels = [{ id: "ch1", name: "general", type: "text" as const }];
    useChatStore.getState().setChannelsForServer("s1", channels);
    expect(useChatStore.getState().channelsByServer["s1"]).toEqual(channels);
  });
  it("addMessage appends to channel message list", () => {
    const msg = { sender: "alice", content: "hi", timestamp: "123", channelId: "ch1" };
    useChatStore.getState().addMessage(msg);
    expect(useChatStore.getState().messagesByChannel["ch1"]).toEqual([msg]);
    useChatStore.getState().addMessage({ ...msg, content: "hello" });
    expect(useChatStore.getState().messagesByChannel["ch1"]).toHaveLength(2);
  });
  it("setChannelMembers stores member list per channel", () => {
    useChatStore.getState().setChannelMembers("ch1", ["alice", "bob"]);
    expect(useChatStore.getState().channelMembers["ch1"]).toEqual(["alice", "bob"]);
  });
  it("setOnlineUsers replaces global online list", () => {
    useChatStore.getState().setOnlineUsers(["alice", "bob"]);
    expect(useChatStore.getState().onlineUsers).toEqual(["alice", "bob"]);
  });
  it("addConnectedServer and removeConnectedServer manage set", () => {
    useChatStore.getState().addConnectedServer("s1");
    expect(useChatStore.getState().connectedServers.has("s1")).toBe(true);
    useChatStore.getState().removeConnectedServer("s1");
    expect(useChatStore.getState().connectedServers.has("s1")).toBe(false);
  });
});
