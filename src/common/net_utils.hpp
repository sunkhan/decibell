#pragma once
#include <vector>
#include <cstdint>
#include <cstring>
#include <string>
#include <boost/asio.hpp>
// Windows socket headers must form a specific order or be guarded
#ifdef _WIN32
    #include <winsock2.h>
#else
    #include <arpa/inet.h>
#endif

namespace chatproj {

    // Helper to prepare a buffer: [4-byte Length][Payload].
    //
    // Implemented as resize-then-memcpy rather than insert-into-empty-vector
    // because GCC 12's -Wstringop-overflow flow analysis reports a false
    // positive on the insert idiom (it loses track of the capacity grown by
    // the first insert and assumes the second writes into a zero-sized
    // region). resize avoids the back-to-back allocations anyway, so this is
    // both warning-free and marginally faster.
    inline std::vector<uint8_t> create_framed_packet(const std::string& serialized_data) {
        const uint32_t length = static_cast<uint32_t>(serialized_data.size());
        const uint32_t net_length = htonl(length);

        std::vector<uint8_t> buffer(4 + serialized_data.size());
        std::memcpy(buffer.data(), &net_length, 4);
        if (!serialized_data.empty()) {
            std::memcpy(buffer.data() + 4, serialized_data.data(), serialized_data.size());
        }
        return buffer;
    }
}