/*
 * Copyright 2026 Con Kolivas
 *
 * Stratum V2 binary codec. All untrusted lengths are bounds-checked.
 */

#include "config.h"

#include <string.h>

#include "libckpool.h"
#include "sv2_codec.h"

bool sv2_decode_header(const uint8_t* buf, size_t len, struct sv2_frame* out) {
    if (!buf || !out || len < SV2_FRAME_HEADER_LEN)
        return false;
    out->extension_type = (uint16_t)buf[0] | ((uint16_t)buf[1] << 8);
    out->msg_type = buf[2];
    out->msg_length = (uint32_t)buf[3] | ((uint32_t)buf[4] << 8) | ((uint32_t)buf[5] << 16);
    out->payload = NULL;
    out->owned = NULL;
    if (out->msg_length > SV2_MAX_PAYLOAD)
        return false;
    return true;
}

void sv2_encode_header(uint8_t* buf, uint16_t extension_type, uint8_t msg_type, uint32_t msg_length) {
    buf[0] = (uint8_t)(extension_type & 0xff);
    buf[1] = (uint8_t)((extension_type >> 8) & 0xff);
    buf[2] = msg_type;
    buf[3] = (uint8_t)(msg_length & 0xff);
    buf[4] = (uint8_t)((msg_length >> 8) & 0xff);
    buf[5] = (uint8_t)((msg_length >> 16) & 0xff);
}

/* Prefer size-based remaining check over *p + n > end (pointer overflow UB). */
static inline bool have_n(const uint8_t* p, const uint8_t* end, size_t n) {
    return n <= (size_t)(end - p);
}

bool sv2_read_u8(const uint8_t** p, const uint8_t* end, uint8_t* v) {
    if (!have_n(*p, end, 1))
        return false;
    *v = *(*p)++;
    return true;
}

bool sv2_read_u16(const uint8_t** p, const uint8_t* end, uint16_t* v) {
    if (!have_n(*p, end, 2))
        return false;
    *v = (uint16_t)(*p)[0] | ((uint16_t)(*p)[1] << 8);
    *p += 2;
    return true;
}

bool sv2_read_u24(const uint8_t** p, const uint8_t* end, uint32_t* v) {
    if (!have_n(*p, end, 3))
        return false;
    *v = (uint32_t)(*p)[0] | ((uint32_t)(*p)[1] << 8) | ((uint32_t)(*p)[2] << 16);
    *p += 3;
    return true;
}

bool sv2_read_u32(const uint8_t** p, const uint8_t* end, uint32_t* v) {
    if (!have_n(*p, end, 4))
        return false;
    *v = (uint32_t)(*p)[0] | ((uint32_t)(*p)[1] << 8) | ((uint32_t)(*p)[2] << 16) | ((uint32_t)(*p)[3] << 24);
    *p += 4;
    return true;
}

bool sv2_read_u64(const uint8_t** p, const uint8_t* end, uint64_t* v) {
    if (!have_n(*p, end, 8))
        return false;
    *v = (uint64_t)(*p)[0] | ((uint64_t)(*p)[1] << 8) | ((uint64_t)(*p)[2] << 16) | ((uint64_t)(*p)[3] << 24) |
         ((uint64_t)(*p)[4] << 32) | ((uint64_t)(*p)[5] << 40) | ((uint64_t)(*p)[6] << 48) | ((uint64_t)(*p)[7] << 56);
    *p += 8;
    return true;
}

bool sv2_read_f32(const uint8_t** p, const uint8_t* end, float* v) {
    uint32_t u;

    if (!sv2_read_u32(p, end, &u))
        return false;
    memcpy(v, &u, sizeof(float));
    return true;
}

bool sv2_read_u256(const uint8_t** p, const uint8_t* end, uint8_t out[32]) {
    if (!have_n(*p, end, 32))
        return false;
    memcpy(out, *p, 32);
    *p += 32;
    return true;
}

bool sv2_read_str0_255(const uint8_t** p, const uint8_t* end, char* out, size_t outsz) {
    uint8_t n;

    if (!sv2_read_u8(p, end, &n))
        return false;
    if (!have_n(*p, end, n))
        return false;
    if ((size_t)n + 1 > outsz)
        return false;
    memcpy(out, *p, n);
    out[n] = '\0';
    *p += n;
    return true;
}

bool sv2_read_b0_32(const uint8_t** p, const uint8_t* end, uint8_t* out, uint8_t* outlen) {
    uint8_t n;

    if (!sv2_read_u8(p, end, &n))
        return false;
    if (n > SV2_MAX_B0_32)
        return false;
    if (!have_n(*p, end, n))
        return false;
    memcpy(out, *p, n);
    *outlen = n;
    *p += n;
    return true;
}

bool sv2_read_b0_64k(const uint8_t** p, const uint8_t* end, uint8_t** out, uint16_t* outlen) {
    uint16_t n;

    /* Caller owns *out on success; on failure leaves *out untouched / NULL. */
    *out = NULL;
    *outlen = 0;
    if (!sv2_read_u16(p, end, &n))
        return false;
    if (!have_n(*p, end, n))
        return false;
    if (n) {
        *out = ckalloc(n);
        memcpy(*out, *p, n);
    }
    *outlen = n;
    *p += n;
    return true;
}

bool sv2_read_bool(const uint8_t** p, const uint8_t* end, bool* v) {
    uint8_t b;

    if (!sv2_read_u8(p, end, &b))
        return false;
    *v = (b & 1) != 0;
    return true;
}

bool sv2_read_option_u32(const uint8_t** p, const uint8_t* end, bool* present, uint32_t* v) {
    uint8_t n;

    /* OPTION[T] as SEQ0_1: 1-byte count 0 or 1 */
    if (!sv2_read_u8(p, end, &n))
        return false;
    if (n > 1)
        return false;
    *present = n == 1;
    if (!*present) {
        *v = 0;
        return true;
    }
    return sv2_read_u32(p, end, v);
}

