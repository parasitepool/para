/*
 * Copyright 2026 Con Kolivas
 *
 * Fuzz / adversarial driver for SV2 plaintext codec decode paths.
 *
 * Build modes:
 *   - Default (make check): standalone mutational fuzzer, fixed iteration budget
 *   - libFuzzer: compile with -DFUZZING_BUILD_MODE_UNSAFE_FOR_PRODUCTION
 *     -fsanitize=fuzzer,address and use LLVMFuzzerTestOneInput as entry
 *
 * Only exercises decode (untrusted input). Does not run Noise crypto.
 */

#include "config.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <time.h>

#include "libckpool.h"
#include "sv2_codec.h"
#include "sv2_types.h"

/* Run every server→client decoder (ckproxy SV2 upstream path) on a payload.
 * NewExtendedMiningJob owns heap buffers on success — free them so a leak
 * sanitizer stays clean. */
static void try_client_decoders(const uint8_t *payload, uint32_t plen)
{
	struct sv2_setup_connection_success scs;
	struct sv2_setup_connection_error sce;
	struct sv2_open_extended_channel_success oes;
	struct sv2_open_channel_error oce;
	struct sv2_new_extended_mining_job nej;
	struct sv2_set_new_prev_hash snph;
	struct sv2_set_target st;
	struct sv2_set_extranonce_prefix sep;
	struct sv2_submit_shares_success sss;
	struct sv2_submit_shares_error sse;
	struct sv2_reconnect rc;

	(void)sv2_decode_setup_connection_success(payload, plen, &scs);
	(void)sv2_decode_setup_connection_error(payload, plen, &sce);
	(void)sv2_decode_open_extended_channel_success(payload, plen, &oes);
	(void)sv2_decode_open_channel_error(payload, plen, &oce);
	if (sv2_decode_new_extended_mining_job(payload, plen, &nej))
		sv2_new_extended_mining_job_free(&nej);
	(void)sv2_decode_set_new_prev_hash(payload, plen, &snph);
	(void)sv2_decode_set_target(payload, plen, &st);
	(void)sv2_decode_set_extranonce_prefix(payload, plen, &sep);
	(void)sv2_decode_submit_shares_success(payload, plen, &sss);
	(void)sv2_decode_submit_shares_error(payload, plen, &sse);
	(void)sv2_decode_reconnect(payload, plen, &rc);
}

/*
 * Every Job Declaration decoder, both directions: the JDS ones (a JD client is
 * an untrusted peer) and the JDC mirrors (so is a pool). Anything that owns
 * heap on success is freed so a leak sanitizer stays clean.
 */
static void try_jd_decoders(const uint8_t *payload, uint32_t plen)
{
	/* Server side: client → server */
	struct sv2_allocate_mining_job_token amjt;
	struct sv2_declare_mining_job dmj;
	struct sv2_provide_missing_transactions_success pmts;
	struct sv2_set_custom_mining_job scmj;
	struct sv2_push_solution ps;
	/* Client side: server → client */
	struct sv2_allocate_mining_job_token_success amjts;
	struct sv2_declare_mining_job_success dmjs;
	struct sv2_declare_mining_job_error dmje;
	struct sv2_provide_missing_transactions pmt;
	struct sv2_set_custom_mining_job_success scmjs;
	struct sv2_set_custom_mining_job_error scmje;

	(void)sv2_decode_allocate_mining_job_token(payload, plen, &amjt);
	if (sv2_decode_declare_mining_job(payload, plen, &dmj))
		sv2_declare_mining_job_free(&dmj);
	if (sv2_decode_provide_missing_transactions_success(payload, plen, &pmts))
		sv2_provide_missing_tx_success_free(&pmts);
	if (sv2_decode_set_custom_mining_job(payload, plen, &scmj))
		sv2_set_custom_mining_job_free(&scmj);
	(void)sv2_decode_push_solution(payload, plen, &ps);

	(void)sv2_decode_allocate_mining_job_token_success(payload, plen, &amjts);
	(void)sv2_decode_declare_mining_job_success(payload, plen, &dmjs);
	(void)sv2_decode_declare_mining_job_error(payload, plen, &dmje);
	if (sv2_decode_provide_missing_transactions(payload, plen, &pmt))
		sv2_provide_missing_transactions_free(&pmt);
	(void)sv2_decode_set_custom_mining_job_success(payload, plen, &scmjs);
	(void)sv2_decode_set_custom_mining_job_error(payload, plen, &scmje);
}

