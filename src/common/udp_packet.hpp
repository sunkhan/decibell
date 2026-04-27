#pragma once
#include <cstdint>

namespace chatproj {

// Sender ID size: 32 bytes is enough for usernames and short JWT identifiers.
// The community server maps tokens→sessions separately, so full JWTs are not
// needed in every UDP packet.  This saves 224 bytes/packet vs the old 256.
constexpr uint16_t SENDER_ID_SIZE = 32;

// Max payload per UDP packet.  With the smaller header the largest packet
// (UdpVideoPacket = 45 + 1400 = 1445 bytes) fits inside the standard
// Ethernet MTU (1500) minus IP(20) + UDP(8) headers = 1472.
constexpr uint16_t UDP_MAX_PAYLOAD = 1400;

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
    uint8_t payload[UDP_MAX_PAYLOAD];
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

}