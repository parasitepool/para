/*
 * Copyright 2026 Con Kolivas
 *
 * Bitcoin serialisation helpers shared by the SV2 Job Declaration server and
 * client. See sv2_tx.h.
 */

#include "config.h"

#include <string.h>

#include "libckpool.h"
#include "sv2_tx.h"

void sv2_write_compact_size(uint8_t** p, uint64_t n) {
    if (n < 0xfd) {
        *(*p)++ = (uint8_t)n;
    } else if (n <= 0xffff) {
        *(*p)++ = 0xfd;
        *(*p)++ = (uint8_t)(n & 0xff);
        *(*p)++ = (uint8_t)((n >> 8) & 0xff);
    } else if (n <= 0xffffffffu) {
        *(*p)++ = 0xfe;
        *(*p)++ = (uint8_t)(n & 0xff);
        *(*p)++ = (uint8_t)((n >> 8) & 0xff);
        *(*p)++ = (uint8_t)((n >> 16) & 0xff);
        *(*p)++ = (uint8_t)((n >> 24) & 0xff);
    } else {
        *(*p)++ = 0xff;
        memcpy(*p, &n, 8);
        *p += 8;
    }
}

size_t sv2_compact_size_len(uint64_t n) {
    if (n < 0xfd)
        return 1;
    if (n <= 0xffff)
        return 3;
    if (n <= 0xffffffffu)
        return 5;
    return 9;
}

bool sv2_read_compact_size(const uint8_t** pp, const uint8_t* end, uint64_t* n) {
    const uint8_t* p = *pp;
    uint8_t        b;

    if (p >= end)
        return false;
    b = *p++;
    if (b < 0xfd) {
        *n = b;
    } else if (b == 0xfd) {
        if (p + 2 > end)
            return false;
        *n = (uint64_t)p[0] | ((uint64_t)p[1] << 8);
        p += 2;
    } else if (b == 0xfe) {
        if (p + 4 > end)
            return false;
        *n = (uint64_t)p[0] | ((uint64_t)p[1] << 8) | ((uint64_t)p[2] << 16) | ((uint64_t)p[3] << 24);
        p += 4;
    } else {
        if (p + 8 > end)
            return false;
        memcpy(n, p, 8);
        p += 8;
    }
    *pp = p;
    return true;
}

/*
 * Single transaction walker behind both sv2_bitcoin_txid() and
 * sv2_bitcoin_tx_parse(). Walks version, optional BIP144 marker/flag, inputs,
 * outputs, witnesses and locktime, reporting the boundaries the callers need:
 *   *vin_start_out .. *vout_end_out  the non-witness body (inputs + outputs)
 *   *locktime_out                    where the 4 locktime bytes begin
 *   *tx_len_out                      total serialised length
 *   *witness_out                     whether marker/flag were present
 */
static bool tx_walk(
    const uint8_t*  tx,
    size_t          max,
    bool*           witness_out,
    const uint8_t** vin_start_out,
    const uint8_t** vout_end_out,
    const uint8_t** locktime_out,
    size_t*         tx_len_out) {
    const uint8_t *p = tx, *end = tx + max;
    const uint8_t *vin_start, *vout_end;
    uint64_t       i, nin, nout, n;
    bool           witness = false;

    if (max < 10)
        return false;
    /* version */
    p += 4;
    if (p + 2 <= end && p[0] == 0x00 && p[1] != 0x00) {
        witness = true;
        p += 2;
    }
    vin_start = p;
    if (!sv2_read_compact_size(&p, end, &nin) || nin > 100000 || !nin)
        return false;
    for (i = 0; i < nin; i++) {
        if (!sv2_have_bytes(p, end, 36))
            return false;
        p += 36; /* prevout */
        if (!sv2_read_compact_size(&p, end, &n) || n > UINT64_MAX - 4 || !sv2_have_bytes(p, end, n + 4))
            return false;
        p += n + 4; /* scriptSig + sequence */
    }
    if (!sv2_read_compact_size(&p, end, &nout) || nout > 100000)
        return false;
    for (i = 0; i < nout; i++) {
        if (!sv2_have_bytes(p, end, 8))
            return false;
        p += 8;
        if (!sv2_read_compact_size(&p, end, &n) || !sv2_have_bytes(p, end, n))
            return false;
        p += n;
    }
    vout_end = p;
    if (witness) {
        for (i = 0; i < nin; i++) {
            uint64_t nstack, j, sz;

            if (!sv2_read_compact_size(&p, end, &nstack) || nstack > 10000)
                return false;
            for (j = 0; j < nstack; j++) {
                if (!sv2_read_compact_size(&p, end, &sz) || !sv2_have_bytes(p, end, sz))
                    return false;
                p += sz;
            }
        }
    }
    if (!sv2_have_bytes(p, end, 4))
        return false;
    if (witness_out)
        *witness_out = witness;
    if (vin_start_out)
        *vin_start_out = vin_start;
    if (vout_end_out)
        *vout_end_out = vout_end;
    if (locktime_out)
        *locktime_out = p;
    if (tx_len_out)
        *tx_len_out = (size_t)(p - tx) + 4;
    return true;
}

