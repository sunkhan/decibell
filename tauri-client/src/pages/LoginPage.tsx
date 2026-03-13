import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export default function LoginPage() {
  const [pingResult, setPingResult] = useState<string>("");

  async function handlePing() {
    const result = await invoke<string>("ping");
    setPingResult(result);
  }

  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="text-center">
        <h1 className="text-4xl font-bold text-accent mb-4">Decibell</h1>
        <p className="text-text-muted mb-6">Login page — coming in Phase 3</p>
        <button
          onClick={handlePing}
          className="rounded-md bg-accent px-6 py-2 text-white font-bold hover:bg-accent-hover active:bg-accent-pressed transition-colors"
        >
          Ping Backend
        </button>
        {pingResult && (
          <p className="mt-4 text-success">Backend says: {pingResult}</p>
        )}
      </div>
    </div>
  );
}