void sv2_write_u8(uint8_t** p, uint8_t v) {
    *(*p)++ = v;
}

void sv2_write_u16(uint8_t** p, uint16_t v) {
    *(*p)++ = (uint8_t)(v & 0xff);
    *(*p)++ = (uint8_t)((v >> 8) & 0xff);
}

void sv2_write_u24(uint8_t** p, uint32_t v) {
    *(*p)++ = (uint8_t)(v & 0xff);
    *(*p)++ = (uint8_t)((v >> 8) & 0xff);
    *(*p)++ = (uint8_t)((v >> 16) & 0xff);
}

void sv2_write_u32(uint8_t** p, uint32_t v) {
    *(*p)++ = (uint8_t)(v & 0xff);
    *(*p)++ = (uint8_t)((v >> 8) & 0xff);
    *(*p)++ = (uint8_t)((v >> 16) & 0xff);
    *(*p)++ = (uint8_t)((v >> 24) & 0xff);
}

void sv2_write_u64(uint8_t** p, uint64_t v) {
    int i;

    for (i = 0; i < 8; i++)
        *(*p)++ = (uint8_t)((v >> (8 * i)) & 0xff);
}

void sv2_write_f32(uint8_t** p, float v) {
    uint32_t u;

    memcpy(&u, &v, sizeof(u));
    sv2_write_u32(p, u);
}

void sv2_write_u256(uint8_t** p, const uint8_t in[32]) {
    memcpy(*p, in, 32);
    *p += 32;
}

void sv2_write_str0_255(uint8_t** p, const char* s) {
    size_t n = s ? strlen(s) : 0;

    if (n > SV2_MAX_STR_LEN)
        n = SV2_MAX_STR_LEN;
    sv2_write_u8(p, (uint8_t)n);
    if (n) {
        memcpy(*p, s, n);
        *p += n;
    }
}

void sv2_write_b0_32(uint8_t** p, const uint8_t* data, uint8_t len) {
    if (len > SV2_MAX_B0_32)
        len = SV2_MAX_B0_32;
    sv2_write_u8(p, len);
    if (len) {
        memcpy(*p, data, len);
        *p += len;
    }
}

void sv2_write_b0_64k(uint8_t** p, const uint8_t* data, uint16_t len) {
    sv2_write_u16(p, len);
    if (len) {
        memcpy(*p, data, len);
        *p += len;
    }
}

void sv2_write_bool(uint8_t** p, bool v) {
    sv2_write_u8(p, v ? 1 : 0);
}

void sv2_write_option_u32(uint8_t** p, bool present, uint32_t v) {
    if (!present) {
        sv2_write_u8(p, 0);
        return;
    }
    sv2_write_u8(p, 1);
    sv2_write_u32(p, v);
}

bool sv2_decode_setup_connection(const uint8_t* payload, uint32_t len, struct sv2_setup_connection* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u8(&p, end, &out->protocol) || !sv2_read_u16(&p, end, &out->min_version) ||
        !sv2_read_u16(&p, end, &out->max_version) || !sv2_read_u32(&p, end, &out->flags) ||
        !sv2_read_str0_255(&p, end, out->endpoint_host, sizeof(out->endpoint_host)) ||
        !sv2_read_u16(&p, end, &out->endpoint_port) || !sv2_read_str0_255(&p, end, out->vendor, sizeof(out->vendor)) ||
        !sv2_read_str0_255(&p, end, out->hardware_version, sizeof(out->hardware_version)) ||
        !sv2_read_str0_255(&p, end, out->firmware, sizeof(out->firmware)) ||
        !sv2_read_str0_255(&p, end, out->device_id, sizeof(out->device_id)))
        return false;
    return true;
}

bool sv2_decode_open_standard_channel(const uint8_t* payload, uint32_t len, struct sv2_open_standard_channel* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->request_id) ||
        !sv2_read_str0_255(&p, end, out->user_identity, sizeof(out->user_identity)) ||
        !sv2_read_f32(&p, end, &out->nominal_hash_rate) || !sv2_read_u256(&p, end, out->max_target))
        return false;
    return true;
}

bool sv2_decode_open_extended_channel(const uint8_t* payload, uint32_t len, struct sv2_open_extended_channel* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->request_id) ||
        !sv2_read_str0_255(&p, end, out->user_identity, sizeof(out->user_identity)) ||
        !sv2_read_f32(&p, end, &out->nominal_hash_rate) || !sv2_read_u256(&p, end, out->max_target) ||
        !sv2_read_u16(&p, end, &out->min_extranonce_size))
        return false;
    return true;
}

bool sv2_decode_submit_shares_standard(const uint8_t* payload, uint32_t len, struct sv2_submit_shares_standard* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) || !sv2_read_u32(&p, end, &out->sequence_number) ||
        !sv2_read_u32(&p, end, &out->job_id) || !sv2_read_u32(&p, end, &out->nonce) ||
        !sv2_read_u32(&p, end, &out->ntime) || !sv2_read_u32(&p, end, &out->version))
        return false;
    return true;
}

bool sv2_decode_submit_shares_extended(const uint8_t* payload, uint32_t len, struct sv2_submit_shares_extended* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->base.channel_id) || !sv2_read_u32(&p, end, &out->base.sequence_number) ||
        !sv2_read_u32(&p, end, &out->base.job_id) || !sv2_read_u32(&p, end, &out->base.nonce) ||
        !sv2_read_u32(&p, end, &out->base.ntime) || !sv2_read_u32(&p, end, &out->base.version) ||
        !sv2_read_b0_32(&p, end, out->extranonce, &out->extranonce_len))
        return false;
    return true;
}

