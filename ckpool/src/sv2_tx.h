/*
 * Copyright 2026 Con Kolivas
 *
 * Bitcoin serialisation helpers shared by the SV2 Job Declaration server
 * (sv2_jd.c) and client (sv2_jdc.c): CompactSize, transaction walking,
 * txid/wtxid, and merkle roots.
 *
 * All readers take untrusted input and bounds check every field.
 */

#ifndef SV2_TX_H
#define SV2_TX_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/*
 * Safe remaining-buffer check: need may be a full CompactSize u64.
 * Never form p + need (pointer overflow UB on huge need).
 */
static inline bool sv2_have_bytes(const uint8_t* p, const uint8_t* end, uint64_t need) {
    if (!p || !end || p > end)
        return false;
    return need <= (uint64_t)(end - p);
}

/* Read compact size; advances *pp. Returns false on overflow/truncation. */
bool sv2_read_compact_size(const uint8_t** pp, const uint8_t* end, uint64_t* n);

/* Write compact size, advancing *p. Caller guarantees 9 bytes of room. */
void sv2_write_compact_size(uint8_t** p, uint64_t n);

/* Encoded length of a compact size, for sizing a buffer before writing. */
size_t sv2_compact_size_len(uint64_t n);

/*
 * Bitcoin txid = SHA256d of the non-witness serialisation.
 * Handles both legacy and BIP144 witness forms.
 */
bool sv2_bitcoin_txid(const uint8_t* tx, size_t len, uint8_t txid_out[32]);

/*
 * Walk one transaction at the head of a buffer of max bytes.
 *
 * On success returns true and stores the serialised length in *tx_len, so the
 * caller can advance to the next transaction in a block. txid_out (non-witness
 * hash) and wtxid_out (hash of the full serialisation, witness included) are
 * each filled when non-NULL; for a legacy transaction the two are identical,
 * which is what BIP141 specifies and what the JDS tx cache keys on.
 */
bool sv2_bitcoin_tx_parse(const uint8_t* tx, size_t max, size_t* tx_len, uint8_t txid_out[32], uint8_t wtxid_out[32]);

/* Merkle root over n leaf hashes, duplicating the last of an odd layer. */
void sv2_merkle_root_from_txids(uint8_t (*txids)[32], int n, uint8_t root[32]);

/*
 * Fold a coinbase txid up a merkle branch (each step hashed on the right) and
 * store the resulting root. This is the merkle_path form both SV2 and the
 * Bitcoin Core IPC BlockTemplate use.
 */
void sv2_merkle_root_from_path(const uint8_t coinbase_txid[32], const uint8_t (*path)[32], int steps, uint8_t root[32]);

/*
 * Network difficulty of an nbits value held as a host-order U32 — the form SV2
 * carries it in, and the form that goes little-endian into a wire header.
 *
 * libckpool's diff_from_nbits() reads its argument's *first* byte as the
 * exponent, so it wants the big-endian bytes (as the "%08x" string reads), not
 * the little-endian bytes of a wire header. Handing it the wire bytes yields an
 * exponent of the target's low byte and a difficulty near zero, which reads as
 * "every share is a block" rather than as an error. Use this instead of
 * converting by hand.
 */
double sv2_diff_from_nbits(uint32_t nbits);

#endif /* SV2_TX_H */
