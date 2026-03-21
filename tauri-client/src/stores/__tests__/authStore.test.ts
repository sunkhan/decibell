import { describe, it, expect, beforeEach } from "vitest";
import { useAuthStore } from "../authStore";

describe("authStore", () => {
  beforeEach(() => {
    useAuthStore.setState({ username: null, isAuthenticated: false, isLoggingIn: false, loginError: null, isRegistering: false, registerResult: null });
  });
  it("login sets username and isAuthenticated", () => {
    useAuthStore.getState().login("alice");
    const s = useAuthStore.getState();
    expect(s.username).toBe("alice");
    expect(s.isAuthenticated).toBe(true);
  });
  it("logout clears auth state", () => {
    useAuthStore.getState().login("alice");
    useAuthStore.getState().logout();
    const s = useAuthStore.getState();
    expect(s.username).toBeNull();
    expect(s.isAuthenticated).toBe(false);
  });
  it("setLoggingIn toggles loading state", () => {
    useAuthStore.getState().setLoggingIn(true);
    expect(useAuthStore.getState().isLoggingIn).toBe(true);
    useAuthStore.getState().setLoggingIn(false);
    expect(useAuthStore.getState().isLoggingIn).toBe(false);
  });
  it("setLoginError sets and clears error", () => {
    useAuthStore.getState().setLoginError("bad password");
    expect(useAuthStore.getState().loginError).toBe("bad password");
    useAuthStore.getState().setLoginError(null);
    expect(useAuthStore.getState().loginError).toBeNull();
  });
  it("setRegisterResult stores result", () => {
    useAuthStore.getState().setRegisterResult({ success: true, message: "ok" });
    expect(useAuthStore.getState().registerResult).toEqual({ success: true, message: "ok" });
  });
});
