#pragma once
// Return-routability for the relay's UDP endpoints (review S11).
//
// A datagram's source address is whatever the sender wrote in it. The relay
// used to bind — and on every change, move — a session's voice / media
// endpoint to the source of any datagram carrying the session's UDP key, so
// a member could join a busy channel, watch every stream, then send one
// keyed packet from a spoofed source and have the relay pour all of it at a
// victim, indefinitely.
//
// Now an endpoint is bound only once it has proven it can receive: a keyed
// datagram from an address that isn't the bound one gets a BIND_CHALLENGE
// (a fresh random cookie) sent to that address, and only a BIND_ECHO from
// the same address carrying the cookie binds it. The first bind on joining
// and every later move (NAT rebinding, Wi-Fi roam) take the same path; the
// client answers from its receive loop, so it costs one round trip.
//
// One UdpPath per session per socket (voice, media), touched only on the io
// thread. Constant memory: one candidate + one cookie at a time.
#include <chrono>
#include <cstdint>
#include <cstring>
#include <boost/asio/ip/udp.hpp>
#include <openssl/crypto.h>
#include <openssl/rand.h>
#include "../common/udp_packet.hpp"

namespace chatproj {

class UdpPath {
public:
    using clock = std::chrono::steady_clock;
    using endpoint = boost::asio::ip::udp::endpoint;

    // At most one challenge per session per socket per interval, so the
    // relay can't be turned into a packet cannon even by a key holder (and
    // each challenge answers a datagram at least as big, never more bytes).
    static constexpr auto kChallengeInterval = std::chrono::milliseconds(250);
    // An echo has to come back within this; a cookie is also single-use.
    static constexpr auto kCookieLifetime = std::chrono::seconds(10);

    // Where the relay sends this session's traffic: port 0 until an echo
    // has bound an address.
    const endpoint& bound() const { return bound_; }

    // Hot path: is `src` the bound endpoint? (One compare per datagram.)
    bool is_bound_to(const endpoint& src) const {
        return bound_.port() != 0 && bound_ == src;
    }

    // A keyed datagram came from `src`, which isn't the bound endpoint.
    // Returns true and fills `out` when a challenge to `src` may go out
    // now. A new address replaces any earlier candidate (one at a time)
    // with a fresh cookie; the same address within the cookie's lifetime
    // is sent the same cookie again, so an echo still in flight on a slow
    // path (round trip > the pacing interval) stays good. The caller has
    // checked the trigger was at least sizeof(out).
    bool make_challenge(const endpoint& src, clock::time_point now, UdpBindChallenge& out) {
        if (sent_any_ && now - last_sent_ < kChallengeInterval) return false;
        const bool resend = pending_ && src == candidate_ && now - issued_at_ < kCookieLifetime;
        if (!resend) {
            uint8_t fresh[UDP_BIND_COOKIE_SIZE];
            if (RAND_bytes(fresh, sizeof(fresh)) != 1) return false;
            std::memcpy(cookie_, fresh, sizeof(cookie_));
            candidate_ = src;
            issued_at_ = now;
            pending_ = true;
        }
        last_sent_ = now;
        sent_any_ = true;
        std::memset(&out, 0, sizeof(out));
        out.packet_type = UdpPacketType::BIND_CHALLENGE;
        std::memcpy(out.cookie, cookie_, sizeof(out.cookie));
        return true;
    }

    // A BIND_ECHO from `src` with `cookie`: binds `src` and returns true
    // when it answers the live challenge sent to exactly that address.
    bool accept_echo(const endpoint& src, const uint8_t* cookie, clock::time_point now) {
        if (!pending_ || src != candidate_) return false;
        if (now - issued_at_ > kCookieLifetime) {
            pending_ = false;
            return false;
        }
        if (CRYPTO_memcmp(cookie, cookie_, sizeof(cookie_)) != 0) return false;
        pending_ = false;   // single use: a replayed echo can't re-bind later
        bound_ = src;
        return true;
    }

private:
    endpoint bound_;
    endpoint candidate_;
    uint8_t cookie_[UDP_BIND_COOKIE_SIZE] = {};
    clock::time_point issued_at_{};   // cookie_ drawn (lifetime)
    clock::time_point last_sent_{};   // last challenge out (pacing)
    bool sent_any_ = false;
    bool pending_ = false;            // cookie_ is live for candidate_
};

} // namespace chatproj
