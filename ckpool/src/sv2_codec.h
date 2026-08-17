/*
 * Copyright 2026 Con Kolivas
 *
 * Stratum V2 binary encode/decode. Untrusted input: all lengths bounds-checked.
 */

#ifndef SV2_CODEC_H
#define SV2_CODEC_H

#include "sv2_types.h"

/* Decode 6-byte plaintext frame header. Returns false on truncated input. */
bool sv2_decode_header(const uint8_t* buf, size_t len, struct sv2_frame* out);

/* Encode 6-byte plaintext frame header into buf[6]. */
void sv2_encode_header(uint8_t* buf, uint16_t extension_type, uint8_t msg_type, uint32_t msg_length);

/* Helpers: little-endian readers/writers (return false if short buffer) */
bool sv2_read_u8(const uint8_t** p, const uint8_t* end, uint8_t* v);
bool sv2_read_u16(const uint8_t** p, const uint8_t* end, uint16_t* v);
bool sv2_read_u24(const uint8_t** p, const uint8_t* end, uint32_t* v);
bool sv2_read_u32(const uint8_t** p, const uint8_t* end, uint32_t* v);
bool sv2_read_u64(const uint8_t** p, const uint8_t* end, uint64_t* v);
bool sv2_read_f32(const uint8_t** p, const uint8_t* end, float* v);
bool sv2_read_u256(const uint8_t** p, const uint8_t* end, uint8_t out[32]);
bool sv2_read_str0_255(const uint8_t** p, const uint8_t* end, char* out, size_t outsz);
bool sv2_read_b0_32(const uint8_t** p, const uint8_t* end, uint8_t* out, uint8_t* outlen);
bool sv2_read_b0_64k(const uint8_t** p, const uint8_t* end, uint8_t** out, uint16_t* outlen);
bool sv2_read_bool(const uint8_t** p, const uint8_t* end, bool* v);
bool sv2_read_option_u32(const uint8_t** p, const uint8_t* end, bool* present, uint32_t* v);

void sv2_write_u8(uint8_t** p, uint8_t v);
void sv2_write_u16(uint8_t** p, uint16_t v);
void sv2_write_u24(uint8_t** p, uint32_t v);
void sv2_write_u32(uint8_t** p, uint32_t v);
void sv2_write_u64(uint8_t** p, uint64_t v);
void sv2_write_f32(uint8_t** p, float v);
void sv2_write_u256(uint8_t** p, const uint8_t in[32]);
void sv2_write_str0_255(uint8_t** p, const char* s);
void sv2_write_b0_32(uint8_t** p, const uint8_t* data, uint8_t len);
void sv2_write_b0_64k(uint8_t** p, const uint8_t* data, uint16_t len);
void sv2_write_bool(uint8_t** p, bool v);
void sv2_write_option_u32(uint8_t** p, bool present, uint32_t v);

/* Decode client→server messages used in Phase 1. Returns false on malformed. */
bool sv2_decode_setup_connection(const uint8_t* payload, uint32_t len, struct sv2_setup_connection* out);
bool sv2_decode_open_standard_channel(const uint8_t* payload, uint32_t len, struct sv2_open_standard_channel* out);
bool sv2_decode_open_extended_channel(const uint8_t* payload, uint32_t len, struct sv2_open_extended_channel* out);
bool sv2_decode_submit_shares_standard(const uint8_t* payload, uint32_t len, struct sv2_submit_shares_standard* out);
bool sv2_decode_submit_shares_extended(const uint8_t* payload, uint32_t len, struct sv2_submit_shares_extended* out);
bool sv2_decode_close_channel(const uint8_t* payload, uint32_t len, struct sv2_close_channel* out);

/* Encode server→client messages. Writes into buf of size bufsz; sets *outlen.
 * Returns false if buffer too small. */
bool sv2_encode_setup_connection_success(
    uint8_t*                                   buf,
    size_t                                     bufsz,
    size_t*                                    outlen,
    const struct sv2_setup_connection_success* m);
bool sv2_encode_setup_connection_error(
    uint8_t*                                 buf,
    size_t                                   bufsz,
    size_t*                                  outlen,
    const struct sv2_setup_connection_error* m);
bool sv2_encode_open_standard_channel_success(
    uint8_t*                                        buf,
    size_t                                          bufsz,
    size_t*                                         outlen,
    const struct sv2_open_standard_channel_success* m);
bool sv2_encode_open_extended_channel_success(
    uint8_t*                                        buf,
    size_t                                          bufsz,
    size_t*                                         outlen,
    const struct sv2_open_extended_channel_success* m);
