#pragma once
#include <cstdint>
#include <cstddef>

namespace chatproj {

// Sender ID size: 32 bytes is enough for usernames and short JWT identifiers.
// The community server maps tokens→sessions separately, so full JWTs are not
// needed in every UDP packet.  This saves 224 bytes/packet vs the old 256.
constexpr uint16_t SENDER_ID_SIZE = 32;

// Max video / FEC payload per UDP packet. Mirrors the Electron client
// (native/src/media/video_packet.rs UDP_MAX_PAYLOAD): 1200 keeps the
// largest datagram (UdpVideoPacket = 45 + 1200 = 1245 bytes) under the
// PPPoE / VPN / tunnel MTUs that shrink the classic 1472-byte budget, and
// leaves room for the 25-byte sealed envelope of P2P DM calls (1270 B).
// Was 1400 through 0.7.7 — the client had already moved to 1200 in 0.5.4,
// so the C++ structs over-allocated and the relay's sizeof-based buffers
// and checks disagreed with what was actually on the wire.
constexpr uint16_t UDP_MAX_PAYLOAD = 1200;

// Max audio payload (AUDIO / STREAM_AUDIO / PING). Mirrors
// native/src/media/packet.rs MAX_PAYLOAD_SIZE — audio never approaches it
// (a 20 ms Opus frame is a few hundred bytes) but the struct layout stays
// byte-compatible with the client's.
constexpr uint16_t AUDIO_MAX_PAYLOAD = 1400;

#pragma pack(push, 1) // Force 1-byte alignment to prevent padding issues across architectures

enum UdpPacketType : uint8_t {
    AUDIO = 0,
    VIDEO = 1,
    KEYFRAME_REQUEST = 2,
    NACK = 3,
    FEC = 4,
    PING = 5,
    STREAM_AUDIO = 6
};

// Max missing packet indices per NACK — keeps packet under MTU
constexpr uint16_t NACK_MAX_ENTRIES = 64;

// VideoCodec wire values are owned by proto/messages.proto (chatproj::VideoCodec).
// UdpVideoPacket.codec stays a plain uint8_t for fixed packet layout — cast
// to/from chatproj::VideoCodec at the boundaries that actually need names.
//   0 = CODEC_UNKNOWN (legacy VP9 slot, retired)
//   1 = CODEC_H264_HW (hardware H.264, preserves existing wire value)
//   2 = CODEC_H264_SW (x264)
//   3 = CODEC_H265
//   4 = CODEC_AV1

// Sent by a viewer to request the streamer to emit a keyframe immediately (PLI)
struct UdpKeyframeRequest {
    uint8_t packet_type;                    // Should be UdpPacketType::KEYFRAME_REQUEST
    char sender_id[SENDER_ID_SIZE];         // Token hash or username of the requester
    char target_username[SENDER_ID_SIZE];   // Username of the streamer to request keyframe from
};

struct UdpAudioPacket {
    uint8_t packet_type;                // Should be UdpPacketType::AUDIO
    char sender_id[SENDER_ID_SIZE];     // Token hash upstream, Username downstream
    uint16_t sequence;                  // Sequence number to drop out-of-order packets
    uint16_t payload_size;              // Exact size of the compressed audio data
    uint8_t payload[AUDIO_MAX_PAYLOAD];
};

// Sent by a viewer to request retransmission of specific missing video packets
struct UdpNackPacket {
    uint8_t packet_type;                    // Should be UdpPacketType::NACK
    char sender_id[SENDER_ID_SIZE];         // Token hash of the requester (viewer)
    char target_username[SENDER_ID_SIZE];   // Username of the streamer
    uint32_t frame_id;                      // Frame containing missing packets
    uint16_t nack_count;                    // Number of entries in missing_indices
    uint16_t missing_indices[NACK_MAX_ENTRIES]; // Indices of missing packets
};

// XOR-based Forward Error Correction — 1 FEC packet per group of data packets.
// If exactly 1 packet in the group is lost, the receiver can reconstruct it
// by XOR-ing the FEC payload with all other received packets in the group.
constexpr uint16_t FEC_GROUP_SIZE = 5;

struct UdpFecPacket {
    uint8_t packet_type;                // Should be UdpPacketType::FEC
    char sender_id[SENDER_ID_SIZE];     // Same as video packets
    uint32_t frame_id;                  // Which frame this FEC covers
    uint16_t group_start;               // First packet_index in the group
    uint16_t group_count;               // Number of data packets in this FEC group
    uint16_t payload_size_xor;          // XOR of all payload_sizes in the group
    uint8_t payload[UDP_MAX_PAYLOAD];   // XOR of all payloads (zero-padded to UDP_MAX_PAYLOAD)
};

struct UdpVideoPacket {
    uint8_t packet_type;                // Should be UdpPacketType::VIDEO
    char sender_id[SENDER_ID_SIZE];     // Token hash or Username
    uint32_t frame_id;                  // Frame number
    uint16_t packet_index;              // Index of this packet within the frame
    uint16_t total_packets;             // Total packets for this frame
    uint16_t payload_size;              // Size of the video chunk
    bool is_keyframe;                   // True if this chunk belongs to a keyframe
    uint8_t codec;                      // VideoCodec: see enum above
    uint8_t payload[UDP_MAX_PAYLOAD];
};
#pragma pack(pop)

// Largest datagram any client can send (the audio struct, now that video /
// FEC chunk at 1200) — sizes the relay's receive buffers so no packet type
// is ever truncated by receive_from.
constexpr std::size_t UDP_MAX_DATAGRAM =
    sizeof(UdpAudioPacket) > sizeof(UdpVideoPacket)
        ? (sizeof(UdpAudioPacket) > sizeof(UdpFecPacket) ? sizeof(UdpAudioPacket)
                                                          : sizeof(UdpFecPacket))
        : (sizeof(UdpVideoPacket) > sizeof(UdpFecPacket) ? sizeof(UdpVideoPacket)
                                                          : sizeof(UdpFecPacket));

}