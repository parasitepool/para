/*
 * Copyright 2026 Con Kolivas
 *
 * Tests for the shared coinbase construction in src/sv2_cb.c.
 *
 * The one that matters is test_declare_setcustom_equivalence(): the coinbase a
 * DeclareMiningJob describes (prefix ‖ extranonce ‖ suffix) and the coinbase a
 * SetCustomMiningJob describes (assembled from its scriptSig prefix, the
 * channel extranonce and the outputs) must hash to the same txid. If they ever
 * diverge, the pool's merkle root differs from the miners' and every share is
 * difficulty-too-low — a failure that is invisible until real hashrate is
 * pointed at a real pool.
 */

#include "config.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "libckpool.h"
#include "sv2_cb.h"
#include "sv2_tx.h"

static int failures;

static void expect(int cond, const char *msg)
{
	if (!cond) {
		fprintf(stderr, "FAIL: %s\n", msg);
		failures++;
	}
}

/* A regtest-shaped template: BIP34 height push, P2WPKH payout, commitment. */
static const uint8_t ssig_prefix[] = { 0x03, 0x66, 0x00, 0x00 };
static const uint8_t payout_script[] = {
	0x00, 0x14, 0x3e, 0x1e, 0xf4, 0x7f, 0x39, 0xb6, 0x8f, 0x03, 0xe6, 0xb7,
	0x46, 0x39, 0x0b, 0x16, 0xbb, 0xf5, 0x39, 0xc0, 0x11, 0xbf
};
/* value(8) + scriptlen + OP_RETURN <36> aa21a9ed <32 byte hash> */
static const uint8_t commitment[] = {
	0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
	0x26, 0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed,
	0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
	0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
	0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00
};
static const uint8_t witness_reserved[32] = {
	0xaa, 0xbb, 0xcc, 0xdd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
	0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
};

/* The channel extranonce: fixed pool prefix, then what the miner rolls. */
static const uint8_t chan_prefix[] = { 0x01, 0x02, 0x03, 0x04, 0x05, 0x06 };
static const uint8_t rollable[] = { 0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7 };

/*
 * A second pool output carrying a real amount. Spec §6.4.3 only lets a JDS add
 * 0-value outputs, and ckpool's own JDS sends nothing but the payout script, so
 * this is what a JDS that departs from that would send us. We are the client
 * here and cannot make a pool conform, so the builder has to stay correct for
 * it either way.
 */
static const uint8_t funded_script[] = {
	0x00, 0x14, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa,
	0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44
};

static uint8_t *put_output(uint8_t *p, uint64_t value, const uint8_t *script,
			   uint8_t slen)
{
	uint64_t le = htole64(value);

	memcpy(p, &le, 8);
	p += 8;
	*p++ = slen;
	memcpy(p, script, slen);
	return p + slen;
}

/*
 * An AllocateMiningJobToken.Success outputs blob. The pool states an amount on
 * the payout too — 0 in ckpool's, a script advertisement — and it must not
 * survive into the coinbase, so give it a wrong one here on purpose.
 */
static size_t alloc_blob(uint8_t *blob, bool with_funded)
{
	uint8_t *p = blob;

	*p++ = with_funded ? 0x02 : 0x01;
	p = put_output(p, 999ULL, payout_script, sizeof(payout_script));
	if (with_funded)
		p = put_output(p, 250000ULL, funded_script, sizeof(funded_script));
	return (size_t)(p - blob);
}

static size_t build_outputs(uint8_t *out, size_t outsz, bool with_commitment)
{
	uint8_t blob[128];
	size_t blen = alloc_blob(blob, false);

	return sv2_cb_build_outputs(out, outsz, blob, (uint16_t)blen,
				    5000002820ULL,
				    with_commitment ? commitment : NULL,
				    with_commitment ? sizeof(commitment) : 0);
}

/*
 * Every output the pool asks for reaches the coinbase: the first takes the
 * reward whatever the pool stated on it, the rest keep the amounts it set.
 */
