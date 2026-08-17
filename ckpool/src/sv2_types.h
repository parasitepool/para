/*
 * Copyright 2026 Con Kolivas
 *
 * Stratum V2 wire constants and shared types (Mining Protocol + common).
 * Spec: ~/Code/sv2-spec (03, 05, 08).
 */

#ifndef SV2_TYPES_H
#define SV2_TYPES_H

#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>

/* Frame header is 6 bytes: extension_type U16 | msg_type U8 | msg_length U24 */
#define SV2_FRAME_HEADER_LEN 6
#define SV2_CHANNEL_MSG_BIT 0x8000
#define SV2_EXTENSION_MASK 0x7fff

/* Noise transport limits (spec 04 §4.6) */
#define SV2_NOISE_MAX_CT_LEN 65535
#define SV2_NOISE_MAC_LEN 16
#define SV2_NOISE_MAX_PT_CHUNK (SV2_NOISE_MAX_CT_LEN - SV2_NOISE_MAC_LEN)
#define SV2_ENCRYPTED_HEADER_LEN (SV2_FRAME_HEADER_LEN + SV2_NOISE_MAC_LEN) /* 22 */

/* Common protocol message types */
#define SV2_MSG_SETUP_CONNECTION 0x00
#define SV2_MSG_SETUP_CONNECTION_SUCCESS 0x01
#define SV2_MSG_SETUP_CONNECTION_ERROR 0x02
#define SV2_MSG_CHANNEL_ENDPOINT_CHANGED 0x03
#define SV2_MSG_RECONNECT 0x04

/* Mining protocol message types */
#define SV2_MSG_OPEN_STANDARD_MINING_CHANNEL 0x10
#define SV2_MSG_OPEN_STANDARD_MINING_CHANNEL_SUCCESS 0x11
#define SV2_MSG_OPEN_MINING_CHANNEL_ERROR 0x12
#define SV2_MSG_OPEN_EXTENDED_MINING_CHANNEL 0x13
#define SV2_MSG_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS 0x14
#define SV2_MSG_NEW_MINING_JOB 0x15
#define SV2_MSG_UPDATE_CHANNEL 0x16
#define SV2_MSG_UPDATE_CHANNEL_ERROR 0x17
#define SV2_MSG_CLOSE_CHANNEL 0x18
#define SV2_MSG_SET_EXTRANONCE_PREFIX 0x19
#define SV2_MSG_SUBMIT_SHARES_STANDARD 0x1a
#define SV2_MSG_SUBMIT_SHARES_EXTENDED 0x1b
#define SV2_MSG_SUBMIT_SHARES_SUCCESS 0x1c
#define SV2_MSG_SUBMIT_SHARES_ERROR 0x1d
#define SV2_MSG_NEW_EXTENDED_MINING_JOB 0x1f
#define SV2_MSG_SET_NEW_PREV_HASH 0x20
#define SV2_MSG_SET_TARGET 0x21
#define SV2_MSG_SET_CUSTOM_MINING_JOB 0x22
#define SV2_MSG_SET_CUSTOM_MINING_JOB_SUCCESS 0x23
#define SV2_MSG_SET_CUSTOM_MINING_JOB_ERROR 0x24
#define SV2_MSG_SET_GROUP_CHANNEL 0x25

/* Job Declaration Protocol message types (spec 08) */
#define SV2_MSG_ALLOCATE_MINING_JOB_TOKEN 0x50
#define SV2_MSG_ALLOCATE_MINING_JOB_TOKEN_SUCCESS 0x51
#define SV2_MSG_PROVIDE_MISSING_TRANSACTIONS 0x55
#define SV2_MSG_PROVIDE_MISSING_TRANSACTIONS_SUCCESS 0x56
#define SV2_MSG_DECLARE_MINING_JOB 0x57
#define SV2_MSG_DECLARE_MINING_JOB_SUCCESS 0x58
#define SV2_MSG_DECLARE_MINING_JOB_ERROR 0x59
#define SV2_MSG_PUSH_SOLUTION 0x60

/* SetupConnection.protocol */
#define SV2_PROTOCOL_MINING 0
#define SV2_PROTOCOL_JOB_DECLARATION 1
#define SV2_PROTOCOL_TEMPLATE_DISTRIBUTION 2

