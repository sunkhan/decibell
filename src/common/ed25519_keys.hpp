#pragma once
// Ed25519 key handling for JWT signing (Theme A).
//
// Central signs tokens with the private key; community servers verify
// with the public key only, so a community operator (or a leaked
// community config) can no longer forge a token for any user. Keys are
// PEM (PKCS#8 private / SubjectPublicKeyInfo public) — what jwt-cpp's
// `jwt::algorithm::ed25519` expects.
#include <openssl/evp.h>
#include <openssl/pem.h>
#include <openssl/bio.h>
#include <openssl/sha.h>
#include <openssl/x509.h>

#include <fstream>
#include <iostream>
#include <memory>
#include <sstream>
#include <string>

namespace chatproj {

inline std::string read_file(const std::string& path) {
    std::ifstream f(path, std::ios::binary);
    if (!f.good()) return {};
    std::ostringstream ss;
    ss << f.rdbuf();
    return ss.str();
}

inline bool write_file(const std::string& path, const std::string& data) {
    std::ofstream f(path, std::ios::binary | std::ios::trunc);
    if (!f.good()) return false;
    f << data;
    return f.good();
}

/// Derives the public PEM from a private PEM.
inline std::string ed25519_public_from_private(const std::string& private_pem) {
    std::unique_ptr<BIO, decltype(&BIO_free)> in(
        BIO_new_mem_buf(private_pem.data(), static_cast<int>(private_pem.size())), BIO_free);
    if (!in) return {};
    std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)> key(
        PEM_read_bio_PrivateKey(in.get(), nullptr, nullptr, nullptr), EVP_PKEY_free);
    if (!key) return {};
    std::unique_ptr<BIO, decltype(&BIO_free)> out(BIO_new(BIO_s_mem()), BIO_free);
    if (!PEM_write_bio_PUBKEY(out.get(), key.get())) return {};
    char* p = nullptr;
    long n = BIO_get_mem_data(out.get(), &p);
    return std::string(p, static_cast<size_t>(n));
}

/// Loads `<path>` (private PEM) and `<path>.pub`; generates both when the
/// private key file doesn't exist. Returns false on any I/O or OpenSSL
/// failure.
inline bool load_or_create_ed25519(const std::string& path,
                                   std::string& private_pem,
                                   std::string& public_pem) {
    private_pem = read_file(path);
    if (private_pem.empty()) {
        std::unique_ptr<EVP_PKEY_CTX, decltype(&EVP_PKEY_CTX_free)> ctx(
            EVP_PKEY_CTX_new_id(EVP_PKEY_ED25519, nullptr), EVP_PKEY_CTX_free);
        if (!ctx || EVP_PKEY_keygen_init(ctx.get()) <= 0) return false;
        EVP_PKEY* raw = nullptr;
        if (EVP_PKEY_keygen(ctx.get(), &raw) <= 0) return false;
        std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)> key(raw, EVP_PKEY_free);
        std::unique_ptr<BIO, decltype(&BIO_free)> out(BIO_new(BIO_s_mem()), BIO_free);
        if (!PEM_write_bio_PrivateKey(out.get(), key.get(), nullptr, nullptr, 0, nullptr, nullptr)) return false;
        char* p = nullptr;
        long n = BIO_get_mem_data(out.get(), &p);
        private_pem.assign(p, static_cast<size_t>(n));
        if (!write_file(path, private_pem)) return false;
        std::cout << "[Auth] Generated new Ed25519 JWT signing key at " << path << "\n";
    }
    public_pem = ed25519_public_from_private(private_pem);
    if (public_pem.empty()) return false;
    // Keep the .pub beside it current (operators copy this file to
    // community servers).
    if (read_file(path + ".pub") != public_pem) {
        write_file(path + ".pub", public_pem);
    }
    return true;
}

/// sha256-hex of a PEM certificate's DER encoding — the fingerprint
/// clients and communities pin against.
inline std::string cert_fingerprint_from_pem(const std::string& cert_pem) {
    std::unique_ptr<BIO, decltype(&BIO_free)> in(
        BIO_new_mem_buf(cert_pem.data(), static_cast<int>(cert_pem.size())), BIO_free);
    if (!in) return {};
    std::unique_ptr<X509, decltype(&X509_free)> cert(
        PEM_read_bio_X509(in.get(), nullptr, nullptr, nullptr), X509_free);
    if (!cert) return {};
    unsigned char* der = nullptr;
    int len = i2d_X509(cert.get(), &der);
    if (len <= 0) return {};
    unsigned char digest[SHA256_DIGEST_LENGTH];
    SHA256(der, static_cast<size_t>(len), digest);
    OPENSSL_free(der);
    static const char kHex[] = "0123456789abcdef";
    std::string out;
    out.reserve(64);
    for (unsigned char b : digest) { out.push_back(kHex[b >> 4]); out.push_back(kHex[b & 0x0F]); }
    return out;
}

/// sha256-hex of an X509 handle's DER (for peers seen during a handshake).
inline std::string cert_fingerprint_from_x509(X509* cert) {
    if (!cert) return {};
    unsigned char* der = nullptr;
    int len = i2d_X509(cert, &der);
    if (len <= 0) return {};
    unsigned char digest[SHA256_DIGEST_LENGTH];
    SHA256(der, static_cast<size_t>(len), digest);
    OPENSSL_free(der);
    static const char kHex[] = "0123456789abcdef";
    std::string out;
    out.reserve(64);
    for (unsigned char b : digest) { out.push_back(kHex[b >> 4]); out.push_back(kHex[b & 0x0F]); }
    return out;
}

} // namespace chatproj
