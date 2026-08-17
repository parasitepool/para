/*
 * Copyright 2026 Con Kolivas
 *
 * Tests for the work-source arbiter helpers in src/sv2_bridge.c: the local tip
 * ring, where a pool tip sits relative to it, and the plausibility filter.
 *
 * These decide whether ckproxy declares a template, bridges onto pool work, or
 * waits — and every one of those is a decision taken once per block, on data we
 * cannot manufacture in a live run, so the branches are pinned here instead.
 * Mirrors the tests in sv2-apps tip_bridge.rs so the port stays comparable.
 */

#include "config.h"

#include <stdio.h>
#include <string.h>

#include "sv2_bridge.h"

static int failures;

static void expect(int cond, const char *msg)
{
	if (!cond) {
		fprintf(stderr, "FAIL: %s\n", msg);
		failures++;
	}
}

/* A distinguishable 32 byte hash. */
static void mkhash(uint8_t out[32], uint8_t seed)
{
	memset(out, seed, 32);
}

static void test_ring(void)
{
	struct sv2_tip_ring r = {};
	uint8_t h[32], out[32];
	int i;

	mkhash(h, 1);
	expect(!sv2_tip_newest(&r, out), "empty ring has no newest tip");
	expect(sv2_tip_relation(&r, h) == SV2_TIP_NO_LOCAL,
	       "empty ring cannot classify a pool tip");

	sv2_tip_push(&r, h, 0x17033ec5, 1700000000);
	expect(r.count == 1, "one push holds one tip");
	expect(sv2_tip_newest(&r, out) && !memcmp(out, h, 32), "newest is the pushed tip");

	/* A fee-bump template on an unchanged tip must not consume the ring. */
	sv2_tip_push(&r, h, 0x17033ec5, 1700000030);
	expect(r.count == 1, "a repeat of the newest tip is ignored");
	expect(r.ntime[r.newest] == 1700000000, "and leaves its recorded ntime alone");

	/* Fill past capacity: the oldest tips fall out, the newest stays newest. */
	for (i = 2; i < 2 + SV2_TIP_RING; i++) {
		mkhash(h, (uint8_t)i);
		sv2_tip_push(&r, h, 0x17033ec5, 1700000000 + i);
	}
	expect(r.count == SV2_TIP_RING, "the ring is bounded");
	expect(sv2_tip_newest(&r, out) && out[0] == (uint8_t)(1 + SV2_TIP_RING),
	       "the last push is still the newest");
	mkhash(h, 1);
	expect(sv2_tip_relation(&r, h) == SV2_TIP_AHEAD,
	       "a tip evicted from the ring is no longer recognised as ours");
}

static void test_relation(void)
{
	struct sv2_tip_ring r = {};
	uint8_t old[32], cur[32], other[32];

	mkhash(old, 10);
	mkhash(cur, 11);
	mkhash(other, 12);
	sv2_tip_push(&r, old, 0x17033ec5, 1700000000);
	sv2_tip_push(&r, cur, 0x17033ec5, 1700000600);

	expect(sv2_tip_relation(&r, cur) == SV2_TIP_SAME,
	       "the pool on our tip is declarable");
	expect(sv2_tip_relation(&r, old) == SV2_TIP_BEHIND,
	       "a tip we have moved past means the pool lags us");
	expect(sv2_tip_relation(&r, other) == SV2_TIP_AHEAD,
	       "a tip we have never had means the pool leads us");
}

static void test_plausible(void)
{
	struct sv2_tip_ring r = {};
	uint32_t local_ntime = 1700000000;
	uint8_t h[32];

	expect(sv2_tip_plausible(&r, 0x1d00ffff, 1) == SV2_TIP_OK,
	       "with no local tip there is nothing to contradict");

	mkhash(h, 20);
	sv2_tip_push(&r, h, 0x17033ec5, local_ntime);
	expect(sv2_tip_plausible(&r, 0x17033ec5, local_ntime + 60) == SV2_TIP_OK,
	       "the same nbits and a nearby ntime is plausible");
	expect(sv2_tip_plausible(&r, 0x1d00ffff, local_ntime) == SV2_TIP_NBITS,
	       "different difficulty bits are not the same chain");
	expect(sv2_tip_plausible(&r, 0x17033ec5,
				 local_ntime + SV2_TIP_NTIME_SKEW_SECS + 1) ==
	       SV2_TIP_NTIME_AHEAD, "an ntime hours ahead is rejected");
	expect(sv2_tip_plausible(&r, 0x17033ec5,
				 local_ntime - SV2_TIP_NTIME_SKEW_SECS - 1) ==
	       SV2_TIP_NTIME_BEHIND, "an ntime hours behind is rejected");
	/* Neither comparison may wrap: a tip near the u32 ceiling is legal. */
	mkhash(h, 21);
	sv2_tip_push(&r, h, 0x17033ec5, 0xfffffff0);
	expect(sv2_tip_plausible(&r, 0x17033ec5, 0xffffffff) == SV2_TIP_OK,
	       "timestamps near the u32 ceiling do not overflow");
	expect(sv2_tip_plausible(&r, 0x17033ec5, 1) == SV2_TIP_NTIME_BEHIND,
	       "and a wrapped-looking ntime is still rejected");
}

static void test_strings(void)
{
	/* Log lines carry these; a missing case would print "unknown" forever. */
	expect(!strcmp(sv2_tip_rel_str(SV2_TIP_SAME), "our tip"), "rel string");
	expect(!strcmp(sv2_tip_reject_str(SV2_TIP_OK), "plausible"), "reject string");
	expect(!strcmp(sv2_work_src_str(SV2_WS_LOCAL_JD), "LOCAL_JD"), "work source string");
	expect(!strcmp(sv2_work_src_str(SV2_WS_BRIDGE), "POOL_BRIDGE"), "bridge string");
}

int main(void)
{
	test_ring();
	test_relation();
	test_plausible();
	test_strings();

	if (failures) {
		fprintf(stderr, "%d work-source arbiter test(s) failed\n", failures);
		return 1;
	}
	printf("sv2_bridge: all tests passed\n");
	return 0;
}
