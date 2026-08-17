/*
 * Copyright 2026 Con Kolivas
 *
 * Tests for the shared bitcoin serialisation helpers in src/sv2_tx.c: CompactSize,
 * transaction walking (txid / wtxid / consumed length) and merkle roots.
 *
 * These underpin both the JDS candidate-block assembly and the JDC template
 * split, where a mis-parse only surfaces as a checkBlock rejection, so they are
 * anchored against external truth (the genesis coinbase txid) and against
 * hand-written serialisations rather than against the walker itself.
 */

#include "config.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "libckpool.h"
#include "sv2_tx.h"

static int failures;

static void expect(int cond, const char *msg)
{
	if (!cond) {
		fprintf(stderr, "FAIL: %s\n", msg);
		failures++;
	}
}

/* Hash in display (reversed) order, as block explorers show it. */
static void display_hex(const uint8_t hash[32], char out[65])
{
	uint8_t rev[32];
	int i;

	for (i = 0; i < 32; i++)
		rev[i] = hash[31 - i];
	__bin2hex(out, rev, 32);
}

static size_t unhex(uint8_t *out, const char *hex)
{
	size_t len = strlen(hex) / 2;

	if (!hex2bin(out, hex, len)) {
		fprintf(stderr, "FAIL: bad test hex\n");
		failures++;
		return 0;
	}
	return len;
}

/* The Bitcoin genesis coinbase: an external anchor for the legacy path. */
static const char *genesis_cb =
	"01000000010000000000000000000000000000000000000000000000000000000000"
	"000000ffffffff4d04ffff001d0104455468652054696d65732030332f4a616e2f32"
	"303039204368616e63656c6c6f72206f6e206272696e6b206f66207365636f6e6420"
	"6261696c6f757420666f722062616e6b73ffffffff0100f2052a0100000043410467"
	"8afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc"
	"3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5fac00000000";
static const char *genesis_txid =
	"4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b";

/* One P2WPKH-paying witness transaction, written out in both serialisations so
 * the walker is checked against hand-made bytes, not against itself. */
static const char *segwit_full =
	"01000000"			/* version */
	"0001"				/* BIP144 marker + flag */
	"01"				/* 1 input */
	"1111111111111111111111111111111111111111111111111111111111111111"
	"00000000"			/* prevout index */
	"00"				/* empty scriptSig */
	"ffffffff"			/* sequence */
	"01"				/* 1 output */
	"00e1f50500000000"		/* 1 BTC */
	"16" "0014" "2222222222222222222222222222222222222222"
	"01" "02" "dead"		/* witness: 1 item, 2 bytes */
	"00000000";			/* locktime */
static const char *segwit_stripped =
	"01000000"
	"01"
	"1111111111111111111111111111111111111111111111111111111111111111"
	"00000000"
	"00"
	"ffffffff"
	"01"
	"00e1f50500000000"
	"16" "0014" "2222222222222222222222222222222222222222"
	"00000000";

static void test_compact_size(void)
{
	uint8_t buf[16];
	uint8_t *p;
	const uint8_t *q;
	uint64_t n;
	const uint64_t vals[] = { 0, 1, 0xfc, 0xfd, 0xffff, 0x10000, 0xffffffffu,
				  0x100000000ull };
	unsigned i;

	for (i = 0; i < sizeof(vals) / sizeof(vals[0]); i++) {
		size_t want = sv2_compact_size_len(vals[i]);

		p = buf;
		sv2_write_compact_size(&p, vals[i]);
		expect((size_t)(p - buf) == want, "compact size len matches write");
		q = buf;
		expect(sv2_read_compact_size(&q, p, &n), "compact size reads back");
		expect(n == vals[i], "compact size round trips");
		expect(q == p, "compact size consumes exactly what it wrote");
	}

	/* Truncation must fail rather than read past the end. */
	p = buf;
	sv2_write_compact_size(&p, 0xffff);
	q = buf;
	expect(!sv2_read_compact_size(&q, buf + 1, &n), "truncated 0xfd rejected");
	q = buf;
	expect(!sv2_read_compact_size(&q, buf, &n), "empty buffer rejected");
}