/* Mining SetupConnection.flags (client → server), bit 0 = LSB */
#define SV2_FLAG_REQUIRES_STANDARD_JOBS (1u << 0)
#define SV2_FLAG_REQUIRES_WORK_SELECTION (1u << 1)
#define SV2_FLAG_REQUIRES_VERSION_ROLLING (1u << 2)

/* Job Declaration SetupConnection.flags */
#define SV2_JD_FLAG_DECLARE_TX_DATA (1u << 0)

/* SetupConnection.Success.flags (server → client) */
#define SV2_FLAG_REQUIRES_FIXED_VERSION (1u << 0)
#define SV2_FLAG_REQUIRES_EXTENDED_CHANNELS (1u << 1)

/* Policy maxima for untrusted decode */
#define SV2_MAX_STR_LEN 255
#define SV2_MAX_B0_32 32
#define SV2_MAX_B0_255 255
#define SV2_MAX_B0_64K 65535
#define SV2_MAX_MERKLE_PATH 32
/*
 * Absolute plaintext frame payload cap (msg_length is U24 → max 16 MiB - 1).
 * Codec / Noise use this as the hard wire maximum. Per-subprotocol policy
 * caps below are tighter.
 */
#define SV2_MAX_PAYLOAD 0x00FFFFFFu
/* Per-tx cap inside B0_16M fields (policy; still ≤ U24) */
#define SV2_MAX_TX_BYTES (4u << 20) /* 4 MiB */
/*
 * Mining protocol plaintext payload policy (jobs/shares/setup). Real frames
 * are << 64 KiB; 1 MiB leaves headroom without letting a post-Noise peer pin
 * ~32 MiB of reassembly buffer (was SV2_MAX_PAYLOAD*2).
 * Enforced as the mining connection rx ceiling and in the connector.
 */
#define SV2_MAX_MINING_PAYLOAD (1u << 20) /* 1 MiB */
/*
 * JD plaintext payload policy cap (below U24). Full-block ProvideMissing is
 * ~block-sized; 8 MiB is enough without pinning 16+32 MiB per JD connection.
 * Enforced in sv2_jd_handle_frame and as the JD connection rx ceiling.
 */
#define SV2_MAX_JD_PAYLOAD (8u << 20) /* 8 MiB */
/* JD Full-Template: max wtxids in DeclareMiningJob (local pre-checkBlock cap) */
#define SV2_MAX_JD_TXNS 25000
#define SV2_MAX_JOB_TOKEN SV2_MAX_B0_255

/* Decoded / encoded frame (plaintext payload after AEAD) */
struct sv2_frame {
    uint16_t       extension_type; /* raw, may include channel_msg bit */
    uint8_t        msg_type;
    uint32_t       msg_length; /* U24 */
    const uint8_t* payload;    /* points into caller buffer or owned copy */
    uint8_t*       owned;      /* if non-NULL, free with dealloc */
};

/* SetupConnection (client → server) */
struct sv2_setup_connection {
    uint8_t  protocol;
    uint16_t min_version;
    uint16_t max_version;
    uint32_t flags;
    char     endpoint_host[SV2_MAX_STR_LEN + 1];
    uint16_t endpoint_port;
    char     vendor[SV2_MAX_STR_LEN + 1];
    char     hardware_version[SV2_MAX_STR_LEN + 1];
    char     firmware[SV2_MAX_STR_LEN + 1];
    char     device_id[SV2_MAX_STR_LEN + 1];
};

struct sv2_setup_connection_success {
    uint16_t used_version;
    uint32_t flags;
};

struct sv2_setup_connection_error {
    uint32_t flags;
    char     error_code[SV2_MAX_STR_LEN + 1];
};

struct sv2_open_standard_channel {
    uint32_t request_id;
    char     user_identity[SV2_MAX_STR_LEN + 1];
    float    nominal_hash_rate;
    uint8_t  max_target[32]; /* U256 LE */
};

struct sv2_open_standard_channel_success {
    uint32_t request_id;
    uint32_t channel_id;
    uint8_t  target[32];
    uint8_t  extranonce_prefix[SV2_MAX_B0_32];
    uint8_t  extranonce_prefix_len;
    uint32_t group_channel_id;
};

struct sv2_open_extended_channel {
    uint32_t request_id;
    char     user_identity[SV2_MAX_STR_LEN + 1];
    float    nominal_hash_rate;
    uint8_t  max_target[32];
    uint16_t min_extranonce_size;
};