/* Feed arbitrary bytes through every decoder, both client→server and
 * server→client. Must not crash, hang, or read out of bounds. Return value
 * ignored by libFuzzer. */
int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size)
{
	struct sv2_frame fr;
	struct sv2_setup_connection sc;
	struct sv2_open_standard_channel os;
	struct sv2_open_extended_channel oe;
	struct sv2_submit_shares_standard ss;
	struct sv2_submit_shares_extended se;
	struct sv2_close_channel cl;
	const uint8_t *payload;
	uint32_t plen;
	uint8_t msg_type;

	if (!data)
		return 0;

	/* Header decode (any length) */
	(void)sv2_decode_header(data, size, &fr);

	/* As full frame: header + payload */
	if (size >= SV2_FRAME_HEADER_LEN && sv2_decode_header(data, size, &fr)) {
		if (fr.msg_length <= SV2_MAX_PAYLOAD &&
		    size >= SV2_FRAME_HEADER_LEN + fr.msg_length) {
			payload = data + SV2_FRAME_HEADER_LEN;
			plen = fr.msg_length;
			msg_type = fr.msg_type;
			switch (msg_type) {
			case SV2_MSG_SETUP_CONNECTION:
				(void)sv2_decode_setup_connection(payload, plen, &sc);
				break;
			case SV2_MSG_OPEN_STANDARD_MINING_CHANNEL:
				(void)sv2_decode_open_standard_channel(payload, plen, &os);
				break;
			case SV2_MSG_OPEN_EXTENDED_MINING_CHANNEL:
				(void)sv2_decode_open_extended_channel(payload, plen, &oe);
				break;
			case SV2_MSG_SUBMIT_SHARES_STANDARD:
				(void)sv2_decode_submit_shares_standard(payload, plen, &ss);
				break;
			case SV2_MSG_SUBMIT_SHARES_EXTENDED:
				(void)sv2_decode_submit_shares_extended(payload, plen, &se);
				break;
			case SV2_MSG_CLOSE_CHANNEL:
				(void)sv2_decode_close_channel(payload, plen, &cl);
				break;
			case SV2_MSG_NEW_EXTENDED_MINING_JOB:
			case SV2_MSG_SET_NEW_PREV_HASH:
			case SV2_MSG_SET_TARGET:
			case SV2_MSG_SET_EXTRANONCE_PREFIX:
			case SV2_MSG_SETUP_CONNECTION_SUCCESS:
			case SV2_MSG_SETUP_CONNECTION_ERROR:
			case SV2_MSG_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS:
			case SV2_MSG_OPEN_MINING_CHANNEL_ERROR:
			case SV2_MSG_SUBMIT_SHARES_SUCCESS:
			case SV2_MSG_SUBMIT_SHARES_ERROR:
			case SV2_MSG_RECONNECT:
				try_client_decoders(payload, plen);
				break;
			case SV2_MSG_ALLOCATE_MINING_JOB_TOKEN:
			case SV2_MSG_ALLOCATE_MINING_JOB_TOKEN_SUCCESS:
			case SV2_MSG_DECLARE_MINING_JOB:
			case SV2_MSG_DECLARE_MINING_JOB_SUCCESS:
			case SV2_MSG_DECLARE_MINING_JOB_ERROR:
			case SV2_MSG_PROVIDE_MISSING_TRANSACTIONS:
			case SV2_MSG_PROVIDE_MISSING_TRANSACTIONS_SUCCESS:
			case SV2_MSG_SET_CUSTOM_MINING_JOB:
			case SV2_MSG_SET_CUSTOM_MINING_JOB_SUCCESS:
			case SV2_MSG_SET_CUSTOM_MINING_JOB_ERROR:
			case SV2_MSG_PUSH_SOLUTION:
				try_jd_decoders(payload, plen);
				break;
			default:
				/* Try every decoder regardless of type — adversarial */
				(void)sv2_decode_setup_connection(payload, plen, &sc);
				(void)sv2_decode_open_standard_channel(payload, plen, &os);
				(void)sv2_decode_open_extended_channel(payload, plen, &oe);
				(void)sv2_decode_submit_shares_standard(payload, plen, &ss);
				(void)sv2_decode_submit_shares_extended(payload, plen, &se);
				(void)sv2_decode_close_channel(payload, plen, &cl);
				try_client_decoders(payload, plen);
				try_jd_decoders(payload, plen);
				break;
			}
		}
	}

	/* Also try raw buffer as payload for each decoder */
	plen = size > SV2_MAX_PAYLOAD ? SV2_MAX_PAYLOAD : (uint32_t)size;
	(void)sv2_decode_setup_connection(data, plen, &sc);
	(void)sv2_decode_open_standard_channel(data, plen, &os);
	(void)sv2_decode_open_extended_channel(data, plen, &oe);
	(void)sv2_decode_submit_shares_standard(data, plen, &ss);
	(void)sv2_decode_submit_shares_extended(data, plen, &se);
	(void)sv2_decode_close_channel(data, plen, &cl);
	try_client_decoders(data, plen);
	try_jd_decoders(data, plen);

	/* Length conversion for nonsense sizes */
	if (size >= 4) {
		uint32_t fake;

		memcpy(&fake, data, 4);
		(void)sv2_pt_len_to_ct_len(fake);
	}

	return 0;
}