static void test_have_bytes(void)
{
	const uint8_t buf[4] = {};

	expect(sv2_have_bytes(buf, buf + 4, 4), "exact fit");
	expect(!sv2_have_bytes(buf, buf + 4, 5), "one over rejected");
	/* A CompactSize can claim a full u64: the check must not form p + need. */
	expect(!sv2_have_bytes(buf, buf + 4, UINT64_MAX), "huge need rejected");
	expect(!sv2_have_bytes(buf + 4, buf, 0), "reversed bounds rejected");
}

static void test_legacy_tx(void)
{
	uint8_t tx[512], txid[32], wtxid[32];
	char hex[65];
	size_t len, txlen = 0;

	len = unhex(tx, genesis_cb);
	expect(sv2_bitcoin_txid(tx, len, txid), "genesis coinbase parses");
	display_hex(txid, hex);
	expect(!strcmp(hex, genesis_txid), "genesis coinbase txid matches");

	expect(sv2_bitcoin_tx_parse(tx, len, &txlen, txid, wtxid),
	       "genesis coinbase walks");
	expect(txlen == len, "legacy tx length is the whole buffer");
	display_hex(txid, hex);
	expect(!strcmp(hex, genesis_txid), "walked txid matches");
	/* BIP141: a legacy transaction's wtxid is its txid. */
	expect(!memcmp(txid, wtxid, 32), "legacy wtxid equals txid");

	/* Trailing bytes must not confuse the walker: length still stops at the
	 * locktime, which is what block splitting relies on. */
	memset(tx + len, 0xab, 16);
	expect(sv2_bitcoin_tx_parse(tx, len + 16, &txlen, NULL, NULL),
	       "walks with trailing bytes");
	expect(txlen == len, "length ignores trailing bytes");

	/* Truncation at every length must be refused, never over-read. */
	{
		size_t i;
		int bad = 0;

		for (i = 0; i < len; i++) {
			if (sv2_bitcoin_tx_parse(tx, i, &txlen, NULL, NULL))
				bad++;
		}
		expect(!bad, "every truncation of a legacy tx is rejected");
	}
}

static void test_witness_tx(void)
{
	uint8_t full[256], stripped[256], txid[32], wtxid[32], want[32];
	size_t flen, slen, txlen = 0;

	flen = unhex(full, segwit_full);
	slen = unhex(stripped, segwit_stripped);

	expect(sv2_bitcoin_tx_parse(full, flen, &txlen, txid, wtxid),
	       "witness tx walks");
	expect(txlen == flen, "witness tx length includes the witness");

	/* wtxid hashes the full serialisation... */
	gen_hash(full, want, (int)flen);
	expect(!memcmp(wtxid, want, 32), "wtxid is SHA256d of the full bytes");
	/* ...txid the hand-written stripped one. */
	gen_hash(stripped, want, (int)slen);
	expect(!memcmp(txid, want, 32), "txid is SHA256d of the stripped bytes");
	expect(memcmp(txid, wtxid, 32), "witness txid and wtxid differ");

	/* sv2_bitcoin_txid must agree with the parse path. */
	expect(sv2_bitcoin_txid(full, flen, wtxid), "witness txid helper parses");
	expect(!memcmp(txid, wtxid, 32), "txid helper agrees with tx_parse");

	{
		size_t i;
		int bad = 0;

		for (i = 0; i < flen; i++) {
			if (sv2_bitcoin_tx_parse(full, i, &txlen, NULL, NULL))
				bad++;
		}
		expect(!bad, "every truncation of a witness tx is rejected");
	}
}

/* Walk two transactions laid end to end, as template_split_block() does. */
static void test_walk_sequence(void)
{
	uint8_t buf[1024];
	size_t a, b, txlen = 0, off = 0;

	a = unhex(buf, genesis_cb);
	b = unhex(buf + a, segwit_full);

	expect(sv2_bitcoin_tx_parse(buf, a + b, &txlen, NULL, NULL), "first tx walks");
	expect(txlen == a, "first tx length");
	off = txlen;
	expect(sv2_bitcoin_tx_parse(buf + off, a + b - off, &txlen, NULL, NULL),
	       "second tx walks");
	expect(txlen == b, "second tx length");
	expect(off + txlen == a + b, "sequence consumed exactly");
}

