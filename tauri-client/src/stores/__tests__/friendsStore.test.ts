import { describe, it, expect, beforeEach } from "vitest";
import { useFriendsStore } from "../friendsStore";

describe("friendsStore", () => {
  beforeEach(() => { useFriendsStore.setState({ friends: [], isLoading: false }); });
  it("setFriends replaces friend list", () => {
    const friends = [{ username: "alice", status: "online" as const }];
    useFriendsStore.getState().setFriends(friends);
    expect(useFriendsStore.getState().friends).toEqual(friends);
  });
  it("updateFriend merges partial update", () => {
    useFriendsStore.getState().setFriends([{ username: "alice", status: "online" }]);
    useFriendsStore.getState().updateFriend("alice", { status: "offline" });
    expect(useFriendsStore.getState().friends[0].status).toBe("offline");
  });
  it("removeFriend filters out by username", () => {
    useFriendsStore.getState().setFriends([{ username: "alice", status: "online" }, { username: "bob", status: "offline" }]);
    useFriendsStore.getState().removeFriend("alice");
    expect(useFriendsStore.getState().friends).toHaveLength(1);
    expect(useFriendsStore.getState().friends[0].username).toBe("bob");
  });
  it("setLoading toggles loading state", () => {
    useFriendsStore.getState().setLoading(true);
    expect(useFriendsStore.getState().isLoading).toBe(true);
  });
});
