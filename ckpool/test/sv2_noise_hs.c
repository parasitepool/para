/*
 * Copyright 2026 Con Kolivas
 *
 * Noise NX self-handshake + partial-frame decrypt regression test.
 *
 * Validates:
 *  - ECDH ell_a/ell_b ordering (initiator vs responder) produces matching keys
 *  - SIGNATURE_NOISE_MESSAGE verifies under the authority key
 *  - Transport AEAD round-trip
 *  - Header decrypt is not re-run on incomplete payload (nonce desync bug)
 */

#include "config.h"

#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <stdbool.h>

#include <sodium.h>
#include <secp256k1.h>
#include <secp256k1_ellswift.h>
#include <secp256k1_extrakeys.h>
#include <secp256k1_schnorrsig.h>

#include "libckpool.h"
#include "sv2_noise.h"
#include "sv2_codec.h"
#include "sv2_types.h"

/* Minimal initiator to exercise the responder path (mirrors spec 04 §4.5). */

struct cipher_state {
	uint8_t k[32];
	uint64_t n;
	bool has_key;
};

struct initiator {
	secp256k1_context *secp;
	uint8_t ck[32], h[32];
	struct cipher_state cs;
	uint8_t e_sk[32], e_ell[64];
	uint8_t re_ell[64], rs_ell[64];
	struct cipher_state send_cs, recv_cs; /* initiator send = c1, recv = c2 */
	uint8_t authority_xonly[32];
	uint8_t server_xonly[32];
};

static const char noise_protocol_name[] =
	"Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256";

static void cs_init_key(struct cipher_state *c, const uint8_t key[32])
{
	memcpy(c->k, key, 32);
	c->n = 0;
	c->has_key = true;
}

static void noise_nonce(uint8_t nonce[12], uint64_t n)
{
	memset(nonce, 0, 4);
	nonce[4] = (uint8_t)(n & 0xff);
	nonce[5] = (uint8_t)((n >> 8) & 0xff);
	nonce[6] = (uint8_t)((n >> 16) & 0xff);
	nonce[7] = (uint8_t)((n >> 24) & 0xff);
	nonce[8] = (uint8_t)((n >> 32) & 0xff);
	nonce[9] = (uint8_t)((n >> 40) & 0xff);
	nonce[10] = (uint8_t)((n >> 48) & 0xff);
	nonce[11] = (uint8_t)((n >> 56) & 0xff);
}

static bool cs_encrypt(struct cipher_state *c, const uint8_t *ad, size_t adlen,
		       const uint8_t *pt, size_t ptlen, uint8_t *ct_out, size_t *ctlen)
{
	uint8_t nonce[12];

	if (!c->has_key) {
		memcpy(ct_out, pt, ptlen);
		*ctlen = ptlen;
		return true;
	}
	noise_nonce(nonce, c->n);
	if (crypto_aead_chacha20poly1305_ietf_encrypt(ct_out, NULL, pt, ptlen,
						     ad, adlen, NULL, nonce, c->k) != 0)
		return false;
	*ctlen = ptlen + SV2_NOISE_MAC_LEN;
	c->n++;
	return true;
}

static bool cs_decrypt(struct cipher_state *c, const uint8_t *ad, size_t adlen,
		       const uint8_t *ct, size_t ctlen, uint8_t *pt_out, size_t *ptlen)
{
	uint8_t nonce[12];

	if (!c->has_key) {
		memcpy(pt_out, ct, ctlen);
		*ptlen = ctlen;
		return true;
	}
	if (ctlen < SV2_NOISE_MAC_LEN)
		return false;
	noise_nonce(nonce, c->n);
	if (crypto_aead_chacha20poly1305_ietf_decrypt(pt_out, NULL, NULL, ct, ctlen,
						     ad, adlen, nonce, c->k) != 0)
		return false;
	*ptlen = ctlen - SV2_NOISE_MAC_LEN;
	c->n++;
	return true;
}

static void hmac_sha256(const uint8_t *key, size_t keylen,
			const uint8_t *data, size_t datalen, uint8_t out[32])
{
	crypto_auth_hmacsha256_state st;

	crypto_auth_hmacsha256_init(&st, key, keylen);
	crypto_auth_hmacsha256_update(&st, data, datalen);
	crypto_auth_hmacsha256_final(&st, out);
}