bool sv2_decode_close_channel(const uint8_t* payload, uint32_t len, struct sv2_close_channel* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) ||
        !sv2_read_str0_255(&p, end, out->reason_code, sizeof(out->reason_code)))
        return false;
    return true;
}

static bool finish_encode(uint8_t* start, uint8_t* p, size_t bufsz, size_t* outlen) {
    size_t n = (size_t)(p - start);

    if (n > bufsz)
        return false;
    *outlen = n;
    return true;
}

bool sv2_encode_setup_connection_success(
    uint8_t*                                   buf,
    size_t                                     bufsz,
    size_t*                                    outlen,
    const struct sv2_setup_connection_success* m) {
    uint8_t* p = buf;

    if (bufsz < 6)
        return false;
    sv2_write_u16(&p, m->used_version);
    sv2_write_u32(&p, m->flags);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_setup_connection_error(
    uint8_t*                                 buf,
    size_t                                   bufsz,
    size_t*                                  outlen,
    const struct sv2_setup_connection_error* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 1 + strlen(m->error_code);

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->flags);
    sv2_write_str0_255(&p, m->error_code);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_open_standard_channel_success(
    uint8_t*                                        buf,
    size_t                                          bufsz,
    size_t*                                         outlen,
    const struct sv2_open_standard_channel_success* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 4 + 32 + 1 + m->extranonce_prefix_len + 4;

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->request_id);
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u256(&p, m->target);
    sv2_write_b0_32(&p, m->extranonce_prefix, m->extranonce_prefix_len);
    sv2_write_u32(&p, m->group_channel_id);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_open_extended_channel_success(
    uint8_t*                                        buf,
    size_t                                          bufsz,
    size_t*                                         outlen,
    const struct sv2_open_extended_channel_success* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 4 + 32 + 2 + 1 + m->extranonce_prefix_len + 4;

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->request_id);
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u256(&p, m->target);
    sv2_write_u16(&p, m->extranonce_size);
    sv2_write_b0_32(&p, m->extranonce_prefix, m->extranonce_prefix_len);
    sv2_write_u32(&p, m->group_channel_id);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_open_channel_error(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_open_channel_error* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 1 + strlen(m->error_code);

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->request_id);
    sv2_write_str0_255(&p, m->error_code);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_new_mining_job(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_new_mining_job* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 4 + 1 + (m->min_ntime_present ? 4 : 0) + 4 + 32;

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u32(&p, m->job_id);
    sv2_write_option_u32(&p, m->min_ntime_present, m->min_ntime);
    sv2_write_u32(&p, m->version);
    sv2_write_u256(&p, m->merkle_root);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_set_new_prev_hash(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_set_new_prev_hash* m) {
    uint8_t* p = buf;

    if (bufsz < 4 + 4 + 32 + 4 + 4)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u32(&p, m->job_id);
    sv2_write_u256(&p, m->prev_hash);
    sv2_write_u32(&p, m->min_ntime);
    sv2_write_u32(&p, m->nbits);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_set_target(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_set_target* m) {
    uint8_t* p = buf;

    if (bufsz < 4 + 32)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u256(&p, m->maximum_target);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_submit_shares_success(
    uint8_t*                                buf,
    size_t                                  bufsz,
    size_t*                                 outlen,
    const struct sv2_submit_shares_success* m) {
    uint8_t* p = buf;

    if (bufsz < 4 + 4 + 4 + 8)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u32(&p, m->last_sequence_number);
    sv2_write_u32(&p, m->new_submits_accepted_count);
    sv2_write_u64(&p, m->new_shares_sum);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_submit_shares_error(
    uint8_t*                              buf,
    size_t                                bufsz,
    size_t*                               outlen,
    const struct sv2_submit_shares_error* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 4 + 1 + strlen(m->error_code);

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u32(&p, m->sequence_number);
    sv2_write_str0_255(&p, m->error_code);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_update_channel_error(
    uint8_t*                               buf,
    size_t                                 bufsz,
    size_t*                                outlen,
    const struct sv2_update_channel_error* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 1 + strlen(m->error_code);

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_str0_255(&p, m->error_code);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_new_extended_mining_job(
    uint8_t*                                  buf,
    size_t                                    bufsz,
    size_t*                                   outlen,
    const struct sv2_new_extended_mining_job* m) {
    uint8_t *p = buf, *start = buf;
    size_t   need;
    uint8_t  i;

    if (m->merkle_count > SV2_MAX_MERKLE_PATH)
        return false;
    need = 4 + 4 + 1 + (m->min_ntime_present ? 4 : 0) + 4 + 1 + 1 + (size_t)m->merkle_count * 32 + 2 +
           m->coinbase_tx_prefix_len + 2 + m->coinbase_tx_suffix_len;
    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u32(&p, m->job_id);
    sv2_write_option_u32(&p, m->min_ntime_present, m->min_ntime);
    sv2_write_u32(&p, m->version);
    sv2_write_bool(&p, m->version_rolling_allowed);
    /* SEQ0_255[U256] — clamped to SV2_MAX_MERKLE_PATH */
    sv2_write_u8(&p, m->merkle_count);
    for (i = 0; i < m->merkle_count; i++)
        sv2_write_u256(&p, m->merkle_path[i]);
    sv2_write_b0_64k(&p, m->coinbase_tx_prefix, m->coinbase_tx_prefix_len);
    sv2_write_b0_64k(&p, m->coinbase_tx_suffix, m->coinbase_tx_suffix_len);
    return finish_encode(start, p, bufsz, outlen);
}

/* B0_255: U8 length + bytes (token fields). */
static bool sv2_read_b0_255(const uint8_t** p, const uint8_t* end, uint8_t* out, size_t outsz, uint8_t* outlen) {
    uint8_t n;

    if (!sv2_read_u8(p, end, &n))
        return false;
    /* n cannot exceed 255 by type, but check the destination anyway so a
     * shrunk field can never be overflowed silently. */
    if (unlikely((size_t)n > outsz))
        return false;
    if (!have_n(*p, end, n))
        return false;
    memcpy(out, *p, n);
    *outlen = n;
    *p += n;
    return true;
}

static void sv2_write_b0_255(uint8_t** p, const uint8_t* data, uint8_t len) {
    sv2_write_u8(p, len);
    if (len) {
        memcpy(*p, data, len);
        *p += len;
    }
}

bool sv2_decode_allocate_mining_job_token(
    const uint8_t*                        payload,
    uint32_t                              len,
    struct sv2_allocate_mining_job_token* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_str0_255(&p, end, out->user_identifier, sizeof(out->user_identifier)) ||
        !sv2_read_u32(&p, end, &out->request_id))
        return false;
    return true;
}

bool sv2_encode_allocate_mining_job_token_success(
    uint8_t*                                            buf,
    size_t                                              bufsz,
    size_t*                                             outlen,
    const struct sv2_allocate_mining_job_token_success* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 1 + m->mining_job_token_len + 2 + m->coinbase_tx_outputs_len;

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->request_id);
    sv2_write_b0_255(&p, m->mining_job_token, m->mining_job_token_len);
    sv2_write_b0_64k(&p, m->coinbase_tx_outputs, m->coinbase_tx_outputs_len);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_decode_declare_mining_job(const uint8_t* payload, uint32_t len, struct sv2_declare_mining_job* out) {
    const uint8_t *p = payload, *end = payload + len;
    uint16_t       wcount = 0;
    uint16_t       i;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->request_id) ||
        !sv2_read_b0_255(&p, end, out->mining_job_token, sizeof(out->mining_job_token), &out->mining_job_token_len) ||
        !sv2_read_u32(&p, end, &out->version) ||
        !sv2_read_b0_64k(&p, end, &out->coinbase_tx_prefix, &out->coinbase_tx_prefix_len) ||
        !sv2_read_b0_64k(&p, end, &out->coinbase_tx_suffix, &out->coinbase_tx_suffix_len))
        goto fail;

    /* SEQ0_64K[U256] */
    if (!sv2_read_u16(&p, end, &wcount))
        goto fail;
    if (wcount > SV2_MAX_JD_TXNS)
        goto fail;
    out->wtxid_count = wcount;
    if (wcount) {
        size_t bytes = (size_t)wcount * 32;

        if (!have_n(p, end, bytes))
            goto fail;
        out->wtxid_list = ckalloc(bytes);
        for (i = 0; i < wcount; i++) {
            if (!sv2_read_u256(&p, end, out->wtxid_list + (size_t)i * 32))
                goto fail;
        }
    }
    if (!sv2_read_b0_64k(&p, end, &out->excess_data, &out->excess_data_len))
        goto fail;
    return true;
fail:
    sv2_declare_mining_job_free(out);
    return false;
}

void sv2_declare_mining_job_free(struct sv2_declare_mining_job* m) {
    if (!m)
        return;
    dealloc(m->coinbase_tx_prefix);
    dealloc(m->coinbase_tx_suffix);
    dealloc(m->wtxid_list);
    dealloc(m->excess_data);
    m->coinbase_tx_prefix = NULL;
    m->coinbase_tx_suffix = NULL;
    m->wtxid_list = NULL;
    m->excess_data = NULL;
    m->coinbase_tx_prefix_len = 0;
    m->coinbase_tx_suffix_len = 0;
    m->wtxid_count = 0;
    m->excess_data_len = 0;
}

bool sv2_encode_declare_mining_job_success(
    uint8_t*                                     buf,
    size_t                                       bufsz,
    size_t*                                      outlen,
    const struct sv2_declare_mining_job_success* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 1 + m->new_mining_job_token_len;

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->request_id);
    sv2_write_b0_255(&p, m->new_mining_job_token, m->new_mining_job_token_len);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_declare_mining_job_error(
    uint8_t*                                   buf,
    size_t                                     bufsz,
    size_t*                                    outlen,
    const struct sv2_declare_mining_job_error* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 1 + strlen(m->error_code) + 2 + m->error_details_len;

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->request_id);
    sv2_write_str0_255(&p, m->error_code);
    sv2_write_b0_64k(&p, m->error_details, m->error_details_len);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_provide_missing_transactions(
    uint8_t*                                       buf,
    size_t                                         bufsz,
    size_t*                                        outlen,
    const struct sv2_provide_missing_transactions* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 2 + (size_t)m->unknown_count * 2;
    uint16_t i;

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->request_id);
    sv2_write_u16(&p, m->unknown_count);
    for (i = 0; i < m->unknown_count; i++)
        sv2_write_u16(&p, m->unknown_tx_position_list[i]);
    return finish_encode(buf, p, bufsz, outlen);
}

/* B0_16M: U24 length + bytes */
static bool sv2_read_b0_16m(const uint8_t** p, const uint8_t* end, uint8_t** out, uint32_t* outlen) {
    uint32_t n = 0;

    *out = NULL;
    *outlen = 0;
    if (!sv2_read_u24(p, end, &n))
        return false;
    /* Cap individual tx (policy); absolute B0_16M max is U24 / SV2_MAX_PAYLOAD */
    if (n > SV2_MAX_TX_BYTES || n > SV2_MAX_PAYLOAD)
        return false;
    if (!have_n(*p, end, n))
        return false;
    if (n) {
        *out = ckalloc(n);
        memcpy(*out, *p, n);
    }
    *outlen = n;
    *p += n;
    return true;
}

bool sv2_decode_provide_missing_transactions_success(
    const uint8_t*                                   payload,
    uint32_t                                         len,
    struct sv2_provide_missing_transactions_success* out) {
    const uint8_t *p = payload, *end = payload + len;
    uint16_t       count = 0, i;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->request_id) || !sv2_read_u16(&p, end, &count))
        return false;
    if (count > SV2_MAX_JD_TXNS)
        return false;
    /* Each transaction is B0_16M, so at minimum a 3 byte U24 length. Refuse
     * to allocate for more entries than the payload could possibly hold,
     * else a 6 byte message claiming 25000 txns costs us 300kB. */
    if (!have_n(p, end, (size_t)count * 3))
        return false;
    out->tx_count = count;
    if (!count)
        return true;
    out->transactions = ckzalloc(sizeof(uint8_t*) * count);
    out->tx_lens = ckzalloc(sizeof(uint32_t) * count);
    for (i = 0; i < count; i++) {
        if (!sv2_read_b0_16m(&p, end, &out->transactions[i], &out->tx_lens[i])) {
            sv2_provide_missing_tx_success_free(out);
            return false;
        }
    }
    return true;
}

void sv2_provide_missing_tx_success_free(struct sv2_provide_missing_transactions_success* m) {
    uint16_t i;

    if (!m)
        return;
    if (m->transactions) {
        for (i = 0; i < m->tx_count; i++)
            dealloc(m->transactions[i]);
        dealloc(m->transactions);
    }
    dealloc(m->tx_lens);
    m->transactions = NULL;
    m->tx_lens = NULL;
    m->tx_count = 0;
}

bool sv2_decode_set_custom_mining_job(const uint8_t* payload, uint32_t len, struct sv2_set_custom_mining_job* out) {
    const uint8_t *p = payload, *end = payload + len;
    uint8_t        mcount = 0, i;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) || !sv2_read_u32(&p, end, &out->request_id) ||
        !sv2_read_b0_255(&p, end, out->mining_job_token, sizeof(out->mining_job_token), &out->mining_job_token_len) ||
        !sv2_read_u32(&p, end, &out->version) || !sv2_read_u256(&p, end, out->prev_hash) ||
        !sv2_read_u32(&p, end, &out->min_ntime) || !sv2_read_u32(&p, end, &out->nbits) ||
        !sv2_read_u32(&p, end, &out->coinbase_tx_version) ||
        !sv2_read_b0_255(&p, end, out->coinbase_prefix, sizeof(out->coinbase_prefix), &out->coinbase_prefix_len) ||
        !sv2_read_u32(&p, end, &out->coinbase_tx_input_nSequence) ||
        !sv2_read_b0_64k(&p, end, &out->coinbase_tx_outputs, &out->coinbase_tx_outputs_len) ||
        !sv2_read_u32(&p, end, &out->coinbase_tx_locktime) || !sv2_read_u8(&p, end, &mcount))
        goto fail;
    if (mcount > SV2_MAX_MERKLE_PATH)
        goto fail;
    out->merkle_count = mcount;
    for (i = 0; i < mcount; i++) {
        if (!sv2_read_u256(&p, end, out->merkle_path[i]))
            goto fail;
    }
    return true;
