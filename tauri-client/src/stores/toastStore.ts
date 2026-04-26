import { create } from "zustand";

export type ToastSeverity = "error" | "warning" | "info" | "success";

export interface Toast {
  id: string;
  severity: ToastSeverity;
  title: string;
  body?: string;
  duration: number;
}

interface ToastState {
  toasts: Toast[];
  push: (input: { severity: ToastSeverity; title: string; body?: string; duration?: number }) => string;
  dismiss: (id: string) => void;
}

export const useToastStore = create<ToastState>((set, get) => ({
  toasts: [],
  push: (input) => {
    // Dedup: if an identical toast is currently visible, return the
    // existing id instead of stacking a duplicate. Useful for spammy
    // event sources — e.g. dropping 12 files when the per-message cap
    // is 10 used to fire two cap-toasts in the same ~100 ms.
    const existing = get().toasts.find(
      (t) =>
        t.severity === input.severity &&
        t.title === input.title &&
        (t.body ?? "") === (input.body ?? ""),
    );
    if (existing) return existing.id;

    const id = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const toast: Toast = {
      id,
      severity: input.severity,
      title: input.title,
      body: input.body,
      duration: input.duration ?? 5000,
    };
    set((s) => ({ toasts: [...s.toasts, toast] }));
    return id;
  },
  dismiss: (id) =>
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

/** Imperative helpers — call from anywhere, no React required. */
export const toast = {
  error: (title: string, body?: string) =>
    useToastStore.getState().push({ severity: "error", title, body }),
  warning: (title: string, body?: string) =>
    useToastStore.getState().push({ severity: "warning", title, body }),
  info: (title: string, body?: string) =>
    useToastStore.getState().push({ severity: "info", title, body }),
  success: (title: string, body?: string) =>
    useToastStore.getState().push({ severity: "success", title, body }),
};
