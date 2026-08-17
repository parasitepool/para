/*
 * Copyright 2026 Con Kolivas
 *
 * Stratum V2 Job Declaration Client (JDC) — ckproxy side.
 *
 * Templates come from the Bitcoin Core mining IPC (createNewBlock + getBlock +
 * waitTipChanged) and are declared to the pool's JDS over a second Noise
 * session. An accepted declare becomes a SetCustomMiningJob on the mining
 * connection, and its job_id then carries downstream work and shares.
 *
 * Declaring needs the pool to be on the same tip we are, so this side also keeps
 * the local tip history the work-source arbiter classifies pool tips against;
 * the generator owns the state machine itself, since it
 * owns the downstream fanout.
 */

#ifndef SV2_JDC_H
#define SV2_JDC_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "sv2_bridge.h"

/* Mirrors of the mining IPC bounds, so this header does not drag in
 * mining_ipc.h (and with it Cap'n Proto) for every consumer. */
#define SV2_JDC_MAX_SCRIPTSIG 128
#define SV2_JDC_MAX_OUTPUT 128
/* notify_instance_t.merklehash is fixed at 16 entries (SV1 parity), so a
 * template needing more steps can never be fanned downstream. */
#define SV2_JDC_MAX_MERKLES 16
/* Same ceiling the JDS applies to a candidate block (SV2_JD_MAX_BLOCK_BYTES):
 * a template above it could never be declared, so it is refused on arrival. */
#define SV2_JDC_MAX_BLOCK_BYTES (4u << 20)
/* Seconds between fee-bump template refreshes on an unchanged tip. */
#define SV2_JDC_REFRESH_SECS 30
/* Floor between waitTipChanged calls that return the same tip immediately (see
 * jdc_tipwaiter): small enough that tip detection stays prompt if the wait is
 * not blocking, large enough not to spin on the IPC socket. */
#define SV2_JDC_TIP_POLL_MS 200
/* Give up on a declare the JDS never answers. Its checkBlock is a full
 * ConnectBlock and may queue behind others, so allow generously; the cost of
 * waiting is only that the next template cannot declare yet. */
#define SV2_JDC_DECLARE_TIMEOUT_SECS 30
/* Attempts on one template before waiting for the next instead. Bounded so a
 * template the JDS keeps refusing cannot spend its checkBlock budget, while
 * still leaving room for a tip race or a momentarily busy pool to clear. */
#define SV2_JDC_DECLARE_MAX_ATTEMPTS 4
/* Back-off after a rate-limited declare: the JDS counts per minute, so a
 * prompt retry would only be refused again. */
#define SV2_JDC_DECLARE_RATELIMIT_SECS 20
/*
 * Drop a JD session that has gone silent while we were waiting on it. A JD
 * connection is legitimately quiet when there is nothing to declare, so this
 * only applies once we have sent something and had nothing back — comfortably
 * more than one declare timeout, so a slow checkBlock is not mistaken for a
 * dead peer. Without it a JDS that stops reading (rather than closing) is
 * indistinguishable from a slow one, and declares vanish into the socket
 * forever; the mining connection has the same guard at 90s.
 */
#define SV2_JDC_SILENCE_SECS 75
/*
 * Give up on a SetCustomMiningJob the pool never answers. Shorter than a
 * declare timeout: the pool does no validation work here (the declare already
 * paid for that), so a reply is a bookkeeping round trip.
 */
#define SV2_JDC_CUSTOM_TIMEOUT_SECS 15
/*
 * Pace the re-declare after a SetCustomMiningJob that failed for a reason on our
 * side of the wire — the channel is not current, its extranonce layout moved, or
 * the send failed. None of those clear within a second, and the declare it would
 * repeat costs the pool a checkBlock.
 */
#define SV2_JDC_CUSTOM_RETRY_SECS 5

/* One non-coinbase transaction of a template. raw points into the template's
 * serialised block, so it is valid exactly as long as the template is. */
struct sv2_jdc_tx {
    const uint8_t* raw;
    uint32_t       len;
    uint8_t        txid[32];
    uint8_t        wtxid[32];
};