fail:
    sv2_set_custom_mining_job_free(out);
    return false;
}

void sv2_set_custom_mining_job_free(struct sv2_set_custom_mining_job* m) {
    if (!m)
        return;
    dealloc(m->coinbase_tx_outputs);
    m->coinbase_tx_outputs = NULL;
    m->coinbase_tx_outputs_len = 0;
}

bool sv2_encode_set_custom_mining_job_success(
    uint8_t*                                        buf,
    size_t                                          bufsz,
    size_t*                                         outlen,
    const struct sv2_set_custom_mining_job_success* m) {
    uint8_t* p = buf;

    if (bufsz < 12)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u32(&p, m->request_id);
    sv2_write_u32(&p, m->job_id);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_set_custom_mining_job_error(
    uint8_t*                                      buf,
    size_t                                        bufsz,
    size_t*                                       outlen,
    const struct sv2_set_custom_mining_job_error* m) {
    uint8_t* p = buf;
    size_t   need = 8 + 1 + strlen(m->error_code);

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u32(&p, m->request_id);
    sv2_write_str0_255(&p, m->error_code);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_decode_push_solution(const uint8_t* payload, uint32_t len, struct sv2_push_solution* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_b0_32(&p, end, out->extranonce, &out->extranonce_len) || !sv2_read_u256(&p, end, out->prev_hash) ||
        !sv2_read_u32(&p, end, &out->nonce) || !sv2_read_u32(&p, end, &out->ntime) ||
        !sv2_read_u32(&p, end, &out->nbits) || !sv2_read_u32(&p, end, &out->version))
        return false;
    return true;
}

/* ===================================================================
 * Client-side codec (ckproxy SV2 upstream): encode client→server,
 * decode server→client. Mirrors of the server-side functions above.
 * =================================================================== */

bool sv2_encode_setup_connection(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_setup_connection* m) {
    uint8_t* p = buf;
    size_t   need = 1 + 2 + 2 + 4 + (1 + strlen(m->endpoint_host)) + 2 + (1 + strlen(m->vendor)) +
                  (1 + strlen(m->hardware_version)) + (1 + strlen(m->firmware)) + (1 + strlen(m->device_id));

    if (bufsz < need)
        return false;
    sv2_write_u8(&p, m->protocol);
    sv2_write_u16(&p, m->min_version);
    sv2_write_u16(&p, m->max_version);
    sv2_write_u32(&p, m->flags);
    sv2_write_str0_255(&p, m->endpoint_host);
    sv2_write_u16(&p, m->endpoint_port);
    sv2_write_str0_255(&p, m->vendor);
    sv2_write_str0_255(&p, m->hardware_version);
    sv2_write_str0_255(&p, m->firmware);
    sv2_write_str0_255(&p, m->device_id);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_open_extended_channel(
    uint8_t*                                buf,
    size_t                                  bufsz,
    size_t*                                 outlen,
    const struct sv2_open_extended_channel* m) {
    uint8_t* p = buf;
    size_t   need = 4 + (1 + strlen(m->user_identity)) + 4 + 32 + 2;

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->request_id);
    sv2_write_str0_255(&p, m->user_identity);
    sv2_write_f32(&p, m->nominal_hash_rate);
    sv2_write_u256(&p, m->max_target);
    sv2_write_u16(&p, m->min_extranonce_size);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_submit_shares_extended(
    uint8_t*                                 buf,
    size_t                                   bufsz,
    size_t*                                  outlen,
    const struct sv2_submit_shares_extended* m) {
    uint8_t* p = buf;
    size_t   need = 4 + 4 + 4 + 4 + 4 + 4 + 1 + m->extranonce_len;

    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->base.channel_id);
    sv2_write_u32(&p, m->base.sequence_number);
    sv2_write_u32(&p, m->base.job_id);
    sv2_write_u32(&p, m->base.nonce);
    sv2_write_u32(&p, m->base.ntime);
    sv2_write_u32(&p, m->base.version);
    sv2_write_b0_32(&p, m->extranonce, m->extranonce_len);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_encode_update_channel(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_update_channel* m) {
    uint8_t* p = buf;

    if (bufsz < 4 + 4 + 32)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_f32(&p, m->nominal_hash_rate);
    sv2_write_u256(&p, m->maximum_target);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_decode_setup_connection_success(
    const uint8_t*                       payload,
    uint32_t                             len,
    struct sv2_setup_connection_success* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u16(&p, end, &out->used_version) || !sv2_read_u32(&p, end, &out->flags))
        return false;
    return true;
}

bool sv2_decode_setup_connection_error(const uint8_t* payload, uint32_t len, struct sv2_setup_connection_error* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->flags) || !sv2_read_str0_255(&p, end, out->error_code, sizeof(out->error_code)))
        return false;
    return true;
}