static void test_merkle(void)
{
	uint8_t leaves[4][32], root[32], want[32], pair[64], l01[32], l23[32];
	uint8_t path[2][32];
	int i;

	for (i = 0; i < 4; i++)
		memset(leaves[i], 0x10 + i, 32);

	/* Single leaf: the root is the leaf. */
	sv2_merkle_root_from_txids(leaves, 1, root);
	expect(!memcmp(root, leaves[0], 32), "one leaf root is the leaf");
	sv2_merkle_root_from_path(leaves[0], NULL, 0, root);
	expect(!memcmp(root, leaves[0], 32), "empty path root is the coinbase");

	/* Two leaves, computed by hand. */
	memcpy(pair, leaves[0], 32);
	memcpy(pair + 32, leaves[1], 32);
	gen_hash(pair, want, 64);
	sv2_merkle_root_from_txids(leaves, 2, root);
	expect(!memcmp(root, want, 32), "two leaf root");

	/* Odd layer duplicates the last leaf. */
	memcpy(pair, leaves[2], 32);
	memcpy(pair + 32, leaves[2], 32);
	gen_hash(pair, l23, 64);
	memcpy(pair, want, 32);
	memcpy(pair + 32, l23, 32);
	gen_hash(pair, want, 64);
	sv2_merkle_root_from_txids(leaves, 3, root);
	expect(!memcmp(root, want, 32), "three leaf root duplicates the last");

	/*
	 * Four leaves: the branch for leaf 0 is [leaf1, hash(leaf2|leaf3)], so
	 * folding it must reproduce the full root. This is exactly the identity
	 * the JDC template self-check relies on (coinbase txid + IPC merkle path
	 * == header merkle root).
	 */
	memcpy(pair, leaves[0], 32);
	memcpy(pair + 32, leaves[1], 32);
	gen_hash(pair, l01, 64);
	memcpy(pair, leaves[2], 32);
	memcpy(pair + 32, leaves[3], 32);
	gen_hash(pair, l23, 64);
	memcpy(pair, l01, 32);
	memcpy(pair + 32, l23, 32);
	gen_hash(pair, want, 64);

	sv2_merkle_root_from_txids(leaves, 4, root);
	expect(!memcmp(root, want, 32), "four leaf root");

	memcpy(path[0], leaves[1], 32);
	memcpy(path[1], l23, 32);
	sv2_merkle_root_from_path(leaves[0], path, 2, root);
	expect(!memcmp(root, want, 32), "path fold reproduces the tree root");
}

/*
 * nbits → difficulty. The byte order matters more than the arithmetic: the
 * little-endian bytes of a wire header fed to libckpool's diff_from_nbits()
 * produce a difficulty near zero, which a solve test reads as "every share is a
 * block" rather than as an error. That shipped once; this pins it.
 */
static void test_nbits_diff(void)
{
	double mainnet = sv2_diff_from_nbits(0x17023ad4);
	double regtest = sv2_diff_from_nbits(0x207fffff);
	double testnet = sv2_diff_from_nbits(0x1d00ffff);

	/* Bitcoin mainnet at height ~960000: ~126 trillion. */
	expect(mainnet > 1.0e14 && mainnet < 1.5e14,
	       "mainnet nbits 0x17023ad4 gives ~1.26e14 difficulty");
	/* Difficulty 1, the value the compact target 0x1d00ffff defines. */
	expect(testnet > 0.9999 && testnet < 1.0001,
	       "nbits 0x1d00ffff is difficulty 1");
	/* The regtest floor is far below 1, so a solve test has to floor it. */
	expect(regtest > 0 && regtest < 1.0e-8,
	       "regtest nbits 0x207fffff is well under difficulty 1");
	/* The trap: byte-swapped nbits reads a target exponent out of the low
	 * byte and lands nowhere near the real difficulty. */
	expect(sv2_diff_from_nbits(0xd43a0217) < mainnet / 1.0e6,
	       "byte-swapped nbits does not resemble the real difficulty");
}

int main(void)
{
	test_compact_size();
	test_have_bytes();
	test_legacy_tx();
	test_witness_tx();
	test_walk_sequence();
	test_merkle();
	test_nbits_diff();

	if (failures) {
		fprintf(stderr, "%d sv2_tx test failure(s)\n", failures);
		return 1;
	}
	printf("sv2_tx: all tests passed\n");
	return 0;
}