/*
 * A local block template, ready to be turned into a DeclareMiningJob and a
 * SetCustomMiningJob.
 *
 * Reference counted. The store holds one reference to the current and previous
 * template; anything that must outlive store rotation — a live custom job in
 * the proxy job ring, an in-flight solve — takes its own with
 * sv2_jdc_template_current() and drops it with sv2_jdc_template_put().
 */
struct sv2_jdc_template {
    /* Header fields, Bitcoin wire (header-internal) byte order. prev_hash
     * is directly comparable with a pool SetNewPrevHash / SV2 U256. */
    uint32_t version;
    uint32_t ntime;
    uint32_t nbits;
    uint8_t  prev_hash[32];
    /* As the template header carries it: zeroed, since the coinbase is not
     * final until we build one. Never a usable merkle root. */
    uint8_t merkle_root[32];
    int     height;

    /* Coinbase constraints. The scriptSig we build is
     * script_sig_prefix ‖ extranonce_prefix ‖ extranonce. */
    uint32_t cb_version;
    uint32_t cb_sequence;
    uint32_t cb_locktime;
    int64_t  reward; /* block_reward_remaining, split across the
                      * pool's payout outputs */
    uint8_t script_sig_prefix[SV2_JDC_MAX_SCRIPTSIG];
    uint8_t script_sig_prefix_len;

    /* Segwit commitment, when the template has one: the required output
     * verbatim (value ‖ scriptPubKey) plus the reserved value it was
     * computed over. Without a commitment the coinbase must be serialised
     * legacy — BIP141 forbids a coinbase witness on such a block. */
    bool    have_commitment;
    uint8_t commitment[SV2_JDC_MAX_OUTPUT];
    uint8_t commitment_len;
    uint8_t witness_reserved[32];

    /* Coinbase merkle branch, internal byte order. */
    int     merkles;
    uint8_t merkle_path[SV2_JDC_MAX_MERKLES][32];

    /* Non-coinbase transactions in template order: the wtxid_list of a
     * DeclareMiningJob and the source of ProvideMissingTransactions data. */
    int                txns;
    struct sv2_jdc_tx* txn;
    uint64_t           txn_bytes;

    /* Private to sv2_jdc.c. */
    uint64_t id; /* monotonic, for logs */
    int      refcount;
    uint8_t* block; /* serialised block backing txn[].raw */
    size_t   blocklen;
    void*    tmpl; /* mining_block_template * for submitSolution */
};

/*
 * Extranonce layout of a live SV2 mining channel, as the JD client needs it to
 * size the declared coinbase's extranonce hole. Filled by the generator, which
 * owns the channel.
 */
struct sv2_jdc_channel {
    bool     current; /* this proxy is supplying work now */
    uint32_t channel_id;
    uint8_t  extranonce_prefix[32];
    uint8_t  extranonce_prefix_len;
    uint16_t extranonce_size; /* rollable bytes the pool granted */
    uint16_t pad_len;         /* zero pad folded into coinb1 */
};

/*
 * Implemented in generator.c: the channel params of proxy entry proxy_id, or
 * false when it has no open SV2 extended channel. The full extranonce hole a
 * declare must leave is extranonce_prefix_len + extranonce_size.
 */
bool sv2_proxy_jd_channel(int proxy_id, struct sv2_jdc_channel* out);

/* True when any proxy entry configures a JD server, i.e. the JD client is
 * wanted. Safe to call before sv2_jdc_start(). */
bool sv2_jdc_configured(void);

/* Start the local template provider. Called once from the generator's
 * proxy_mode(); a no-op when JD is not configured or IPC is unavailable. */
void sv2_jdc_start(void);

/* Release the IPC connections on the shutdown path. */
void sv2_jdc_stop(void);

/* Current template with a reference held, or NULL when none is usable. */
struct sv2_jdc_template* sv2_jdc_template_current(void);

/* Take an additional reference on a template already held. Returns t. */
struct sv2_jdc_template* sv2_jdc_template_ref(struct sv2_jdc_template* t);

/* Drop a reference taken by sv2_jdc_template_current(). */
void sv2_jdc_template_put(struct sv2_jdc_template* t);

