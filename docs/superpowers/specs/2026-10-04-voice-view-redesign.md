# Voice view redesign — tiled stage + floating dock (2026-10-04)

The community voice view (`VoicePanel`, the view you get when you join a voice channel) was a
cluster of floating 80 px avatars, a full-width bar of bordered text buttons, and a stream grid that
*replaced* the people as soon as anyone went live. This redesign brings it in line with the DM
`CallStage`. Visual design: the "Voice View Redesign" canvas (six states × four palettes; Console Split
shows the voice view on Console Light's canvas).

## Layout

- **Header** (48 px, same type as the chat header): channel glyph + name, MLS badge, people count, ping.
  While a stream is focused the name becomes a breadcrumb (`Lounge › mira's screen`); the channel part
  goes back to the grid.
- **Stage**: 16:9 tiles. `fitGrid` tries every column count and keeps the widest tile; the row is
  capped to that count so flex-wrap can't repack it.
  - *Alone*: your tile (≤ 480 px) + "It's just you in …" + Invite (MANAGE_INVITES, active server) /
    Share screen.
  - *People only*: best-fit grid, tiles 200–560 px.
  - *Streams live*: streams on top (best-fit, ≤ 960 px), people in **one** compact row (176 px) below.
  - *Focused* (`StreamViewPanel`): the video, then a filmstrip (128 px tiles): the other streams first,
    then people.
- **Dock**: floating, centred, `bg-light` + `shadow-float`: mic, deafen │ share/stop (+ stream-audio
  apps while live) │ leave (solid error, `on-error` glyph). Fullscreen draws it on a dark scrim.

## Tiles

- **Participant**: `bg-light` mixed with the avatar colour (`--tile-tint`: 16 / 13 / 22 / 12 % for
  graphite / graphite-light / console / console-light). Avatar ≈ 38 % of the tile height (32–96, 8 px
  steps), lifted on small tiles so the chip clears it. Bottom-left chip: name, "you", a filled red badge
  for a moderator mute/deafen, the red mic/headphones glyph for a self mute/deafen, the accent speaker
  for "muted by you". LIVE pill while streaming (not in the filmstrip, where the stream sits beside it).
  Speaking: instant `outline-success` + a 4 px `success/25` halo on the avatar, with no transition.
- **Stream**: video / thumbnail / avatar on black. Everything over it is a fixed dark scrim with white
  type in every theme (the CodecBadge rule): LIVE (+ accent "Watching"), quality (`1080p60 · AV1`, codec
  colour, lock if enforced), owner + audio glyph, viewer count. Watched → accent outline.
- **LIVE** is one fixed red (`--color-live`, #d1362e). White on each palette's own error red misses
  4.5:1 on the dark themes, and the pill floats over video anyway.

## Big channels: "+N more"

- Capacity = how many tiles of the floor size fit (grid) or fit in one row (streams row, filmstrip).
  Over capacity, the last slot is a "+N more" tile (three stacked avatars + count). Click → a popover
  listing the hidden people (same click / right-click as tiles). It outlines in success while someone
  in it talks.
- **Who is visible** (`useVisibleParticipants`): you and every streamer are pinned. The rest fill in
  roster order; when there are too many, the unpinned user quiet longest is dropped first (ties: the
  latest joiner).
- **Hidden speakers swap in** (owner decision, 2026-10-04): a hidden user who talks for 600 ms takes
  the slot of the visible, unpinned, silent user who has been quiet longest, **in place**, so nobody else
  moves. At most one swap per 3 s. Speaking is read with a store subscription, not a selector, so VAD
  flips don't re-render the grid. React only hears about it on an actual swap.

## Watching several streams

| Hovered stream | Actions (first = tile click) |
|---|---|
| not watched, nothing else watched | **Watch stream** → watch + focus |
| not watched, something else watched | **Switch** (stop the others, focus this) · **Watch too** (add, stay on the grid) |
| watched | **Focus** · **Stop** |
| your own | **Preview** |
| codec we can't decode | dimmed, "Can't play AV1", no actions |

In the focused view the filmstrip's watched streams keep playing (each has its own decoder); a click
there focuses without stopping anything. Close (✕) stops only the focused one.

## Player handoff

The single persistent `StreamVideoPlayer` (StreamPipManager) is reparented between the full view, the
mini player and now **the grid tile of the stream it holds** (`StreamTile`, `hostsPip`). Backing out to
the grid keeps it playing in place. The old 20 s "idle on the grid" drop is gone: the stream is visible
there, so the drop only swapped the tile to a second cold decoder. Watched streams that were never
focused run their own tile player, as before.

## Controls in the focused view

The overlays show on mouse move and fade after 2.5 s (panel and fullscreen alike). Double-click toggles
fullscreen; the controls swallow their own double-clicks. A single click on the video no longer goes
back to the grid. That now goes through Esc, the grid button or the breadcrumb.
