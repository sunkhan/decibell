import { useUiStore } from "../stores/uiStore";

/// Native refuses a TLS handshake whose certificate doesn't match the pin
/// with an error of the form `CERT_MISMATCH:<host>:<port>:<sha256hex>`.
/// Route such errors to the CertMismatchModal (which offers a deliberate
/// re-trust) instead of a generic failure message. Returns true when the
/// error was a pin failure and has been handled.
export function handleCertMismatch(err: unknown, retry?: () => Promise<void> | void): boolean {
  const text = String(err);
  const idx = text.indexOf("CERT_MISMATCH:");
  if (idx < 0) return false;
  const rest = text.slice(idx + "CERT_MISMATCH:".length);
  // host may contain ":" (IPv6) — port and fingerprint are the last two fields.
  const parts = rest.split(":");
  if (parts.length < 3) return false;
  const fingerprint = parts.pop()!.trim();
  const port = parseInt(parts.pop()!, 10);
  const host = parts.join(":");
  if (!host || !Number.isFinite(port)) return false;
  useUiStore.getState().setCertMismatch({ host, port, fingerprint, retry });
  return true;
}