bool sv2_encode_open_channel_error(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_open_channel_error* m);
bool sv2_encode_new_mining_job(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_new_mining_job* m);
bool sv2_encode_set_new_prev_hash(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_set_new_prev_hash* m);
bool sv2_encode_set_target(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_set_target* m);
bool sv2_encode_submit_shares_success(
    uint8_t*                                buf,
    size_t                                  bufsz,
    size_t*                                 outlen,
    const struct sv2_submit_shares_success* m);
bool sv2_encode_submit_shares_error(
    uint8_t*                              buf,
    size_t                                bufsz,
    size_t*                               outlen,
    const struct sv2_submit_shares_error* m);
bool sv2_encode_update_channel_error(
    uint8_t*                               buf,
    size_t                                 bufsz,
    size_t*                                outlen,
    const struct sv2_update_channel_error* m);
bool sv2_encode_new_extended_mining_job(
    uint8_t*                                  buf,
    size_t                                    bufsz,
    size_t*                                   outlen,
    const struct sv2_new_extended_mining_job* m);

/* Job Declaration Protocol (Phase 2) */
bool sv2_decode_allocate_mining_job_token(
    const uint8_t*                        payload,
    uint32_t                              len,
    struct sv2_allocate_mining_job_token* out);
bool sv2_encode_allocate_mining_job_token_success(
    uint8_t*                                            buf,
    size_t                                              bufsz,
    size_t*                                             outlen,
    const struct sv2_allocate_mining_job_token_success* m);
bool sv2_decode_declare_mining_job(const uint8_t* payload, uint32_t len, struct sv2_declare_mining_job* out);
/* Free owned buffers from a successful sv2_decode_declare_mining_job. */
void sv2_declare_mining_job_free(struct sv2_declare_mining_job* m);
bool sv2_encode_declare_mining_job_success(
    uint8_t*                                     buf,
    size_t                                       bufsz,
    size_t*                                      outlen,
    const struct sv2_declare_mining_job_success* m);
bool sv2_encode_declare_mining_job_error(
    uint8_t*                                   buf,
    size_t                                     bufsz,
    size_t*                                    outlen,
    const struct sv2_declare_mining_job_error* m);
bool sv2_encode_provide_missing_transactions(
    uint8_t*                                       buf,
    size_t                                         bufsz,
    size_t*                                        outlen,
    const struct sv2_provide_missing_transactions* m);
bool sv2_decode_provide_missing_transactions_success(
    const uint8_t*                                   payload,
    uint32_t                                         len,
    struct sv2_provide_missing_transactions_success* out);
void sv2_provide_missing_tx_success_free(struct sv2_provide_missing_transactions_success* m);

bool sv2_decode_set_custom_mining_job(const uint8_t* payload, uint32_t len, struct sv2_set_custom_mining_job* out);
void sv2_set_custom_mining_job_free(struct sv2_set_custom_mining_job* m);
bool sv2_encode_set_custom_mining_job_success(
    uint8_t*                                        buf,
    size_t                                          bufsz,
    size_t*                                         outlen,
    const struct sv2_set_custom_mining_job_success* m);
bool sv2_encode_set_custom_mining_job_error(
    uint8_t*                                      buf,
    size_t                                        bufsz,
    size_t*                                       outlen,
    const struct sv2_set_custom_mining_job_error* m);
bool sv2_decode_push_solution(const uint8_t* payload, uint32_t len, struct sv2_push_solution* out);

/* --- Client-side codec (ckproxy SV2 upstream) --- */

/* Encode client→server messages. */
bool sv2_encode_setup_connection(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_setup_connection* m);
bool sv2_encode_open_extended_channel(
    uint8_t*                                buf,
    size_t                                  bufsz,
    size_t*                                 outlen,
    const struct sv2_open_extended_channel* m);
bool sv2_encode_submit_shares_extended(
    uint8_t*                                 buf,
    size_t                                   bufsz,
    size_t*                                  outlen,
    const struct sv2_submit_shares_extended* m);
bool sv2_encode_update_channel(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_update_channel* m);

/* Decode server→client messages (untrusted: all lengths bounds-checked). */
bool sv2_decode_setup_connection_success(
    const uint8_t*                       payload,
    uint32_t                             len,
    struct sv2_setup_connection_success* out);
bool sv2_decode_setup_connection_error(const uint8_t* payload, uint32_t len, struct sv2_setup_connection_error* out);
bool sv2_decode_open_extended_channel_success(
    const uint8_t*                            payload,
    uint32_t                                  len,
    struct sv2_open_extended_channel_success* out);
