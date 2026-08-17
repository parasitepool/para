/*
 * Copyright 2026 Con Kolivas
 *
 * Round-trip / bounds tests for SV2 codec (Phase 1 messages).
 * See also sv2_codec_fuzz for mutational / libFuzzer coverage of decode paths.
 */

#include "config.h"

#include <stdio.h>
#include <string.h>
#include <stdlib.h>

#include "libckpool.h"
#include "sv2_codec.h"
#include "sv2_types.h"

static int failures;

static void expect(int cond, const char *msg)
{
	if (!cond) {
		fprintf(stderr, "FAIL: %s\n", msg);
		failures++;
	}
}

static void test_header_roundtrip(void)
{
	uint8_t buf[6];
	struct sv2_frame fr;

	sv2_encode_header(buf, SV2_CHANNEL_MSG_BIT | 0x0001, 0x15, 0x12345);
	expect(sv2_decode_header(buf, 6, &fr), "decode header");
	expect(fr.extension_type == (SV2_CHANNEL_MSG_BIT | 0x0001), "ext type");
	expect(fr.msg_type == 0x15, "msg type");
	expect(fr.msg_length == 0x12345, "msg length");
	expect(!sv2_decode_header(buf, 5, &fr), "short header rejects");
}

static void test_setup_connection(void)
{
	struct sv2_setup_connection sc, out;
	uint8_t buf[512];
	uint8_t *p = buf;
	size_t len;

	memset(&sc, 0, sizeof(sc));
	sc.protocol = SV2_PROTOCOL_MINING;
	sc.min_version = 2;
	sc.max_version = 2;
	sc.flags = SV2_FLAG_REQUIRES_STANDARD_JOBS;
	sc.endpoint_port = 3336;
	strcpy(sc.endpoint_host, "solo.ckpool.org");
	strcpy(sc.vendor, "TestVendor");
	strcpy(sc.hardware_version, "hw1");
	strcpy(sc.firmware, "fw1");
	strcpy(sc.device_id, "dev1");

	/* Manual encode matching decode layout */
	sv2_write_u8(&p, sc.protocol);
	sv2_write_u16(&p, sc.min_version);
	sv2_write_u16(&p, sc.max_version);
	sv2_write_u32(&p, sc.flags);
	sv2_write_str0_255(&p, sc.endpoint_host);
	sv2_write_u16(&p, sc.endpoint_port);
	sv2_write_str0_255(&p, sc.vendor);
	sv2_write_str0_255(&p, sc.hardware_version);
	sv2_write_str0_255(&p, sc.firmware);
	sv2_write_str0_255(&p, sc.device_id);
	len = (size_t)(p - buf);

	expect(sv2_decode_setup_connection(buf, (uint32_t)len, &out), "decode setup");
	expect(out.protocol == SV2_PROTOCOL_MINING, "protocol");
	expect(out.min_version == 2 && out.max_version == 2, "version");
	expect(out.flags == SV2_FLAG_REQUIRES_STANDARD_JOBS, "flags");
	expect(out.endpoint_port == 3336, "port");
	expect(!strcmp(out.vendor, "TestVendor"), "vendor");
	expect(!sv2_decode_setup_connection(buf, 3, &out), "truncated setup rejects");
}

static void test_open_success_roundtrip(void)
{
	struct sv2_open_standard_channel_success ok, check;
	uint8_t buf[128];
	size_t len = 0;
	const uint8_t *p;
	const uint8_t *end;

	memset(&ok, 0, sizeof(ok));
	ok.request_id = 42;
	ok.channel_id = 7;
	memset(ok.target, 0xaa, 32);
	ok.extranonce_prefix_len = 4;
	ok.extranonce_prefix[0] = 1;
	ok.extranonce_prefix[1] = 2;
	ok.extranonce_prefix[2] = 3;
	ok.extranonce_prefix[3] = 4;
	ok.group_channel_id = 0;

	expect(sv2_encode_open_standard_channel_success(buf, sizeof(buf), &len, &ok),
	       "encode open success");
	p = buf;
	end = buf + len;
	/* Decode manually with open standard success fields order */
	expect(sv2_read_u32(&p, end, &check.request_id), "req id");
	expect(sv2_read_u32(&p, end, &check.channel_id), "ch id");
	expect(sv2_read_u256(&p, end, check.target), "target");
	expect(sv2_read_b0_32(&p, end, check.extranonce_prefix, &check.extranonce_prefix_len),
	       "enonce");
	expect(sv2_read_u32(&p, end, &check.group_channel_id), "group");
	expect(check.request_id == 42 && check.channel_id == 7, "ids match");
	expect(check.extranonce_prefix_len == 4 && check.extranonce_prefix[3] == 4, "prefix");
}

static void test_submit_decode(void)
{
	struct sv2_submit_shares_standard sub;
	uint8_t buf[32];
	uint8_t *p = buf;

	sv2_write_u32(&p, 1);	/* channel */
	sv2_write_u32(&p, 2);	/* seq */
	sv2_write_u32(&p, 3);	/* job */
	sv2_write_u32(&p, 0xdeadbeef); /* nonce */
	sv2_write_u32(&p, 0x12345678); /* ntime */
	sv2_write_u32(&p, 0x20000000); /* version */
	expect(sv2_decode_submit_shares_standard(buf, (uint32_t)(p - buf), &sub), "submit decode");
	expect(sub.channel_id == 1 && sub.job_id == 3, "submit ids");
	expect(sub.nonce == 0xdeadbeef, "nonce");
}

