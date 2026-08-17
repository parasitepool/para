/*
 * Copyright 2026 Con Kolivas
 *
 * Coinbase construction shared by the SV2 Job Declaration server and client.
 * See sv2_cb.h.
 */

#include "config.h"

#include <string.h>

#include "libckpool.h"
#include "sv2_cb.h"
#include "sv2_tx.h"

size_t sv2_cb_assemble(
    uint8_t*       out,
    size_t         outsz,
    uint32_t       cb_version,
    uint32_t       nsequence,
    uint32_t       locktime,
    const uint8_t* ssig_prefix,
    uint8_t        ssig_prefix_len,
    const uint8_t* en1,
    uint8_t        en1_len,
    const uint8_t* extranonce,
    uint16_t       extranonce_len,
    const uint8_t* outputs,
    uint16_t       outputs_len) {
    uint8_t* p = out;
    uint32_t le;
    size_t   ssig_len = (size_t)ssig_prefix_len + en1_len + extranonce_len;
    size_t   need;

    if (ssig_len > 255)
        return 0;
    need = 4 + 1 + 36 + 1 + ssig_len + 4 + outputs_len + 4;
    if (need > outsz)
        return 0;

    le = htole32(cb_version);
    memcpy(p, &le, 4);
    p += 4;
    *p++ = 0x01;      /* one input */
    memset(p, 0, 32); /* null prevout hash */
    p += 32;
    memset(p, 0xff, 4); /* prevout index */
    p += 4;
    *p++ = (uint8_t)ssig_len;
    if (ssig_prefix_len) {
        memcpy(p, ssig_prefix, ssig_prefix_len);
        p += ssig_prefix_len;
    }
    if (en1_len) {
        memcpy(p, en1, en1_len);
        p += en1_len;
    }
    if (extranonce_len) {
        memcpy(p, extranonce, extranonce_len);
        p += extranonce_len;
    }
    le = htole32(nsequence);
    memcpy(p, &le, 4);
    p += 4;
    if (outputs_len) {
        memcpy(p, outputs, outputs_len);
        p += outputs_len;
    }
    le = htole32(locktime);
    memcpy(p, &le, 4);
    p += 4;
    return (size_t)(p - out);
}

bool sv2_cb_declare_parts(
    const struct sv2_cb_spec* spec,
    uint8_t**                 prefix,
    uint16_t*                 prefix_len,
    uint8_t**                 suffix,
    uint16_t*                 suffix_len) {
    size_t   ssig_len, plen, slen;
    uint8_t* p;
    uint32_t le;

    *prefix = NULL;
    *suffix = NULL;
    *prefix_len = 0;
    *suffix_len = 0;
    if (!spec || !spec->outputs || !spec->outputs_len)
        return false;
    /* No extranonce hole means nothing for the pool and the miner to roll:
     * an extended channel always grants some, so this is a caller bug. */
    if (!spec->hole_len)
        return false;
    ssig_len = (size_t)spec->ssig_prefix_len + spec->hole_len;
    /* A coinbase over the consensus scriptSig limit can never be a valid
     * block, so refuse here rather than have the pool's checkBlock say so. */
    if (ssig_len > SV2_CB_MAX_SCRIPTSIG)
        return false;

    /*
     * prefix: version [marker flag] 01 <null prevout> CompactSize(ssig_len)
     *         <our scriptSig bytes>, stopping where the extranonce begins.
     */
    plen = 4 + (spec->witness_reserved ? 2 : 0) + 1 + 36 + sv2_compact_size_len(ssig_len) + spec->ssig_prefix_len;
    /* suffix: nSequence outputs [witness stack] locktime */
    slen = 4 + spec->outputs_len + (spec->witness_reserved ? 2 + 32 : 0) + 4;
    if (plen > UINT16_MAX || slen > UINT16_MAX)
        return false;

    p = ckalloc(plen);
    *prefix = p;
    le = htole32(spec->version);
    memcpy(p, &le, 4);
    p += 4;
    if (spec->witness_reserved) {
        *p++ = 0x00; /* BIP141 marker */
        *p++ = 0x01; /* BIP141 flag */
    }
    *p++ = 0x01; /* one input */
    memset(p, 0, 32);
    p += 32;
    memset(p, 0xff, 4);
    p += 4;
    sv2_write_compact_size(&p, ssig_len);
    if (spec->ssig_prefix_len) {
        memcpy(p, spec->ssig_prefix, spec->ssig_prefix_len);
        p += spec->ssig_prefix_len;
    }
    *prefix_len = (uint16_t)(p - *prefix);

    p = ckalloc(slen);
    *suffix = p;
    le = htole32(spec->nsequence);
    memcpy(p, &le, 4);
    p += 4;
    memcpy(p, spec->outputs, spec->outputs_len);
    p += spec->outputs_len;
    if (spec->witness_reserved) {
        *p++ = 0x01; /* one witness stack item */
        *p++ = 0x20; /* 32 bytes */
        memcpy(p, spec->witness_reserved, 32);
        p += 32;
    }
    le = htole32(spec->locktime);
    memcpy(p, &le, 4);
    p += 4;
    *suffix_len = (uint16_t)(p - *suffix);

    return true;
}

/*
 * Consume one consensus output at *p, bounds checked against end, advancing *p
 * past it. An empty scriptPubKey is treated as malformed: it can pay nobody, so
 * a pool asking us to fund one is not something we can honour.
 */