static void test_multi_payout(void)
{
	uint8_t blob[128], out[256];
	const uint8_t *script;
	uint64_t value = 0;
	uint8_t slen = 0;
	size_t blen, n;

	blen = alloc_blob(blob, true);
	expect(sv2_cb_outputs_count(blob, (uint16_t)blen) == 2, "blob has two payouts");

	n = sv2_cb_build_outputs(out, sizeof(out), blob, (uint16_t)blen,
				 5000002820ULL, commitment, sizeof(commitment));
	expect(n, "two payouts plus a commitment build");
	expect(out[0] == 0x03, "three coinbase outputs");

	expect(sv2_cb_output_at(out, (uint16_t)n, 0, &value, &script, &slen),
	       "first coinbase output parses");
	expect(value == 5000002820ULL - 250000ULL,
	       "first payout takes what the funded output leaves, not its stated 999");
	expect(slen == sizeof(payout_script) &&
	       !memcmp(script, payout_script, slen), "first payout script survives");

	expect(sv2_cb_output_at(out, (uint16_t)n, 1, &value, &script, &slen),
	       "second coinbase output parses");
	expect(value == 250000ULL, "funded output keeps the amount the pool set");
	expect(slen == sizeof(funded_script) &&
	       !memcmp(script, funded_script, slen), "funded output script survives");

	expect(sv2_cb_output_at(out, (uint16_t)n, 2, &value, &script, &slen),
	       "commitment output parses");
	expect(value == 0, "commitment stays zero value");
	expect(!sv2_cb_output_at(out, (uint16_t)n, 3, &value, &script, &slen),
	       "no fourth output");

	/* A blob naming more outputs than it carries must not be read past. */
	blob[0] = 0x03;
	expect(sv2_cb_outputs_count(blob, (uint16_t)blen) == -1,
	       "output count beyond the blob rejected");
	expect(!sv2_cb_build_outputs(out, sizeof(out), blob, (uint16_t)blen,
				     5000002820ULL, NULL, 0),
	       "truncated payout list refuses to build");
}

/*
 * The JDS half of the same contract: what its payout rule accounts for has to
 * be what the client built. Mirrors sv2_jd.c's account_spendable_out over the
 * coinbase outputs our builder produces, so the two halves cannot drift into a
 * pool naming an output the declare path then reads as diverted reward.
 */
static void test_funded_output_round_trip(void)
{
	uint64_t payout_value = 0, funded_value = 0, other_value = 0;
	uint8_t blob[128], out[256];
	const uint8_t *script;
	uint64_t value = 0;
	uint8_t slen = 0;
	size_t blen, n;
	int i, nouts;

	blen = alloc_blob(blob, true);
	n = sv2_cb_build_outputs(out, sizeof(out), blob, (uint16_t)blen,
				 5000002820ULL, commitment, sizeof(commitment));
	nouts = sv2_cb_outputs_count(out, (uint16_t)n);
	expect(nouts == 3, "round trip builds three outputs");

	for (i = 0; i < nouts; i++) {
		expect(sv2_cb_output_at(out, (uint16_t)n, i, &value, &script, &slen),
		       "coinbase output parses");
		if (!value)
			continue;	/* commitment: never spendable reward */
		if (slen == sizeof(payout_script) &&
		    !memcmp(script, payout_script, slen))
			payout_value += value;
		else if (slen == sizeof(funded_script) &&
			 !memcmp(script, funded_script, slen))
			funded_value += value;
		else
			other_value += value;
	}
	expect(payout_value == 5000002820ULL - 250000ULL,
	       "pool payout accounted at the reward less the funded output");
	expect(funded_value == 250000ULL,
	       "funded output accounted at the advertised amount");
	expect(!other_value,
	       "nothing counts as diverted reward, so the shared-mode rule holds");
	/*
	 * The one that makes the block valid: a coinbase may claim no more than
	 * the template left it, so paying the reward in full *and* the fixed
	 * outputs on top is over-claiming and fails bad-cb-amount at checkBlock.
	 */
	expect(payout_value + funded_value == 5000002820ULL,
	       "outputs total exactly the reward, never over-claiming it");
}

/* A pool cannot ask for more than the block is worth. */
static void test_fixed_over_reward(void)
{
	uint8_t blob[128], out[256];
	const uint8_t *script;
	uint64_t value = 0;
	uint8_t slen = 0;
	size_t blen, n;

	blen = alloc_blob(blob, true);		/* second output fixed at 250000 */
	expect(!sv2_cb_build_outputs(out, sizeof(out), blob, (uint16_t)blen,
				     250000ULL - 1, commitment, sizeof(commitment)),
	       "fixed outputs over the whole reward refuse to build");

	/* Exactly the reward is fundable, leaving the payout empty-handed. */
	n = sv2_cb_build_outputs(out, sizeof(out), blob, (uint16_t)blen, 250000ULL,
				 commitment, sizeof(commitment));
	expect(n, "fixed outputs equal to the reward still build");
	expect(sv2_cb_output_at(out, (uint16_t)n, 0, &value, &script, &slen) && !value,
	       "payout takes nothing when the fixed outputs take it all");
}

