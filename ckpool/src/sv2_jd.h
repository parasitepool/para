/*
 * Copyright 2026 Con Kolivas
 *
 * Stratum V2 Job Declaration Server (JDS) — Phase 2.
 * Full-Template: Allocate → Declare → session checkBlock (once) → local payout
 * on later declares → mining SetCustomMiningJob.
 */

#ifndef SV2_JD_H
#define SV2_JD_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

/* Process one plaintext JD frame. Returns heap reply frame or NULL. */
uint8_t* sv2_jd_handle_frame(int64_t client_id, const uint8_t* frame, size_t framelen, size_t* replylen);

void sv2_jd_drop_client(int64_t client_id);

/* Drop every JD client, token, and pending declare (bulk teardown). */
void sv2_jd_drop_all(void);

void sv2_jd_note_client(int64_t client_id, const char* address, int server);

/* True if token was checkBlock-accepted (DeclareMiningJob.Success path).
 * Used by mining SetCustomMiningJob to gate custom work. */
bool sv2_jd_token_is_declared(const uint8_t* token, uint8_t token_len);

/*
 * Our payout policy for SetCustomMiningJob.coinbase_tx_outputs. Spec §6.4.3
 * requires only that the pool payout output be funded at all; the rest is ours
 * and the shared rule departs from it (see payout_rule_ok() in sv2_jd.c).
 * True if token has no payout requirement, or outputs satisfy mode rules:
 * shared — 100% of spendable value to pool script; solo — miner script
 * funded (other spendable outs allowed). False if token missing / not
 * declared / payout rule failed.
 */
bool sv2_jd_token_outputs_fund_payout(
    const uint8_t* token,
    uint8_t        token_len,
    const uint8_t* outputs,
    uint16_t       outputs_len);

/* True if Job Declaration listen is configured (sv2jdurls > 0). */
bool sv2_jd_enabled(void);

/* Currently connected JD clients (accepted, not yet dropped). */
int sv2_jd_client_count(void);

/* Rebuild a full solved block for a declared token (mining-path block solve).
 * *block_out is ckalloc'd; caller frees. Returns false if material missing. */
bool sv2_jd_rebuild_solved_block(
    const uint8_t* token,
    uint8_t        token_len,
    const uint8_t* extranonce,
    uint8_t        extranonce_len,
    uint32_t       version,
    uint32_t       ntime,
    uint32_t       nonce,
    uint32_t       nbits,
    const uint8_t  prev_hash[32],
    uint8_t**      block_out,
    size_t*        block_len);

/* Operator metrics for pool.status / logs. Snapshot only. */
struct sv2_jd_stats {
    uint64_t allocate_ok;
    uint64_t allocate_rate_limited;
    uint64_t declare_ok;
    uint64_t declare_error;
    uint64_t declare_rate_limited;
    uint64_t checkblock_busy;
    uint64_t checkblock_fail;
    uint64_t coalesce_hit;
    uint64_t provide_missing;
    uint64_t push_solution;
    int      checkblock_inflight;
    int      tokens;
    int      tx_cache;
    uint64_t tx_cache_bytes;
    int      pending_declares;
};

void sv2_jd_get_stats(struct sv2_jd_stats* out);

/* JSON object string for pool.status (caller dealloc). NULL if JD disabled. */
char* sv2_jd_stats_json(void);

/* Call on every pool tip/work update: drop coalesce cache so checkBlock is
 * not skipped for templates validated against a previous tip. */
void sv2_jd_on_tip_change(void);

#endif /* SV2_JD_H */