static bool
read_output(const uint8_t** p, const uint8_t* end, uint64_t* value, const uint8_t** script, uint8_t* script_len) {
    uint64_t slen, le64;

    if (!sv2_have_bytes(*p, end, 8))
        return false;
    memcpy(&le64, *p, 8);
    *p += 8;
    if (!sv2_read_compact_size(p, end, &slen) || slen > 255 || !sv2_have_bytes(*p, end, slen) || !slen)
        return false;
    if (value)
        *value = le64toh(le64);
    if (script)
        *script = *p;
    if (script_len)
        *script_len = (uint8_t)slen;
    *p += slen;
    return true;
}

/*
 * Walk the whole blob. rest spans outputs 1..n-1 as they will be copied, and
 * rest_value totals what they claim — the part of the reward already spoken
 * for before the first output takes what is left.
 */
static bool walk_outputs(
    const uint8_t*  outputs,
    uint16_t        olen,
    uint64_t*       nout,
    const uint8_t** first_script,
    uint8_t*        first_len,
    const uint8_t** rest,
    size_t*         rest_len,
    uint64_t*       rest_value) {
    const uint8_t *p, *end, *from;
    uint64_t       i, total = 0;

    if (!outputs || !olen)
        return false;
    p = outputs;
    end = outputs + olen;
    if (!sv2_read_compact_size(&p, end, nout) || *nout < 1 || *nout > SV2_MAX_PAYOUT_OUTS)
        return false;
    if (!read_output(&p, end, NULL, first_script, first_len))
        return false;
    from = p;
    for (i = 1; i < *nout; i++) {
        uint64_t v = 0;

        if (!read_output(&p, end, &v, NULL, NULL))
            return false;
        if (v > UINT64_MAX - total)
            return false;
        total += v;
    }
    if (rest)
        *rest = from;
    if (rest_len)
        *rest_len = (size_t)(p - from);
    if (rest_value)
        *rest_value = total;
    return true;
}

size_t sv2_cb_build_outputs(
    uint8_t*       out,
    size_t         outsz,
    const uint8_t* payouts,
    uint16_t       payouts_len,
    uint64_t       value,
    const uint8_t* commitment,
    uint8_t        commitment_len) {
    const uint8_t *first_script, *rest;
    uint8_t*       p = out;
    uint64_t       nout, total, le64, fixed = 0;
    uint8_t        first_len;
    size_t         rest_len, need;

    if (!walk_outputs(payouts, payouts_len, &nout, &first_script, &first_len, &rest, &rest_len, &fixed))
        return 0;
    /*
     * value is the most this coinbase may claim in total, so the outputs the
     * pool fixed come out of it rather than being added on top: paying it in
     * full to the first output and the fixed ones as well over-claims by
     * exactly their sum, which is a coinbase no node will accept. A pool
     * asking for more than the block is worth cannot be honoured at all.
     */
    if (fixed > value)
        return 0;
    total = nout + (commitment_len ? 1 : 0);
    need = sv2_compact_size_len(total) + 8 + sv2_compact_size_len(first_len) + first_len + rest_len + commitment_len;
    if (need > outsz)
        return 0;

    sv2_write_compact_size(&p, total);
    /*
     * Whatever the pool stated on its first output is replaced, not added to:
     * that slot is where the remainder lands.
     */
    le64 = htole64(value - fixed);
    memcpy(p, &le64, 8);
    p += 8;
    sv2_write_compact_size(&p, first_len);
    memcpy(p, first_script, first_len);
    p += first_len;
    memcpy(p, rest, rest_len);
    p += rest_len;
    /*
     * The commitment output is copied whole (its own value and script), and
     * that value is zero, so it is not diverted reward under the pool's
     * payout rule.
     */
    if (commitment_len) {
        memcpy(p, commitment, commitment_len);
        p += commitment_len;
    }
    return (size_t)(p - out);
}

int sv2_cb_outputs_count(const uint8_t* outputs, uint16_t olen) {
    const uint8_t* first_script;
    uint8_t        first_len;
    uint64_t       nout;

    if (!walk_outputs(outputs, olen, &nout, &first_script, &first_len, NULL, NULL, NULL))
        return -1;
    return (int)nout;
}

bool sv2_cb_output_at(
    const uint8_t*  outputs,
    uint16_t        olen,
    int             idx,
    uint64_t*       value,
    const uint8_t** script,
    uint8_t*        script_len) {
    const uint8_t *p, *end;
    uint64_t       nout;
    int            i;

    if (script)
        *script = NULL;
    if (script_len)
        *script_len = 0;
    if (!outputs || !olen || idx < 0)
        return false;
    p = outputs;
    end = outputs + olen;
    if (!sv2_read_compact_size(&p, end, &nout) || nout < 1 || nout > SV2_MAX_PAYOUT_OUTS || (uint64_t)idx >= nout)
        return false;
    for (i = 0; i < idx; i++) {
        if (!read_output(&p, end, NULL, NULL, NULL))
            return false;
    }
    return read_output(&p, end, value, script, script_len);
}

bool sv2_cb_first_output_script(const uint8_t* outputs, uint16_t olen, const uint8_t** script, uint8_t* script_len) {
    return sv2_cb_output_at(outputs, olen, 0, NULL, script, script_len);
}
