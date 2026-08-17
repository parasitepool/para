/*
 * Copyright 2026 Con Kolivas
 *
 * Unit tests for SV2 handshake rate-limit reservation (sv2_conn.c):
 *  - per-IP cap is enforced atomically (try_reserve, not allow-then-note)
 *  - global in-flight cap
 *  - release frees a global slot
 */

#include "config.h"

#include <stdio.h>
#include <string.h>
#include <stdlib.h>

#include "libckpool.h"
#include "sv2_conn.h"

static int fails;

static void expect(bool cond, const char *msg)
{
	if (!cond) {
		fprintf(stderr, "FAIL: %s\n", msg);
		fails++;
	} else {
		printf("ok: %s\n", msg);
	}
}

int main(void)
{
	int i;

	fails = 0;

	/* Per-IP: SV2_HS_MAX_PER_MIN is 30 — first 30 succeed, 31st fails. */
	for (i = 0; i < 30; i++) {
		if (!sv2_handshake_try_reserve("10.0.0.1")) {
			fprintf(stderr, "FAIL: reserve %d/30 for 10.0.0.1\n", i + 1);
			fails++;
			break;
		}
	}
	expect(i == 30, "30 reserves for one IP succeed");
	expect(!sv2_handshake_try_reserve("10.0.0.1"),
	       "31st reserve for same IP is denied");

	/* Different IP still allowed (global not full at 30). */
	expect(sv2_handshake_try_reserve("10.0.0.2"),
	       "different IP can still reserve");

	/* Release one global slot; per-IP window for .1 is still full. */
	sv2_handshake_release();
	expect(!sv2_handshake_try_reserve("10.0.0.1"),
	       "release does not reset per-IP window count");

	/*
	 * After release: global inflight = 30 (29 residual from .1's 30
	 * reserves + 1 from .2, or equivalent). Fill to SV2_HS_MAX_GLOBAL
	 * (256) with distinct IPs, then the next must fail.
	 */
	{
		int held = 30;
		char addr[64];
		int n;

		for (n = 3; held < 256; n++) {
			snprintf(addr, sizeof(addr), "10.1.%d.%d", n / 250, n % 250);
			if (!sv2_handshake_try_reserve(addr)) {
				fprintf(stderr, "FAIL: reserve while filling global at held=%d addr=%s\n",
					held, addr);
				fails++;
				break;
			}
			held++;
		}
		expect(held == 256, "can fill global inflight to 256");
		expect(!sv2_handshake_try_reserve("10.9.9.9"),
		       "257th concurrent handshake denied by global cap");
	}

	/* One release allows one more global grant (new IP with fresh window). */
	sv2_handshake_release();
	expect(sv2_handshake_try_reserve("10.9.9.9"),
	       "after release, new IP can reserve under global cap");

	if (fails) {
		fprintf(stderr, "%d failure(s)\n", fails);
		return 1;
	}
	printf("All SV2 handshake limit tests passed\n");
	return 0;
}
