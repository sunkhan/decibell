// Shared by the Settings pages (settings-*, passphrase-*, rename-modal, password-modal): the
// renderer's real MainLayout — with its real SettingsModal, ChangePasswordModal,
// RenameUsernameModal and PassphraseModal — signed in as perf1 from the shell's fixture, then the
// stores seeded from the GPUI Settings fixture (crates/decibell-settings/src/fixture.rs, exported
// by `cargo run -p decibell-settings --features fixtures --bin decibell-settings-fixture --
// pages/fixtures`) and the window opened where the GPUI screen opens it. Drawn as the whole window
// (`Framed`: the modal places itself against it), 1280 × 800 (settings-narrow: 900 × 800).
import { Framed } from "./_frame";
import { ShellPage, replay, type ShellFixture } from "./_shell";
import { frames, respond, sleep } from "../runtime/harness";
import { useUiStore } from "@app/stores/uiStore";
import { useDmStore } from "@app/stores/dmStore";
import { useE2eeStore } from "@app/stores/e2eeStore";
import { useHotkeysStore } from "@app/stores/hotkeysStore";
import { useTransfersStore } from "@app/stores/transfersStore";
import { useUpdateStore } from "@app/stores/updateStore";

export type SettingsFixture = {
  shell: ShellFixture;
  now: number;
  settings: Record<string, unknown>;
  devices: unknown[];
  e2ee: { supported: boolean; status: string; keyId: number; fingerprint: string };
  hotkeys: { bindings: { id: string; action: string; keys: string[] }[]; status: Record<string, unknown>; recording: boolean };
  audio: { inputs: unknown[]; outputs: unknown[]; level: number };
  codecs: { useAv1: boolean; useH265: boolean; encode: unknown[]; decode: unknown[] };
  update: { status: unknown; mode: string; currentVersion: string };
  tab: string;
  dialog: string;
  downloadsDefault: string;
};

export function SettingsPage() {
  return (
    <Framed>
      <ShellPage />
    </Framed>
  );
}

const realNow = Date.now.bind(Date);

/// Replay the shell fixture, seed what Settings reads, open the window (and a dialog).
export async function openSettings(fixture: SettingsFixture) {
  // The fixed clock ("Last active 3 h ago"): the GPUI fixture's `NOW`.
  const offset = fixture.now * 1000 - realNow();
  Date.now = () => realNow() + offset;

  // What the tabs ask native for on mount.
  respond("listDevices", fixture.devices);
  respond("listAudioDevices", { inputs: fixture.audio.inputs, outputs: fixture.audio.outputs });
  respond("getCodecSettings", { useAv1: fixture.codecs.useAv1, useH265: fixture.codecs.useH265 });
  respond("getCaps", { encode: fixture.codecs.encode, decode: fixture.codecs.decode });
  respond("downloads.defaultFolder", fixture.downloadsDefault);
  respond("hotkeys.desktop", "kde");

  await replay(fixture.shell);
  const s = fixture.settings as Record<string, never>;
  const ui = useUiStore.getState();
  ui.setTextSizePx(s.text_size_px || 14.5);
  ui.setRowScale(s.list_density || 1);
  ui.setTimeFormat(s.time_format);
  ui.setInputDevice(s.input_device);
  ui.setOutputDevice(s.output_device);
  ui.setSeparateStreamOutput(s.separate_stream_output);
  ui.setStreamOutputDevice(s.stream_output_device);
  ui.setStreamStereo(s.stream_stereo);
  ui.setAecEnabled(s.aec_enabled);
  ui.setNoiseSuppressionLevel(s.noise_suppression_level);
  ui.setAgcEnabled(s.agc_enabled);
  ui.setVoiceThresholdDb(s.voice_threshold_db);
  ui.setLinkPreviewsEnabled(s.link_previews_enabled);
  ui.setGifUnfiltered(s.gif_unfiltered);
  ui.setCrashReportingEnabled(s.crash_reporting_enabled);
  ui.setAwayAfterMinutes(s.away_after_minutes);
  ui.setUploadLimitBps(s.upload_limit_bps);
  ui.setDownloadLimitBps(s.download_limit_bps);
  ui.setTakeStreamOnChannelSwitch(s.take_stream_on_channel_switch);
  useDmStore.getState().setFriendsOnlyDms(s.friends_only_dms);
  useTransfersStore.setState({ downloadDir: s.download_dir, askDownloadLocation: s.ask_download_location });
  useHotkeysStore.setState({
    bindings: fixture.hotkeys.bindings as never,
    inputMode: s.input_mode === "push_to_talk" ? "push_to_talk" : "voice_activity",
    pttReleaseDelayMs: s.ptt_release_delay_ms ?? 100,
    status: fixture.hotkeys.status as never,
  });
  useE2eeStore.getState().setStatus(fixture.e2ee as never);
  useUpdateStore.getState().setFromEvent(fixture.update.status as never, fixture.update.mode as never, fixture.update.currentVersion);

  ui.setSettingsTab(fixture.tab);
  ui.openModal("settings");
  await frames(4);
  // Let the tabs' mount-time loads (devices, codecs, audio devices) land.
  await sleep(50);
  await frames(2);

  const button = (text: string) =>
    [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text) as HTMLButtonElement | undefined;
  switch (fixture.dialog) {
    case "passphrase-setup":
      useE2eeStore.getState().openPassphraseModal("setup");
      break;
    case "passphrase-unlock":
      useE2eeStore.getState().openPassphraseModal("unlock");
      break;
    case "rename":
      button("Change username")?.click();
      break;
    case "password":
      button("Change password")?.click();
      break;
  }
  if (fixture.hotkeys.recording) {
    // The first row's keys: its record button.
    (document.querySelector("[title='Click to change']") as HTMLButtonElement | null)?.click();
  }
  await frames(4);
  // The GPUI captures show nothing focused: the dialogs focus their first input after 60 ms.
  await sleep(120);
  (document.activeElement as HTMLElement | null)?.blur();
  await frames(2);
  return { framed: true, tab: fixture.tab, dialog: fixture.dialog };
}