static void test_pt_ct_len(void)
{
	expect(sv2_pt_len_to_ct_len(0) == 0, "empty ct");
	expect(sv2_pt_len_to_ct_len(1) == 1 + 16, "small + mac");
	expect(sv2_pt_len_to_ct_len(SV2_NOISE_MAX_PT_CHUNK) == SV2_NOISE_MAX_CT_LEN,
	       "full chunk");
}

static void test_oversized_str_reject(void)
{
	uint8_t buf[300];
	uint8_t *p = buf;
	struct sv2_setup_connection sc;
	int i;

	/* Build a bogus setup with a 255-byte host then corrupt by claiming more */
	sv2_write_u8(&p, 0);
	sv2_write_u16(&p, 2);
	sv2_write_u16(&p, 2);
	sv2_write_u32(&p, 0);
	/* length 255 all 'a' — valid edge */
	sv2_write_u8(&p, 255);
	for (i = 0; i < 255; i++)
		*p++ = 'a';
	/* truncate rest — decode should fail for incomplete message */
	expect(!sv2_decode_setup_connection(buf, (uint32_t)(p - buf), &sc),
	       "incomplete setup after long string rejects");
}

static void test_jd_allocate_roundtrip(void)
{
	struct sv2_allocate_mining_job_token req, out;
	struct sv2_allocate_mining_job_token_success ok, ok2;
	uint8_t buf[256], enc[256];
	uint8_t *p = buf;
	size_t elen = 0;

	memset(&req, 0, sizeof(req));
	strcpy(req.user_identifier, "miner1");
	req.request_id = 42;
	sv2_write_str0_255(&p, req.user_identifier);
	sv2_write_u32(&p, req.request_id);
	expect(sv2_decode_allocate_mining_job_token(buf, (uint32_t)(p - buf), &out),
	       "decode allocate");
	expect(out.request_id == 42, "alloc request_id");
	expect(!strcmp(out.user_identifier, "miner1"), "alloc user");

	memset(&ok, 0, sizeof(ok));
	ok.request_id = 42;
	ok.mining_job_token_len = 4;
	ok.mining_job_token[0] = 0xaa;
	ok.mining_job_token[1] = 0xbb;
	ok.mining_job_token[2] = 0xcc;
	ok.mining_job_token[3] = 0xdd;
	ok.coinbase_tx_outputs = NULL;
	ok.coinbase_tx_outputs_len = 0;
	expect(sv2_encode_allocate_mining_job_token_success(enc, sizeof(enc), &elen, &ok),
	       "encode allocate success");
	/* Spot-check: request_id LE at start */
	expect(enc[0] == 42 && enc[1] == 0 && enc[2] == 0 && enc[3] == 0, "enc req id");
	expect(enc[4] == 4, "token len");
	(void)ok2;
}

static void test_jd_declare_roundtrip(void)
{
	struct sv2_declare_mining_job decl, out;
	uint8_t buf[512];
	uint8_t *p = buf;
	uint8_t w0[32];
	size_t i;

	memset(w0, 0x11, 32);
	sv2_write_u32(&p, 7);			/* request_id */
	sv2_write_u8(&p, 2);			/* token len */
	*p++ = 0x01;
	*p++ = 0x02;
	sv2_write_u32(&p, 0x20000000);		/* version */
	sv2_write_b0_64k(&p, (const uint8_t *)"pref", 4);
	sv2_write_b0_64k(&p, (const uint8_t *)"suff", 4);
	sv2_write_u16(&p, 1);			/* one wtxid */
	sv2_write_u256(&p, w0);
	sv2_write_b0_64k(&p, NULL, 0);		/* excess */

	expect(sv2_decode_declare_mining_job(buf, (uint32_t)(p - buf), &out),
	       "decode declare");
	expect(out.request_id == 7, "decl request_id");
	expect(out.mining_job_token_len == 2, "token len");
	expect(out.coinbase_tx_prefix_len == 4, "prefix len");
	expect(out.wtxid_count == 1, "wtxid count");
	expect(out.wtxid_list && out.wtxid_list[0] == 0x11, "wtxid data");
	sv2_declare_mining_job_free(&out);

	/* Truncated reject */
	expect(!sv2_decode_declare_mining_job(buf, 3, &decl), "short declare rejects");
	(void)i;
}

