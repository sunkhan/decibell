import { create } from "zustand";

export type ToastSeverity = "error" | "warning" | "info" | "success";

/// A button on the toast; running it dismisses the toast.
export interface ToastAction {
  label: string;
  run: () => void;
}

export interface Toast {
  id: string;
  severity: ToastSeverity;
  title: string;
  body?: string;
  duration: number;
  action?: ToastAction;
}

export interface ToastOptions {
  duration?: number;
  action?: ToastAction;
}

interface ToastState {
  toasts: Toast[];
  push: (input: { severity: ToastSeverity; title: string; body?: string } & ToastOptions) => string;
  dismiss: (id: string) => void;
}

export const useToastStore = create<ToastState>((set, get) => ({
  toasts: [],
  push: (input) => {
    // Dedup: if an identical toast is currently visible, return the
    // existing id instead of stacking a duplicate. Useful for spammy
    // event sources — e.g. the prefer-hardware → prefer-software
    // encoder fallback can fire twice on a remount and we don't want
    // two stacked GPU-fallback warnings.
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
      action: input.action,
    };
    set((s) => ({ toasts: [...s.toasts, toast] }));
    return id;
  },
  dismiss: (id) =>
    set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

/** Imperative helpers — call from anywhere, no React required. */
export const toast = {
  error: (title: string, body?: string, opts?: ToastOptions) =>
    useToastStore.getState().push({ severity: "error", title, body, ...opts }),
  warning: (title: string, body?: string, opts?: ToastOptions) =>
    useToastStore.getState().push({ severity: "warning", title, body, ...opts }),
  info: (title: string, body?: string, opts?: ToastOptions) =>
    useToastStore.getState().push({ severity: "info", title, body, ...opts }),
  success: (title: string, body?: string, opts?: ToastOptions) =>
    useToastStore.getState().push({ severity: "success", title, body, ...opts }),
};
