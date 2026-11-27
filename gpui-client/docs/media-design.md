# Playing video and audio attachments in the GPUI client

`docs/FEATURES.md` §6 "Media playback": the players in a message's card. The Electron client hands the file
to Chromium's `<video>` / `<audio>`; this client has no Chromium, so the engine is ours:
`crates/decibell-media` (no GPUI in it) on `decibell-video`'s decoders, wired into the cards by
`decibell-chat-content/src/playback.rs`. Written 2026-11-26 with the first build; the numbers are from this
machine (i7-12700K, RTX 4080, NVIDIA 595, KDE Wayland) and each says how it was taken.

## 1. What the renderer does, and what this client does differently

Read from `PersistentVideoLayer.tsx`, `PersistentAudioLayer.tsx`, `videoPlaybackState.ts`,
`audioPlaybackState.ts`, `audioController.ts`, `AttachmentList.tsx` (`VideoItem`, `AudioItem`),
`mediaServer.ts`, `attachmentFetch.ts`, `protocol.ts`.

| Rule (renderer) | Here |
|---|---|
| One `<video>` and one `<audio>` exist, at app level; the card is a placeholder. A video and an audio attachment can play at the same time; a second video replaces the first, a second audio the first. Nothing ties them to voice channels, streams or calls. | The same: `Playback` (an entity, global) owns at most one video `Player` and one audio `Player`, following `model.chat.video.active` / `chat.audio.active`. Voice is not touched in either direction. |
| Playback survives the row unmounting (scrolled away, trimmed, a jump window). | The same: the player belongs to `Playback`, not to the row. While the row is not drawn the player is told (`set_visible(false)`): sound and clock go on, pictures are decoded and neither copied nor presented. |
| **A channel switch stops it** (`clearChannel` in both state modules, and the audio layer's own subscription) and forgets the channel's positions. FEATURES.md's "survive … channel switches" is not what the code does. What the renderer's listeners key on is `activeChannelId` alone, so playback *survives* every navigation that leaves that id as it is (the DM view entered directly, a voice / stream / settings view, another server with a channel of the same bare id): §9. | Leaving the channel clears both and the channel's "Paused at" positions and posters; so does a change of the open DM conversation (the renderer has no attachments in DMs). Reported to the architect, not changed. |
| There is no mini-player, docked player or "back to message" for attachments (those exist for streams only). | None built. |
| Click on the poster → plays at once, resuming where that attachment was left (five positions per channel, LRU). The bar under a standalone card says "Paused at m:ss" for a video left part-way. | The same (`Playback::resume`, `paused_label`). |
| Video controls: over the picture's lower edge, `from-black/85 via-black/50 to-transparent`, scrub bar (4 px track, accent fill), then play / pause, `m:ss / m:ss`, mute, an 80 px volume bar (wheel: ±5 %), fullscreen. Shown while paused, while scrubbing, or while the pointer moved over the video in the last 3 s; they fade and slide 300 ms. | The same layout and rules, the 300 ms ease-out fade and 8 px slide (`motion::transition_flag`: frames only while it moves, 0 list frames once the controls are gone) and the bars' hover thumbs included. |
| Single click toggles play after 220 ms; a double click toggles fullscreen instead. Keys on the focused player: Space, ← / → (5 s; repeats at most every 80 ms), F, Esc leaves fullscreen. | The same (the repeat throttle is not needed: seeks replace each other in the player). |
| Fullscreen: the wrapper becomes `fixed inset-0` and the window goes fullscreen; the cursor is hidden while playing, hovered and idle; a download button top right. | Window fullscreen + a shell overlay with the same stage; the cursor rests with the controls (`hide_cursor_until_mouse_moves`, GPUI patch 0013); the download button top right, fading with the controls. |
| Volume and mute are settings (`mediaVideoVolume` / `mediaVideoMuted`, `mediaAudioVolume` / `mediaAudioMuted`), separate for video and audio, persisted, adjustable before anything plays (audio card). Dragging to 0 mutes; any other value unmutes. | The same settings (`media_*` in the core's config, through `model.settings`). |
| Sound goes to Chromium's default output: the renderer never calls `setSinkId`, so Settings → Audio's output device (and the separate stream output) do not apply to attachments. | **Differs** (owner, 2026-11-26): attachments are routed like other people's stream audio. The separate stream-output device when that option is on, else the voice output device; no name saved means the system's default. A stream of the player's own on that device (`output.rs`, §3 "The output device"). |
| At the end the element pauses on its last frame; play starts again from 0. Nothing loops. | The same (`Phase::Ended`). `Player::set_looping` exists and is tested; nothing turns it on. |
| Audio card: play / pause, scrub (only once the element reported a duration), `m:ss / m:ss` seeded from the upload's `durationMs`, mute, volume with wheel; Space on the focused card toggles. An MP3's `Infinity` duration is guarded to 0. | The same, Space on the focused card included (a press on the card focuses it). Durations are always finite here: the container's, else FFmpeg's estimate from the bitrate, else 0. |
| A media error: the video element is reloaded and resumed where it was; the audio card logs. | A file that cannot be opened or decoded shows why on the video's box ("Unsupported format (…)", the server's or reader's error) and a click tries again; the audio card shows `Failed to load: <reason>` under its row with the X on its button, and the button or Space opens the player again (the renderer's button is disabled there; its only error is a URL that cannot be built). A decoder that fails mid-file is replaced by the next (`decibell-video`'s order) from the keyframe before the current position, at most four times without a picture in between. |
| A deleted message's video goes on playing with nothing showing it, until the channel changes. | The same. |
| The poster is the upload's thumbnail (else the ThumbHash blur); after a video was played, a JPEG of its last frame. | The same picture by other means: when a player is dropped, the slot its last picture is in is kept (`decibell_video::Still`, two plane textures: 3 MB for 1080p) and the card draws it (`StillView`) under the same veil and play button, with "Paused at". No read-back, no JPEG, no decoder for a card at rest (`media-poster`: 0 frames at rest with it on screen); five a channel, dropped with the channel. |
| Bytes: `<video>` range requests through a loopback HTTP proxy that adds the token and decrypts sealed files. | `MediaReader` (range reads, sealed chunks opened in memory, 256 KiB read ahead, the download cap) behind a custom AVIO context. Nothing on disk, no local socket. |

## 2. The codec matrix

What the upload path calls video or audio is anything whose MIME type starts with `video/` or `audio/`
(`classify` in `uploadAttachment.ts`; `guessMime` knows mp4, webm, mov, mkv, avi, mp3, wav, ogg, m4a, flac,
opus). What plays is what this Electron's Chromium (Castlabs Electron 33 = Chromium 130, proprietary codecs,
`PlatformHEVCDecoderSupport` on) decodes. That set is the matrix; every FFmpeg component added for playback is
one of its rows.

| Container (demuxer) | Video | Audio | Chromium 130 | Here |
|---|---|---|---|---|
| MP4 / M4V / MOV (`mov`) | H.264 | AAC, MP3, Opus, FLAC, PCM | yes | yes |
| | HEVC 8-bit | | with a platform decoder | yes, hardware or software |
| | AV1, VP9 | | yes | yes |
| WebM / Matroska (`matroska`) | VP8, VP9, AV1, H.264, HEVC | Opus, Vorbis, AAC, FLAC, PCM | yes (`.mkv` by sniffing) | yes |
| Ogg (`ogg`) | — (Theora left Chromium in 123) | Vorbis, Opus, FLAC | yes | yes |
| MP3 (`mp3`) | | MP3 | yes | yes |
| WAV (`wav`) | | PCM s16 / s24 / s32 / f32 / u8, A-law, µ-law | yes | yes |
| FLAC (`flac`) | | FLAC | yes | yes |
| ADTS (`aac`) | | AAC | yes | yes |
| AVI | | | no (no demuxer) | no: "This file can't be played" |

10-bit 4:2:0 plays (HEVC Main 10, AV1 Main at 10 bits, VP9 profile 2; §2a). Not played, on purpose: **4:2:2 /
4:4:4 video** (the client's picture slots are 4:2:0, `video-design.md` §5.2; Chromium plays 4:4:4 in software,
rare in what people share), AC-3 / E-AC-3 / DTS /
ALAC / Theora / MPEG-4 Part 2 (Chromium does not play them either). A file with a picture we cannot decode is
refused whole ("Unsupported format (codec)") rather than played as sound.

**What decodes each video codec on this machine** (RTX 4080; `StreamStats::backend`, the same order as live
streams, `video-design.md` §0.5):

| Codec | First choice here | Fallbacks |
|---|---|---|
| H.264, HEVC, AV1 | Vulkan Video on the renderer's device | NVDEC, then software (FFmpeg's h264 / hevc, dav1d) |
| VP9 | Vulkan Video (`VK_KHR_video_decode_vp9`) | NVDEC, then software (FFmpeg's `vp9`) |
| VP8 | **software** (FFmpeg's `vp8`) | — |
| 10-bit HEVC / AV1 / VP9 | Vulkan Video, the codec's 10-bit profile, into 16-bit planes | software (NVDEC and VA-API do not hand 10-bit pictures over yet) |

**VP9 on the GPU** (the commonest WebM codec; software 1080p60 VP9 costs a core): FFmpeg's `vp9_vulkan`,
`vp9_nvdec` and `vp9_vaapi` hwaccels are in the prefix, the Vulkan device is created with
`VK_KHR_video_decode_vp9` and its feature bit where the driver has them (the extension is newer than the pinned
`ash`: its three structs are declared in `linux/mod.rs`), and D3D11VA has the `VP9_VLD_PROFILE0` profile. VP9
is a codec only files use: `Codec::ALL`, the start-up probe and what the client advertises stay the three
stream codecs. Its probe is lazy instead (`probe::file_codec_works`): the first time a file needs a hardware
decoder for VP9, the same 320 × 240 test picture (a 156-byte VP9 keyframe in the binary) is decoded on it, read
back and compared; a decoder that fails is given up for VP9 for the session with its reason and the file goes on
to the next of the order. Once per decoder per session, on the file's decoder thread. VP8 stays software.
AMD / Intel: the same order with VA-API in NVDEC's place, exactly as for streams; unverified here as there.

### 2a. 10-bit, and HDR on an SDR target

**The decision: 16-bit plane textures, the same shader.** A 10-bit picture is handed to the renderer in `R16` /
`RG16` slots with its code in the high bits (P010's layout), and the surface's matrix is scaled for the depth
(GPUI patch `0031`). That is the path with the least cost on the hardware side: Vulkan Video decodes Main 10 /
AV1 10-bit / VP9 profile 2 into a `G10X6_B10X6R10X6` image whose planes are copied into the slot by the same
two `vkCmdCopyImage` regions as an 8-bit picture's, with no conversion pass and nothing on the CPU. Converting
to 8 bits instead would have needed a render or compute pass per picture (a multi-planar image cannot be
blitted) and thrown the depth away. Software pictures (`yuv420p10` / `yuv420p12` / P010) are shifted into the
same layout in the one staging copy that an 8-bit picture also gets. A device without 16-bit normalised textures
(wgpu's `TEXTURE_FORMAT_16BIT_NORM`), the OpenGL fallback and Direct3D's software slots get the 8 most
significant bits (`decode::Narrow`) and say so (`VideoHub::deep_planes`).

| | 8-bit | 10-bit |
|---|---|---|
| Vulkan Video | as before | the driver is asked for the codec's 10-bit profile at open (`hardware_support_at`); P010 into 16-bit slots |
| NVDEC, VA-API, D3D11VA | as before | refused at open with a reason; the file goes on to software. Open item (§8) |
| Software | as before | FFmpeg's hevc / vp9 and dav1d decode 10 and 12 bits; one shifted copy into 16-bit slots |

The demuxer tells the stream that its pictures are deep (`Stream::set_deep`) before the first packet. A hardware
decoder that fails on a deep file is given up **for that file only**: a GPU without Main 10 still decodes the
codec's 8-bit streams, so the session-wide "this decoder does not work for this codec" rule is not applied.
Live streams stay 8-bit.

**HDR.** Chromium on an SDR desktop tone-maps PQ and HLG video to the screen: it undoes the transfer function,
converts BT.2020 to the display's primaries and compresses highlights with `x (1 + x / peak²) / (1 + x)`, with
the peak from the file's mastering metadata. This client does the same in the surface's fragment shader
(`surface_to_sdr`, both renderers), per picture, from the frame's own colour description (`color_trc`,
`color_primaries`): PQ's EOTF, or HLG's inverse OETF with the 1000-nit OOTF (gamma 1.2); linear light with
1.0 = 203 nits; BT.2020 → BT.709; that highlight curve on the largest channel; sRGB encoding. **The
approximation**: the peak is always taken as 1000 nits (mastering and content light level metadata are not
read), there is no per-scene analysis, and out-of-gamut colours are clipped after the matrix. Gamma-encoded
BT.2020 without an HDR transfer gets the primaries' conversion alone. Without this an HDR file drawn through
the SDR matrix is washed out (PQ) or flat (HLG); with it a mid grey comes out where the formula puts it
(`media-10bit` checks the drawn value against the formula; whether it *looks* right is for the owner's eye,
HANDOFF §6). An HDR output (a Wayland HDR surface, Windows' HDR) is not used: the window is SDR.

### 2b. Rotation

A phone's portrait video is stored on its side with a display matrix in the container. `Demuxer` reads it
(`AV_PKT_DATA_DISPLAYMATRIX` of the track) into `decibell_video::Orientation` (the four rotations and the
mirrors; anything else is drawn as stored, as Chromium does with a matrix it does not recognise), reports the
**turned** size everywhere (`VideoTrack`, `Probe`, `Status::video`, `Stream::picture_size`), and tells the
stream (`Stream::set_orientation`) before the first packet. The renderer turns the texture coordinates when it
draws the surface (GPUI patch `0030`, both renderers): no pixel is copied, the slots and the decoders know
nothing about it, the fit (`Contain` in the card and in fullscreen) works on the turned size, and the kept
last picture (`Still`) is turned the same way. The card's box and poster are the upload's (Electron uploads
record the turned size and a turned thumbnail).

### The FFmpeg cost

`tools/build-ffmpeg.sh` gained, for this matrix and nothing else: `--enable-demuxer=mov,matroska,ogg,mp3,wav,
flac,aac`, `--enable-parser=vp8,vp9,aac,mpegaudio,opus,vorbis,flac`, `--enable-decoder=vp8,vp9,aac,mp3float,opus,
vorbis,flac,pcm_s16le,pcm_s24le,pcm_s32le,pcm_f32le,pcm_u8,pcm_alaw,pcm_mulaw` (`libswresample` was already built).
Still `--disable-everything` first, LGPL-2.1+, static. `crates/decibell-app/build.rs` refuses a prefix without
them (the decoders in `libavcodec.a`, the demuxers in `libavformat.a`, `libswresample.a`).

| | Before | After |
|---|---|---|
| The prefix (`vendor/ffmpeg`, static archives with symbols) | 79 MB | 93 MB |
| `libavcodec.a` / `libavformat.a` | 23.9 / 2.0 MB | 35.3 / 5.0 MB |
| `decibell` (release, stripped, `dev-local`), the same sources linked against each prefix | 81,809,000 B | 85,716,456 B (**+3.9 MB**, +4.8 %) |
| The same with the engine, the players and their self-tests in it | | 86,258,696 B (+0.5 MB of ours) |

Windows: vcpkg's FFmpeg is not built with `--disable-everything`; with its default features (`avformat`,
`swresample`, which the build script now checks) it has every row. No overlay or workflow change.

## 3. The engine (`crates/decibell-media`)

```text
decibell-media (control thread, one per player)            decibell-video-<n> (the hub's, one per video)
  av_read_frame ──video──> stash ──> Stream::push_timed ──> decode ──> Timing::admit: sleep until the
       │                    (≤ 24)     (queue of 3)                     picture is due on the clock, then
       └──audio──> decode, resample ──> backlog ──> ring ──┐            copy into a slot and present
                   (control thread)     (≤ 0.5 s)  (1.4 s) v
                                  device callback: Renderer::render ──> the clock
```

- **Bytes** (`source.rs`): any `Read + Seek` behind an `AVIOContext` with a 64 KiB buffer. The app's is
  `MediaReader::blocking()`, created on the control thread when the attachment service is there.
- **Demux** (`demux.rs`): `avformat_open_input` on the custom I/O (format by content, never by name), stream
  info within 2 s of analysis, the first playable video track (cover art is not one) and the first audio track,
  everything else discarded at the demuxer. Video packets go to the decoder in FFmpeg's own buffers
  (`PacketBytes` over a recycled `AVPacket` shell: no copy); audio packets are decoded where they are read.
- **Video** is a **timed stream** of `decibell-video` (`timed.rs`, `video-design.md` §0.11): the same
  decoders, slots and presenter as a live stream, with three differences. Its packets are queued three deep
  and handed back when the queue is full, never shed; its decoder reorders (B-frames) and is flushed, not
  reopened, at a seek; and each decoded picture waits in `Timing::admit` on the decoder thread until it is due.
  So the thread decodes exactly one picture ahead and sleeps: **one wake per picture, at the file's rate,
  nothing polls** (a 24 fps file wakes it 24 times a second whatever the display's rate).
- **Audio** (`audio.rs`, `render.rs`, `output.rs`): FFmpeg's decoder → `libswresample` to interleaved stereo
  `f32` at the device's rate (mono is spread at its own level, as Chromium does; more than two channels are
  mixed down by the resampler's default matrix) → a lock-free ring of 65,536 frames → `Renderer::render` in the
  device's callback. The device is the default output through cpal (the version the core links), a stream of
  the player's own: opened when it has sound to make, **closed 250 ms after its envelope reached zero** (pause,
  end), so a paused player has no audio thread and makes no device wakeups.
- **The clock** is the renderer's: after each callback it publishes "the sample at media time *t* reaches the
  device at instant *i*" (cpal's playback timestamp), and `Shared::now` reads that without a lock. It stands
  still whenever the envelope is at zero. A file without sound, a machine without a device, and the tail of a
  video whose sound ends first run on a monotonic clock started with the first picture.
- **The output device** (`output::Device`, chosen by `playback.rs` from Settings → Audio): the saved name is
  resolved at every open the way the core resolves a saved voice device (Linux: ALSA's `pulse` PCM with
  `PULSE_SINK` naming the sink, so a sink that is not there leaves the stream on the server's default;
  elsewhere cpal's device name, the default device when it is not connected). **Changing it while a file plays**
  (`Player::set_output`): the sound fades to zero on the old device (the next 10 ms of the file), the old stream
  is closed once it reported 250 ms of rendered silence, the new one opens on silence, and the renderer starts
  from zero **on the sample that is due at that moment** (`Shared::sync_to`: samples in between are left
  unplayed, at envelope zero). The picture does not stall: from the fade to the first sample on the new device
  the player's clock is the wall clock, started from the audio clock's last value; the sound takes the clock
  back when it flows again, and the two agree because the renderer aligned itself to the same anchor. A device
  with another sample rate is the one case that costs a seek (the sound is decoded again at that rate from
  where the file is). Paused, the next stream simply opens on the new device.
- **A seek** is a new epoch: the picture queue is dropped; the demuxer goes to the keyframe at or before the
  target; pictures before the target are decoded and skipped; the first at or after it is shown at once,
  playing or not; the sound is faded out, decoded from the same keyframe, **trimmed to the target's sample**,
  and held behind a gate until that picture is up, then faded in. Seeks that arrive while one is in progress
  replace each other (a dragged scrub bar).
- **The end**: the decoder is drained (an empty packet), the ring's last sample is known in advance and the
  envelope reaches zero on it; `Phase::Ended` once the sound has been played and the clock passed the last
  picture.

### What exists when

| State | Threads of ours | Decoders | Device | Timers |
|---|---|---|---|---|
| No player (cards at rest, any number on screen) | none | none | none | none |
| Playing | `decibell-media` (asleep unless a queue has room: woken once per picture and once per half ring), `decibell-video-<n>` (asleep until the next picture is due), cpal's callback thread | one video, one audio | one output stream | the UI's 250 ms tick, only while a running time is on screen |
| Paused | the first two, **parked** (the decoder thread inside `admit` with the next picture in hand: a resume shows it on the next clock tick, no keyframe needed) | kept: the decoder session, its five slots, the audio decoder | closed 250 ms after the fade-out | none |
| Ended | `decibell-media` parked; the video thread leaves 3 s later and frees the decoder (the last picture's slot stays on screen) | audio kept | closed | none |
| Dropped (another attachment, the channel left, sign-out) | the control thread fades the sound out, waits for the device to have played it (≤ 2 s) and exits | freed | closed | none |

A paused player costs its parked threads' stacks (two, untouched), the video decoder's session and slots on the
GPU (a 1080p Vulkan Video session: the decode picture buffer plus five NV12 slots) and the 0.5 MB ring: no CPU,
no wakeups (`media-idle`).

### Bounds

Read ahead: `MediaReader`'s 256 KiB + the AVIO buffer's 64 KiB. Encoded: ≤ 24 video packets stashed + 3 queued.
Decoded video: one picture in hand + five slots. Decoded audio: the ring (65,536 frames, 0.5 MB) + ≤ 0.5 s of
backlog. All of it goes with the player.

## 4. No pops

Rule (`render.rs`, module doc): the device is only ever handed `sample × shape(envelope) × volume`; the
envelope and the volume each move by at most `1 / ramp` per sample (10 ms from silence to full scale); `shape`
is a raised cosine; and the envelope heads for zero as soon as the samples left in the ring are only enough to
finish the way down. Starting, pausing, resuming, seeking, the end of the file, an underrun, a volume or mute
change and closing are therefore one continuous curve, and samples are consumed only while it is above zero
(a pause skips nothing). A device stream is opened onto silence and closed only after 250 ms of rendered
silence (more than any device buffers). A device that dies with sound in it (unplugged) is the one case where
its own buffer is cut off by the hardware; the next stream starts from zero and ramps.

Tests, all on what the sink is handed, sample by sample (`cargo test -p decibell-media`):

| Test | Source | Transitions | Largest step between two samples |
|---|---|---|---|
| `render::tests::*` (12, offline: the renderer driven block by block) | full-scale constant (any gap is a step), a ramp, a 440 Hz sine | start, pause, resume on the next sample, seek, end on the last sample, a 2 ms file, underrun + trickle + recovery, volume / mute, device hand-over, the gate | ≤ the ramp's steepest (π / 2 / 480 per sample at full scale = 0.0033) |
| `no_transition_hands_the_device_a_step` (the whole player, real threads, the recording sink) | constant at half scale | start, pause, resume, resume on a new stream after the device closed, seek while playing, volume, mute, seek while paused, the end, drop | 0.00164 of full scale; allowed 0.00167 |
| `an_underrun_and_an_unplugged_device_are_silent_too` | the same, from a reader that stops for 4.2 s; then a sink that is unplugged | 2.9 s of silence in the middle, every sample played once; the new stream starts at 5·10⁻⁶ | 0.00164 |

By ear this cannot be judged in an agent session: the owner's listening checks are listed in `HANDOFF.md` §6.

## 5. Sync, seeking, allocations (headless: software decode into the hub's counting sink, the recording sink)

- **A/V sync** (`pictures_stay_within_twenty_milliseconds_of_the_sound_for_a_minute`: `sync60.mp4`, 60 s,
  25 fps H.264 with B-frames whose pictures carry their frame number, AAC click track): 1,500 pictures shown, 0
  dropped; the moment a picture is handed over against the moment its sound is heard: **max 0.14 ms, mean
  0.02 ms** (target ±20 ms). This measures the clock and the scheduling, not the screen: on a display the
  picture is on glass up to one refresh later (the present), which is the same for live streams.
- **Seek accuracy**: the picture shown is the one covering the target (eight targets across a 10 s group of
  pictures: worst 20 ms = half a picture, by definition of "covering"); the first sample heard is the target's
  sample (three targets in a WAV whose samples say where they are: **0 samples off**).
- **Allocations** (`tests/alloc.rs`, a counting global allocator, every thread): **0 Rust allocations** over
  100 pictures and 4 s of sound, and over 4 s of sound alone. FFmpeg's own `malloc`s are not counted (the demuxer
  makes a buffer per packet), the line `decibell-video`'s test draws too.
- **Every matrix row** decodes and plays to its end with its 50 pictures in order and its three clicks half a
  second apart (`every_file_of_the_matrix_plays_to_its_end`, 21 files); a sealed file plays from a reader that
  decrypts 64 KiB chunks in memory, by range (2 of 60 s read less than half the file).

### In the app (release build, the content fixture's `#clips`, the null audio sink; `media-*` self-tests)

| | `sync60.mp4` (64 × 16, 25 fps) | 1080p60 H.264, 11.4 Mbit/s (`DECIBELL_MEDIA_CLIP`) |
|---|---|---|
| Decoder | Vulkan Video | Vulkan Video (`nvidia-smi` decoder utilisation 7 % while it played) |
| Click → first picture on screen | 50 ms | 62 ms |
| Pictures published, 4 s, controls hidden | 100 (25.00 / s), 0 dropped | 241 (60.2 / s), 0 dropped |
| Frames the window drew meanwhile | **0** (100 presents of the kept scene) | **0** (120 presents: the self-test's window is unfocused and has GPUI's default 30 fps inactive cap, `video-design.md` §7; the app's main window has none) |
| CPU while playing, whole process | 1.5 % of a core | 3.0 % of a core |
| Wakeups / s: UI thread, decoder thread, control thread, the null sink | 125, 51, 25, 100 | 148, 127, 60, 100 |
| Paused 3 s | 0 pictures, 0 frames, 0 presents, 0 ms of CPU in the player's threads, the sink thread gone | the same |
| Seek to 2 s before the end → picture | 50 ms | 25 ms |
| Private memory before / playing / after it was dropped | 150.4 / 157.4 / 151.3 MB (+0.8) | 186.0 / 244.2 / 198.3 MB (**+12.3**: the 43 MB clip is held in memory by the self-test's source and read through; the difference that stays is not explained yet, §8) |

`media-idle`: video and audio cards on screen, nothing playing, 5 s: 0 frames drawn, 0 presented, no playback
thread, 0 decoder threads. `media-persist`: the row scrolled away for 2 s, the position goes on (0.73 → 2.73 s)
and 0 pictures are published; scrolled back, pictures again; a second video replaces the first and an audio
attachment plays beside it (two players); another channel stops both and no thread is left.
`media-live` (the local stack, signed in as `perf2`): a 133 KB clip uploaded to `#off-topic`, its card clicked:
first picture 58 ms later **from the attachment service's range reader**, Vulkan Video, duration 60.00 s; a seek
45 s ahead shows its picture after 558 ms (a new range request, then the decode from the keyframe 5 s before the
target); the same attachment as sound alone plays; the message is deleted again.
`media-audio-play`: sound 526 ms after the click (the 100 ms pre-buffer and the mirrored time's first tick), a
pause leaves 0 frames and no sink thread, a seek to 58 s lands, the end is the end. Load average 5–10 during
these runs (other agents' builds).

## 6. Test media (`crates/decibell-media/tests/assets`, 376 KB)

Made by `tests/assets/make.py` with the system's `ffmpeg` and committed: a 16-cell barcode of the frame number
over grey, and a click every half second. Nothing downloaded, nothing anybody's. 2 s files are 2–36 KB each;
`sync60.mp4` (64 × 16, 60 s) is 137 KB. The client's own FFmpeg has no muxers and no software H.264 / VP8 / VP9
encoder, so the files are not generated at test time.

## 7. The UI (`decibell-chat-content/src/playback.rs`)

`Playback` (global entity): the two players, the positions, the video's control state, the tick. The video
card's surface asks `video_stage` and gets the picture (`VideoView` on the player's `Viewer`) with the controls
over it, drawn **in the card's own element**, so it scrolls and clips with the list in the same frame (the
`VideoHosts` bounds of the seam are still recorded but nothing needs them). A picture costs one present of the
kept scene; the list is redrawn only for the controls. The audio card keeps drawing from `chat.audio`'s
mirrored state, which `Playback` feeds, and gains its scrub and volume tracks (`track`: a canvas that follows
the pointer over the whole window while the button is held).

## 8. Open

- **Fullscreen by the F key with a resting pointer** shows the controls (and the download button) only once
  the pointer moves: the fullscreen stage is another element than the card's and starts un-hovered. The
  renderer's wrapper is the same element in both states and keeps its hover.
- **Deleting the only message of an encrypted channel leaves its row drawn** (seen by `media-sealed` in the
  stack's `#vault`; the server did delete it). Not playback's; for whoever owns the list.
- **The captures** of the cards in seven themes beside Electron pages are not made: a capture renders one
  headless frame and no picture is decoded there. What a capture screen needs is a `decibell_video::Still`
  made from a fixture frame uploaded into a slot (the poster and the stage both draw one).
- **10-bit on NVDEC, VA-API and D3D11VA**: refused at open, software decodes instead (Vulkan Video is the first
  choice on all three Linux vendors, so this is the fallback's fallback; on Windows it means 10-bit is software
  until P010 slots are written in `windows/slots.rs`).
- **HDR**: the conversion is an approximation (§2a) nobody has looked at; mastering metadata is not read; no
  HDR output.
- **A device at another sample rate** costs a seek when the output device changes mid-file (§3).
- **4:2:2 / 4:4:4** video is refused ("Unsupported format (codec, not 4:2:0)"); Chromium plays it in software.
- **Uploads from this client record no size, duration or poster for a video** (`decibell-attachments`'
  `MediaProbe` has no implementor): `decibell_media::probe` gives the turned size and the duration from the
  header and would be the place to start; the card's box then falls back to its default shape.
- **The shared FFmpeg prefix** has no VP9 hwaccels until it is rebuilt from `tools/build-ffmpeg.sh` (the stamp
  differs, so the script rebuilds; `cargo clean -p ffmpeg-sys-next` in every target directory after the swap).
  Against the old prefix VP9 fails the lazy probe on every hardware decoder with its reason and plays in
  software: nothing breaks.
- **Not run**: AMD / Intel (VA-API VP9, the shared-queue Vulkan path for VP9 and 10-bit), Windows (everything:
  `windows-owner-checks.md`).

## 9. Where the renderer's playback survives a navigation (told, not changed)

The three places that stop a player (`videoPlaybackState.ts:96-99`, `audioPlaybackState.ts:77-79`,
`PersistentAudioLayer.tsx:26-36`) all fire only when `useChatStore.activeChannelId` changes to another value.
So in the Electron client a playing video (hidden, still audible) or audio attachment goes on through:

- entering the DM view directly: an unread DM tile in the server bar (`ServerBar.tsx:212-215`), Send in the
  profile popup, Message in the profile modal, accepting a call, the mini stream player's expand during a call
  (none of them touch `activeChannelId`), and from there through every switch between DM conversations;
- another server that has a channel with the same bare id (`openServer.ts:11-21` keeps the channel when the new
  server has one with that id: every server's `general`), where a card with the same attachment id would even
  host the other server's video;
- the voice view, a stream view, browse, Settings and every other modal;
- a search Jump or "Go to message" inside the same channel (the row unmounts and remounts), the active
  channel or server clicked again.

It stops on: another channel of the same server, another server without that channel id, the Home button,
leaving the server, the channel being deleted, a Jump to another channel, sign-out. No attachment can be
played inside a DM (DM bubbles get no attachments). The native client stops on every channel change and on
every change of the open DM conversation.