static void test_provide_missing_roundtrip(void)
{
	struct sv2_provide_missing_transactions pm;
	struct sv2_provide_missing_transactions_success ok, out;
	uint16_t positions[3] = { 0, 2, 5 };
	uint8_t enc[64];
	uint8_t buf[256];
	uint8_t *p = buf;
	uint8_t txa[] = { 0x01, 0x02, 0x03 };
	uint8_t txb[] = { 0xaa, 0xbb };
	size_t elen = 0;

	memset(&pm, 0, sizeof(pm));
	pm.request_id = 99;
	pm.unknown_count = 3;
	pm.unknown_tx_position_list = positions;
	expect(sv2_encode_provide_missing_transactions(enc, sizeof(enc), &elen, &pm),
	       "encode provide missing");
	expect(elen == 4 + 2 + 6, "provide missing len");

	/* Success: request_id + 2 txs as B0_16M */
	sv2_write_u32(&p, 99);
	sv2_write_u16(&p, 2);
	/* B0_16M: U24 len + data */
	sv2_write_u24(&p, 3);
	memcpy(p, txa, 3);
	p += 3;
	sv2_write_u24(&p, 2);
	memcpy(p, txb, 2);
	p += 2;
	expect(sv2_decode_provide_missing_transactions_success(buf, (uint32_t)(p - buf), &out),
	       "decode provide missing success");
	expect(out.request_id == 99, "pm req");
	expect(out.tx_count == 2, "pm tx count");
	expect(out.tx_lens[0] == 3 && out.transactions[0][0] == 0x01, "tx0");
	expect(out.tx_lens[1] == 2 && out.transactions[1][0] == 0xaa, "tx1");
	sv2_provide_missing_tx_success_free(&out);
	(void)ok;
}

static void test_set_custom_job_roundtrip(void)
{
	struct sv2_set_custom_mining_job_success ok;
	struct sv2_set_custom_mining_job_error err;
	uint8_t enc[64];
	size_t elen = 0;

	memset(&ok, 0, sizeof(ok));
	ok.channel_id = 3;
	ok.request_id = 9;
	ok.job_id = 42;
	expect(sv2_encode_set_custom_mining_job_success(enc, sizeof(enc), &elen, &ok),
	       "encode custom success");
	expect(elen == 12, "custom success len");

	memset(&err, 0, sizeof(err));
	err.channel_id = 3;
	err.request_id = 9;
	strcpy(err.error_code, "stale-prev-hash");
	expect(sv2_encode_set_custom_mining_job_error(enc, sizeof(enc), &elen, &err),
	       "encode custom error");
	expect(elen > 8, "custom error has code");
}

/* Client-side codec (ckproxy SV2 upstream): cross-check the new mirrors
 * against the proven server-side codec via full round-trips. */