static void hkdf_sha256(const uint8_t chaining_key[32], const uint8_t *ikm, size_t ikm_len,
			uint8_t out1[32], uint8_t out2[32])
{
	uint8_t temp_key[32], buf[33];

	hmac_sha256(chaining_key, 32, ikm ? ikm : (const uint8_t *)"", ikm_len, temp_key);
	buf[0] = 0x01;
	hmac_sha256(temp_key, 32, buf, 1, out1);
	memcpy(buf, out1, 32);
	buf[32] = 0x02;
	hmac_sha256(temp_key, 32, buf, 33, out2);
	sodium_memzero(temp_key, sizeof(temp_key));
}

static void mix_hash(uint8_t h[32], const uint8_t *data, size_t len)
{
	crypto_hash_sha256_state st;

	crypto_hash_sha256_init(&st);
	crypto_hash_sha256_update(&st, h, 32);
	crypto_hash_sha256_update(&st, data, len);
	crypto_hash_sha256_final(&st, h);
}

static void mix_key(struct initiator *ini, const uint8_t *ikm, size_t ikm_len)
{
	uint8_t ck_new[32], temp_k[32];

	hkdf_sha256(ini->ck, ikm, ikm_len, ck_new, temp_k);
	memcpy(ini->ck, ck_new, 32);
	cs_init_key(&ini->cs, temp_k);
	sodium_memzero(temp_k, sizeof(temp_k));
}

static bool ellswift_ecdh_init(secp256k1_context *secp, const uint8_t *our_sk,
			       const uint8_t *our_ell, const uint8_t *their_ell,
			       bool we_are_initiator, uint8_t shared[32])
{
	if (we_are_initiator)
		return secp256k1_ellswift_xdh(secp, shared, our_ell, their_ell, our_sk, 0,
					      secp256k1_ellswift_xdh_hash_function_bip324,
					      NULL) == 1;
	return secp256k1_ellswift_xdh(secp, shared, their_ell, our_ell, our_sk, 1,
				      secp256k1_ellswift_xdh_hash_function_bip324,
				      NULL) == 1;
}

static bool decrypt_and_hash(struct initiator *ini, const uint8_t *ct, size_t ctlen,
			     uint8_t *pt, size_t *ptlen)
{
	if (!cs_decrypt(&ini->cs, ini->h, 32, ct, ctlen, pt, ptlen))
		return false;
	mix_hash(ini->h, ct, ctlen);
	return true;
}

static void init_hash(struct initiator *ini)
{
	uint8_t h2[32];
	size_t nlen = strlen(noise_protocol_name);

	crypto_hash_sha256(ini->h, (const uint8_t *)noise_protocol_name, nlen);
	memcpy(ini->ck, ini->h, 32);
	crypto_hash_sha256(h2, ini->h, 32);
	memcpy(ini->h, h2, 32);
	memset(&ini->cs, 0, sizeof(ini->cs));
}

static bool gen_e(struct initiator *ini)
{
	secp256k1_pubkey pub;
	unsigned char rnd[32];

	do {
		randombytes_buf(ini->e_sk, 32);
	} while (!secp256k1_ec_seckey_verify(ini->secp, ini->e_sk));
	if (!secp256k1_ec_pubkey_create(ini->secp, &pub, ini->e_sk))
		return false;
	randombytes_buf(rnd, 32);
	return secp256k1_ellswift_encode(ini->secp, ini->e_ell, &pub, rnd) == 1;
}

/* Build act1: e (64) + MixHash empty */
static bool initiator_act1(struct initiator *ini, uint8_t out[64])
{
	init_hash(ini);
	if (!gen_e(ini))
		return false;
	memcpy(out, ini->e_ell, 64);
	mix_hash(ini->h, ini->e_ell, 64);
	/* EncryptAndHash empty with empty k → MixHash("") */
	mix_hash(ini->h, (const uint8_t *)"", 0);
	return true;
}

