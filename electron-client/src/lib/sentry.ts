import * as Sentry from "@sentry/electron/renderer";

// Initialize Sentry in the renderer. Reads boot config from the
// preload bridge — main process has already decided whether Sentry
// is enabled, generated the install ID, and passed both via
// additionalArguments. The renderer never makes that decision itself,
// so this function just consumes.
//
// DSN is inherited from main via @sentry/electron's internal IPC
// channel — no DSN appears in the renderer bundle, which means
// source maps stay clean.
export function initRendererSentry(): boolean {
  const config = window.decibell.sentryConfig;
  if (!config.enabled) {
    console.log("[sentry] renderer disabled");
    return false;
  }

  Sentry.init({
    release: `decibell@${config.version}`,
    environment: "production",
    sampleRate: 1.0,
    autoSessionTracking: false,
    initialScope: {
      user: { id: config.installId },
      tags: {
        platform: window.decibell.platform,
        is_packaged: true,
      },
    },
    beforeSend(event) {
      if (event.user) delete event.user.ip_address;
      // Same filter as main process — "No component available" with
      // no stacktrace is Chromium internal noise, not a real bug.
      const top = event.exception?.values?.[0];
      if (top?.value === "No component available" && !top?.stacktrace) {
        return null;
      }
      return event;
    },
  });
  console.log(`[sentry] renderer initialized (release=decibell@${config.version})`);
  return true;
}