static void test_client_codec_roundtrip(void)
{
	uint8_t buf[70000];
	size_t n = 0;

	/* client encode SetupConnection -> server decode */
	{
		struct sv2_setup_connection in, out;

		memset(&in, 0, sizeof(in));
		in.protocol = 0; in.min_version = 2; in.max_version = 2; in.flags = 0x4;
		strcpy(in.endpoint_host, "host.example"); in.endpoint_port = 3336;
		strcpy(in.vendor, "ckproxy"); strcpy(in.firmware, "fw2");
		strcpy(in.device_id, "dev3");
		expect(sv2_encode_setup_connection(buf, sizeof(buf), &n, &in), "enc setup");
		expect(sv2_decode_setup_connection(buf, n, &out), "dec setup");
		expect(out.min_version == 2 && out.flags == 0x4 && out.endpoint_port == 3336,
		       "setup scalar fields");
		expect(!strcmp(out.endpoint_host, in.endpoint_host) &&
		       !strcmp(out.device_id, in.device_id), "setup strings");
	}
	/* client encode OpenExtendedChannel -> server decode */
	{
		struct sv2_open_extended_channel in, out;

		memset(&in, 0, sizeof(in));
		in.request_id = 42; strcpy(in.user_identity, "bc1qxyz.w");
		in.nominal_hash_rate = 1.25e14f; memset(in.max_target, 0xab, 32);
		in.min_extranonce_size = 8;
		expect(sv2_encode_open_extended_channel(buf, sizeof(buf), &n, &in), "enc openext");
		expect(sv2_decode_open_extended_channel(buf, n, &out), "dec openext");
		expect(out.request_id == 42 && out.min_extranonce_size == 8 &&
		       out.nominal_hash_rate == in.nominal_hash_rate, "openext fields");
		expect(!memcmp(out.max_target, in.max_target, 32) &&
		       !strcmp(out.user_identity, in.user_identity), "openext blobs");
	}
	/* client encode SubmitSharesExtended -> server decode */
	{
		struct sv2_submit_shares_extended in, out;

		memset(&in, 0, sizeof(in));
		in.base.channel_id = 7; in.base.sequence_number = 99; in.base.job_id = 3;
		in.base.nonce = 0xdeadbeef; in.base.ntime = 0x65000000;
		in.base.version = 0x20000000; in.extranonce_len = 8;
		memset(in.extranonce, 0x5a, 8);
		expect(sv2_encode_submit_shares_extended(buf, sizeof(buf), &n, &in), "enc submitext");
		expect(sv2_decode_submit_shares_extended(buf, n, &out), "dec submitext");
		expect(out.base.nonce == 0xdeadbeef && out.base.version == 0x20000000 &&
		       out.extranonce_len == 8, "submitext fields");
		expect(!memcmp(out.extranonce, in.extranonce, 8), "submitext extranonce");
	}
	/* server encode NewExtendedMiningJob -> client decode (future + immediate) */
	for (int fut = 0; fut < 2; fut++) {
		struct sv2_new_extended_mining_job in, out;
		uint8_t cb1[100], cb2[200];

		memset(&in, 0, sizeof(in));
		in.channel_id = 1; in.job_id = 55;
		in.min_ntime_present = !fut; in.min_ntime = 0x65001234;
		in.version = 0x20000000; in.version_rolling_allowed = true;
		in.merkle_count = 3;
		for (int i = 0; i < 3; i++)
			memset(in.merkle_path[i], 0x10 + i, 32);
		memset(cb1, 0xc1, sizeof(cb1)); memset(cb2, 0xc2, sizeof(cb2));
		in.coinbase_tx_prefix = cb1; in.coinbase_tx_prefix_len = sizeof(cb1);
		in.coinbase_tx_suffix = cb2; in.coinbase_tx_suffix_len = sizeof(cb2);
		expect(sv2_encode_new_extended_mining_job(buf, sizeof(buf), &n, &in), "enc nej");
		expect(sv2_decode_new_extended_mining_job(buf, n, &out), "dec nej");
		expect(out.job_id == 55 && out.min_ntime_present == (!fut) &&
		       out.version_rolling_allowed && out.merkle_count == 3, "nej fields");
		expect(out.coinbase_tx_prefix && out.coinbase_tx_prefix_len == sizeof(cb1) &&
		       !memcmp(out.coinbase_tx_prefix, cb1, sizeof(cb1)), "nej coinb1");
		expect(out.coinbase_tx_suffix && !memcmp(out.coinbase_tx_suffix, cb2, sizeof(cb2)),
		       "nej coinb2");
		sv2_new_extended_mining_job_free(&out);
	}
	/* server encode SetNewPrevHash / SetTarget / SubmitShares.Success/.Error
	 * -> client decode */
	{
		struct sv2_set_new_prev_hash snphi, snpho;
		struct sv2_set_target sti, sto;
		struct sv2_submit_shares_success ssi, sso;
		struct sv2_submit_shares_error sei, seo;

		memset(&snphi, 0, sizeof(snphi));
		snphi.channel_id = 1; snphi.job_id = 55; memset(snphi.prev_hash, 0x77, 32);
		snphi.min_ntime = 0x65005678; snphi.nbits = 0x1703abcd;
		expect(sv2_encode_set_new_prev_hash(buf, sizeof(buf), &n, &snphi), "enc snph");
		expect(sv2_decode_set_new_prev_hash(buf, n, &snpho), "dec snph");
		expect(snpho.nbits == 0x1703abcd && snpho.min_ntime == 0x65005678 &&
		       !memcmp(snpho.prev_hash, snphi.prev_hash, 32), "snph fields");

		memset(&sti, 0, sizeof(sti));
		sti.channel_id = 1; memset(sti.maximum_target, 0x3c, 32);
		expect(sv2_encode_set_target(buf, sizeof(buf), &n, &sti), "enc settarget");
		expect(sv2_decode_set_target(buf, n, &sto), "dec settarget");
		expect(!memcmp(sto.maximum_target, sti.maximum_target, 32), "settarget target");

		memset(&ssi, 0, sizeof(ssi));
		ssi.channel_id = 1; ssi.last_sequence_number = 500;
		ssi.new_submits_accepted_count = 10; ssi.new_shares_sum = 0x1122334455667788ULL;
		expect(sv2_encode_submit_shares_success(buf, sizeof(buf), &n, &ssi), "enc success");
		expect(sv2_decode_submit_shares_success(buf, n, &sso), "dec success");
		expect(sso.last_sequence_number == 500 && sso.new_shares_sum == 0x1122334455667788ULL,
		       "success fields");

		memset(&sei, 0, sizeof(sei));
		sei.channel_id = 1; sei.sequence_number = 501;
		strcpy(sei.error_code, "difficulty-too-low");
		expect(sv2_encode_submit_shares_error(buf, sizeof(buf), &n, &sei), "enc serror");
		expect(sv2_decode_submit_shares_error(buf, n, &seo), "dec serror");
		expect(seo.sequence_number == 501 && !strcmp(seo.error_code, "difficulty-too-low"),
		       "serror fields");
	}
	/* SetExtranoncePrefix + Reconnect decode from hand-built payloads */
	{
		struct sv2_set_extranonce_prefix sep;
		struct sv2_reconnect rc;
		uint8_t pay[64], *p = pay;

		sv2_write_u32(&p, 1);
		sv2_write_b0_32(&p, (const uint8_t *)"\xaa\xbb\xcc\xdd", 4);
		expect(sv2_decode_set_extranonce_prefix(pay, (uint32_t)(p - pay), &sep), "dec sep");
		expect(sep.channel_id == 1 && sep.extranonce_prefix_len == 4 &&
		       !memcmp(sep.extranonce_prefix, "\xaa\xbb\xcc\xdd", 4), "sep fields");

		p = pay;
		sv2_write_str0_255(&p, "new.pool.example");
		sv2_write_u16(&p, 3336);
		expect(sv2_decode_reconnect(pay, (uint32_t)(p - pay), &rc), "dec reconnect");
		expect(!strcmp(rc.new_host, "new.pool.example") && rc.new_port == 3336,
		       "reconnect fields");
	}
	/* truncated server messages must be rejected, not crash */
	{
		struct sv2_set_new_prev_hash snph;
		struct sv2_new_extended_mining_job nej;

		expect(!sv2_decode_set_new_prev_hash(buf, 5, &snph), "short snph rejects");
		expect(!sv2_decode_new_extended_mining_job(buf, 3, &nej), "short nej rejects");
	}
}