bool sv2_decode_open_extended_channel_success(
    const uint8_t*                            payload,
    uint32_t                                  len,
    struct sv2_open_extended_channel_success* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->request_id) || !sv2_read_u32(&p, end, &out->channel_id) ||
        !sv2_read_u256(&p, end, out->target) || !sv2_read_u16(&p, end, &out->extranonce_size) ||
        !sv2_read_b0_32(&p, end, out->extranonce_prefix, &out->extranonce_prefix_len) ||
        !sv2_read_u32(&p, end, &out->group_channel_id))
        return false;
    return true;
}

bool sv2_decode_open_channel_error(const uint8_t* payload, uint32_t len, struct sv2_open_channel_error* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->request_id) ||
        !sv2_read_str0_255(&p, end, out->error_code, sizeof(out->error_code)))
        return false;
    return true;
}

bool sv2_decode_new_extended_mining_job(const uint8_t* payload, uint32_t len, struct sv2_new_extended_mining_job* out) {
    const uint8_t *p = payload, *end = payload + len;
    uint8_t        mcount = 0, i;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) || !sv2_read_u32(&p, end, &out->job_id) ||
        !sv2_read_option_u32(&p, end, &out->min_ntime_present, &out->min_ntime) ||
        !sv2_read_u32(&p, end, &out->version) || !sv2_read_bool(&p, end, &out->version_rolling_allowed) ||
        !sv2_read_u8(&p, end, &mcount))
        goto fail;
    if (mcount > SV2_MAX_MERKLE_PATH)
        goto fail;
    out->merkle_count = mcount;
    for (i = 0; i < mcount; i++) {
        if (!sv2_read_u256(&p, end, out->merkle_path[i]))
            goto fail;
    }
    if (!sv2_read_b0_64k(&p, end, &out->coinbase_tx_prefix, &out->coinbase_tx_prefix_len) ||
        !sv2_read_b0_64k(&p, end, &out->coinbase_tx_suffix, &out->coinbase_tx_suffix_len))
        goto fail;
    return true;
