// Gives a page's setup() real input: window.__harness.input(events) → webContents.sendInputEvent
// in shoot.cjs. Nothing else crosses the bridge (window.decibell is the page's own stub).
const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("__harness", {
  input: (events) => ipcRenderer.invoke("harness:input", events),
});