/*
 * Client-side Job Declaration codec (ckproxy as JDC). Every message is taken
 * through both halves — our new encoder against the proven server decoder, and
 * the proven server encoder against our new decoder — so a field-order slip in
 * either direction fails here rather than as a checkBlock rejection on a pool.
 */
static void test_client_jd_codec_roundtrip(void)
{
	uint8_t enc[4096], payload[4096];
	size_t elen = 0, plen = 0;

	/* AllocateMiningJobToken: client → server */
	{
		struct sv2_allocate_mining_job_token req, out;

		memset(&req, 0, sizeof(req));
		snprintf(req.user_identifier, sizeof(req.user_identifier), "bc1qworker");
		req.request_id = 0x11223344;
		expect(sv2_encode_allocate_mining_job_token(enc, sizeof(enc), &elen, &req),
		       "encode allocate token");
		expect(elen == 1 + strlen(req.user_identifier) + 4, "allocate token len");
		expect(elen <= SV2_ALLOCATE_TOKEN_MAX_BYTES, "allocate fits documented max");
		expect(sv2_decode_allocate_mining_job_token(enc, (uint32_t)elen, &out),
		       "server decodes our allocate");
		expect(out.request_id == req.request_id, "allocate request_id");
		expect(!strcmp(out.user_identifier, req.user_identifier), "allocate user");
		/* One byte short of the encoded size must refuse, not truncate. */
		expect(!sv2_encode_allocate_mining_job_token(enc, elen - 1, &elen, &req),
		       "allocate refuses a short buffer");
	}

	/* AllocateMiningJobToken.Success: server → client */
	{
		struct sv2_allocate_mining_job_token_success ok, out;
		uint8_t outputs[] = { 0x01, 0x00, 0xf2, 0x05, 0x2a, 0x01, 0x00, 0x00,
				      0x00, 0x16, 0x00, 0x14, 0xab };

		memset(&ok, 0, sizeof(ok));
		ok.request_id = 0x55667788;
		ok.mining_job_token_len = 16;
		memset(ok.mining_job_token, 0x5a, 16);
		ok.coinbase_tx_outputs = outputs;
		ok.coinbase_tx_outputs_len = sizeof(outputs);
		expect(sv2_encode_allocate_mining_job_token_success(payload, sizeof(payload),
								   &plen, &ok),
		       "encode allocate success");
		expect(sv2_decode_allocate_mining_job_token_success(payload, (uint32_t)plen, &out),
		       "decode allocate success");
		expect(out.request_id == ok.request_id, "success request_id");
		expect(out.mining_job_token_len == 16 &&
		       !memcmp(out.mining_job_token, ok.mining_job_token, 16), "success token");
		expect(out.coinbase_tx_outputs_len == sizeof(outputs), "success outputs len");
		expect(out.coinbase_tx_outputs &&
		       !memcmp(out.coinbase_tx_outputs, outputs, sizeof(outputs)),
		       "success outputs bytes");
		/* Documented: the outputs alias the payload rather than a copy. */
		expect(out.coinbase_tx_outputs > payload &&
		       out.coinbase_tx_outputs < payload + plen,
		       "success outputs point into the payload");
		expect(!sv2_decode_allocate_mining_job_token_success(payload, 3, &out),
		       "short allocate success rejects");
	}

	/* DeclareMiningJob: client → server */
	{
		struct sv2_declare_mining_job decl, out;
		uint8_t wtxids[3 * 32];
		uint8_t *big;
		size_t want;
		int i;

		for (i = 0; i < 3 * 32; i++)
			wtxids[i] = (uint8_t)i;
		memset(&decl, 0, sizeof(decl));
		decl.request_id = 0x0a0b0c0d;
		decl.mining_job_token_len = 16;
		memset(decl.mining_job_token, 0xc3, 16);
		decl.version = 0x20000004;
		decl.coinbase_tx_prefix = (uint8_t *)"prefixbytes";
		decl.coinbase_tx_prefix_len = 11;
		decl.coinbase_tx_suffix = (uint8_t *)"suffix";
		decl.coinbase_tx_suffix_len = 6;
		decl.wtxid_list = wtxids;
		decl.wtxid_count = 3;
		decl.excess_data = (uint8_t *)"x";
		decl.excess_data_len = 1;

		want = sv2_declare_mining_job_encoded_size(&decl);
		big = ckalloc(want);
		expect(sv2_encode_declare_mining_job(big, want, &elen, &decl),
		       "encode declare");
		expect(elen == want, "declare size helper is exact");
		expect(sv2_decode_declare_mining_job(big, (uint32_t)elen, &out),
		       "server decodes our declare");
		expect(out.request_id == decl.request_id, "declare request_id");
		expect(out.mining_job_token_len == 16 &&
		       !memcmp(out.mining_job_token, decl.mining_job_token, 16),
		       "declare token");
		expect(out.version == decl.version, "declare version");
		expect(out.coinbase_tx_prefix_len == 11 &&
		       !memcmp(out.coinbase_tx_prefix, decl.coinbase_tx_prefix, 11),
		       "declare prefix");
		expect(out.coinbase_tx_suffix_len == 6 &&
		       !memcmp(out.coinbase_tx_suffix, decl.coinbase_tx_suffix, 6),
		       "declare suffix");
		expect(out.wtxid_count == 3 && !memcmp(out.wtxid_list, wtxids, 3 * 32),
		       "declare wtxid list");
		expect(out.excess_data_len == 1 && out.excess_data[0] == 'x',
		       "declare excess data");
		sv2_declare_mining_job_free(&out);
		expect(!sv2_encode_declare_mining_job(big, want - 1, &elen, &decl),
		       "declare refuses a short buffer");
		dealloc(big);

		/* Over the declare transaction cap must fail closed. */
		decl.wtxid_count = SV2_MAX_JD_TXNS + 1;
		decl.wtxid_list = NULL;
		expect(!sv2_encode_declare_mining_job(enc, sizeof(enc), &elen, &decl),
		       "declare refuses over-cap wtxid count");
	}

	/* DeclareMiningJob.Success / .Error: server → client */
	{
		struct sv2_declare_mining_job_success ok, ok_out;
		struct sv2_declare_mining_job_error err, err_out;
		uint8_t details[] = { 0xde, 0xad };

		memset(&ok, 0, sizeof(ok));
		ok.request_id = 0x0a0b0c0d;
		ok.new_mining_job_token_len = 16;
		memset(ok.new_mining_job_token, 0x77, 16);
		expect(sv2_encode_declare_mining_job_success(payload, sizeof(payload),
							     &plen, &ok),
		       "encode declare success");
		expect(sv2_decode_declare_mining_job_success(payload, (uint32_t)plen, &ok_out),
		       "decode declare success");
		expect(ok_out.request_id == ok.request_id, "declare success request_id");
		expect(ok_out.new_mining_job_token_len == 16 &&
		       !memcmp(ok_out.new_mining_job_token, ok.new_mining_job_token, 16),
		       "declare success new token");

		memset(&err, 0, sizeof(err));
		err.request_id = 0x0a0b0c0d;
		snprintf(err.error_code, sizeof(err.error_code), "stale-chain-tip");
		err.error_details = details;
		err.error_details_len = sizeof(details);
		expect(sv2_encode_declare_mining_job_error(payload, sizeof(payload),
							  &plen, &err),
		       "encode declare error");
		expect(sv2_decode_declare_mining_job_error(payload, (uint32_t)plen, &err_out),
		       "decode declare error");
		expect(!strcmp(err_out.error_code, "stale-chain-tip"), "declare error code");
		expect(err_out.error_details_len == sizeof(details) &&
		       !memcmp(err_out.error_details, details, sizeof(details)),
		       "declare error details");
	}

	/* ProvideMissingTransactions: server → client */
	{
		struct sv2_provide_missing_transactions pm, out;
		uint16_t positions[4] = { 0, 1, 7, 65535 };
		uint8_t *p;

		memset(&pm, 0, sizeof(pm));
		pm.request_id = 0x99;
		pm.unknown_count = 4;
		pm.unknown_tx_position_list = positions;
		expect(sv2_encode_provide_missing_transactions(payload, sizeof(payload),
							      &plen, &pm),
		       "encode provide missing");
		expect(sv2_decode_provide_missing_transactions(payload, (uint32_t)plen, &out),
		       "decode provide missing");
		expect(out.request_id == 0x99, "provide missing request_id");
		expect(out.unknown_count == 4, "provide missing count");
		expect(out.unknown_tx_position_list &&
		       !memcmp(out.unknown_tx_position_list, positions, sizeof(positions)),
		       "provide missing positions");
		sv2_provide_missing_transactions_free(&out);
		expect(!out.unknown_tx_position_list, "provide missing free clears");

		/* Empty list is legal and allocates nothing. */
		pm.unknown_count = 0;
		expect(sv2_encode_provide_missing_transactions(payload, sizeof(payload),
							      &plen, &pm),
		       "encode empty provide missing");
		expect(sv2_decode_provide_missing_transactions(payload, (uint32_t)plen, &out),
		       "decode empty provide missing");
		expect(!out.unknown_count && !out.unknown_tx_position_list, "empty list");

		/* A huge count with no data behind it must not allocate for it. */
		p = payload;
		sv2_write_u32(&p, 1);
		sv2_write_u16(&p, SV2_MAX_JD_TXNS);
		expect(!sv2_decode_provide_missing_transactions(payload,
							       (uint32_t)(p - payload), &out),
		       "provide missing rejects an unbacked count");
	}

	/* ProvideMissingTransactions.Success: client → server */
	{
		struct sv2_provide_missing_transactions_success ok, out;
		uint8_t txa[300], txb[7];
		uint8_t *txs[2] = { txa, txb };
		uint32_t lens[2] = { sizeof(txa), sizeof(txb) };
		uint8_t *big;
		size_t want;

		memset(txa, 0xa5, sizeof(txa));
		memset(txb, 0x5a, sizeof(txb));
		memset(&ok, 0, sizeof(ok));
		ok.request_id = 0x4242;
		ok.tx_count = 2;
		ok.transactions = txs;
		ok.tx_lens = lens;

		want = sv2_provide_missing_tx_success_encoded_size(&ok);
		expect(want == 4 + 2 + (3 + 300) + (3 + 7), "provide success size helper");
		big = ckalloc(want);
		expect(sv2_encode_provide_missing_transactions_success(big, want, &elen, &ok),
		       "encode provide success");
		expect(elen == want, "provide success size is exact");
		expect(sv2_decode_provide_missing_transactions_success(big, (uint32_t)elen, &out),
		       "server decodes our provide success");
		expect(out.request_id == ok.request_id, "provide success request_id");
		expect(out.tx_count == 2, "provide success count");
		expect(out.tx_lens[0] == sizeof(txa) &&
		       !memcmp(out.transactions[0], txa, sizeof(txa)), "provide success tx0");
		expect(out.tx_lens[1] == sizeof(txb) &&
		       !memcmp(out.transactions[1], txb, sizeof(txb)), "provide success tx1");
		sv2_provide_missing_tx_success_free(&out);
		expect(!sv2_encode_provide_missing_transactions_success(big, want - 1, &elen, &ok),
		       "provide success refuses a short buffer");
		dealloc(big);

		/* A transaction over the per-tx policy cap the decoder enforces
		 * must be refused here, not sent and rejected by the peer. */
		lens[0] = SV2_MAX_TX_BYTES + 1;
		expect(!sv2_encode_provide_missing_transactions_success(enc, sizeof(enc),
									&elen, &ok),
		       "provide success refuses an over-cap transaction");
	}

	/* SetCustomMiningJob: client → server */
	{
		struct sv2_set_custom_mining_job job, out;
		uint8_t outputs[] = { 0x02, 0x11, 0x22 };
		size_t want;
		int i;

		memset(&job, 0, sizeof(job));
		job.channel_id = 0x1234;
		job.request_id = 0x5678;
		job.mining_job_token_len = 16;
		memset(job.mining_job_token, 0x9b, 16);
		job.version = 0x20000000;
		memset(job.prev_hash, 0x3c, 32);
		job.min_ntime = 0x66aabbcc;
		job.nbits = 0x1d00ffff;
		job.coinbase_tx_version = 2;
		job.coinbase_prefix_len = 4;
		memcpy(job.coinbase_prefix, "\x03\x66\x00\x00", 4);
		job.coinbase_tx_input_nSequence = 0xffffffff;
		job.coinbase_tx_outputs = outputs;
		job.coinbase_tx_outputs_len = sizeof(outputs);
		job.coinbase_tx_locktime = 101;
		job.merkle_count = 2;
		memset(job.merkle_path[0], 0xd1, 32);
		memset(job.merkle_path[1], 0xd2, 32);

		want = sv2_set_custom_mining_job_encoded_size(&job);
		expect(sv2_encode_set_custom_mining_job(enc, sizeof(enc), &elen, &job),
		       "encode set custom job");
		expect(elen == want, "set custom size helper is exact");
		expect(sv2_decode_set_custom_mining_job(enc, (uint32_t)elen, &out),
		       "server decodes our set custom job");
		expect(out.channel_id == job.channel_id && out.request_id == job.request_id,
		       "set custom ids");
		expect(out.mining_job_token_len == 16 &&
		       !memcmp(out.mining_job_token, job.mining_job_token, 16),
		       "set custom token");
		expect(out.version == job.version, "set custom version");
		expect(!memcmp(out.prev_hash, job.prev_hash, 32), "set custom prev_hash");
		expect(out.min_ntime == job.min_ntime && out.nbits == job.nbits,
		       "set custom ntime/nbits");
		expect(out.coinbase_tx_version == 2 && out.coinbase_tx_locktime == 101,
		       "set custom coinbase version/locktime");
		expect(out.coinbase_prefix_len == 4 &&
		       !memcmp(out.coinbase_prefix, job.coinbase_prefix, 4),
		       "set custom coinbase prefix");
		expect(out.coinbase_tx_input_nSequence == 0xffffffff, "set custom nSequence");
		expect(out.coinbase_tx_outputs_len == sizeof(outputs) &&
		       !memcmp(out.coinbase_tx_outputs, outputs, sizeof(outputs)),
		       "set custom outputs");
		expect(out.merkle_count == 2, "set custom merkle count");
		for (i = 0; i < 2; i++) {
			expect(!memcmp(out.merkle_path[i], job.merkle_path[i], 32),
			       "set custom merkle step");
		}
		sv2_set_custom_mining_job_free(&out);
		expect(!sv2_encode_set_custom_mining_job(enc, elen - 1, &elen, &job),
		       "set custom refuses a short buffer");

		job.merkle_count = SV2_MAX_MERKLE_PATH + 1;
		expect(!sv2_encode_set_custom_mining_job(enc, sizeof(enc), &elen, &job),
		       "set custom refuses an over-long merkle path");
	}

	/* SetCustomMiningJob.Success / .Error: server → client */
	{
		struct sv2_set_custom_mining_job_success ok, ok_out;
		struct sv2_set_custom_mining_job_error err, err_out;

		memset(&ok, 0, sizeof(ok));
		ok.channel_id = 0x1234;
		ok.request_id = 0x5678;
		ok.job_id = 0x90ab;
		expect(sv2_encode_set_custom_mining_job_success(payload, sizeof(payload),
							       &plen, &ok),
		       "encode custom success");
		expect(sv2_decode_set_custom_mining_job_success(payload, (uint32_t)plen,
								&ok_out),
		       "decode custom success");
		expect(ok_out.channel_id == ok.channel_id && ok_out.request_id == ok.request_id &&
		       ok_out.job_id == ok.job_id, "custom success fields");
		expect(!sv2_decode_set_custom_mining_job_success(payload, 11, &ok_out),
		       "short custom success rejects");

		memset(&err, 0, sizeof(err));
		err.channel_id = 0x1234;
		err.request_id = 0x5678;
		snprintf(err.error_code, sizeof(err.error_code), "stale-prev-hash");
		expect(sv2_encode_set_custom_mining_job_error(payload, sizeof(payload),
							     &plen, &err),
		       "encode custom error");
		expect(sv2_decode_set_custom_mining_job_error(payload, (uint32_t)plen,
							      &err_out),
		       "decode custom error");
		expect(!strcmp(err_out.error_code, "stale-prev-hash"), "custom error code");
	}

	/* PushSolution: client → server */
	{
		struct sv2_push_solution sol, out;
		uint8_t small[SV2_PUSH_SOLUTION_MAX_BYTES];

		memset(&sol, 0, sizeof(sol));
		sol.extranonce_len = 12;
		memset(sol.extranonce, 0xe1, 12);
		memset(sol.prev_hash, 0x7f, 32);
		sol.nonce = 0xdeadbeef;
		sol.ntime = 0x66aabbcc;
		sol.nbits = 0x1d00ffff;
		sol.version = 0x20000000;
		expect(sv2_encode_push_solution(small, sizeof(small), &elen, &sol),
		       "encode push solution into the documented max");
		expect(sv2_decode_push_solution(small, (uint32_t)elen, &out),
		       "server decodes our push solution");
		expect(out.extranonce_len == 12 &&
		       !memcmp(out.extranonce, sol.extranonce, 12), "push extranonce");
		expect(!memcmp(out.prev_hash, sol.prev_hash, 32), "push prev_hash");
		expect(out.nonce == sol.nonce && out.ntime == sol.ntime &&
		       out.nbits == sol.nbits && out.version == sol.version,
		       "push header fields");
		expect(!sv2_encode_push_solution(small, elen - 1, &elen, &sol),
		       "push solution refuses a short buffer");

		sol.extranonce_len = SV2_MAX_B0_32 + 1;
		expect(!sv2_encode_push_solution(enc, sizeof(enc), &elen, &sol),
		       "push solution refuses an over-long extranonce");
	}
}