fail:
    sv2_new_extended_mining_job_free(out);
    return false;
}

void sv2_new_extended_mining_job_free(struct sv2_new_extended_mining_job* m) {
    if (!m)
        return;
    dealloc(m->coinbase_tx_prefix);
    dealloc(m->coinbase_tx_suffix);
    m->coinbase_tx_prefix = NULL;
    m->coinbase_tx_suffix = NULL;
    m->coinbase_tx_prefix_len = 0;
    m->coinbase_tx_suffix_len = 0;
}

bool sv2_decode_set_new_prev_hash(const uint8_t* payload, uint32_t len, struct sv2_set_new_prev_hash* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) || !sv2_read_u32(&p, end, &out->job_id) ||
        !sv2_read_u256(&p, end, out->prev_hash) || !sv2_read_u32(&p, end, &out->min_ntime) ||
        !sv2_read_u32(&p, end, &out->nbits))
        return false;
    return true;
}

bool sv2_decode_set_target(const uint8_t* payload, uint32_t len, struct sv2_set_target* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) || !sv2_read_u256(&p, end, out->maximum_target))
        return false;
    return true;
}

bool sv2_decode_set_extranonce_prefix(const uint8_t* payload, uint32_t len, struct sv2_set_extranonce_prefix* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) ||
        !sv2_read_b0_32(&p, end, out->extranonce_prefix, &out->extranonce_prefix_len))
        return false;
    return true;
}

bool sv2_decode_submit_shares_success(const uint8_t* payload, uint32_t len, struct sv2_submit_shares_success* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) || !sv2_read_u32(&p, end, &out->last_sequence_number) ||
        !sv2_read_u32(&p, end, &out->new_submits_accepted_count) || !sv2_read_u64(&p, end, &out->new_shares_sum))
        return false;
    return true;
}

