import { describe, it, expect, beforeEach } from "vitest";
import { useUiStore } from "../uiStore";

describe("uiStore", () => {
  beforeEach(() => { useUiStore.setState({ sidebarCollapsed: false, activeModal: null, connectionStatus: "connected", activeView: "home" }); });
  it("setConnectionStatus updates status", () => {
    useUiStore.getState().setConnectionStatus("reconnecting");
    expect(useUiStore.getState().connectionStatus).toBe("reconnecting");
  });
  it("setActiveView switches view", () => {
    useUiStore.getState().setActiveView("server");
    expect(useUiStore.getState().activeView).toBe("server");
  });
  it("openModal and closeModal manage modal state", () => {
    useUiStore.getState().openModal("server-discovery");
    expect(useUiStore.getState().activeModal).toBe("server-discovery");
    useUiStore.getState().closeModal();
    expect(useUiStore.getState().activeModal).toBeNull();
  });
});
