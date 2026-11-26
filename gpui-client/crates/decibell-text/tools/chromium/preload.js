const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("harness", {
  corpus: () => ipcRenderer.invoke("corpus"),
  shot: (name, rect) => ipcRenderer.invoke("shot", name, rect),
  done: (result) => ipcRenderer.invoke("done", result),
});