bool sv2_decode_submit_shares_error(const uint8_t* payload, uint32_t len, struct sv2_submit_shares_error* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) || !sv2_read_u32(&p, end, &out->sequence_number) ||
        !sv2_read_str0_255(&p, end, out->error_code, sizeof(out->error_code)))
        return false;
    return true;
}

bool sv2_decode_reconnect(const uint8_t* payload, uint32_t len, struct sv2_reconnect* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_str0_255(&p, end, out->new_host, sizeof(out->new_host)) || !sv2_read_u16(&p, end, &out->new_port))
        return false;
    return true;
}

/* ===================================================================
 * Client-side Job Declaration codec (ckproxy as JDC): encode
 * client→server, decode server→client. Exact mirrors of the JDS
 * functions above — the two halves of each message must stay in step.
 * =================================================================== */

/* B0_16M writer (mirrors sv2_read_b0_16m). Caller has sized the buffer. */
static void sv2_write_b0_16m(uint8_t** p, const uint8_t* data, uint32_t len) {
    sv2_write_u24(p, len);
    if (len) {
        memcpy(*p, data, len);
        *p += len;
    }
}

/*
 * Zero-copy B0_64K: points into the payload rather than allocating, for the
 * small server→client fields the JDC copies straight into its own state.
 */
static bool read_b0_64k_ref(const uint8_t** p, const uint8_t* end, const uint8_t** out, uint16_t* outlen) {
    uint16_t n;

    *out = NULL;
    *outlen = 0;
    if (!sv2_read_u16(p, end, &n))
        return false;
    if (!have_n(*p, end, n))
        return false;
    if (n)
        *out = *p;
    *outlen = n;
    *p += n;
    return true;
}

bool sv2_encode_allocate_mining_job_token(
    uint8_t*                                    buf,
    size_t                                      bufsz,
    size_t*                                     outlen,
    const struct sv2_allocate_mining_job_token* m) {
    uint8_t* p = buf;
    size_t   need = 1 + strlen(m->user_identifier) + 4;

    if (bufsz < need)
        return false;
    sv2_write_str0_255(&p, m->user_identifier);
    sv2_write_u32(&p, m->request_id);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_decode_allocate_mining_job_token_success(
    const uint8_t*                                payload,
    uint32_t                                      len,
    struct sv2_allocate_mining_job_token_success* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->request_id) ||
        !sv2_read_b0_255(&p, end, out->mining_job_token, sizeof(out->mining_job_token), &out->mining_job_token_len) ||
        !read_b0_64k_ref(&p, end, &out->coinbase_tx_outputs, &out->coinbase_tx_outputs_len))
        return false;
    return true;
}

size_t sv2_declare_mining_job_encoded_size(const struct sv2_declare_mining_job* m) {
    return 4 + 1 + m->mining_job_token_len + 4 + 2 + m->coinbase_tx_prefix_len + 2 + m->coinbase_tx_suffix_len + 2 +
           (size_t)m->wtxid_count * 32 + 2 + m->excess_data_len;
}

bool sv2_encode_declare_mining_job(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_declare_mining_job* m) {
    uint8_t* p = buf;
    size_t   need = sv2_declare_mining_job_encoded_size(m);
    uint16_t i;

    /* Fail closed rather than build a frame the JDS must reject. */
    if (m->wtxid_count > SV2_MAX_JD_TXNS || need > SV2_MAX_JD_PAYLOAD)
        return false;
    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->request_id);
    sv2_write_b0_255(&p, m->mining_job_token, m->mining_job_token_len);
    sv2_write_u32(&p, m->version);
    sv2_write_b0_64k(&p, m->coinbase_tx_prefix, m->coinbase_tx_prefix_len);
    sv2_write_b0_64k(&p, m->coinbase_tx_suffix, m->coinbase_tx_suffix_len);
    sv2_write_u16(&p, m->wtxid_count);
    for (i = 0; i < m->wtxid_count; i++)
        sv2_write_u256(&p, m->wtxid_list + (size_t)i * 32);
    sv2_write_b0_64k(&p, m->excess_data, m->excess_data_len);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_decode_declare_mining_job_success(
    const uint8_t*                         payload,
    uint32_t                               len,
    struct sv2_declare_mining_job_success* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->request_id) ||
        !sv2_read_b0_255(
            &p, end, out->new_mining_job_token, sizeof(out->new_mining_job_token), &out->new_mining_job_token_len))
        return false;
    return true;
}

bool sv2_decode_declare_mining_job_error(
    const uint8_t*                       payload,
    uint32_t                             len,
    struct sv2_declare_mining_job_error* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->request_id) ||
        !sv2_read_str0_255(&p, end, out->error_code, sizeof(out->error_code)) ||
        !read_b0_64k_ref(&p, end, &out->error_details, &out->error_details_len))
        return false;
    return true;
}

bool sv2_decode_provide_missing_transactions(
    const uint8_t*                           payload,
    uint32_t                                 len,
    struct sv2_provide_missing_transactions* out) {
    const uint8_t *p = payload, *end = payload + len;
    uint16_t       count = 0, i;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->request_id) || !sv2_read_u16(&p, end, &count))
        return false;
    if (count > SV2_MAX_JD_TXNS)
        return false;
    /* Each position is 2 bytes: refuse to allocate for more entries than the
     * payload can hold, else a 6 byte message costs us 50kB. */
    if (!have_n(p, end, (size_t)count * 2))
        return false;
    out->unknown_count = count;
    if (!count)
        return true;
    out->unknown_tx_position_list = ckzalloc(sizeof(uint16_t) * count);
    for (i = 0; i < count; i++) {
        if (!sv2_read_u16(&p, end, &out->unknown_tx_position_list[i])) {
            sv2_provide_missing_transactions_free(out);
            return false;
        }
    }
    return true;
}