static void test_outputs(void)
{
	uint8_t out[256];
	const uint8_t *script;
	uint8_t slen = 0;
	uint64_t value = 0;
	size_t n;

	n = build_outputs(out, sizeof(out), true);
	expect(n == 1 + 8 + 1 + sizeof(payout_script) + sizeof(commitment),
	       "outputs length with commitment");
	expect(out[0] == 0x02, "two outputs");
	memcpy(&value, out + 1, 8);
	expect(le64toh(value) == 5000002820ULL, "payout funded with the whole reward");

	/* The payout must be the first output: that is where the JDS looks for
	 * the script it advertised in AllocateMiningJobToken.Success. */
	expect(sv2_cb_first_output_script(out, (uint16_t)n, &script, &slen),
	       "first output script parses");
	expect(slen == sizeof(payout_script) &&
	       !memcmp(script, payout_script, slen), "first output is the payout");

	/* The commitment output follows verbatim, so its own zero value and
	 * OP_RETURN script survive unchanged. */
	expect(!memcmp(out + n - sizeof(commitment), commitment, sizeof(commitment)),
	       "commitment output copied verbatim");

	n = build_outputs(out, sizeof(out), false);
	expect(out[0] == 0x01, "one output without a commitment");
	expect(n == 1 + 8 + 1 + sizeof(payout_script), "outputs length without commitment");

	expect(!build_outputs(out, 4, true), "outputs refuse a short buffer");
	expect(!sv2_cb_build_outputs(out, sizeof(out), NULL, 0, 1, NULL, 0),
	       "outputs refuse a missing payout script");

	/* Malformed blobs must not be read past. */
	expect(!sv2_cb_first_output_script(out, 3, &script, &slen),
	       "truncated outputs blob rejected");
	expect(!sv2_cb_first_output_script(NULL, 0, &script, &slen),
	       "empty outputs blob rejected");
}

/*
 * Core invariant: both shapes are the same coinbase.
 */
static void test_declare_setcustom_equivalence(void)
{
	uint8_t outputs[256], assembled[1024], joined[1024];
	uint8_t txid_declare[32], txid_custom[32];
	uint8_t *prefix = NULL, *suffix = NULL;
	uint16_t prefix_len = 0, suffix_len = 0;
	struct sv2_cb_spec spec;
	size_t olen, alen, jlen;
	uint8_t hole;
	int pass;

	olen = build_outputs(outputs, sizeof(outputs), true);
	hole = (uint8_t)(sizeof(chan_prefix) + sizeof(rollable));

	/* Once legacy, once BIP141: the declare carries the witness, the custom
	 * job never does, and the txid must be identical either way. */
	for (pass = 0; pass < 2; pass++) {
		bool witness = (pass == 1);

		memset(&spec, 0, sizeof(spec));
		spec.version = 2;
		spec.nsequence = 0xffffffff;
		spec.locktime = 101;
		spec.ssig_prefix = ssig_prefix;
		spec.ssig_prefix_len = sizeof(ssig_prefix);
		spec.hole_len = hole;
		spec.outputs = outputs;
		spec.outputs_len = (uint16_t)olen;
		spec.witness_reserved = witness ? witness_reserved : NULL;

		expect(sv2_cb_declare_parts(&spec, &prefix, &prefix_len,
					    &suffix, &suffix_len),
		       "declare parts build");

		/* Declare form: prefix ‖ full extranonce ‖ suffix. */
		jlen = 0;
		memcpy(joined, prefix, prefix_len);
		jlen += prefix_len;
		memcpy(joined + jlen, chan_prefix, sizeof(chan_prefix));
		jlen += sizeof(chan_prefix);
		memcpy(joined + jlen, rollable, sizeof(rollable));
		jlen += sizeof(rollable);
		memcpy(joined + jlen, suffix, suffix_len);
		jlen += suffix_len;

		/* SetCustom form: scriptSig prefix only, server appends en1 and
		 * the miner's extranonce, and it is never witness-serialised. */
		alen = sv2_cb_assemble(assembled, sizeof(assembled), spec.version,
				       spec.nsequence, spec.locktime,
				       ssig_prefix, sizeof(ssig_prefix),
				       chan_prefix, sizeof(chan_prefix),
				       rollable, sizeof(rollable),
				       outputs, (uint16_t)olen);
		expect(alen > 0, "custom coinbase assembles");

		expect(sv2_bitcoin_txid(joined, jlen, txid_declare),
		       "declared coinbase parses as a transaction");
		expect(sv2_bitcoin_txid(assembled, alen, txid_custom),
		       "custom coinbase parses as a transaction");
		expect(!memcmp(txid_declare, txid_custom, 32),
		       witness ? "witness declare txid == custom txid"
			       : "legacy declare txid == custom txid");

		/* Without a witness the two serialisations are byte-identical;
		 * with one the declared form is longer by marker+flag+stack. */
		if (!witness) {
			expect(jlen == alen && !memcmp(joined, assembled, jlen),
			       "legacy declare bytes == custom bytes");
		} else {
			expect(jlen == alen + 2 + 2 + 32,
			       "witness declare is longer by marker/flag/stack");
		}

		/*
		 * And the JDS must derive our hole back out of the prefix: it
		 * zero-fills exactly (scriptSig length - bytes present), so an
		 * off-by-one here misaligns the outputs and bitcoind throws.
		 */
		{
			const uint8_t *p = prefix, *end = prefix + prefix_len;
			uint64_t sslen = 0;

			p += 4;
			if (witness) {
				expect(p[0] == 0x00 && p[1] == 0x01,
				       "BIP141 marker and flag present");
				p += 2;
			}
			expect(*p == 0x01, "one coinbase input");
			p++;
			p += 36;
			expect(sv2_read_compact_size(&p, end, &sslen),
			       "scriptSig length readable");
			expect(sslen == (uint64_t)(sizeof(ssig_prefix) + hole),
			       "scriptSig length covers prefix and hole");
			expect((uint64_t)(end - p) == sizeof(ssig_prefix),
			       "prefix ends where the extranonce begins");
			expect(sslen - (uint64_t)(end - p) == hole,
			       "JDS derives our exact hole length");
		}

		dealloc(prefix);
		dealloc(suffix);
	}
}

