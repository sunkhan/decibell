# Theme A — asymmetric JWTs, separate community secret, cert pinning

Status: implemented 2026-08-22 (central, community, native client, Electron main).
Closes the top item in `CODE_REVIEW.md`. Old-client compatibility not kept.

## The problem

JWTs were HS256: the signing key *is* the verify key, and every community
server held it (to verify user tokens) and also reused it as the
community↔central shared secret. So any community operator — or one leaked
config — could forge a token for any user on any server, and, combined with
`verify_none` on every TLS link, an on-path attacker could MITM the
handshake, read the secret from the first heartbeat, and mint tokens.

## What changed

### 1. Ed25519 JWTs (central signs, community verifies)
- Central generates an Ed25519 keypair on first boot at
  `DECIBELL_JWT_KEY_FILE` (default `jwt_ed25519.pem`), writing the public
  half to `<file>.pub` (`src/common/ed25519_keys.hpp`).
- Tokens are `EdDSA`-signed with a `uid` claim.
- Community servers verify with the **public key only**
  (`DECIBELL_JWT_PUBLIC_KEY_FILE`) — verify, never mint. Attachment HTTP too.

### 2. Separate community↔central secret
- `DECIBELL_COMMUNITY_SECRET` (both sides) authenticates community→central
  packets (heartbeat, invite / membership / picture sync); no longer a
  signing key. `DECIBELL_JWT_SECRET` retired (both servers log a notice).

### 3. Stable `uid`
- `users.uid` (BIGSERIAL) on central, carried as the `uid` claim.
- Community `members.uid` / `bans.uid`; `is_banned` matches username OR uid,
  so a ban survives a username change/reuse. Members back-fill on next auth.

### 4. Certificate pinning (replaces verify_none everywhere)
Identity is sha256 of the leaf certificate (self-signed → WebPKI is moot).
- Community reports `sha256(server.crt DER)` in heartbeats; central stores it
  (`community_servers.cert_fingerprint`) and serves it in
  `CommunityServerInfo.cert_fingerprint` (directory, memberships) and
  `InviteResolveResponse.cert_fingerprint`.
- **Native** (`net/pins.rs` + `net/tls.rs`): central = TOFU (`cert_pins.json`);
  community = pinned to central's reported fingerprint when known, else TOFU.
  Handshake signature still verified. Mismatch →
  `CERT_MISMATCH:<host>:<port>:<fp>`.
- **Community→central**: Boost verify callback pins central's cert
  (`DECIBELL_CENTRAL_CERT_FINGERPRINT` or TOFU in `server_meta`).
- **Electron main**: `setCertificateVerifyProc` / `certificate-error` pin the
  attachment HTTPS host to the fingerprint native accepted (via
  `attachmentRegistry`); unknown hosts fall through to Chromium (Sentry, updater).
- **Re-trust**: `CERT_MISMATCH` → `CertMismatchModal` → `trust_certificate`
  (native `pins::retrust`) + retry; servers can pin explicitly via env.

## Operations
- Central generates its key on first boot; copy `jwt_ed25519.pem.pub` to each
  community (`DECIBELL_JWT_PUBLIC_KEY_FILE`). Choose one
  `DECIBELL_COMMUNITY_SECRET` for central + every community.
- Community cert rotation: clients hit the re-trust modal, or pick up the new
  fingerprint automatically after the next heartbeat + directory refresh.
- Central cert rotation: bump `DECIBELL_CENTRAL_CERT_FINGERPRINT` on
  communities (or clear their meta); clients hit the re-trust modal.

## Deferred
Public-key fetch from central at startup (rotation without redeploy); mTLS;
short-lived tokens + refresh.
