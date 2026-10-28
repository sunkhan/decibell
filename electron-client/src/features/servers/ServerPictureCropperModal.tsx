// Server picture editor: the shared picture editor with a frame of the
// server tile's shape (serverTileDimensions.ts) and previews of the tile
// and an invite card.
//
// Save renders the crop at 880×302, 2× the 440 px it used to be, so the
// 432 px invite banner stays sharp on HiDPI screens (JPEG, still far under
// the 1 MB cap), and sends it with update_server_picture → the community
// checks Manage Server + size, answers (server_picture_update_responded),
// forwards it to central → SERVER_PICTURE_CHANGED → every member's tile.
// The editor stays on "Saving…" until that answer.

import { invoke, listen } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { Crop, PictureEditor, PreviewLabel } from "../../components/pictureEditor/PictureEditor";
import type { Size } from "../../components/pictureEditor/geometry";
import { ActiveTileGlow, TilePictureLayers } from "./ServerBar";
import { TILE_ASPECT, TILE_HEIGHT, TILE_WIDTH } from "./serverTileDimensions";

interface Props {
  serverId: string;
  file: File;
  /// The saved picture, shown in Server Settings until the tile has it.
  onSaved: (url: string) => void;
  onClose: () => void;
}

const OUTPUT = { w: 880, h: Math.round(880 / TILE_ASPECT) };

/// Servers with a save out: their refusal is answered in the editor, not
/// with a toast (useServerEvents).
export const pictureSavesInFlight = new Set<string>();

/// The tile's corner, scaled with the frame.
const tileRadius = (frame: Size) => `calc(var(--radius-md) * ${frame.w / TILE_WIDTH})`;

export function ServerPictureCropperModal({ serverId, file, onSaved, onClose }: Props) {
  return (
    <PictureEditor
      file={file}
      kind="banner"
      title="Edit server picture"
      aspect={TILE_ASPECT}
      fit
      radius={tileRadius}
      output={OUTPUT}
      target="server-picture"
      previews={<ServerPreviews serverId={serverId} />}
      onSave={(data) => savePicture(serverId, data)}
      onSaved={onSaved}
      onClose={onClose}
    />
  );
}

async function savePicture(serverId: string, data: Uint8Array): Promise<void> {
  pictureSavesInFlight.add(serverId);
  let settle: { resolve: () => void; reject: (e: Error) => void } | null = null;
  const answer = new Promise<void>((resolve, reject) => {
    settle = { resolve, reject };
  });
  // Listening before the request goes out, so a quick answer can't beat it.
  const unlisten = await listen<{ serverId: string; success: boolean; message: string }>(
    "server_picture_update_responded",
    (e) => {
      if (e.payload.serverId !== serverId) return;
      if (e.payload.success) settle?.resolve();
      else settle?.reject(new Error(e.payload.message || "The server refused the picture."));
    },
  );
  const timer = window.setTimeout(() => settle?.reject(new Error("The server didn't answer. Try again.")), 10_000);
  try {
    await invoke("update_server_picture", { serverId, data });
    await answer;
  } finally {
    window.clearTimeout(timer);
    unlisten();
    pictureSavesInFlight.delete(serverId);
  }
}

/// The tile as members see it, at its real size, resting and selected; and
/// an invite card's banner, which shows the middle of a wider band.
function ServerPreviews({ serverId }: { serverId: string }) {
  const name = useChatStore((s) => s.serverMeta[serverId]?.name ?? s.servers.find((x) => x.id === serverId)?.name ?? "Server");
  const members = useChatStore((s) => s.servers.find((x) => x.id === serverId)?.memberCount ?? 0);
  const tile = (active: boolean) => (
    <div className="relative shrink-0" style={{ width: TILE_WIDTH, height: TILE_HEIGHT }}>
      {active && <ActiveTileGlow />}
      <div className="relative flex h-full w-full items-center justify-center overflow-hidden rounded-md px-3">
        <TilePictureLayers name={name} active={active}>
          <div className="absolute inset-0">
            <Crop width={TILE_WIDTH} height={TILE_HEIGHT} />
          </div>
        </TilePictureLayers>
      </div>
    </div>
  );
  return (
    <div className="flex items-start gap-6">
      <div>
        <PreviewLabel>Server bar</PreviewLabel>
        <div className="flex gap-2 rounded-lg bg-bg-darkest px-3 pb-3.5 pt-2.5">
          {tile(false)}
          {tile(true)}
        </div>
      </div>
      <div className="min-w-0 flex-1">
        <PreviewLabel>Invite card</PreviewLabel>
        {/* 432 × 112 in chat, here at 60 %. */}
        <div className="w-65 overflow-hidden rounded-lg border border-border bg-bg-dark">
          <Crop width={260} height={67} />
          <div className="flex items-baseline gap-2 px-3 py-2">
            <span className="truncate text-[13px] font-semibold text-text-bright">{name}</span>
            {members > 0 && <span className="shrink-0 font-meta text-[11px] text-text-muted">{members} members</span>}
          </div>
        </div>
      </div>
    </div>
  );
}
