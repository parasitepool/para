/*
 * Copyright 2026 Con Kolivas
 *
 * SV2 authority Base58Check test vector from sv2-spec 04 §4.7.1.
 */

#include "config.h"

#include <stdio.h>
#include <string.h>
#include <stdlib.h>

#include "sv2_noise.h"

int main(void)
{
	/* Spec 04.7.1 raw_ca_public_key */
	static const uint8_t raw[32] = {
		118, 99, 112, 0, 151, 156, 28, 17, 175, 12, 48, 11, 205, 140, 127, 228,
		134, 16, 252, 233, 185, 193, 30, 61, 174, 227, 90, 224, 176, 138, 116, 85
	};
	static const char *expect = "9bXiEd8boQVhq7WddEcERUL5tyyJVFYdU8th3HfbNXK3Yw6GRXh";
	char *got;

	got = sv2_noise_authority_xonly_to_b58(raw);
	if (!got) {
		fprintf(stderr, "FAIL: encode returned NULL\n");
		return 1;
	}
	if (strcmp(got, expect) != 0) {
		fprintf(stderr, "FAIL: got %s\n  expected %s\n", got, expect);
		free(got);
		return 1;
	}
	printf("sv2_authority_b58: test vector OK (%s)\n", got);
	free(got);
	return 0;
}