struct sv2_open_extended_channel_success {
    uint32_t request_id;
    uint32_t channel_id;
    uint8_t  target[32];
    uint16_t extranonce_size;
    uint8_t  extranonce_prefix[SV2_MAX_B0_32];
    uint8_t  extranonce_prefix_len;
    uint32_t group_channel_id;
};

struct sv2_open_channel_error {
    uint32_t request_id;
    char     error_code[SV2_MAX_STR_LEN + 1];
};

struct sv2_new_mining_job {
    uint32_t channel_id;
    uint32_t job_id;
    bool     min_ntime_present;
    uint32_t min_ntime;
    uint32_t version;
    uint8_t  merkle_root[32];
};

struct sv2_set_new_prev_hash {
    uint32_t channel_id;
    uint32_t job_id;
    uint8_t  prev_hash[32];
    uint32_t min_ntime;
    uint32_t nbits;
};

struct sv2_set_target {
    uint32_t channel_id;
    uint8_t  maximum_target[32];
};

/* SetExtranoncePrefix (server → client) */
struct sv2_set_extranonce_prefix {
    uint32_t channel_id;
    uint8_t  extranonce_prefix[SV2_MAX_B0_32];
    uint8_t  extranonce_prefix_len;
};

/* Reconnect (server → client, common protocol) */
struct sv2_reconnect {
    char     new_host[SV2_MAX_STR_LEN + 1];
    uint16_t new_port;
};

/* UpdateChannel (client → server) */
struct sv2_update_channel {
    uint32_t channel_id;
    float    nominal_hash_rate;
    uint8_t  maximum_target[32]; /* U256 LE */
};

/* UpdateChannel.Error (server → client) */
struct sv2_update_channel_error {
    uint32_t channel_id;
    char     error_code[SV2_MAX_STR_LEN + 1];
};

struct sv2_submit_shares_standard {
    uint32_t channel_id;
    uint32_t sequence_number;
    uint32_t job_id;
    uint32_t nonce;
    uint32_t ntime;
    uint32_t version;
};

struct sv2_submit_shares_extended {
    struct sv2_submit_shares_standard base;
    uint8_t                           extranonce[SV2_MAX_B0_32];
    uint8_t                           extranonce_len;
};

struct sv2_submit_shares_success {
    uint32_t channel_id;
    uint32_t last_sequence_number;
    uint32_t new_submits_accepted_count;
    uint64_t new_shares_sum;
};

struct sv2_submit_shares_error {
    uint32_t channel_id;
    uint32_t sequence_number;
    char     error_code[SV2_MAX_STR_LEN + 1];
};

struct sv2_close_channel {
    uint32_t channel_id;
    char     reason_code[SV2_MAX_STR_LEN + 1];
};

struct sv2_new_extended_mining_job {
    uint32_t channel_id;
    uint32_t job_id;
    bool     min_ntime_present;
    uint32_t min_ntime;
    uint32_t version;
    bool     version_rolling_allowed;
    uint8_t  merkle_count;
    uint8_t  merkle_path[SV2_MAX_MERKLE_PATH][32];
    uint8_t* coinbase_tx_prefix;
    uint16_t coinbase_tx_prefix_len;
    uint8_t* coinbase_tx_suffix;
    uint16_t coinbase_tx_suffix_len;
};

/* --- Job Declaration Protocol (Phase 2) --- */

struct sv2_allocate_mining_job_token {
    char     user_identifier[SV2_MAX_STR_LEN + 1];
    uint32_t request_id;
};

struct sv2_allocate_mining_job_token_success {
    uint32_t request_id;
    uint8_t  mining_job_token[SV2_MAX_JOB_TOKEN];
    uint8_t  mining_job_token_len;
    /* CompactSize-prefixed consensus outputs; may be NULL/0 */
    const uint8_t* coinbase_tx_outputs;
    uint16_t       coinbase_tx_outputs_len;
};

struct sv2_declare_mining_job {
    uint32_t request_id;
    uint8_t  mining_job_token[SV2_MAX_JOB_TOKEN];
    uint8_t  mining_job_token_len;
    uint32_t version;
    /* Owned heap buffers — caller must free with dealloc after decode */
    uint8_t* coinbase_tx_prefix;
    uint16_t coinbase_tx_prefix_len;
    uint8_t* coinbase_tx_suffix;
    uint16_t coinbase_tx_suffix_len;
    /* Owned: wtxid_count * 32 bytes, or NULL if empty */
    uint8_t* wtxid_list;
    uint16_t wtxid_count;
    uint8_t* excess_data;
    uint16_t excess_data_len;
};

