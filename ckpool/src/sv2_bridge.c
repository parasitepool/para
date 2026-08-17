/*
 * Copyright 2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

/* Work-source arbiter helpers. See sv2_bridge.h. */

#include "config.h"

#include <string.h>

#include "sv2_bridge.h"

void sv2_tip_push(struct sv2_tip_ring* r, const uint8_t hash[32], uint32_t nbits, uint32_t ntime) {
    if (!r || !hash)
        return;
    /* A fee-bump template on the same tip refreshes nothing here. */
    if (r->count && !memcmp(r->hash[r->newest], hash, 32))
        return;
    r->newest = r->count ? (r->newest + 1) % SV2_TIP_RING : 0;
    memcpy(r->hash[r->newest], hash, 32);
    r->nbits[r->newest] = nbits;
    r->ntime[r->newest] = ntime;
    if (r->count < SV2_TIP_RING)
        r->count++;
}

bool sv2_tip_newest(const struct sv2_tip_ring* r, uint8_t out[32]) {
    if (!r || !r->count)
        return false;
    memcpy(out, r->hash[r->newest], 32);
    return true;
}

enum sv2_tip_rel sv2_tip_relation(const struct sv2_tip_ring* r, const uint8_t pool_prev[32]) {
    int i;

    if (!r || !r->count || !pool_prev)
        return SV2_TIP_NO_LOCAL;
    if (!memcmp(r->hash[r->newest], pool_prev, 32))
        return SV2_TIP_SAME;
    /* Anything older we recorded is a tip our node has already left. */
    for (i = 1; i < r->count; i++) {
        int idx = (r->newest - i + SV2_TIP_RING) % SV2_TIP_RING;

        if (!memcmp(r->hash[idx], pool_prev, 32))
            return SV2_TIP_BEHIND;
    }
    return SV2_TIP_AHEAD;
}

enum sv2_tip_reject sv2_tip_plausible(const struct sv2_tip_ring* r, uint32_t pool_nbits, uint32_t pool_ntime) {
    uint32_t local_nbits, local_ntime;

    if (!r || !r->count)
        return SV2_TIP_OK;
    local_nbits = r->nbits[r->newest];
    local_ntime = r->ntime[r->newest];
    /*
     * Both are the bits for the block being worked on, so they agree on any
     * tip a block apart — except across a retarget, where a false reject only
     * costs the bridge label, never the pool work itself.
     */
    if (pool_nbits != local_nbits)
        return SV2_TIP_NBITS;
    /* 64 bit arithmetic: either timestamp may be near the u32 ceiling. */
    if ((uint64_t)pool_ntime > (uint64_t)local_ntime + SV2_TIP_NTIME_SKEW_SECS)
        return SV2_TIP_NTIME_AHEAD;
    if ((uint64_t)local_ntime > (uint64_t)pool_ntime + SV2_TIP_NTIME_SKEW_SECS)
        return SV2_TIP_NTIME_BEHIND;
    return SV2_TIP_OK;
}

const char* sv2_tip_rel_str(enum sv2_tip_rel rel) {
    switch (rel) {
        case SV2_TIP_NO_LOCAL:
            return "no local tip";
        case SV2_TIP_SAME:
            return "our tip";
        case SV2_TIP_BEHIND:
            return "behind us";
        case SV2_TIP_AHEAD:
            return "ahead of us";
    }
    return "unknown";
}

const char* sv2_tip_reject_str(enum sv2_tip_reject rej) {
    switch (rej) {
        case SV2_TIP_OK:
            return "plausible";
        case SV2_TIP_NBITS:
            return "different nbits to our tip";
        case SV2_TIP_NTIME_AHEAD:
            return "min_ntime far ahead of our template";
        case SV2_TIP_NTIME_BEHIND:
            return "min_ntime far behind our template";
    }
    return "unknown";
}

const char* sv2_work_src_str(enum sv2_work_src src) {
    switch (src) {
        case SV2_WS_POOL:
            return "POOL";
        case SV2_WS_LOCAL_JD:
            return "LOCAL_JD";
        case SV2_WS_BRIDGE:
            return "POOL_BRIDGE";
    }
    return "unknown";
}
