#pragma once
#include <vector>
#include <cstdint>
#include <string>
#include <boost/asio.hpp>
// Windows socket headers must form a specific order or be guarded
#ifdef _WIN32
    #include <winsock2.h>
#else
    #include <arpa/inet.h>
#endif

namespace chatproj {

    // Helper to prepare a buffer: [4-byte Length][Payload]
    inline std::vector<uint8_t> create_framed_packet(const std::string& serialized_data) {
        std::vector<uint8_t> buffer;

        // 1. Calculate length
        uint32_t length = static_cast<uint32_t>(serialized_data.size());

        // 2. Convert to Network Byte Order (Big Endian)
        // Note: htonl takes a u_long, so we cast safely
        uint32_t net_length = htonl(length);

        // 3. Append Length (4 bytes)
        const uint8_t* len_bytes = reinterpret_cast<const uint8_t*>(&net_length);
        buffer.insert(buffer.end(), len_bytes, len_bytes + 4);

        // 4. Append Payload
        buffer.insert(buffer.end(), serialized_data.begin(), serialized_data.end());

        return buffer;
    }
}