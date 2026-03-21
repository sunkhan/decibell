import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAuthStore } from "../../stores/authStore";
import { useNavigate } from "react-router-dom";

export function useAuthEvents() {
  const navigate = useNavigate();

  useEffect(() => {
    const unlistenSuccess = listen<{ username: string }>(
      "login_succeeded",
      (event) => {
        useAuthStore.getState().login(event.payload.username);
        navigate("/");
      }
    );

    const unlistenFailed = listen<{ message: string }>(
      "login_failed",
      (event) => {
        useAuthStore.getState().setLoginError(event.payload.message);
      }
    );

    const unlistenRegister = listen<{ success: boolean; message: string }>(
      "register_responded",
      (event) => {
        useAuthStore.getState().setRegisterResult(event.payload);
      }
    );

    return () => {
      unlistenSuccess.then((fn) => fn());
      unlistenFailed.then((fn) => fn());
      unlistenRegister.then((fn) => fn());
    };
  }, [navigate]);
}