bool sv2_decode_open_channel_error(const uint8_t* payload, uint32_t len, struct sv2_open_channel_error* out);
bool sv2_decode_new_extended_mining_job(const uint8_t* payload, uint32_t len, struct sv2_new_extended_mining_job* out);
/* Free owned buffers from a successful sv2_decode_new_extended_mining_job. */
void sv2_new_extended_mining_job_free(struct sv2_new_extended_mining_job* m);
bool sv2_decode_set_new_prev_hash(const uint8_t* payload, uint32_t len, struct sv2_set_new_prev_hash* out);
bool sv2_decode_set_target(const uint8_t* payload, uint32_t len, struct sv2_set_target* out);
bool sv2_decode_set_extranonce_prefix(const uint8_t* payload, uint32_t len, struct sv2_set_extranonce_prefix* out);
bool sv2_decode_submit_shares_success(const uint8_t* payload, uint32_t len, struct sv2_submit_shares_success* out);
bool sv2_decode_submit_shares_error(const uint8_t* payload, uint32_t len, struct sv2_submit_shares_error* out);
bool sv2_decode_reconnect(const uint8_t* payload, uint32_t len, struct sv2_reconnect* out);

/* --- Client-side Job Declaration codec (ckproxy as JDC) --- */

/*
 * Buffer sizes that always suffice for the fixed-shape client JD messages;
 * the variable ones have an _encoded_size() helper to size a heap buffer.
 */
#define SV2_ALLOCATE_TOKEN_MAX_BYTES (1 + SV2_MAX_STR_LEN + 4)
#define SV2_PUSH_SOLUTION_MAX_BYTES (1 + SV2_MAX_B0_32 + 32 + 16)

bool sv2_encode_allocate_mining_job_token(
    uint8_t*                                    buf,
    size_t                                      bufsz,
    size_t*                                     outlen,
    const struct sv2_allocate_mining_job_token* m);

/*
 * NOTE: out->coinbase_tx_outputs points *into* payload — it is valid only
 * while that buffer lives, so copy the payout script if it is retained.
 */
bool sv2_decode_allocate_mining_job_token_success(
    const uint8_t*                                payload,
    uint32_t                                      len,
    struct sv2_allocate_mining_job_token_success* out);

/* Size a buffer for sv2_encode_declare_mining_job (wtxid list can reach
 * SV2_MAX_JD_TXNS * 32 bytes, so this never fits on the stack). */
size_t sv2_declare_mining_job_encoded_size(const struct sv2_declare_mining_job* m);
bool sv2_encode_declare_mining_job(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_declare_mining_job* m);

bool sv2_decode_declare_mining_job_success(
    const uint8_t*                         payload,
    uint32_t                               len,
    struct sv2_declare_mining_job_success* out);
/* NOTE: out->error_details points into payload (see above). */
bool sv2_decode_declare_mining_job_error(
    const uint8_t*                       payload,
    uint32_t                             len,
    struct sv2_declare_mining_job_error* out);

/* Owns unknown_tx_position_list on success — free with the call below. */
bool sv2_decode_provide_missing_transactions(
    const uint8_t*                           payload,
    uint32_t                                 len,
    struct sv2_provide_missing_transactions* out);
void sv2_provide_missing_transactions_free(struct sv2_provide_missing_transactions* m);

/* Reply carrying full transaction bytes: size it, then encode into a heap
 * buffer. Refuses anything over SV2_MAX_JD_PAYLOAD. */
size_t sv2_provide_missing_tx_success_encoded_size(const struct sv2_provide_missing_transactions_success* m);
bool   sv2_encode_provide_missing_transactions_success(
      uint8_t*                                               buf,
      size_t                                                 bufsz,
      size_t*                                                outlen,
      const struct sv2_provide_missing_transactions_success* m);

size_t sv2_set_custom_mining_job_encoded_size(const struct sv2_set_custom_mining_job* m);
bool   sv2_encode_set_custom_mining_job(
      uint8_t*                                buf,
      size_t                                  bufsz,
      size_t*                                 outlen,
      const struct sv2_set_custom_mining_job* m);
bool sv2_decode_set_custom_mining_job_success(
    const uint8_t*                            payload,
    uint32_t                                  len,
    struct sv2_set_custom_mining_job_success* out);
bool sv2_decode_set_custom_mining_job_error(
    const uint8_t*                          payload,
    uint32_t                                len,
    struct sv2_set_custom_mining_job_error* out);

bool sv2_encode_push_solution(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_push_solution* m);

/* Build full plaintext frame (header + payload) into *out (ckalloc'd). */
bool sv2_build_frame(
    uint16_t       extension_type,
    uint8_t        msg_type,
    const uint8_t* payload,
    uint32_t       payload_len,
    uint8_t**      out,
    size_t*        outlen);

#endif /* SV2_CODEC_H */
