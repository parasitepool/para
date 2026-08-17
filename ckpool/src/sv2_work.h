/*
 * Copyright 2026 Con Kolivas
 *
 * Stratifier helpers for SV2 job material (workbase snapshot).
 */

#ifndef SV2_WORK_H
#define SV2_WORK_H

#include "config.h"

#ifdef HAVE_SV2

#include <stdbool.h>
#include <stdint.h>

/* Snapshot of current pool work enough to build standard jobs / validate shares. */
struct sv2_work_snap {
    int64_t       wb_id;
    uint32_t      version; /* block version host order */
    uint32_t      ntime;
    uint32_t      nbits;
    unsigned char prevhash[32];
    unsigned char coinb1[256];
    int           coinb1len;
    unsigned char coinb2[512];
    int           coinb2len;
    int           enonce1varlen;
    int           enonce2varlen;
    unsigned char merklebin[16][32];
    int           merkles;
};

/* Fill *out from sdata->current_workbase under workbase_lock. False if none.
 * If instance_id is non-zero and btcsolo, coinb2 is that user's generation. */
bool stratifier_sv2_snapshot_work(struct sv2_work_snap* out, int64_t instance_id);

/* Allocate a unique extranonce prefix of len bytes (2-8) from the same
 * global enonce1 counter SV1 clients use, guaranteeing no overlap between
 * any SV1 client or SV2 channel on the same workbase. False if stratifier
 * is not yet initialised. */
bool stratifier_sv2_alloc_enonce1(uint8_t* out, int len);

/* Create authorised stratum_instance for an SV2 channel. On success fills
 * *out_instance_id. Returns false if auth rejected (e.g. invalid solo address). */
bool stratifier_sv2_open_session(
    int64_t        connector_id,
    uint32_t       channel_id,
    const char*    user_identity,
    const char*    address,
    int            server,
    const uint8_t* enonce1,
    int            enonce1_len,
    double         diff,
    int64_t*       out_instance_id);

/* Tear down stratum instance created for SV2. */
void stratifier_sv2_close_session(int64_t instance_id);

/* Validate + account a share. nonce2hex NULL => all-zero enonce2 (standard).
 * For extended, pass hex of client extranonce (length = enonce2varlen*2). */
bool stratifier_sv2_submit_share(
    int64_t     instance_id,
    int64_t     workbase_id,
    uint32_t    ntime,
    uint32_t    nonce,
    uint32_t    version,
    const char* nonce2hex,
    char*       errbuf,
    size_t      errbufsz,
    double*     sdiff_out);

/* Compute merkle root (header byte order) for instance's enonce1 on current wb. */
bool stratifier_sv2_merkle_root(
    int64_t   instance_id,
    uint8_t   merkle_root_le[32],
    int64_t*  wb_id_out,
    uint32_t* version_out,
    uint32_t* ntime_out,
    uint32_t* nbits_out,
    uint8_t   prevhash_out[32]);

/* Tip fields for JD candidate-block assembly (no client required).
 * prevhash_header is in Bitcoin header internal byte order (ready to memcpy
 * into header[4..36]). */
bool stratifier_sv2_tip_for_jd(
    uint32_t* version_out,
    uint32_t* ntime_out,
    uint32_t* nbits_out,
    uint8_t   prevhash_header[32]);

/* Account a share whose PoW hash was already validated by the caller (e.g.
 * custom-job path). hash is the double-SHA256 share hash; sdiff its difficulty.
 * wb_id is used for duplicate-share tracking (0 = current tip).
 * If network_diff_met is non-NULL, set when sdiff meets network difficulty. */
bool stratifier_sv2_account_share(
    int64_t             instance_id,
    int64_t             workbase_id,
    const unsigned char hash[32],
    double              sdiff,
    char*               errbuf,
    size_t              errbufsz,
    bool*               network_diff_met);

/* Submit a fully serialised block found via JD PushSolution / custom solve.
 * block is header+txs (Bitcoin wire format). Returns true if node accepted.
 * instance_id: mining session for "Solved by …" attribution (0 if unknown).
 * workername_opt: override worker label (e.g. JD token user); may be NULL. */
bool stratifier_sv2_submit_block_bin(
    const unsigned char* block,
    size_t               block_len,
    int64_t              instance_id,
    const char*          workername_opt);

/* Current network difficulty from tip workbase (0 if none). */
double stratifier_sv2_network_diff(void);

/* Height of the block being mined from tip workbase (0 if none). */
int stratifier_sv2_tip_height(void);

/* Update share-accounting difficulty for an SV2 virtual session (int64 ceil). */
void stratifier_sv2_set_diff(int64_t instance_id, double diff);

#endif /* HAVE_SV2 */
#endif /* SV2_WORK_H */