struct sv2_declare_mining_job_success {
    uint32_t request_id;
    uint8_t  new_mining_job_token[SV2_MAX_JOB_TOKEN];
    uint8_t  new_mining_job_token_len;
};

struct sv2_declare_mining_job_error {
    uint32_t       request_id;
    char           error_code[SV2_MAX_STR_LEN + 1];
    const uint8_t* error_details;
    uint16_t       error_details_len;
};

struct sv2_provide_missing_transactions {
    uint32_t request_id;
    uint16_t unknown_count;
    /* Owned: unknown_count * sizeof(uint16_t) */
    uint16_t* unknown_tx_position_list;
};

struct sv2_provide_missing_transactions_success {
    uint32_t request_id;
    uint16_t tx_count;
    /* Owned arrays — free with sv2_provide_missing_tx_success_free */
    uint8_t** transactions; /* tx_count pointers */
    uint32_t* tx_lens;      /* each length (B0_16M, max 16M) */
};

/* Mining Protocol SetCustomMiningJob (client → server) */
struct sv2_set_custom_mining_job {
    uint32_t channel_id;
    uint32_t request_id;
    uint8_t  mining_job_token[SV2_MAX_JOB_TOKEN];
    uint8_t  mining_job_token_len;
    uint32_t version;
    uint8_t  prev_hash[32];
    uint32_t min_ntime;
    uint32_t nbits;
    uint32_t coinbase_tx_version;
    uint8_t  coinbase_prefix[SV2_MAX_B0_255];
    uint8_t  coinbase_prefix_len;
    uint32_t coinbase_tx_input_nSequence;
    /* Owned — free with sv2_set_custom_mining_job_free */
    uint8_t* coinbase_tx_outputs;
    uint16_t coinbase_tx_outputs_len;
    uint32_t coinbase_tx_locktime;
    uint8_t  merkle_count;
    uint8_t  merkle_path[SV2_MAX_MERKLE_PATH][32];
};

struct sv2_set_custom_mining_job_success {
    uint32_t channel_id;
    uint32_t request_id;
    uint32_t job_id;
};

struct sv2_set_custom_mining_job_error {
    uint32_t channel_id;
    uint32_t request_id;
    char     error_code[SV2_MAX_STR_LEN + 1];
};

/* Job Declaration PushSolution (client → server) */
struct sv2_push_solution {
    uint8_t  extranonce[SV2_MAX_B0_32];
    uint8_t  extranonce_len;
    uint8_t  prev_hash[32];
    uint32_t nonce;
    uint32_t ntime;
    uint32_t nbits;
    uint32_t version;
};

/* Convert plaintext payload length to ciphertext length (spec 04). */
static inline uint32_t sv2_pt_len_to_ct_len(uint32_t pt_len) {
    uint32_t rem = pt_len % SV2_NOISE_MAX_PT_CHUNK;

    if (rem > 0)
        rem += SV2_NOISE_MAC_LEN;
    return (pt_len / SV2_NOISE_MAX_PT_CHUNK) * SV2_NOISE_MAX_CT_LEN + rem;
}

/*
 * Reassembly ceiling for a peer whose largest permitted plaintext payload is
 * pt_max. Must be derived from the ciphertext size, not the plaintext: noise
 * adds an encrypted header plus a MAC per 64kB chunk, which at JD's 8MiB cap
 * is over 2kB of tags. Add one socket read of slack so a maximum sized frame
 * arriving interleaved with the head of the next one still fits.
 */
#define SV2_RX_READ_CHUNK 8192
/* Reassembly buffer starts here and is returned here whenever it drains. */
#define SV2_RX_CAP_INITIAL 8192

static inline size_t sv2_rx_max_for_payload(uint32_t pt_max) {
    return (size_t)SV2_ENCRYPTED_HEADER_LEN + sv2_pt_len_to_ct_len(pt_max) + SV2_RX_READ_CHUNK;
}

static inline bool sv2_is_channel_msg(uint16_t extension_type) {
    return (extension_type & SV2_CHANNEL_MSG_BIT) != 0;
}

#endif /* SV2_TYPES_H */
