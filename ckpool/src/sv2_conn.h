/*
 * Copyright 2026 Con Kolivas
 *
 * Per-connection SV2 state used by the connector (reassembly, Noise session).
 * Outbound encrypt MUST run on the owning sender shard only.
 */

#ifndef SV2_CONN_H
#define SV2_CONN_H

#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>

#include "sv2_noise.h"
#include "sv2_types.h"

struct sv2_conn {
    sv2_noise_session_t* noise;
    /* Recv reassembly buffer (ciphertext stream) */
    uint8_t* rx;
    size_t   rx_len;
    size_t   rx_cap;
    /* Hard ceiling for rx_cap growth (mining vs JD policy). */
    size_t rx_max;
    /* Handshake rate-limit bookkeeping (set by connector) */
    bool handshake_started;
    /* True while this conn holds a global in-flight handshake slot */
    bool hs_inflight;
};

struct sv2_conn* sv2_conn_new(const struct sv2_noise_server_keys* keys);
/* Override reassembly ceiling; use sv2_rx_max_for_payload() to size it. */
void sv2_conn_set_rx_max(struct sv2_conn* c, size_t rx_max);
void sv2_conn_free(struct sv2_conn* c);

/* Append socket bytes; may produce ciphertext reply (handshake) and/or
 * zero or more plaintext frames. Caller frees reply and each frame with dealloc.
 * Returns false on fatal protocol/crypto error (drop connection). */
bool sv2_conn_feed(
    struct sv2_conn* c,
    const uint8_t*   data,
    size_t           len,
    uint8_t**        reply,
    size_t*          reply_len,
    uint8_t***       frames,
    size_t**         frame_lens,
    size_t*          nframes);

/* Encrypt plaintext frame for send (shard thread only). */
bool sv2_conn_encrypt(struct sv2_conn* c, const uint8_t* plain, size_t plainlen, uint8_t** out, size_t* outlen);

bool sv2_conn_ready(const struct sv2_conn* c);

/* Global server keys (process-wide). */
bool                                sv2_init_server_keys(const char* authority_path, const char* static_path);
void                                sv2_clear_server_keys(void);
const struct sv2_noise_server_keys* sv2_get_server_keys(void);

/*
 * Handshake DoS: atomically check per-IP + global caps and reserve a slot
 * before expensive Noise keygen. On success the caller owns a global
 * in-flight slot and must call sv2_handshake_release() on complete or drop
 * (sv2_conn_free does this when hs_inflight is set).
 */
bool sv2_handshake_try_reserve(const char* address_name);
/* Release global in-flight handshake slot (complete or drop). */
void sv2_handshake_release(void);

#endif /* SV2_CONN_H */