static bool verify_cert(struct initiator *ini, const uint8_t sigmsg[74])
{
	uint8_t signed_fields[42];
	uint8_t mhash[32];
	secp256k1_xonly_pubkey auth;

	/* Reconstruct signed material: version|from|to|server_xonly */
	memcpy(signed_fields, sigmsg, 10);
	/* Derive server xonly from rs_ell */
	{
		secp256k1_pubkey pub;
		secp256k1_xonly_pubkey xop;

		if (!secp256k1_ellswift_decode(ini->secp, &pub, ini->rs_ell))
			return false;
		if (!secp256k1_xonly_pubkey_from_pubkey(ini->secp, &xop, NULL, &pub))
			return false;
		if (!secp256k1_xonly_pubkey_serialize(ini->secp, signed_fields + 10, &xop))
			return false;
		memcpy(ini->server_xonly, signed_fields + 10, 32);
	}
	crypto_hash_sha256(mhash, signed_fields, 42);
	if (!secp256k1_xonly_pubkey_parse(ini->secp, &auth, ini->authority_xonly))
		return false;
	return secp256k1_schnorrsig_verify(ini->secp, sigmsg + 10, mhash, 32, &auth) == 1;
}

static bool initiator_act2(struct initiator *ini, const uint8_t *msg, size_t msglen)
{
	uint8_t shared[32], pt[128];
	size_t ptlen = 0;
	uint8_t k1[32], k2[32];
	const uint8_t *p = msg;

	if (msglen < 64 + 80 + 90)
		return false;

	/* re */
	memcpy(ini->re_ell, p, 64);
	p += 64;
	mix_hash(ini->h, ini->re_ell, 64);

	/* ee as initiator */
	if (!ellswift_ecdh_init(ini->secp, ini->e_sk, ini->e_ell, ini->re_ell, true, shared))
		return false;
	mix_key(ini, shared, 32);

	/* decrypt s (80 bytes) */
	if (!decrypt_and_hash(ini, p, 80, pt, &ptlen) || ptlen != 64)
		return false;
	memcpy(ini->rs_ell, pt, 64);
	p += 80;

	/* es as initiator: ECDH(e.sk, rs) */
	if (!ellswift_ecdh_init(ini->secp, ini->e_sk, ini->e_ell, ini->rs_ell, true, shared))
		return false;
	mix_key(ini, shared, 32);

	/* SIGNATURE_NOISE_MESSAGE (90 bytes CT) */
	if (!decrypt_and_hash(ini, p, 90, pt, &ptlen) || ptlen != 74)
		return false;
	if (!verify_cert(ini, pt)) {
		fprintf(stderr, "FAIL: certificate signature verify\n");
		return false;
	}

	hkdf_sha256(ini->ck, NULL, 0, k1, k2);
	/* Initiator encrypts with c1, decrypts with c2 */
	cs_init_key(&ini->send_cs, k1);
	cs_init_key(&ini->recv_cs, k2);
	sodium_memzero(k1, 32);
	sodium_memzero(k2, 32);
	return true;
}

static int fail(const char *msg)
{
	fprintf(stderr, "FAIL: %s\n", msg);
	return 1;
}

