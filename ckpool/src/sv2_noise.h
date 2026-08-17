/*
 * Copyright 2026 Con Kolivas
 *
 * Stratum V2 Noise NX session (responder) + AEAD framing.
 *
 * CipherState encrypt/decrypt for a connection MUST only run on the connector
 * sender shard that owns the client (id % nsenders) for outbound, and the
 * single epoll reader for inbound — never from the stratifier.
 *
 * Nonce policy: no Noise transport rekey. When either
 * direction's AEAD counter approaches the 64-bit limit (see
 * SV2_NOISE_NONCE_WATERMARK in sv2_noise.c), encrypt/decrypt fails closed so
 * the connector or proxy drops the TCP session and re-handshakes.
 */

#ifndef SV2_NOISE_H
#define SV2_NOISE_H

#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>

/* Opaque Noise session (one per SV2 TCP connection). */
typedef struct sv2_noise_session sv2_noise_session_t;

/* Server static + authority material for responder handshake. */
struct sv2_noise_server_keys {
    uint8_t  static_sk[32];       /* server static private key */
    uint8_t  static_ellswift[64]; /* server static public (ElligatorSwift) */
    uint8_t  static_xonly[32];    /* server static x-only pub for cert */
    uint8_t  authority_sk[32];    /* authority private (signs cert) */
    uint8_t  authority_xonly[32]; /* authority public (published to miners) */
    uint32_t cert_valid_from;
    uint32_t cert_not_valid_after;
    uint16_t cert_version;
};

/* Load seckeys from files (64 hex chars) or generate if missing.
 * When path is non-NULL and file is absent, a new key is generated and
 * written mode 0600. Publish the logged authority x-only pubkey to miners. */
bool sv2_noise_load_server_keys(
    struct sv2_noise_server_keys* keys,
    const char*                   authority_path,
    const char*                   static_path,
    uint32_t                      valid_days);

void sv2_noise_server_keys_clear(struct sv2_noise_server_keys* keys);

/* Create responder session. Takes a copy of key material pointers (keys must
 * outlive the session or stay valid for handshake duration). */
sv2_noise_session_t* sv2_noise_session_new(const struct sv2_noise_server_keys* keys);

/* Create initiator (client) session for an SV2 upstream (ckproxy). The pool
 * authority x-only pubkey (from the mining URL path, spec 04 §4.7) is what the
 * server certificate must be signed by. Free with sv2_noise_session_free. */
sv2_noise_session_t* sv2_noise_client_session_new(const uint8_t authority_xonly[32]);

/* Initiator act 1: fill out[64] with our ephemeral EllSwift pubkey to send
 * first. Call once before sv2_noise_client_act2. */
bool sv2_noise_client_act1(sv2_noise_session_t* s, uint8_t out[64]);

/* Initiator act 2: process the 234-byte responder message (e, ee, encrypted
 * static, es, encrypted SIGNATURE_NOISE_MESSAGE). Verifies the server
 * certificate against the authority key and its validity window, then derives
 * transport CipherStates. Returns false on any failure (drop the connection).
 * After success use sv2_noise_encrypt_frame / sv2_noise_decrypt_frame. */
bool sv2_noise_client_act2(sv2_noise_session_t* s, const uint8_t* in, size_t inlen);

void sv2_noise_session_free(sv2_noise_session_t* s);

bool sv2_noise_handshake_complete(const sv2_noise_session_t* s);

/* Feed raw socket bytes into handshake. May produce reply bytes in *out
 * (caller frees with dealloc). Returns false on hard failure (drop conn). */
bool sv2_noise_handshake_read(sv2_noise_session_t* s, const uint8_t* in, size_t inlen, uint8_t** out, size_t* outlen);

/* After handshake: decrypt one full transport frame from in/inlen (consumes
 * a prefix of the buffer). On success *plain is ckalloc'd plaintext frame
 * (header+payload). Returns 0 ok, -1 need more data, -2 fatal AEAD/format. */
int sv2_noise_decrypt_frame(
    sv2_noise_session_t* s,
    const uint8_t*       in,
    size_t               inlen,
    size_t*              consumed,
    uint8_t**            plain,
    size_t*              plainlen);

/* Encrypt plaintext SV2 frame (header+payload) for send. *out is ckalloc'd.
 * MUST be called only on the connection's owning sender shard thread.
 * Returns false on AEAD failure or send-nonce watermark (caller drops conn). */
bool sv2_noise_encrypt_frame(
    sv2_noise_session_t* s,
    const uint8_t*       plain,
    size_t               plainlen,
    uint8_t**            out,
    size_t*              outlen);

/* Times transport AEAD was refused due to the nonce watermark (process total). */
uint64_t sv2_noise_nonce_watermark_hits(void);

/* Test helper: set transport nonce counters after handshake (unit tests only). */
void sv2_noise_test_set_transport_nonces(sv2_noise_session_t* s, uint64_t send_n, uint64_t recv_n);

/* Base58Check encode authority pubkey for URL path (spec 04 §4.7). Caller frees.
 * Form: base58check( LE_u16(1) || xonly_pubkey[32] ) */
char* sv2_noise_authority_pubkey_b58(const struct sv2_noise_server_keys* keys);

/* Test helper: encode raw 32-byte x-only pubkey the same way. Caller frees. */
char* sv2_noise_authority_xonly_to_b58(const uint8_t xonly[32]);

/* Decode a base58check authority pubkey (from a stratum2+tcp URL path) into
 * its 32-byte x-only form. False on bad length / version / checksum. */
bool sv2_noise_authority_b58_to_xonly(const char* b58, uint8_t xonly[32]);

#endif /* SV2_NOISE_H */