void sv2_provide_missing_transactions_free(struct sv2_provide_missing_transactions* m) {
    if (!m)
        return;
    dealloc(m->unknown_tx_position_list);
    m->unknown_tx_position_list = NULL;
    m->unknown_count = 0;
}

size_t sv2_provide_missing_tx_success_encoded_size(const struct sv2_provide_missing_transactions_success* m) {
    size_t   need = 4 + 2;
    uint16_t i;

    for (i = 0; i < m->tx_count; i++)
        need += 3 + m->tx_lens[i];
    return need;
}

bool sv2_encode_provide_missing_transactions_success(
    uint8_t*                                               buf,
    size_t                                                 bufsz,
    size_t*                                                outlen,
    const struct sv2_provide_missing_transactions_success* m) {
    uint8_t* p = buf;
    size_t   need;
    uint16_t i;

    if (m->tx_count > SV2_MAX_JD_TXNS)
        return false;
    if (m->tx_count && (!m->transactions || !m->tx_lens))
        return false;
    for (i = 0; i < m->tx_count; i++) {
        if (m->tx_lens[i] > SV2_MAX_TX_BYTES)
            return false;
    }
    need = sv2_provide_missing_tx_success_encoded_size(m);
    if (need > SV2_MAX_JD_PAYLOAD || bufsz < need)
        return false;
    sv2_write_u32(&p, m->request_id);
    sv2_write_u16(&p, m->tx_count);
    for (i = 0; i < m->tx_count; i++)
        sv2_write_b0_16m(&p, m->transactions[i], m->tx_lens[i]);
    return finish_encode(buf, p, bufsz, outlen);
}

size_t sv2_set_custom_mining_job_encoded_size(const struct sv2_set_custom_mining_job* m) {
    return 4 + 4 + 1 + m->mining_job_token_len + 4 + 32 + 4 + 4 + 4 + 1 + m->coinbase_prefix_len + 4 + 2 +
           m->coinbase_tx_outputs_len + 4 + 1 + (size_t)m->merkle_count * 32;
}

bool sv2_encode_set_custom_mining_job(
    uint8_t*                                buf,
    size_t                                  bufsz,
    size_t*                                 outlen,
    const struct sv2_set_custom_mining_job* m) {
    uint8_t* p = buf;
    size_t   need = sv2_set_custom_mining_job_encoded_size(m);
    uint8_t  i;

    if (m->merkle_count > SV2_MAX_MERKLE_PATH)
        return false;
    if (bufsz < need)
        return false;
    sv2_write_u32(&p, m->channel_id);
    sv2_write_u32(&p, m->request_id);
    sv2_write_b0_255(&p, m->mining_job_token, m->mining_job_token_len);
    sv2_write_u32(&p, m->version);
    sv2_write_u256(&p, m->prev_hash);
    sv2_write_u32(&p, m->min_ntime);
    sv2_write_u32(&p, m->nbits);
    sv2_write_u32(&p, m->coinbase_tx_version);
    sv2_write_b0_255(&p, m->coinbase_prefix, m->coinbase_prefix_len);
    sv2_write_u32(&p, m->coinbase_tx_input_nSequence);
    sv2_write_b0_64k(&p, m->coinbase_tx_outputs, m->coinbase_tx_outputs_len);
    sv2_write_u32(&p, m->coinbase_tx_locktime);
    sv2_write_u8(&p, m->merkle_count);
    for (i = 0; i < m->merkle_count; i++)
        sv2_write_u256(&p, m->merkle_path[i]);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_decode_set_custom_mining_job_success(
    const uint8_t*                            payload,
    uint32_t                                  len,
    struct sv2_set_custom_mining_job_success* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) || !sv2_read_u32(&p, end, &out->request_id) ||
        !sv2_read_u32(&p, end, &out->job_id))
        return false;
    return true;
}

bool sv2_decode_set_custom_mining_job_error(
    const uint8_t*                          payload,
    uint32_t                                len,
    struct sv2_set_custom_mining_job_error* out) {
    const uint8_t *p = payload, *end = payload + len;

    memset(out, 0, sizeof(*out));
    if (!sv2_read_u32(&p, end, &out->channel_id) || !sv2_read_u32(&p, end, &out->request_id) ||
        !sv2_read_str0_255(&p, end, out->error_code, sizeof(out->error_code)))
        return false;
    return true;
}

bool sv2_encode_push_solution(uint8_t* buf, size_t bufsz, size_t* outlen, const struct sv2_push_solution* m) {
    uint8_t* p = buf;
    size_t   need = 1 + m->extranonce_len + 32 + 16;

    if (m->extranonce_len > SV2_MAX_B0_32)
        return false;
    if (bufsz < need)
        return false;
    sv2_write_b0_32(&p, m->extranonce, m->extranonce_len);
    sv2_write_u256(&p, m->prev_hash);
    sv2_write_u32(&p, m->nonce);
    sv2_write_u32(&p, m->ntime);
    sv2_write_u32(&p, m->nbits);
    sv2_write_u32(&p, m->version);
    return finish_encode(buf, p, bufsz, outlen);
}

bool sv2_build_frame(
    uint16_t       extension_type,
    uint8_t        msg_type,
    const uint8_t* payload,
    uint32_t       payload_len,
    uint8_t**      out,
    size_t*        outlen) {
    uint8_t* buf;

    if (payload_len > SV2_MAX_PAYLOAD)
        return false;
    *outlen = SV2_FRAME_HEADER_LEN + payload_len;
    buf = ckalloc(*outlen);
    sv2_encode_header(buf, extension_type, msg_type, payload_len);
    if (payload_len && payload)
        memcpy(buf + SV2_FRAME_HEADER_LEN, payload, payload_len);
    *out = buf;
    return true;
}