int main(void)
{
	struct sv2_noise_server_keys keys;
	sv2_noise_session_t *resp = NULL;
	struct initiator ini;
	uint8_t act1[64];
	uint8_t *act2 = NULL;
	size_t act2len = 0;
	uint8_t plain[64], *ct = NULL, *dec = NULL;
	size_t ctlen = 0, declen = 0, consumed = 0;
	int rc;

	if (sodium_init() < 0)
		return fail("sodium_init");

	memset(&keys, 0, sizeof(keys));
	memset(&ini, 0, sizeof(ini));
	ini.secp = secp256k1_context_create(SECP256K1_CONTEXT_NONE);
	if (!ini.secp)
		return fail("secp create");

	/* Ephemeral server keys in memory (no files) */
	if (!sv2_noise_load_server_keys(&keys, NULL, NULL, 365))
		return fail("load_server_keys");
	memcpy(ini.authority_xonly, keys.authority_xonly, 32);

	resp = sv2_noise_session_new(&keys);
	if (!resp)
		return fail("session_new");

	/* --- Handshake --- */
	if (!initiator_act1(&ini, act1))
		return fail("initiator act1");
	if (!sv2_noise_handshake_read(resp, act1, 64, &act2, &act2len))
		return fail("responder handshake_read");
	if (!act2 || act2len < 170)
		return fail("act2 length");
	if (!sv2_noise_handshake_complete(resp))
		return fail("handshake not complete");
	if (!initiator_act2(&ini, act2, act2len))
		return fail("initiator act2 / cert");
	dealloc(act2);
	act2 = NULL;

	printf("sv2_noise_hs: handshake + cert OK\n");

	/* --- Encrypt a small frame initiator→responder (recv_cs on responder) --- */
	/* Build plaintext: SetupConnection.Success-like empty-ish header+payload */
	sv2_encode_header(plain, 0, SV2_MSG_SETUP_CONNECTION_SUCCESS, 6);
	/* used_version u16 + flags u32 */
	plain[6] = 2;
	plain[7] = 0;
	plain[8] = plain[9] = plain[10] = plain[11] = 0;

	{
		size_t hdr_ct = 0, pay_ct = 0;
		uint8_t tmp[128];

		/* Manual encrypt with initiator send_cs to match responder recv */
		if (!cs_encrypt(&ini.send_cs, NULL, 0, plain, 6, tmp, &hdr_ct) ||
		    hdr_ct != SV2_ENCRYPTED_HEADER_LEN)
			return fail("init encrypt header");
		if (!cs_encrypt(&ini.send_cs, NULL, 0, plain + 6, 6, tmp + hdr_ct, &pay_ct))
			return fail("init encrypt payload");
		ctlen = hdr_ct + pay_ct;
		ct = ckalloc(ctlen);
		memcpy(ct, tmp, ctlen);
	}

	/* Partial feed: only header ciphertext first — must return -1, not -2 */
	rc = sv2_noise_decrypt_frame(resp, ct, SV2_ENCRYPTED_HEADER_LEN,
				     &consumed, &dec, &declen);
	if (rc != -1)
		return fail("partial header should need more data");
	if (dec)
		return fail("partial should not produce plaintext");

	/* Still incomplete: header + 1 byte of payload */
	rc = sv2_noise_decrypt_frame(resp, ct, SV2_ENCRYPTED_HEADER_LEN + 1,
				     &consumed, &dec, &declen);
	if (rc != -1)
		return fail("partial payload should need more data");

	/* Full frame */
	rc = sv2_noise_decrypt_frame(resp, ct, ctlen, &consumed, &dec, &declen);
	if (rc != 0)
		return fail("full frame decrypt");
	if (consumed != ctlen || declen != 12 || memcmp(dec, plain, 12) != 0)
		return fail("plaintext mismatch");
	dealloc(dec);
	dealloc(ct);

	/* --- Responder encrypt → initiator decrypt --- */
	{
		uint8_t p2[12];
		uint8_t *out = NULL;
		size_t outlen = 0, got = 0;
		uint8_t pt_hdr[6], pt_pay[16];

		sv2_encode_header(p2, 0, SV2_MSG_SETUP_CONNECTION_SUCCESS, 6);
		p2[6] = 2;
		memset(p2 + 7, 0, 5);
		if (!sv2_noise_encrypt_frame(resp, p2, 12, &out, &outlen))
			return fail("responder encrypt");
		if (!cs_decrypt(&ini.recv_cs, NULL, 0, out, SV2_ENCRYPTED_HEADER_LEN,
				pt_hdr, &got) || got != 6)
			return fail("init decrypt header");
		if (!cs_decrypt(&ini.recv_cs, NULL, 0, out + SV2_ENCRYPTED_HEADER_LEN,
				outlen - SV2_ENCRYPTED_HEADER_LEN, pt_pay, &got) ||
		    got != 6)
			return fail("init decrypt payload");
		if (memcmp(pt_hdr, p2, 6) || memcmp(pt_pay, p2 + 6, 6))
			return fail("responder→initiator mismatch");
		dealloc(out);
	}

	sv2_noise_session_free(resp);

	/* --- src initiator (ckproxy SV2 upstream path) vs src responder --- */
	{
		sv2_noise_session_t *sresp, *scli, *bresp, *bcli;
		uint8_t a1[64], *a2 = NULL;
		size_t a2len = 0;
		uint8_t pt[12], *c = NULL, *d = NULL;
		size_t clen = 0, dlen = 0, cons = 0;
		uint8_t badkey[32];

		sresp = sv2_noise_session_new(&keys);
		scli = sv2_noise_client_session_new(keys.authority_xonly);
		if (!sresp || !scli)
			return fail("src client/resp new");
		if (!sv2_noise_client_act1(scli, a1))
			return fail("src client act1");
		if (!sv2_noise_handshake_read(sresp, a1, 64, &a2, &a2len))
			return fail("src resp read act1");
		if (!sv2_noise_client_act2(scli, a2, a2len))
			return fail("src client act2/cert");
		if (!sv2_noise_handshake_complete(scli))
			return fail("src client handshake incomplete");
		dealloc(a2);
		a2 = NULL;

		/* client → server transport frame */
		sv2_encode_header(pt, 0, SV2_MSG_SETUP_CONNECTION, 6);
		memset(pt + 6, 0x33, 6);
		if (!sv2_noise_encrypt_frame(scli, pt, 12, &c, &clen))
			return fail("src client encrypt");
		if (sv2_noise_decrypt_frame(sresp, c, clen, &cons, &d, &dlen) != 0 ||
		    dlen != 12 || memcmp(d, pt, 12))
			return fail("src client→server frame");
		dealloc(c);
		dealloc(d);
		c = d = NULL;
		cons = 0;

		/* server → client transport frame */
		sv2_encode_header(pt, SV2_CHANNEL_MSG_BIT, SV2_MSG_SET_TARGET, 6);
		memset(pt + 6, 0x44, 6);
		if (!sv2_noise_encrypt_frame(sresp, pt, 12, &c, &clen))
			return fail("src server encrypt");
		if (sv2_noise_decrypt_frame(scli, c, clen, &cons, &d, &dlen) != 0 ||
		    dlen != 12 || memcmp(d, pt, 12))
			return fail("src server→client frame");
		dealloc(c);
		dealloc(d);
		c = d = NULL;
		cons = 0;

		/* Nonce watermark: force counters near 2^64 and refuse AEAD
		 * (close-and-reconnect policy, no rekey). */
		{
			uint64_t hits0 = sv2_noise_nonce_watermark_hits();
			uint8_t *c2 = NULL;
			size_t c2len = 0;

			/* Small frame uses 2 nonces (header + 1 payload chunk). */
			sv2_noise_test_set_transport_nonces(scli,
				UINT64_MAX - (UINT64_C(1) << 20), 0);
			sv2_encode_header(pt, 0, SV2_MSG_SETUP_CONNECTION, 6);
			memset(pt + 6, 0x55, 6);
			if (sv2_noise_encrypt_frame(scli, pt, 12, &c2, &c2len)) {
				dealloc(c2);
				return fail("encrypt should fail at send nonce watermark");
			}
			if (sv2_noise_nonce_watermark_hits() <= hits0)
				return fail("watermark hit counter not incremented");

			/* Recv watermark on decrypt of a still-valid ciphertext:
			 * re-encrypt from a fresh-nonce peer then force recv high. */
			sv2_noise_test_set_transport_nonces(sresp, 0, 0);
			if (!sv2_noise_encrypt_frame(sresp, pt, 12, &c2, &c2len))
				return fail("re-encrypt after reset for recv watermark");
			sv2_noise_test_set_transport_nonces(scli, 0,
				UINT64_MAX - (UINT64_C(1) << 20));
			hits0 = sv2_noise_nonce_watermark_hits();
			if (sv2_noise_decrypt_frame(scli, c2, c2len, &cons, &d, &dlen) != -2)
				return fail("decrypt should fail at recv nonce watermark");
			if (sv2_noise_nonce_watermark_hits() <= hits0)
				return fail("recv watermark hit counter not incremented");
			dealloc(c2);
			dealloc(d);
			c2 = d = NULL;
			printf("sv2_noise_hs: nonce watermark refuse OK\n");
		}

		sv2_noise_session_free(sresp);
		sv2_noise_session_free(scli);

		/* Wrong authority key must be rejected at act2 */
		memset(badkey, 0, 32);
		badkey[31] = 1;
		bresp = sv2_noise_session_new(&keys);
		bcli = sv2_noise_client_session_new(badkey);
		if (!bresp || !bcli)
			return fail("badkey sessions");
		if (!sv2_noise_client_act1(bcli, a1) ||
		    !sv2_noise_handshake_read(bresp, a1, 64, &a2, &a2len))
			return fail("badkey handshake setup");
		if (sv2_noise_client_act2(bcli, a2, a2len))
			return fail("wrong authority key accepted - SECURITY BUG");
		dealloc(a2);
		sv2_noise_session_free(bresp);
		sv2_noise_session_free(bcli);
		printf("sv2_noise_hs: src initiator interop + wrong-key reject OK\n");
	}

	sv2_noise_server_keys_clear(&keys);
	secp256k1_context_destroy(ini.secp);
	printf("sv2_noise_hs: all OK\n");
	return 0;
}