/* SHA256d over version || vin || vout || locktime, skipping any witness. */
static void hash_nonwitness(
    const uint8_t* tx,
    const uint8_t* vin_start,
    const uint8_t* vout_end,
    const uint8_t* locktime,
    uint8_t        txid_out[32]) {
    size_t   body_len = (size_t)(vout_end - vin_start);
    size_t   nw_len = 4 + body_len + 4;
    uint8_t* nw = ckalloc(nw_len);
    uint8_t* q = nw;

    memcpy(q, tx, 4);
    q += 4;
    memcpy(q, vin_start, body_len);
    q += body_len;
    memcpy(q, locktime, 4);
    gen_hash(nw, txid_out, (int)nw_len);
    dealloc(nw);
}

bool sv2_bitcoin_txid(const uint8_t* tx, size_t len, uint8_t txid_out[32]) {
    const uint8_t *vin_start, *vout_end, *locktime;
    bool           witness = false;

    if (!tx || !txid_out)
        return false;
    if (!tx_walk(tx, len, &witness, &vin_start, &vout_end, &locktime, NULL))
        return false;
    if (!witness) {
        gen_hash((uchar*)tx, txid_out, (int)len);
        return true;
    }
    hash_nonwitness(tx, vin_start, vout_end, locktime, txid_out);
    return true;
}

bool sv2_bitcoin_tx_parse(const uint8_t* tx, size_t max, size_t* tx_len, uint8_t txid_out[32], uint8_t wtxid_out[32]) {
    const uint8_t *vin_start, *vout_end, *locktime;
    bool           witness = false;
    size_t         len = 0;

    if (!tx)
        return false;
    if (!tx_walk(tx, max, &witness, &vin_start, &vout_end, &locktime, &len))
        return false;
    if (tx_len)
        *tx_len = len;
    /*
     * wtxid is the hash of the full serialisation; for a legacy transaction
     * that is also the txid, so hash once and copy (BIP141).
     */
    if (wtxid_out)
        gen_hash((uchar*)tx, wtxid_out, (int)len);
    if (txid_out) {
        if (!witness) {
            if (wtxid_out)
                memcpy(txid_out, wtxid_out, 32);
            else
                gen_hash((uchar*)tx, txid_out, (int)len);
        } else
            hash_nonwitness(tx, vin_start, vout_end, locktime, txid_out);
    }
    return true;
}

void sv2_merkle_root_from_txids(uint8_t (*txids)[32], int n, uint8_t root[32]) {
    uint8_t *layer, *next;
    int      count = n, i;

    if (n < 1) {
        memset(root, 0, 32);
        return;
    }
    layer = ckalloc((size_t)n * 32);
    memcpy(layer, txids, (size_t)n * 32);
    while (count > 1) {
        int next_count = (count + 1) / 2;

        next = ckalloc((size_t)next_count * 32);
        for (i = 0; i < next_count; i++) {
            uint8_t pair[64];
            int     a = i * 2, b = a + 1;

            memcpy(pair, layer + a * 32, 32);
            if (b < count)
                memcpy(pair + 32, layer + b * 32, 32);
            else
                memcpy(pair + 32, layer + a * 32, 32);
            gen_hash(pair, next + i * 32, 64);
        }
        dealloc(layer);
        layer = next;
        count = next_count;
    }
    memcpy(root, layer, 32);
    dealloc(layer);
}

void sv2_merkle_root_from_path(
    const uint8_t coinbase_txid[32],
    const uint8_t (*path)[32],
    int     steps,
    uint8_t root[32]) {
    uint8_t pair[64], hash[32];
    int     i;

    memcpy(pair, coinbase_txid, 32);
    for (i = 0; i < steps; i++) {
        memcpy(pair + 32, path[i], 32);
        gen_hash(pair, hash, 64);
        memcpy(pair, hash, 32);
    }
    memcpy(root, pair, 32);
}

double sv2_diff_from_nbits(uint32_t nbits) {
    uint32_t be = htobe32(nbits);
    uint8_t  bytes[4];

    memcpy(bytes, &be, sizeof(bytes));
    return diff_from_nbits((char*)bytes);
}
