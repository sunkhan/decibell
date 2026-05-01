import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAuthStore } from "../../stores/authStore";
import { useAuthEvents } from "./useAuthEvents";

export default function LoginPage() {
  useAuthEvents();

  const [mode, setMode] = useState<"login" | "register">("login");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [email, setEmail] = useState("");

  const isLoggingIn = useAuthStore((s) => s.isLoggingIn);
  const loginError = useAuthStore((s) => s.loginError);
  const isRegistering = useAuthStore((s) => s.isRegistering);
  const registerResult = useAuthStore((s) => s.registerResult);

  const handleLogin = async () => {
    useAuthStore.getState().setLoggingIn(true);
    try {
      await invoke("login", { username, password });
    } catch (err) {
      useAuthStore.getState().setLoginError(String(err));
    }
  };

  const handleRegister = async () => {
    useAuthStore.getState().setRegistering(true);
    try {
      await invoke("register", { username, email, password });
    } catch (err) {
      useAuthStore.getState().setRegisterResult({ success: false, message: String(err) });
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (mode === "login") handleLogin();
    else handleRegister();
  };

  const isLoading = mode === "login" ? isLoggingIn : isRegistering;

  return (
    <div className="relative flex h-screen w-screen items-center justify-center bg-bg-primary">
      <form
        onSubmit={handleSubmit}
        className="w-full max-w-[400px] rounded-2xl border border-border bg-bg-secondary p-8 shadow-2xl"
      >
        {/* Logo */}
        <div className="mx-auto mb-5 flex h-12 w-12 items-center justify-center rounded-xl bg-gradient-to-br from-accent to-accent-bright">
          <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="white" strokeWidth="2.5" strokeLinecap="round">
            <path d="M9 18V5l12-2v13" />
            <circle cx="6" cy="18" r="3" fill="white" stroke="none" />
            <circle cx="18" cy="16" r="3" fill="white" stroke="none" />
          </svg>
        </div>

        <h1 className="mb-1 text-center text-xl font-semibold text-text-bright">
          {mode === "login" ? "Decibell" : "Create an account"}
        </h1>
        <p className="mb-6 text-center text-sm text-text-secondary">
          {mode === "login"
            ? "Decentralized game chat"
            : "Join the conversation"}
        </p>

        <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-text-muted">
          Username
        </label>
        <input
          type="text"
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          className="mb-3 w-full rounded-lg border border-border bg-bg-primary px-3 py-2.5 text-sm text-text-primary outline-none transition-colors focus:border-accent"
          required
        />

        {mode === "register" && (
          <>
            <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-text-muted">
              Email
            </label>
            <input
              type="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              className="mb-3 w-full rounded-lg border border-border bg-bg-primary px-3 py-2.5 text-sm text-text-primary outline-none transition-colors focus:border-accent"
              required
            />
          </>
        )}

        <label className="mb-1.5 block text-[11px] font-bold uppercase tracking-wider text-text-muted">
          Password
        </label>
        <input
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          className="mb-5 w-full rounded-lg border border-border bg-bg-primary px-3 py-2.5 text-sm text-text-primary outline-none transition-colors focus:border-accent"
          required
        />

        <button
          type="submit"
          disabled={isLoading}
          className="flex w-full items-center justify-center rounded-lg bg-accent py-2.5 text-sm font-bold text-white transition-colors hover:bg-accent-hover active:bg-accent-pressed disabled:opacity-50"
        >
          {isLoading ? (
            <svg className="h-5 w-5 animate-spin" viewBox="0 0 24 24" fill="none">
              <circle cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" className="opacity-25" />
              <path d="M4 12a8 8 0 018-8" stroke="currentColor" strokeWidth="3" strokeLinecap="round" className="opacity-75" />
            </svg>
          ) : mode === "login" ? (
            "Log In"
          ) : (
            "Register"
          )}
        </button>

        {loginError && (
          <p className="mt-3 text-center text-sm text-error">{loginError}</p>
        )}
        {registerResult && (
          <p className={`mt-3 text-center text-sm ${registerResult.success ? "text-success" : "text-error"}`}>
            {registerResult.message}
          </p>
        )}

        <p className="mt-4 text-center text-sm text-text-secondary">
          {mode === "login" ? (
            <>
              Need an account?{" "}
              <button type="button" onClick={() => setMode("register")} className="font-semibold text-accent hover:text-accent-bright">
                Register
              </button>
            </>
          ) : (
            <>
              Already have an account?{" "}
              <button type="button" onClick={() => setMode("login")} className="font-semibold text-accent hover:text-accent-bright">
                Log In
              </button>
            </>
          )}
        </p>
      </form>
      <p className="absolute bottom-3 right-4 text-[11px] text-text-muted">Decibell 0.5.5</p>
    </div>
  );
}
