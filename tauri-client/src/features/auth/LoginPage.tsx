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
      useAuthStore.getState().setRegisterResult({
        success: false,
        message: String(err),
      });
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (mode === "login") handleLogin();
    else handleRegister();
  };

  const isLoading = mode === "login" ? isLoggingIn : isRegistering;

  return (
    <div className="flex h-screen w-screen items-center justify-center bg-bg-primary">
      <form
        onSubmit={handleSubmit}
        className="w-full max-w-[400px] rounded-xl bg-bg-secondary p-8 shadow-lg"
      >
        <h1 className="mb-1 text-center text-xl font-bold text-text-primary">
          {mode === "login" ? "Welcome back!" : "Create an account"}
        </h1>
        <p className="mb-6 text-center text-sm text-text-muted">
          {mode === "login"
            ? "We're so excited to see you again!"
            : "Join the conversation"}
        </p>

        <label className="mb-1 block text-xs font-semibold uppercase text-text-muted">
          Username
        </label>
        <input
          type="text"
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          className="mb-3 w-full rounded-md border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none transition-colors focus:border-accent"
          required
        />

        {mode === "register" && (
          <>
            <label className="mb-1 block text-xs font-semibold uppercase text-text-muted">
              Email
            </label>
            <input
              type="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              className="mb-3 w-full rounded-md border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none transition-colors focus:border-accent"
              required
            />
          </>
        )}

        <label className="mb-1 block text-xs font-semibold uppercase text-text-muted">
          Password
        </label>
        <input
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          className="mb-4 w-full rounded-md border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none transition-colors focus:border-accent"
          required
        />

        <button
          type="submit"
          disabled={isLoading}
          className="flex w-full items-center justify-center rounded-md bg-accent py-2.5 text-sm font-semibold text-white transition-colors hover:bg-accent-hover active:bg-accent-pressed disabled:opacity-50"
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

        <p className="mt-4 text-center text-sm text-text-muted">
          {mode === "login" ? (
            <>
              Need an account?{" "}
              <button type="button" onClick={() => setMode("register")} className="text-accent hover:underline">
                Register
              </button>
            </>
          ) : (
            <>
              Already have an account?{" "}
              <button type="button" onClick={() => setMode("login")} className="text-accent hover:underline">
                Log In
              </button>
            </>
          )}
        </p>
      </form>
    </div>
  );
}