#ifndef FUZZING_BUILD_MODE_UNSAFE_FOR_PRODUCTION

/* Deterministic PRNG (xorshift64) so make check is reproducible */
static uint64_t rng_state = 0xc0ffeef00dc0ffeull;

static uint64_t rng_next(void)
{
	uint64_t x = rng_state;

	x ^= x << 13;
	x ^= x >> 7;
	x ^= x << 17;
	rng_state = x;
	return x;
}

static void rng_fill(uint8_t *buf, size_t n)
{
	size_t i;

	for (i = 0; i < n; i++)
		buf[i] = (uint8_t)(rng_next() & 0xff);
}

/* Seed corpus: minimal valid-shaped and intentionally broken frames */
static void seed_inputs(void)
{
	uint8_t buf[512];
	uint8_t *p;
	size_t len;
	int i;

	/* Empty / tiny */
	LLVMFuzzerTestOneInput(NULL, 0);
	LLVMFuzzerTestOneInput((const uint8_t *)"", 0);
	LLVMFuzzerTestOneInput((const uint8_t *)"\x00", 1);
	LLVMFuzzerTestOneInput((const uint8_t *)"\x00\x00\x00\x00\x00", 5);

	/* Header only with huge claimed length */
	memset(buf, 0, 6);
	buf[2] = SV2_MSG_SETUP_CONNECTION;
	buf[3] = 0xff;
	buf[4] = 0xff;
	buf[5] = 0x0f; /* length 0xfffff but truncated body */
	LLVMFuzzerTestOneInput(buf, 6);

	/* Valid-ish SetupConnection */
	p = buf + 6;
	sv2_encode_header(buf, 0, SV2_MSG_SETUP_CONNECTION, 0); /* length fixed below */
	sv2_write_u8(&p, SV2_PROTOCOL_MINING);
	sv2_write_u16(&p, 2);
	sv2_write_u16(&p, 2);
	sv2_write_u32(&p, 0);
	sv2_write_str0_255(&p, "host");
	sv2_write_u16(&p, 3336);
	sv2_write_str0_255(&p, "vendor");
	sv2_write_str0_255(&p, "hw");
	sv2_write_str0_255(&p, "fw");
	sv2_write_str0_255(&p, "id");
	len = (size_t)(p - buf);
	sv2_encode_header(buf, 0, SV2_MSG_SETUP_CONNECTION, (uint32_t)(len - 6));
	LLVMFuzzerTestOneInput(buf, len);

	/* Submit standard */
	p = buf + 6;
	for (i = 0; i < 6; i++)
		sv2_write_u32(&p, (uint32_t)i + 1);
	len = (size_t)(p - buf);
	sv2_encode_header(buf, SV2_CHANNEL_MSG_BIT, SV2_MSG_SUBMIT_SHARES_STANDARD,
			  (uint32_t)(len - 6));
	LLVMFuzzerTestOneInput(buf, len);

	/* Valid-ish NewExtendedMiningJob (server→client, owned buffers path) */
	{
		struct sv2_new_extended_mining_job ej;
		uint8_t cb[32];
		size_t plen = 0;

		memset(&ej, 0, sizeof(ej));
		ej.channel_id = 1;
		ej.job_id = 7;
		ej.min_ntime_present = true;
		ej.min_ntime = 0x65000000;
		ej.version = 0x20000000;
		ej.version_rolling_allowed = true;
		ej.merkle_count = 2;
		memset(ej.merkle_path[0], 0xa1, 32);
		memset(ej.merkle_path[1], 0xa2, 32);
		memset(cb, 0xcb, sizeof(cb));
		ej.coinbase_tx_prefix = cb;
		ej.coinbase_tx_prefix_len = sizeof(cb);
		ej.coinbase_tx_suffix = cb;
		ej.coinbase_tx_suffix_len = sizeof(cb);
		if (sv2_encode_new_extended_mining_job(buf + 6, sizeof(buf) - 6, &plen, &ej)) {
			sv2_encode_header(buf, SV2_CHANNEL_MSG_BIT,
					  SV2_MSG_NEW_EXTENDED_MINING_JOB, (uint32_t)plen);
			LLVMFuzzerTestOneInput(buf, 6 + plen);
		}
	}

	/* Valid-ish SetNewPrevHash (server→client) */
	{
		struct sv2_set_new_prev_hash ph;
		size_t plen = 0;

		memset(&ph, 0, sizeof(ph));
		ph.channel_id = 1;
		ph.job_id = 7;
		memset(ph.prev_hash, 0x9b, 32);
		ph.min_ntime = 0x65000000;
		ph.nbits = 0x1703abcd;
		if (sv2_encode_set_new_prev_hash(buf + 6, sizeof(buf) - 6, &plen, &ph)) {
			sv2_encode_header(buf, SV2_CHANNEL_MSG_BIT,
					  SV2_MSG_SET_NEW_PREV_HASH, (uint32_t)plen);
			LLVMFuzzerTestOneInput(buf, 6 + plen);
		}
	}

	/* Valid-ish Job Declaration frames, so the mutator starts from message
	 * shapes the JD decoders actually accept (variable length lists, owned
	 * buffers) rather than only from Phase-1 ones. */
	{
		struct sv2_declare_mining_job dmj;
		struct sv2_provide_missing_transactions pmt;
		struct sv2_allocate_mining_job_token_success amjts;
		struct sv2_set_custom_mining_job scmj;
		uint8_t wtxids[2 * 32], outputs[8];
		uint16_t positions[3] = { 0, 1, 2 };
		size_t plen = 0;

		memset(wtxids, 0x3b, sizeof(wtxids));
		memset(outputs, 0x07, sizeof(outputs));

		memset(&dmj, 0, sizeof(dmj));
		dmj.request_id = 5;
		dmj.mining_job_token_len = 8;
		memset(dmj.mining_job_token, 0x42, 8);
		dmj.version = 0x20000000;
		dmj.coinbase_tx_prefix = outputs;
		dmj.coinbase_tx_prefix_len = sizeof(outputs);
		dmj.coinbase_tx_suffix = outputs;
		dmj.coinbase_tx_suffix_len = sizeof(outputs);
		dmj.wtxid_list = wtxids;
		dmj.wtxid_count = 2;
		if (sv2_encode_declare_mining_job(buf + 6, sizeof(buf) - 6, &plen, &dmj)) {
			sv2_encode_header(buf, 0, SV2_MSG_DECLARE_MINING_JOB, (uint32_t)plen);
			LLVMFuzzerTestOneInput(buf, 6 + plen);
		}

		memset(&pmt, 0, sizeof(pmt));
		pmt.request_id = 5;
		pmt.unknown_count = 3;
		pmt.unknown_tx_position_list = positions;
		if (sv2_encode_provide_missing_transactions(buf + 6, sizeof(buf) - 6, &plen,
							   &pmt)) {
			sv2_encode_header(buf, 0, SV2_MSG_PROVIDE_MISSING_TRANSACTIONS,
					  (uint32_t)plen);
			LLVMFuzzerTestOneInput(buf, 6 + plen);
		}

		memset(&amjts, 0, sizeof(amjts));
		amjts.request_id = 5;
		amjts.mining_job_token_len = 16;
		memset(amjts.mining_job_token, 0x9a, 16);
		amjts.coinbase_tx_outputs = outputs;
		amjts.coinbase_tx_outputs_len = sizeof(outputs);
		if (sv2_encode_allocate_mining_job_token_success(buf + 6, sizeof(buf) - 6,
								&plen, &amjts)) {
			sv2_encode_header(buf, 0, SV2_MSG_ALLOCATE_MINING_JOB_TOKEN_SUCCESS,
					  (uint32_t)plen);
			LLVMFuzzerTestOneInput(buf, 6 + plen);
		}

		memset(&scmj, 0, sizeof(scmj));
		scmj.channel_id = 1;
		scmj.request_id = 5;
		scmj.mining_job_token_len = 16;
		memset(scmj.mining_job_token, 0x9a, 16);
		scmj.version = 0x20000000;
		memset(scmj.prev_hash, 0x5c, 32);
		scmj.min_ntime = 0x65000000;
		scmj.nbits = 0x1703abcd;
		scmj.coinbase_tx_version = 2;
		scmj.coinbase_prefix_len = 4;
		memset(scmj.coinbase_prefix, 0x33, 4);
		scmj.coinbase_tx_input_nSequence = 0xffffffff;
		scmj.coinbase_tx_outputs = outputs;
		scmj.coinbase_tx_outputs_len = sizeof(outputs);
		scmj.merkle_count = 2;
		memset(scmj.merkle_path[0], 0xe1, 32);
		memset(scmj.merkle_path[1], 0xe2, 32);
		if (sv2_encode_set_custom_mining_job(buf + 6, sizeof(buf) - 6, &plen, &scmj)) {
			sv2_encode_header(buf, SV2_CHANNEL_MSG_BIT,
					  SV2_MSG_SET_CUSTOM_MINING_JOB, (uint32_t)plen);
			LLVMFuzzerTestOneInput(buf, 6 + plen);
		}
	}

	/* Length prefix 255 string but short buffer */
	memset(buf, 0, sizeof(buf));
	buf[0] = 255;
	LLVMFuzzerTestOneInput(buf, 10);
}

