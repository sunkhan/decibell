// Not a screen: renders every icon component of the renderer to static SVG markup and returns it
// from setup() (saved as icons-dump-<theme>.json). icons/extract.mjs turns that into
// gpui-client/assets/icons/<group>/<name>.svg. Run: icons/extract.sh
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import * as stage from "@app/features/voice/stage/icons";
import * as viewer from "@app/features/chat/imageViewer/icons";
import * as picture from "@app/components/pictureEditor/icons";
import * as transfers from "@app/features/transfers/icons";
import { Icon as SettingsIcon } from "@app/features/servers/tabs/settingsUi";
import settingsUiSource from "@app/features/servers/tabs/settingsUi.tsx?raw";
import settingsModalSource from "@app/features/settings/SettingsModal.tsx?raw";
import serverSettingsSource from "@app/features/servers/ServerSettingsModal.tsx?raw";
import callStageSource from "@app/features/call/CallStage.tsx?raw";
import attachmentListSource from "@app/features/chat/AttachmentList.tsx?raw";
import replyPreviewSource from "@app/features/chat/ReplyPreview.tsx?raw";
import messageBubbleSource from "@app/features/chat/MessageBubble.tsx?raw";
import devicesTabSource from "@app/features/settings/tabs/DevicesTab.tsx?raw";
import participantListSource from "@app/features/voice/VoiceParticipantList.tsx?raw";

export const size = { width: 64, height: 64 };

export default function IconsDump() {
  return <div />;
}

type Entry = { group: string; name: string; svg: string; source: string };

const kebab = (s: string) =>
  s
    .replace(/Icon$/, "")
    .replace(/([a-z0-9])([A-Z])/g, "$1-$2")
    .toLowerCase();

function renderAll(group: string, source: string, module: Record<string, unknown>, variants: Record<string, Record<string, unknown>[]> = {}): Entry[] {
  const out: Entry[] = [];
  for (const [exported, value] of Object.entries(module)) {
    if (typeof value !== "function" || exported === "Spinner") continue;
    const component = value as ComponentType<Record<string, unknown>>;
    for (const props of variants[exported] ?? [{}]) {
      const suffix = Object.entries(props).map(([k, v]) => `-${typeof v === "boolean" ? k : v}`).join("");
      out.push({ group, name: kebab(exported) + suffix, svg: renderToStaticMarkup(createElement(component, props)), source });
    }
  }
  return out;
}

/// JSX <svg> markup written inline in a module (the settings tabs' icons), as SVG.
function inlineSvgs(group: string, source: string, file: string, names: string[]): Entry[] {
  const svgs = [...source.matchAll(/<svg[\s\S]*?<\/svg>/g)].map((m) => m[0]);
  return svgs.slice(0, names.length).map((jsx, i) => ({
    group,
    name: names[i],
    source: file,
    svg: jsx
      .replace(/\s+/g, " ")
      .replace(/\b(stroke|fill|clip)(Width|Linecap|Linejoin|Opacity|Rule)=/g, (_, a, b) => `${a}-${b.toLowerCase()}=`)
      .replace(/=\{(-?[\d.]+)\}/g, '="$1"')
      .replace(/\s[a-zA-Z-]+=\{[^}]*\}/g, "")
      .replace(/<title>[^<]*<\/title>/g, "")
      .replace(/ className="[^"]*"/g, ""),
  }));
}

/// The first <svg> of a module's local (unexported) function `fn`.
function localSvg(group: string, name: string, source: string, file: string, fn: string): Entry[] {
  const at = source.search(new RegExp(`function ${fn}\\(`));
  if (at < 0) throw new Error(`${file}: no function ${fn}`);
  return inlineSvgs(group, source.slice(at), file, [name]);
}

export async function setup() {
  const entries: Entry[] = [];
  entries.push(
    ...renderAll("stage", "features/voice/stage/icons.tsx", stage, { SpeakerIcon: [{}, { muted: true }] }),
    ...renderAll("viewer", "features/chat/imageViewer/icons.tsx", viewer, { ChevronIcon: [{ dir: "left" }, { dir: "right" }] }),
    ...renderAll("picture", "components/pictureEditor/icons.tsx", picture),
    ...renderAll("transfers", "features/transfers/icons.tsx", transfers, {
      KindIcon: ["image", "video", "audio", "document"].map((kind) => ({ kind })),
    }),
  );
  // settingsUi's one stroke set: every PATHS key.
  const block = settingsUiSource.slice(settingsUiSource.indexOf("const PATHS = {"), settingsUiSource.indexOf("} as const;"));
  const keys = [...block.matchAll(/^ {2}([a-zA-Z]+):/gm)].map((m) => m[1]);
  for (const key of keys) {
    entries.push({
      group: "settings",
      name: kebab(key),
      source: "features/servers/tabs/settingsUi.tsx",
      svg: renderToStaticMarkup(createElement(SettingsIcon as ComponentType<Record<string, unknown>>, { name: key })),
    });
  }
  const tabIds = (src: string) => [...src.matchAll(/id: "([a-z-]+)",\s*\n\s*label: "[^"]*",\s*\n\s*icon: \(/g)].map((m) => m[1]);
  const tabs = tabIds(settingsModalSource);
  entries.push(...inlineSvgs("settings-tab", settingsModalSource.slice(settingsModalSource.indexOf("const TABS")), "features/settings/SettingsModal.tsx", tabs));
  const serverTabs = tabIds(serverSettingsSource);
  entries.push(...inlineSvgs("server-tab", serverSettingsSource.slice(serverSettingsSource.indexOf(serverTabs.length ? `id: "${serverTabs[0]}"` : "icon: (")), "features/servers/ServerSettingsModal.tsx", serverTabs));
  // Glyph components local to the feature that draws them.
  entries.push(
    ...localSvg("call", "theater", callStageSource, "features/call/CallStage.tsx", "TheaterIcon"),
    ...localSvg("chat", "audio", attachmentListSource, "features/chat/AttachmentList.tsx", "AudioIcon"),
    ...localSvg("chat", "reply", replyPreviewSource, "features/chat/ReplyPreview.tsx", "ReplyGlyph"),
    ...localSvg("chat", "lock", messageBubbleSource, "features/chat/MessageBubble.tsx", "LockGlyph"),
    ...localSvg("devices", "monitor", devicesTabSource, "features/settings/tabs/DevicesTab.tsx", "MonitorGlyph"),
    ...localSvg("voice", "mute", participantListSource, "features/voice/VoiceParticipantList.tsx", "MuteIcon"),
    ...localSvg("voice", "local-mute", participantListSource, "features/voice/VoiceParticipantList.tsx", "LocalMuteIcon"),
    ...localSvg("voice", "deafen", participantListSource, "features/voice/VoiceParticipantList.tsx", "DeafenIcon"),
  );
  return { entries };
}
