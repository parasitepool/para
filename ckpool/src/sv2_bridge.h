/*
 * Copyright 2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

/*
 * Work-source arbiter helpers for the SV2 job declaration client: how a tip
 * the pool announces relates to our own chain, and whether it is plausible
 * enough to expect our node to catch up to it.
 *
 * Ported from sv2-apps miner-apps/jd-client/src/lib/channel_manager/tip_bridge.rs
 * at commit 8ce74f74("Merge branch 'jd-accept-remote-work' into jd-ckp"), keeping
 * its classification so the two implementations stay comparable. Pure: no locks
 * and no globals, so the caller owns both the ring and the state machine, and
 * test/sv2_bridge.c can exercise every branch.
 */

#ifndef SV2_BRIDGE_H
#define SV2_BRIDGE_H

#include <stdbool.h>
#include <stdint.h>

/*
 * Local tips remembered. Deep enough to tell "the pool is a few blocks behind
 * us" from "the pool is on a chain we have never seen", shallow enough that a
 * pool lagging further than this is treated as ahead — which only costs a bridge
 * we would enter anyway, since pool work is what the pool credits.
 */
#define SV2_TIP_RING 8

/* How far a pool min_ntime may differ from our tip's template ntime. */
#define SV2_TIP_NTIME_SKEW_SECS 7200

/* Bounded history of tips we have built a local template on. */
struct sv2_tip_ring {
    uint8_t  hash[SV2_TIP_RING][32];
    uint32_t nbits[SV2_TIP_RING];
    uint32_t ntime[SV2_TIP_RING];
    int      count;  /* entries held, up to SV2_TIP_RING */
    int      newest; /* index of the most recent entry */
};

/* How a pool tip relates to the ring. */
enum sv2_tip_rel {
    SV2_TIP_NO_LOCAL, /* no local tip yet: nothing to compare */
    SV2_TIP_SAME,     /* the pool is on our tip: declarable */
    SV2_TIP_BEHIND,   /* a tip of ours our node has moved past */
    SV2_TIP_AHEAD,    /* a tip we have never had: the pool leads */
};

/* Why a pool tip is not plausibly one our node can catch up to. */
enum sv2_tip_reject {
    SV2_TIP_OK,
    SV2_TIP_NBITS,        /* different difficulty bits to our tip */
    SV2_TIP_NTIME_AHEAD,  /* min_ntime far beyond our template's */
    SV2_TIP_NTIME_BEHIND, /* min_ntime far before our template's */
};

/* Work-source multiplexer states. */
enum sv2_work_src {
    SV2_WS_POOL,     /* pool jobs: no JD, or nothing declarable */
    SV2_WS_LOCAL_JD, /* our declared custom work is live */
    SV2_WS_BRIDGE,   /* pool jobs for a tip our node lacks */
};

/*
 * Bounded window to spend on pool work while waiting for our node to reach the
 * pool's tip. Nothing downstream changes when it expires — pool work is always
 * what the pool credits — but a node that has not caught up in this long is
 * stuck rather than racing, and the operator should hear about it.
 */
#define SV2_BRIDGE_TIMEOUT_SECS 30

/*
 * Record a local tip. A repeat of the newest entry is ignored, so fee-bump
 * templates on an unchanged tip do not consume the ring.
 */
void sv2_tip_push(struct sv2_tip_ring* r, const uint8_t hash[32], uint32_t nbits, uint32_t ntime);

/* Our current tip, or false when none has been recorded. */
bool sv2_tip_newest(const struct sv2_tip_ring* r, uint8_t out[32]);

/* Where a pool tip sits relative to our chain. Both hashes are header-internal
 * order, as an SV2 U256 and a template header carry them. */
enum sv2_tip_rel sv2_tip_relation(const struct sv2_tip_ring* r, const uint8_t pool_prev[32]);

/*
 * Cheap sanity checks on a pool tip we have never seen, against our newest one:
 * not a header proof, only a filter for tips that cannot be a sibling or child
 * of ours. Passes vacuously when the ring is empty.
 *
 * pool_ntime is a SetNewPrevHash min_ntime and the ring's is the ntime of the
 * template we built on that tip — both proposals for the next block's timestamp,
 * so they are comparable within the skew window.
 */
enum sv2_tip_reject sv2_tip_plausible(const struct sv2_tip_ring* r, uint32_t pool_nbits, uint32_t pool_ntime);

const char* sv2_tip_rel_str(enum sv2_tip_rel rel);
const char* sv2_tip_reject_str(enum sv2_tip_reject rej);
const char* sv2_work_src_str(enum sv2_work_src src);

#endif /* SV2_BRIDGE_H */
