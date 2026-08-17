/*
 * Copyright 2026 Con Kolivas
 *
 * Coinbase construction shared by the SV2 Job Declaration server and client.
 *
 * One transaction has three on-wire shapes and they must
 * describe identical bytes:
 *
 *   DeclareMiningJob   coinbase_tx_prefix ‖ <extranonce hole> ‖ coinbase_tx_suffix
 *   SetCustomMiningJob coinbase_prefix (scriptSig bytes only) + outputs + fields
 *   SV1 notify         coinb1 = declare prefix ‖ channel prefix ‖ pad, coinb2 = suffix
 *
 * The first two are built here from one spec so they cannot drift, and the
 * assembler is the same code the server uses to hash custom-job shares.
 */

#ifndef SV2_CB_H
#define SV2_CB_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Consensus limit on a coinbase scriptSig. A declared coinbase whose
 * prefix + extranonce hole exceeds this could never be a valid block. */
#define SV2_CB_MAX_SCRIPTSIG 100

/*
 * Assemble the non-witness coinbase that a SetCustomMiningJob describes:
 *
 *   version | 01 | null prevout | scriptSig(prefix ‖ en1 ‖ extranonce)
 *           | nSequence | outputs | locktime
 *
 * outputs is the CompactSize-prefixed output list exactly as it travels on the
 * wire. Returns the number of bytes written, or 0 if it does not fit in outsz
 * or the scriptSig would exceed 255 bytes.
 *
 * This is the txid preimage: no BIP141 marker/flag and no witness, which is
 * what a txid is defined over.
 */
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
    uint16_t       outputs_len);

/* Everything needed to describe one coinbase, independent of which shape it is
 * later serialised into. */
struct sv2_cb_spec {
    uint32_t version;
    uint32_t nsequence;
    uint32_t locktime;
    /* scriptSig bytes we contribute (the template's script_sig_prefix). */
    const uint8_t* ssig_prefix;
    uint8_t        ssig_prefix_len;
    /* Extranonce bytes left for the pool and the miner: the full extranonce,
     * channel prefix included (the JDS derives this length back out of the
     * declared prefix). */
    uint8_t hole_len;
    /* CompactSize count ‖ serialised outputs. */
    const uint8_t* outputs;
    uint16_t       outputs_len;
    /* 32 byte witness reserved value for a BIP141 serialisation, or NULL to
     * serialise legacy. Must be NULL when the template has no witness
     * commitment: BIP141 forbids a coinbase witness on such a block. */
    const uint8_t* witness_reserved;
};

/*
 * Split a spec into the DeclareMiningJob prefix/suffix pair. Both buffers are
 * ckalloc'd; the caller deallocs. False if the spec is unusable (scriptSig over
 * SV2_CB_MAX_SCRIPTSIG, missing outputs, oversize parts).
 */
bool sv2_cb_declare_parts(
    const struct sv2_cb_spec* spec,
    uint8_t**                 prefix,
    uint16_t*                 prefix_len,
    uint8_t**                 suffix,
    uint16_t*                 suffix_len);

/* Most payout outputs an AllocateMiningJobToken.Success may constrain us to,
 * and the room their serialised form needs. A pool wanting more than this
 * cannot be paid what it asked for, so the declare fails rather than quietly
 * paying a subset. */
#define SV2_MAX_PAYOUT_OUTS 16
#define SV2_MAX_PAYOUT_BLOB 1024

/*
 * Serialise the coinbase outputs of a declared job from the pool's
 * AllocateMiningJobToken.Success outputs blob: every output the pool asked for,
 * the ones after the first at the amounts the pool set and the first taking
 * what remains of value after them, then the witness commitment output verbatim
 * (value 0, so it does not count as diverted reward).
 *
 * value is the whole claim the coinbase is allowed, so the fixed outputs are
 * subtracted from it and not added to it. The reference JDC assigns the first
 * output the entire remaining reward and copies the rest untouched
 * (jd-client channel_manager: coinbase_outputs[0].value = coinbase_tx_value_
 * remaining), which balances only while those rest outputs are the value 0
 * script advertisements its JDS sends. Against a pool that states real amounts
 * that rule over-claims by their sum and builds a block no node will accept, so
 * the remainder is taken here instead.
 *
 * Returns bytes written, or 0 if the blob is malformed, asks for more than
 * value, or does not fit.
 */
size_t sv2_cb_build_outputs(
    uint8_t*       out,
    size_t         outsz,
    const uint8_t* payouts,
    uint16_t       payouts_len,
    uint64_t       value,
    const uint8_t* commitment,
    uint8_t        commitment_len);

/*
 * Outputs in a CompactSize-prefixed blob, or -1 if it is malformed or asks for
 * more than SV2_MAX_PAYOUT_OUTS.
 */
int sv2_cb_outputs_count(const uint8_t* outputs, uint16_t olen);

/*
 * Output idx of a CompactSize-prefixed outputs blob — the pool payout scripts
 * of an AllocateMiningJobToken.Success (spec §6.4.3). Zero-copy: *script points
 * into outputs. value, script and script_len may each be NULL.
 */
bool sv2_cb_output_at(
    const uint8_t*  outputs,
    uint16_t        olen,
    int             idx,
    uint64_t*       value,
    const uint8_t** script,
    uint8_t*        script_len);

/*
 * The first output's scriptPubKey, the payout that takes the reward.
 */
bool sv2_cb_first_output_script(const uint8_t* outputs, uint16_t olen, const uint8_t** script, uint8_t* script_len);

#endif /* SV2_CB_H */