static void mutate(uint8_t *buf, size_t *len, size_t cap)
{
	uint64_t r = rng_next();
	unsigned op = (unsigned)(r % 6);
	size_t i, n;

	if (*len == 0) {
		*len = 1 + (rng_next() % (cap > 64 ? 64 : cap));
		rng_fill(buf, *len);
		return;
	}
	switch (op) {
	case 0: /* bit flip */
		i = rng_next() % *len;
		buf[i] ^= (uint8_t)(1u << (rng_next() % 8));
		break;
	case 1: /* random byte */
		i = rng_next() % *len;
		buf[i] = (uint8_t)rng_next();
		break;
	case 2: /* splice length field in header */
		if (*len >= 6) {
			buf[3] = (uint8_t)rng_next();
			buf[4] = (uint8_t)rng_next();
			buf[5] = (uint8_t)(rng_next() & 0x0f);
		}
		break;
	case 3: /* extend */
		n = 1 + (rng_next() % 16);
		if (*len + n > cap)
			n = cap - *len;
		rng_fill(buf + *len, n);
		*len += n;
		break;
	case 4: /* shrink */
		if (*len > 1)
			*len -= 1 + (rng_next() % (*len > 8 ? 8 : *len));
		break;
	default: /* full re-random small */
		*len = 1 + (rng_next() % 128);
		if (*len > cap)
			*len = cap;
		rng_fill(buf, *len);
		break;
	}
}

/* Standalone: seed corpus + mutational fuzz for a fixed budget (make check). */
int main(int argc, char **argv)
{
	uint8_t buf[4096];
	size_t len = 0;
	unsigned long iters = 50000;
	unsigned long i;
	const char *env;

	(void)argc;
	(void)argv;
	env = getenv("SV2_FUZZ_ITERS");
	if (env && *env) {
		unsigned long v = strtoul(env, NULL, 10);

		if (v > 0 && v < 10000000UL)
			iters = v;
	}

	seed_inputs();

	/* Mutate from last seed */
	len = 64;
	rng_fill(buf, len);
	for (i = 0; i < iters; i++) {
		mutate(buf, &len, sizeof(buf));
		LLVMFuzzerTestOneInput(buf, len);
	}

	printf("sv2_codec_fuzz: completed %lu mutational iterations + seeds (no crash)\n",
	       iters);
	return 0;
}

#endif /* !FUZZING_BUILD_MODE_UNSAFE_FOR_PRODUCTION */