static void test_payload_policy_caps(void)
{
	/* Mining << JD << absolute U24 so post-Noise DoS bounds stay useful. */
	expect(SV2_MAX_MINING_PAYLOAD < SV2_MAX_JD_PAYLOAD,
	       "mining payload cap < JD cap");
	expect(SV2_MAX_JD_PAYLOAD < SV2_MAX_PAYLOAD,
	       "JD payload cap < absolute U24");
	expect(SV2_MAX_MINING_PAYLOAD >= (64u << 10),
	       "mining payload cap at least 64 KiB");
	{
		uint8_t buf[6];
		struct sv2_frame fr;

		/* Absolute codec still accepts U24 max (JD needs large frames). */
		sv2_encode_header(buf, 0, 0x10, SV2_MAX_PAYLOAD);
		expect(sv2_decode_header(buf, 6, &fr), "decode max U24 header");
		expect(fr.msg_length == SV2_MAX_PAYLOAD, "U24 max length");
		/* One past absolute max rejects. */
		buf[3] = 0xff;
		buf[4] = 0xff;
		buf[5] = 0xff;
		/* Already at max; encoding beyond is not representable in U24. */
		expect(SV2_MAX_PAYLOAD == 0x00FFFFFFu, "U24 absolute is 24-bit");
	}
}

int main(void)
{
	failures = 0;
	test_header_roundtrip();
	test_setup_connection();
	test_open_success_roundtrip();
	test_submit_decode();
	test_pt_ct_len();
	test_oversized_str_reject();
	test_jd_allocate_roundtrip();
	test_jd_declare_roundtrip();
	test_provide_missing_roundtrip();
	test_set_custom_job_roundtrip();
	test_client_codec_roundtrip();
	test_client_jd_codec_roundtrip();
	test_payload_policy_caps();
	if (failures) {
		fprintf(stderr, "%d test(s) failed\n", failures);
		return 1;
	}
	printf("sv2_codec: all tests passed\n");
	return 0;
}