/*
 * One accepted declare, ready to become a SetCustomMiningJob. The JD client
 * builds this; the generator owns the mining connection and sends it.
 *
 * Every buffer is borrowed for the duration of the call — the generator copies
 * whatever it keeps, and takes its own template reference. The template supplies
 * the header fields, coinbase constraints and merkle path, so they are not
 * duplicated here.
 *
 * Two coinbase splits, because one transaction has two serialisations:
 *
 *  - lcb_* is the legacy (txid) form. The merkle root is over txids, so this is
 *    what downstream SV1 miners must hash and what the pool reconstructs to
 *    validate a share.
 *  - dcb_* is the declared form, which carries the BIP141 marker/flag and the
 *    coinbase witness when the template has a commitment. Only a block submit
 *    uses it.
 *
 * Both leave the same hole_len-byte extranonce hole, so the txid of either
 * assembled coinbase is the same (asserted by test/sv2_cb.c).
 */
struct sv2_jdc_custom_job {
    int                      proxy_id;
    struct sv2_jdc_template* t;
    const uint8_t*           token;
    uint8_t                  token_len;
    const uint8_t*           outputs; /* coinbase outputs, both splits share it */
    uint16_t                 outputs_len;
    const uint8_t *          lcb_prefix, *lcb_suffix;
    uint16_t                 lcb_prefix_len, lcb_suffix_len;
    const uint8_t *          dcb_prefix, *dcb_suffix;
    uint16_t                 dcb_prefix_len, dcb_suffix_len;
    uint16_t                 hole_len; /* extranonce_prefix_len + extranonce_size */
};

/*
 * A share on a custom job that met the network target. The mining connection
 * has already carried the share upstream; this covers the two solve paths that
 * do not depend on the pool rebuilding the block.
 */
struct sv2_jdc_solution {
    struct sv2_jdc_template* t;        /* pinned template this job was declared from */
    const uint8_t*           coinbase; /* declared-form coinbase, hole filled */
    size_t                   coinbase_len;
    const uint8_t*           extranonce; /* full hole contents: EP ‖ pad ‖ en1var ‖ en2 */
    uint8_t                  extranonce_len;
    uint8_t                  prev_hash[32]; /* header-internal order */
    uint32_t                 version, ntime, nonce, nbits;
};

/*
 * Implemented in generator.c: send a SetCustomMiningJob for cj on the mining
 * connection of its proxy entry. False when there is no usable channel or the
 * send failed; the custom job only goes downstream once the pool answers with
 * Success (see sv2_jdc_custom_result).
 */
bool sv2_proxy_set_custom_job(const struct sv2_jdc_custom_job* cj);

/* Generator → JD client: the pool's answer to a SetCustomMiningJob. code is the
 * SV2 error code when !ok, NULL on success. */
void sv2_jdc_custom_result(bool ok, uint32_t job_id, const char* code);

/* Generator → JD client: custom work already live has been discarded (the
 * channel's extranonce layout changed), so the current template needs declaring
 * again from scratch. why appears in the log. */
void sv2_jdc_custom_dropped(const char* why);

/* Generator → JD client: PushSolution upstream and submit the block locally. */
void sv2_jdc_solved(const struct sv2_jdc_solution* sol);

/*
 * Generator → JD client: proxy entry proxy_id's pool announced tip prev (SV2
 * header-internal order) with nbits and min_ntime from its SetNewPrevHash.
 *
 * Returns where that tip sits relative to our own chain, so the caller can run
 * the work-source state machine, and sets *why to a plausibility complaint when
 * the pool has moved to a tip that cannot be a neighbour of ours (NULL
 * otherwise). Declaring is gated on the answer being SV2_TIP_SAME, and a pool
 * that has just caught up to us is declared to immediately rather than on the
 * session thread's next idle tick.
 *
 * A proxy entry with no JD session of its own always answers SV2_TIP_NO_LOCAL.
 */
enum sv2_tip_rel
sv2_jdc_pool_tip(int proxy_id, const uint8_t prev[32], uint32_t nbits, uint32_t min_ntime, const char** why);

/*
 * Generator → JD client: proxy entry proxy_id's mining channel is gone, so what
 * it last announced says nothing about the pool's tip now. Without this a
 * reconnect could declare against a tip remembered from the old session. Also
 * drops the local stamp the tip latency report pairs against, which only has
 * meaning as the other half of a race with this connection.
 */
void sv2_jdc_pool_tip_clear(int proxy_id);

#endif /* SV2_JDC_H */