static void test_declare_parts_limits(void)
{
	uint8_t outputs[256];
	uint8_t *prefix = NULL, *suffix = NULL;
	uint16_t prefix_len = 0, suffix_len = 0;
	struct sv2_cb_spec spec;
	size_t olen;

	olen = build_outputs(outputs, sizeof(outputs), true);
	memset(&spec, 0, sizeof(spec));
	spec.version = 2;
	spec.nsequence = 0xffffffff;
	spec.locktime = 101;
	spec.ssig_prefix = ssig_prefix;
	spec.ssig_prefix_len = sizeof(ssig_prefix);
	spec.outputs = outputs;
	spec.outputs_len = (uint16_t)olen;

	/* Right at the consensus scriptSig ceiling, and one past it. */
	spec.hole_len = SV2_CB_MAX_SCRIPTSIG - sizeof(ssig_prefix);
	expect(sv2_cb_declare_parts(&spec, &prefix, &prefix_len, &suffix, &suffix_len),
	       "scriptSig at the 100 byte limit is accepted");
	dealloc(prefix);
	dealloc(suffix);
	spec.hole_len = SV2_CB_MAX_SCRIPTSIG - sizeof(ssig_prefix) + 1;
	expect(!sv2_cb_declare_parts(&spec, &prefix, &prefix_len, &suffix, &suffix_len),
	       "scriptSig over the 100 byte limit is refused");
	expect(!prefix && !suffix, "no buffers leak on refusal");

	spec.hole_len = 12;
	spec.outputs = NULL;
	spec.outputs_len = 0;
	expect(!sv2_cb_declare_parts(&spec, &prefix, &prefix_len, &suffix, &suffix_len),
	       "missing outputs refused");
	spec.outputs = outputs;
	spec.outputs_len = (uint16_t)olen;
	spec.hole_len = 0;
	expect(!sv2_cb_declare_parts(&spec, &prefix, &prefix_len, &suffix, &suffix_len),
	       "an empty extranonce hole is refused");
}

static void test_assemble_limits(void)
{
	uint8_t outputs[256], out[1024], big[300];
	size_t olen;

	olen = build_outputs(outputs, sizeof(outputs), false);
	memset(big, 0x5a, sizeof(big));

	expect(!sv2_cb_assemble(out, 16, 2, 0xffffffff, 0, ssig_prefix,
				sizeof(ssig_prefix), NULL, 0, rollable,
				sizeof(rollable), outputs, (uint16_t)olen),
	       "assemble refuses a short buffer");
	/* scriptSig over 255 cannot be encoded in one length byte: refuse rather
	 * than wrap (the shape that made the old caller need a wrap check). */
	expect(!sv2_cb_assemble(out, sizeof(out), 2, 0xffffffff, 0, big, 200,
				NULL, 0, big, 100, outputs, (uint16_t)olen),
	       "assemble refuses a scriptSig over 255 bytes");
	/* Empty extranonce is legal for the assembler itself (the JDS calls it
	 * with a zero-length one when probing). */
	expect(sv2_cb_assemble(out, sizeof(out), 2, 0xffffffff, 0, ssig_prefix,
			       sizeof(ssig_prefix), NULL, 0, NULL, 0,
			       outputs, (uint16_t)olen) > 0,
	       "assemble accepts an empty extranonce");
}

int main(void)
{
	test_outputs();
	test_multi_payout();
	test_funded_output_round_trip();
	test_fixed_over_reward();
	test_declare_setcustom_equivalence();
	test_declare_parts_limits();
	test_assemble_limits();

	if (failures) {
		fprintf(stderr, "%d sv2_cb test failure(s)\n", failures);
		return 1;
	}
	printf("sv2_cb: all tests passed\n");
	return 0;
}
