/*
 * Copyright 2026 Con Kolivas
 *
 * Stratum V2 Noise NX responder + ChaCha20-Poly1305 transport framing.
 * Primitives: libsodium (AEAD), libsecp256k1 (ellswift ECDH, BIP340 Schnorr).
 */

#include "config.h"

#ifdef HAVE_SV2

#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include <sodium.h>
#include <secp256k1.h>
#include <secp256k1_ellswift.h>
#include <secp256k1_extrakeys.h>
#include <secp256k1_schnorrsig.h>

#include "libckpool.h"
#include "sv2_types.h"
#include "sv2_codec.h"
#include "sv2_noise.h"

/* Noise protocol name for SV2 NX (as used in community implementations). */
static const char noise_protocol_name[] = "Noise_NX_Secp256k1+EllSwift_ChaChaPoly_SHA256";

/*
 * Transport nonce exhaustion: close-and-reconnect, no
 * rekey. Refuse AEAD once a direction's counter reaches this watermark so a
 * multi-chunk frame cannot wrap the 64-bit Noise nonce. 2^20 headroom covers
 * header + worst-case U24 payload chunking.
 */
#define SV2_NOISE_NONCE_WATERMARK (UINT64_MAX - (UINT64_C(1) << 20))

static uint64_t g_nonce_watermark_hits;

struct cipher_state {
    uint8_t  k[32];
    uint64_t n;
    bool     has_key;
};

/* Noise AEAD operations for one SV2 frame: 1 (header) + payload chunks. */
static uint64_t noise_frame_chunk_count(uint32_t pt_payload_len) {
    uint64_t n = 1;

    if (pt_payload_len)
        n += ((uint64_t)pt_payload_len + SV2_NOISE_MAX_PT_CHUNK - 1) / SV2_NOISE_MAX_PT_CHUNK;
    return n;
}

/* True if cipher state can consume `need` nonces without hitting the watermark. */
static bool noise_nonce_has_budget(const struct cipher_state* c, uint64_t need) {
    if (!c->has_key || need < 1)
        return false;
    if (c->n >= SV2_NOISE_NONCE_WATERMARK)
        return false;
    if (need > SV2_NOISE_NONCE_WATERMARK - c->n)
        return false;
    return true;
}

static void noise_nonce_watermark_hit(const char* dir, uint64_t n) {
    g_nonce_watermark_hits++;
    LOGNOTICE("SV2 Noise %s nonce watermark (n=%" PRIu64 ") — close for re-handshake", dir, n);
}

uint64_t sv2_noise_nonce_watermark_hits(void) {
    return g_nonce_watermark_hits;
}

/* Process-wide secp256k1 context (thread-safe for concurrent use after create).
 * Avoids secp256k1_context_create per connection (handshake DoS amplifier). */
static secp256k1_context* g_secp;
static pthread_once_t     g_secp_once = PTHREAD_ONCE_INIT;

static void secp_init_once(void) {
    uint8_t seed[32];

    g_secp = secp256k1_context_create(SECP256K1_CONTEXT_NONE);
    if (g_secp) {
        randombytes_buf(seed, 32);
        if (!secp256k1_context_randomize(g_secp, seed))
            LOGWARNING("SV2 secp256k1_context_randomize failed");
        sodium_memzero(seed, sizeof(seed));
    }
}

static secp256k1_context* sv2_secp(void) {
    if (sodium_init() < 0)
        return NULL;
    pthread_once(&g_secp_once, secp_init_once);
    return g_secp;
}

struct sv2_noise_session {
    const struct sv2_noise_server_keys* keys;
    secp256k1_context*                  secp; /* borrowed process-wide context; do not destroy */

    /* True for the client/initiator role (ckproxy SV2 upstream). Responder
     * (pool server) sessions leave this false and set keys. */
    bool initiator;
    /* Initiator: pool authority x-only pubkey (from URL path) that must
     * have signed the server certificate. Unused by the responder. */
    uint8_t authority_xonly[32];

    /* Handshake chaining */
    uint8_t             ck[32];
    uint8_t             h[32];
    struct cipher_state cs; /* during handshake encrypt/decrypt with h as AD */

    uint8_t e_sk[32];
    uint8_t e_ellswift[64];
    uint8_t re_ellswift[64]; /* remote ephemeral */
    uint8_t rs_ellswift[64]; /* remote static (server static; initiator learns it in act2) */

    struct cipher_state send_cs;
    struct cipher_state recv_cs;

    bool handshake_done;
    /* Handshake: waiting for initiator act1 (64 bytes e) */
    bool got_e;

    /* Buffered incomplete handshake input */
    uint8_t hs_buf[256];
    size_t  hs_buf_len;

    /*
     * Partial transport frame: header is decrypted exactly once.
     * On incomplete payload we stash the plaintext header and expected CT
     * length; re-entry must not call cs_decrypt on the header again
     * (would advance recv_cs.n and desync the AEAD nonce).
     */
    bool     hdr_pending;
    uint8_t  hdr_pt[SV2_FRAME_HEADER_LEN];
    uint32_t pending_pt_len; /* plaintext payload length from header */
    size_t   pending_need;   /* full ciphertext frame length */
};

void sv2_noise_test_set_transport_nonces(sv2_noise_session_t* s, uint64_t send_n, uint64_t recv_n) {
    if (!s || !s->handshake_done)
        return;
    s->send_cs.n = send_n;
    s->recv_cs.n = recv_n;
}

static void cs_init_empty(struct cipher_state* c) {
    memset(c, 0, sizeof(*c));
    c->has_key = false;
}

static void cs_init_key(struct cipher_state* c, const uint8_t key[32]) {
    memcpy(c->k, key, 32);
    c->n = 0;
    c->has_key = true;
}

/* Noise nonce: 4 zero bytes || LE u64 counter (spec 04). */
static void noise_nonce(uint8_t nonce[12], uint64_t n) {
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

static bool cs_encrypt(
    struct cipher_state* c,
    const uint8_t*       ad,
    size_t               adlen,
    const uint8_t*       pt,
    size_t               ptlen,
    uint8_t*             ct_out,
    size_t*              ctlen) {
    uint8_t nonce[12];

    /* Noise EncryptWithAd passes plaintext through when k is empty, but
     * every ckpool call site operates on a keyed state (act2 after ee;
     * transport after split). Fail closed rather than copy unbounded
     * data through fixed-size buffers on a state machine bug. */
    if (!c->has_key)
        return false;
    noise_nonce(nonce, c->n);
    if (crypto_aead_chacha20poly1305_ietf_encrypt(ct_out, NULL, pt, ptlen, ad, adlen, NULL, nonce, c->k) != 0)
        return false;
    *ctlen = ptlen + SV2_NOISE_MAC_LEN;
    c->n++;
    return true;
}

static bool cs_decrypt(
    struct cipher_state* c,
    const uint8_t*       ad,
    size_t               adlen,
    const uint8_t*       ct,
    size_t               ctlen,
    uint8_t*             pt_out,
    size_t*              ptlen) {
    uint8_t nonce[12];

    /* See cs_encrypt: keyless DecryptWithAd passthrough is unused and
     * would overflow fixed-size plaintext buffers; fail closed. */
    if (!c->has_key)
        return false;
    if (ctlen < SV2_NOISE_MAC_LEN)
        return false;
    noise_nonce(nonce, c->n);
    if (crypto_aead_chacha20poly1305_ietf_decrypt(pt_out, NULL, NULL, ct, ctlen, ad, adlen, nonce, c->k) != 0)
        return false;
    *ptlen = ctlen - SV2_NOISE_MAC_LEN;
    c->n++;
    return true;
}

/* HMAC-SHA256 with variable-length key (Noise HKDF). */
static void hmac_sha256(const uint8_t* key, size_t keylen, const uint8_t* data, size_t datalen, uint8_t out[32]) {
    crypto_auth_hmacsha256_state st;

    crypto_auth_hmacsha256_init(&st, key, keylen);
    crypto_auth_hmacsha256_update(&st, data, datalen);
    crypto_auth_hmacsha256_final(&st, out);
}

static void
hkdf_sha256(const uint8_t chaining_key[32], const uint8_t* ikm, size_t ikm_len, uint8_t out1[32], uint8_t out2[32]) {
    uint8_t temp_key[32];
    uint8_t buf[33];

    /* Extract: temp_key = HMAC(chaining_key, ikm) */
    hmac_sha256(chaining_key, 32, ikm ? ikm : (const uint8_t*)"", ikm_len, temp_key);

    /* Expand: out1 = HMAC(temp_key, 0x01); out2 = HMAC(temp_key, out1 || 0x02) */
    buf[0] = 0x01;
    hmac_sha256(temp_key, 32, buf, 1, out1);
    memcpy(buf, out1, 32);
    buf[32] = 0x02;
    hmac_sha256(temp_key, 32, buf, 33, out2);

    sodium_memzero(temp_key, sizeof(temp_key));
}

static void mix_hash(uint8_t h[32], const uint8_t* data, size_t len) {
    crypto_hash_sha256_state st;

    crypto_hash_sha256_init(&st);
    crypto_hash_sha256_update(&st, h, 32);
    crypto_hash_sha256_update(&st, data, len);
    crypto_hash_sha256_final(&st, h);
}

static void mix_key(sv2_noise_session_t* s, const uint8_t* ikm, size_t ikm_len) {
    uint8_t ck_new[32], temp_k[32];

    hkdf_sha256(s->ck, ikm, ikm_len, ck_new, temp_k);
    memcpy(s->ck, ck_new, 32);
    cs_init_key(&s->cs, temp_k);
    sodium_memzero(temp_k, sizeof(temp_k));
}

static bool encrypt_and_hash(sv2_noise_session_t* s, const uint8_t* pt, size_t ptlen, uint8_t* ct, size_t* ctlen) {
    if (!cs_encrypt(&s->cs, s->h, 32, pt, ptlen, ct, ctlen))
        return false;
    mix_hash(s->h, ct, *ctlen);
    return true;
}

static bool decrypt_and_hash(sv2_noise_session_t* s, const uint8_t* ct, size_t ctlen, uint8_t* pt, size_t* ptlen) {
    if (!cs_decrypt(&s->cs, s->h, 32, ct, ctlen, pt, ptlen))
        return false;
    mix_hash(s->h, ct, ctlen);
    return true;
}

/*
 * BIP324 / SV2 ECDH (spec 04 §4.4):
 *   initiator: tagged_hash(own_ell, remote_ell, x)
 *   responder: tagged_hash(remote_ell, own_ell, x)
 * secp256k1_ellswift_xdh party=0 means seckey matches ell_a (first argument).
 */
static bool ellswift_ecdh(
    secp256k1_context* secp,
    const uint8_t*     our_sk,
    const uint8_t*     our_ellswift,
    const uint8_t*     their_ellswift,
    bool               we_are_initiator,
    uint8_t            shared[32]) {
    if (we_are_initiator)
        return secp256k1_ellswift_xdh(
                   secp, shared, our_ellswift, their_ellswift, our_sk, 0, secp256k1_ellswift_xdh_hash_function_bip324,
                   NULL) == 1;
    return secp256k1_ellswift_xdh(
               secp, shared, their_ellswift, our_ellswift, our_sk, 1, secp256k1_ellswift_xdh_hash_function_bip324,
               NULL) == 1;
}

/* Derive public material from an existing seckey. */
static bool
seckey_to_pubs(secp256k1_context* secp, const uint8_t sk[32], uint8_t* ellswift /* nullable */, uint8_t xonly[32]) {
    secp256k1_pubkey       pub;
    secp256k1_xonly_pubkey xopub;
    unsigned char          rnd32[32];

    if (!secp256k1_ec_seckey_verify(secp, sk))
        return false;
    if (!secp256k1_ec_pubkey_create(secp, &pub, sk))
        return false;
    if (ellswift) {
        randombytes_buf(rnd32, 32);
        if (!secp256k1_ellswift_encode(secp, ellswift, &pub, rnd32))
            return false;
    }
    if (xonly) {
        if (!secp256k1_xonly_pubkey_from_pubkey(secp, &xopub, NULL, &pub))
            return false;
        if (!secp256k1_xonly_pubkey_serialize(secp, xonly, &xopub))
            return false;
    }
    return true;
}

static bool generate_ellswift_keypair(
    secp256k1_context* secp,
    uint8_t            sk[32],
    uint8_t*           ellswift /* nullable */,
    uint8_t            xonly[32]) {
    do {
        randombytes_buf(sk, 32);
    } while (!secp256k1_ec_seckey_verify(secp, sk));
    return seckey_to_pubs(secp, sk, ellswift, xonly);
}

/*
 * Key file format (text):
 *   - Optional comment lines starting with '#'
 *   - One line of 64 lowercase/uppercase hex characters = 32-byte seckey
 * Permissions should be 0600. Files are written mode 0600 when created.
 */
static bool read_seckey_file(const char* path, uint8_t sk[32]) {
    FILE*  f;
    char   line[256];
    char   hex[65];
    size_t n = 0;

    f = fopen(path, "re");
    if (!f)
        return false;
    hex[0] = '\0';
    while (fgets(line, sizeof(line), f)) {
        char* p = line;

        while (*p == ' ' || *p == '\t')
            p++;
        if (*p == '#' || *p == '\n' || *p == '\r' || !*p)
            continue;
        n = 0;
        while (isxdigit((unsigned char)p[n]) && n < 64)
            n++;
        if (n != 64)
            continue;
        memcpy(hex, p, 64);
        hex[64] = '\0';
        break;
    }
    fclose(f);
    if (n != 64)
        return false;
    if (!hex2bin(sk, hex, 32))
        return false;
    return true;
}

static bool write_seckey_file(const char* path, const uint8_t sk[32], const char* label) {
    char  hex[65];
    FILE* f;
    int   fd;

    /* Create exclusively when possible so we never clobber an existing key. */
    fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) {
        if (errno != EEXIST)
            LOGERR("SV2 failed to create key file %s: %s", path, strerror(errno));
        return false;
    }
    f = fdopen(fd, "w");
    if (!f) {
        close(fd);
        return false;
    }
    __bin2hex(hex, sk, 32);
    fprintf(f, "# ckpool SV2 Noise %s seckey — keep secret, mode 0600\n", label ? label : "key");
    fprintf(f, "%s\n", hex);
    if (fclose(f)) {
        LOGERR("SV2 failed writing key file %s", path);
        return false;
    }
    LOGWARNING("SV2 wrote new Noise %s key to %s", label ? label : "", path);
    return true;
}

/*
 * SIGNATURE_NOISE_MESSAGE (spec 04 §4.5.2): 74 bytes plaintext
 *   version U16 | valid_from U32 | not_valid_after U32 | signature SIGNATURE(64)
 *
 * CERTIFICATE (constructed by initiator, not sent as one blob):
 *   signed fields: version | valid_from | not_valid_after | server_public_key (x-only 32)
 *   m = SHA256(signed fields); signature = BIP340(authority_sk, m)
 *   authority_public_key is known a priori (URL path), not in the Noise message.
 *   server_public_key arrives separately as encrypted static (rs) in act2.
 */
#define SV2_SIG_NOISE_MSG_LEN 74

static bool
build_signature_noise_message(const struct sv2_noise_server_keys* keys, secp256k1_context* secp, uint8_t out[74]) {
    uint8_t           signed_fields[42];
    secp256k1_keypair keypair;
    uint8_t           mhash[32];

    /* LE fields matching wire layout of version/valid_from/not_valid_after */
    signed_fields[0] = (uint8_t)(keys->cert_version & 0xff);
    signed_fields[1] = (uint8_t)((keys->cert_version >> 8) & 0xff);
    signed_fields[2] = (uint8_t)(keys->cert_valid_from & 0xff);
    signed_fields[3] = (uint8_t)((keys->cert_valid_from >> 8) & 0xff);
    signed_fields[4] = (uint8_t)((keys->cert_valid_from >> 16) & 0xff);
    signed_fields[5] = (uint8_t)((keys->cert_valid_from >> 24) & 0xff);
    signed_fields[6] = (uint8_t)(keys->cert_not_valid_after & 0xff);
    signed_fields[7] = (uint8_t)((keys->cert_not_valid_after >> 8) & 0xff);
    signed_fields[8] = (uint8_t)((keys->cert_not_valid_after >> 16) & 0xff);
    signed_fields[9] = (uint8_t)((keys->cert_not_valid_after >> 24) & 0xff);
    memcpy(signed_fields + 10, keys->static_xonly, 32);

    /* Wire: version|valid_from|not_valid_after (10) || sig (64) */
    memcpy(out, signed_fields, 10);
    if (!secp256k1_keypair_create(secp, &keypair, keys->authority_sk))
        return false;
    crypto_hash_sha256(mhash, signed_fields, 42);
    if (!secp256k1_schnorrsig_sign32(secp, out + 10, mhash, &keypair, NULL))
        return false;
    return true;
}

static void handshake_init_hash(sv2_noise_session_t* s) {
    size_t  nlen = strlen(noise_protocol_name);
    uint8_t h2[32];

    /* Spec 4.5.1: h = HASH(protocolName); name is >32 so always SHA256 */
    if (nlen <= 32) {
        memset(s->h, 0, 32);
        memcpy(s->h, noise_protocol_name, nlen);
    } else
        crypto_hash_sha256(s->h, (const uint8_t*)noise_protocol_name, nlen);
    memcpy(s->ck, s->h, 32);
    /* Spec 4.5.1 step 3: h = HASH(h) */
    crypto_hash_sha256(h2, s->h, 32);
    memcpy(s->h, h2, 32);
    cs_init_empty(&s->cs);
}

bool sv2_noise_load_server_keys(
    struct sv2_noise_server_keys* keys,
    const char*                   authority_path,
    const char*                   static_path,
    uint32_t                      valid_days) {
    secp256k1_context* secp;
    time_t             now = time(NULL);
    bool               ok = false;
    bool               auth_new = false, static_new = false;

    memset(keys, 0, sizeof(*keys));
    if (sodium_init() < 0) {
        LOGERR("libsodium init failed");
        return false;
    }
    secp = sv2_secp();
    if (!secp)
        return false;

    keys->cert_version = 1;
    keys->cert_valid_from = (uint32_t)now;
    if (valid_days == 0)
        valid_days = 365;
    keys->cert_not_valid_after = (uint32_t)now + valid_days * 86400u;

    /* Authority seckey */
    if (authority_path && authority_path[0] && read_seckey_file(authority_path, keys->authority_sk)) {
        if (!seckey_to_pubs(secp, keys->authority_sk, NULL, keys->authority_xonly)) {
            LOGERR("SV2 authority key file %s is not a valid secp256k1 seckey", authority_path);
            goto out;
        }
        LOGNOTICE("SV2 loaded authority key from %s", authority_path);
    } else {
        if (!generate_ellswift_keypair(secp, keys->authority_sk, NULL, keys->authority_xonly))
            goto out;
        auth_new = true;
        if (authority_path && authority_path[0]) {
            if (!write_seckey_file(authority_path, keys->authority_sk, "authority"))
                LOGWARNING("SV2 could not persist authority key to %s — ephemeral this run", authority_path);
        } else
            LOGWARNING("SV2 using ephemeral authority key (set sv2_authority_key to persist)");
    }

    /* Server static seckey (+ ElligatorSwift pub for handshake) */
    if (static_path && static_path[0] && read_seckey_file(static_path, keys->static_sk)) {
        if (!seckey_to_pubs(secp, keys->static_sk, keys->static_ellswift, keys->static_xonly)) {
            LOGERR("SV2 static key file %s is not a valid secp256k1 seckey", static_path);
            goto out;
        }
        LOGNOTICE("SV2 loaded static server key from %s", static_path);
    } else {
        if (!generate_ellswift_keypair(secp, keys->static_sk, keys->static_ellswift, keys->static_xonly))
            goto out;
        static_new = true;
        if (static_path && static_path[0]) {
            if (!write_seckey_file(static_path, keys->static_sk, "static"))
                LOGWARNING("SV2 could not persist static key to %s — ephemeral this run", static_path);
        } else
            LOGWARNING("SV2 using ephemeral static server key (set sv2_static_key to persist)");
    }

    {
        char* b58 = sv2_noise_authority_pubkey_b58(keys);
        char* hex = ckalloc(65);

        __bin2hex(hex, keys->authority_xonly, 32);
        /* Spec 04 §4.7 URL form: stratum2+tcp://host:port/<base58check> */
        if (b58) {
            LOGWARNING(
                "SV2 authority URL path (stratum2+tcp://<host>:<port>/%s)%s", b58,
                (auth_new || static_new) ? " [new/ephemeral material]" : "");
        } else {
            LOGWARNING(
                "SV2 authority base58check encode failed%s",
                (auth_new || static_new) ? " [new/ephemeral material]" : "");
        }
        LOGNOTICE("SV2 authority pubkey x-only hex: %s", hex);
        dealloc(b58);
        dealloc(hex);
    }
    ok = true;
out:
    if (!ok)
        sodium_memzero(keys, sizeof(*keys));
    return ok;
}

void sv2_noise_server_keys_clear(struct sv2_noise_server_keys* keys) {
    if (keys)
        sodium_memzero(keys, sizeof(*keys));
}

sv2_noise_session_t* sv2_noise_session_new(const struct sv2_noise_server_keys* keys) {
    sv2_noise_session_t* s;

    if (sodium_init() < 0 || !keys)
        return NULL;
    s = ckzalloc(sizeof(*s));
    s->keys = keys;
    s->secp = sv2_secp();
    if (!s->secp) {
        dealloc(s);
        return NULL;
    }
    handshake_init_hash(s);
    if (!generate_ellswift_keypair(s->secp, s->e_sk, s->e_ellswift, NULL)) {
        sv2_noise_session_free(s);
        return NULL;
    }
    return s;
}

void sv2_noise_session_free(sv2_noise_session_t* s) {
    if (!s)
        return;
    /* secp is process-wide — do not destroy */
    sodium_memzero(s, sizeof(*s));
    dealloc(s);
}

bool sv2_noise_handshake_complete(const sv2_noise_session_t* s) {
    return s && s->handshake_done;
}

static bool handshake_act2(sv2_noise_session_t* s, uint8_t** out, size_t* outlen) {
    uint8_t  buf[256];
    uint8_t* p = buf;
    uint8_t  shared[32];
    uint8_t  enc[128];
    size_t   enclen;
    uint8_t  sigmsg[SV2_SIG_NOISE_MSG_LEN];
    uint8_t  k1[32], k2[32];

    /* e (plaintext 64) */
    memcpy(p, s->e_ellswift, 64);
    p += 64;
    mix_hash(s->h, s->e_ellswift, 64);

    /* ee: responder is not the handshake initiator (spec 04 §4.4 initiator=false) */
    if (!ellswift_ecdh(s->secp, s->e_sk, s->e_ellswift, s->re_ellswift, false, shared))
        return false;
    mix_key(s, shared, 32);

    /* s encrypted */
    if (!encrypt_and_hash(s, s->keys->static_ellswift, 64, enc, &enclen))
        return false;
    memcpy(p, enc, enclen);
    p += enclen;

    /* es: our static sk, their ephemeral; still responder (initiator=false) */
    if (!ellswift_ecdh(s->secp, s->keys->static_sk, s->keys->static_ellswift, s->re_ellswift, false, shared))
        return false;
    mix_key(s, shared, 32);

    /* SIGNATURE_NOISE_MESSAGE */
    if (!build_signature_noise_message(s->keys, s->secp, sigmsg))
        return false;
    if (!encrypt_and_hash(s, sigmsg, SV2_SIG_NOISE_MSG_LEN, enc, &enclen))
        return false;
    memcpy(p, enc, enclen);
    p += enclen;

    /* Split: temp_k1, temp_k2 = HKDF(ck, zerolen) — initiator→responder, responder→initiator */
    hkdf_sha256(s->ck, NULL, 0, k1, k2);
    /* Responder decrypts initiator messages with c1, encrypts with c2 */
    cs_init_key(&s->recv_cs, k1);
    cs_init_key(&s->send_cs, k2);
    sodium_memzero(k1, 32);
    sodium_memzero(k2, 32);

    s->handshake_done = true;
    *outlen = (size_t)(p - buf);
    *out = ckalloc(*outlen);
    memcpy(*out, buf, *outlen);
    LOGDEBUG("SV2 Noise handshake act2 complete (%zu bytes)", *outlen);
    return true;
}

bool sv2_noise_handshake_read(sv2_noise_session_t* s, const uint8_t* in, size_t inlen, uint8_t** out, size_t* outlen) {
    *out = NULL;
    *outlen = 0;

    if (!s || s->handshake_done)
        return false;

    /* Buffer until we have initiator e (64 bytes) */
    if (s->hs_buf_len + inlen > sizeof(s->hs_buf))
        return false;
    memcpy(s->hs_buf + s->hs_buf_len, in, inlen);
    s->hs_buf_len += inlen;

    if (!s->got_e) {
        if (s->hs_buf_len < 64)
            return true; /* need more */
        memcpy(s->re_ellswift, s->hs_buf, 64);
        mix_hash(s->h, s->re_ellswift, 64);
        /* Spec 4.5.1.2: DecryptAndHash(empty) with empty k → MixHash("") */
        mix_hash(s->h, (const uint8_t*)"", 0);
        s->got_e = true;
        /* Consume act1; leftover discarded (NX act1 is exactly 64) */
        s->hs_buf_len = 0;
        return handshake_act2(s, out, outlen);
    }
    return false;
}

/* --- Initiator (client) side: ckproxy SV2 upstream (spec 04 §4.5.2.2/§4.5.3) --- */

/*
 * Verify the server certificate carried in SIGNATURE_NOISE_MESSAGE (74 bytes):
 *   version U16 | valid_from U32 | not_valid_after U32 | signature(64)
 * The signed message is m = SHA256(version || valid_from || not_valid_after ||
 * server_static_xonly), where the server static key is the rs learned in act2.
 * The signature must verify under the configured pool authority x-only pubkey,
 * and the current time must fall within the certificate validity window.
 */
static bool client_verify_certificate(sv2_noise_session_t* s, const uint8_t sigmsg[74]) {
    uint8_t                signed_fields[42];
    uint8_t                mhash[32];
    secp256k1_pubkey       pub;
    secp256k1_xonly_pubkey xopub, auth;
    uint32_t               valid_from, not_valid_after;
    time_t                 now = time(NULL);

    /* version|valid_from|not_valid_after: first 10 wire bytes (LE) */
    memcpy(signed_fields, sigmsg, 10);
    /* server_static_xonly derived from rs_ellswift (learned this act2) */
    if (!secp256k1_ellswift_decode(s->secp, &pub, s->rs_ellswift))
        return false;
    if (!secp256k1_xonly_pubkey_from_pubkey(s->secp, &xopub, NULL, &pub))
        return false;
    if (!secp256k1_xonly_pubkey_serialize(s->secp, signed_fields + 10, &xopub))
        return false;

    crypto_hash_sha256(mhash, signed_fields, 42);
    if (!secp256k1_xonly_pubkey_parse(s->secp, &auth, s->authority_xonly))
        return false;
    if (secp256k1_schnorrsig_verify(s->secp, sigmsg + 10, mhash, 32, &auth) != 1) {
        LOGNOTICE("SV2 client: server certificate signature invalid against authority key");
        return false;
    }

    valid_from =
        (uint32_t)sigmsg[2] | ((uint32_t)sigmsg[3] << 8) | ((uint32_t)sigmsg[4] << 16) | ((uint32_t)sigmsg[5] << 24);
    not_valid_after =
        (uint32_t)sigmsg[6] | ((uint32_t)sigmsg[7] << 8) | ((uint32_t)sigmsg[8] << 16) | ((uint32_t)sigmsg[9] << 24);
    if ((uint32_t)now < valid_from || (uint32_t)now > not_valid_after) {
        LOGNOTICE(
            "SV2 client: server certificate outside validity window "
            "(now %u, valid %u..%u)",
            (uint32_t)now, valid_from, not_valid_after);
        return false;
    }
    return true;
}

sv2_noise_session_t* sv2_noise_client_session_new(const uint8_t authority_xonly[32]) {
    sv2_noise_session_t* s;

    if (sodium_init() < 0 || !authority_xonly)
        return NULL;
    s = ckzalloc(sizeof(*s));
    s->initiator = true;
    s->keys = NULL;
    memcpy(s->authority_xonly, authority_xonly, 32);
    s->secp = sv2_secp();
    if (!s->secp) {
        dealloc(s);
        return NULL;
    }
    handshake_init_hash(s);
    if (!generate_ellswift_keypair(s->secp, s->e_sk, s->e_ellswift, NULL)) {
        sv2_noise_session_free(s);
        return NULL;
    }
    return s;
}

bool sv2_noise_client_act1(sv2_noise_session_t* s, uint8_t out[64]) {
    if (!s || !s->initiator || s->handshake_done || s->got_e || !out)
        return false;
    /* Spec 4.5.1.1: buffer e.public_key (64B EllSwift plaintext), MixHash(e),
     * EncryptAndHash(empty) with empty k reduces to MixHash(""). got_e marks
     * act1 sent for the initiator (symmetric with the responder's use). */
    memcpy(out, s->e_ellswift, 64);
    mix_hash(s->h, s->e_ellswift, 64);
    mix_hash(s->h, (const uint8_t*)"", 0);
    s->got_e = true;
    return true;
}

bool sv2_noise_client_act2(sv2_noise_session_t* s, const uint8_t* in, size_t inlen) {
    uint8_t        shared[32], pt[128];
    size_t         ptlen = 0;
    uint8_t        k1[32], k2[32];
    const uint8_t* p = in;

    if (!s || !s->initiator || !s->got_e || s->handshake_done || !in)
        return false;
    /* re(64) + enc static(64+MAC) + enc sig(74+MAC) = 234 */
    if (inlen < 64 + 64 + SV2_NOISE_MAC_LEN + SV2_SIG_NOISE_MSG_LEN + SV2_NOISE_MAC_LEN)
        return false;

    /* re: responder ephemeral, plaintext 64 */
    memcpy(s->re_ellswift, p, 64);
    p += 64;
    mix_hash(s->h, s->re_ellswift, 64);

    /* ee: our ephemeral sk, their ephemeral; initiator=true */
    if (!ellswift_ecdh(s->secp, s->e_sk, s->e_ellswift, s->re_ellswift, true, shared))
        return false;
    mix_key(s, shared, 32);

    /* decrypt server static (64 + MAC) into rs_ellswift */
    if (!decrypt_and_hash(s, p, 64 + SV2_NOISE_MAC_LEN, pt, &ptlen) || ptlen != 64)
        return false;
    memcpy(s->rs_ellswift, pt, 64);
    p += 64 + SV2_NOISE_MAC_LEN;

    /* es: our ephemeral sk, their static; initiator=true */
    if (!ellswift_ecdh(s->secp, s->e_sk, s->e_ellswift, s->rs_ellswift, true, shared))
        return false;
    mix_key(s, shared, 32);

    /* decrypt SIGNATURE_NOISE_MESSAGE (74 + MAC) and verify the certificate */
    if (!decrypt_and_hash(s, p, SV2_SIG_NOISE_MSG_LEN + SV2_NOISE_MAC_LEN, pt, &ptlen) ||
        ptlen != SV2_SIG_NOISE_MSG_LEN)
        return false;
    if (!client_verify_certificate(s, pt)) {
        sodium_memzero(shared, sizeof(shared));
        return false;
    }

    /* Split — initiator encrypts with c1, decrypts with c2 */
    hkdf_sha256(s->ck, NULL, 0, k1, k2);
    cs_init_key(&s->send_cs, k1);
    cs_init_key(&s->recv_cs, k2);
    sodium_memzero(k1, 32);
    sodium_memzero(k2, 32);
    sodium_memzero(shared, sizeof(shared));

    s->handshake_done = true;
    LOGDEBUG("SV2 Noise client handshake complete, certificate verified");
    return true;
}

int sv2_noise_decrypt_frame(
    sv2_noise_session_t* s,
    const uint8_t*       in,
    size_t               inlen,
    size_t*              consumed,
    uint8_t**            plain,
    size_t*              plainlen) {
    uint8_t          hdr_pt[SV2_FRAME_HEADER_LEN];
    size_t           hdr_pt_len = 0;
    struct sv2_frame fr;
    uint32_t         pt_len;
    size_t           need;
    uint8_t*         payload_pt;
    uint8_t*         frame;
    uint64_t         pay_chunks;

    *consumed = 0;
    *plain = NULL;
    *plainlen = 0;

    if (!s || !s->handshake_done)
        return -2;

    if (s->hdr_pending) {
        /* Header already decrypted; wait for full ciphertext frame */
        need = s->pending_need;
        if (inlen < need)
            return -1;
        memcpy(hdr_pt, s->hdr_pt, SV2_FRAME_HEADER_LEN);
        pt_len = s->pending_pt_len;
        s->hdr_pending = false;
        s->pending_need = 0;
        s->pending_pt_len = 0;
        /* Payload chunks only — header nonce already consumed. */
        pay_chunks = noise_frame_chunk_count(pt_len) - 1;
        if (pay_chunks && !noise_nonce_has_budget(&s->recv_cs, pay_chunks)) {
            noise_nonce_watermark_hit("recv", s->recv_cs.n);
            return -2;
        }
    } else {
        if (inlen < SV2_ENCRYPTED_HEADER_LEN)
            return -1;
        /* Need room for header AEAD before decrypting (may stash). */
        if (!noise_nonce_has_budget(&s->recv_cs, 1)) {
            noise_nonce_watermark_hit("recv", s->recv_cs.n);
            return -2;
        }

        if (!cs_decrypt(&s->recv_cs, NULL, 0, in, SV2_ENCRYPTED_HEADER_LEN, hdr_pt, &hdr_pt_len) ||
            hdr_pt_len != SV2_FRAME_HEADER_LEN)
            return -2;
        if (!sv2_decode_header(hdr_pt, SV2_FRAME_HEADER_LEN, &fr))
            return -2;

        pt_len = fr.msg_length;
        need = SV2_ENCRYPTED_HEADER_LEN + sv2_pt_len_to_ct_len(pt_len);
        pay_chunks = noise_frame_chunk_count(pt_len) - 1;
        if (pay_chunks && !noise_nonce_has_budget(&s->recv_cs, pay_chunks)) {
            /* Header nonce already advanced; drop session. */
            noise_nonce_watermark_hit("recv", s->recv_cs.n);
            return -2;
        }
        if (inlen < need) {
            /* Stash plaintext header; do NOT re-decrypt on retry */
            memcpy(s->hdr_pt, hdr_pt, SV2_FRAME_HEADER_LEN);
            s->pending_pt_len = pt_len;
            s->pending_need = need;
            s->hdr_pending = true;
            return -1;
        }
    }

    payload_pt = ckalloc(pt_len ? pt_len : 1);
    if (pt_len) {
        /* Decrypt payload in Noise chunks */
        const uint8_t* cp = in + SV2_ENCRYPTED_HEADER_LEN;
        size_t         remaining_pt = pt_len;
        size_t         out_ofs = 0;

        while (remaining_pt > 0) {
            size_t chunk_pt = remaining_pt > SV2_NOISE_MAX_PT_CHUNK ? SV2_NOISE_MAX_PT_CHUNK : remaining_pt;
            size_t chunk_ct = chunk_pt + SV2_NOISE_MAC_LEN;
            size_t got = 0;

            if (!cs_decrypt(&s->recv_cs, NULL, 0, cp, chunk_ct, payload_pt + out_ofs, &got) || got != chunk_pt) {
                dealloc(payload_pt);
                return -2;
            }
            cp += chunk_ct;
            out_ofs += got;
            remaining_pt -= chunk_pt;
        }
    }

    frame = ckalloc(SV2_FRAME_HEADER_LEN + pt_len);
    memcpy(frame, hdr_pt, SV2_FRAME_HEADER_LEN);
    if (pt_len)
        memcpy(frame + SV2_FRAME_HEADER_LEN, payload_pt, pt_len);
    dealloc(payload_pt);

    *plain = frame;
    *plainlen = SV2_FRAME_HEADER_LEN + pt_len;
    *consumed = need;
    return 0;
}

bool sv2_noise_encrypt_frame(
    sv2_noise_session_t* s,
    const uint8_t*       plain,
    size_t               plainlen,
    uint8_t**            out,
    size_t*              outlen) {
    struct sv2_frame fr;
    uint32_t         ct_payload_len;
    uint8_t*         buf;
    size_t           hdr_ct_len = 0;
    size_t           ofs;
    uint64_t         chunks;

    if (!s || !s->handshake_done || plainlen < SV2_FRAME_HEADER_LEN)
        return false;
    if (!sv2_decode_header(plain, plainlen, &fr))
        return false;
    if (plainlen != SV2_FRAME_HEADER_LEN + fr.msg_length)
        return false;

    /* Fail closed before any AEAD so a multi-chunk frame never wraps. */
    chunks = noise_frame_chunk_count(fr.msg_length);
    if (!noise_nonce_has_budget(&s->send_cs, chunks)) {
        noise_nonce_watermark_hit("send", s->send_cs.n);
        return false;
    }

    ct_payload_len = sv2_pt_len_to_ct_len(fr.msg_length);
    *outlen = SV2_ENCRYPTED_HEADER_LEN + ct_payload_len;
    buf = ckalloc(*outlen);

    if (!cs_encrypt(&s->send_cs, NULL, 0, plain, SV2_FRAME_HEADER_LEN, buf, &hdr_ct_len) ||
        hdr_ct_len != SV2_ENCRYPTED_HEADER_LEN) {
        dealloc(buf);
        return false;
    }

    ofs = SV2_ENCRYPTED_HEADER_LEN;
    if (fr.msg_length) {
        const uint8_t* pp = plain + SV2_FRAME_HEADER_LEN;
        size_t         remaining = fr.msg_length;

        while (remaining > 0) {
            size_t chunk_pt = remaining > SV2_NOISE_MAX_PT_CHUNK ? SV2_NOISE_MAX_PT_CHUNK : remaining;
            size_t chunk_ct = 0;

            if (!cs_encrypt(&s->send_cs, NULL, 0, pp, chunk_pt, buf + ofs, &chunk_ct)) {
                dealloc(buf);
                return false;
            }
            ofs += chunk_ct;
            pp += chunk_pt;
            remaining -= chunk_pt;
        }
    }
    *out = buf;
    return true;
}

/*
 * Base58Check for SV2 authority pubkey (spec 04 §4.7):
 *   payload = version_le_u16(0x0001 as bytes [1,0]) || xonly_pubkey(32)
 *   checksum = first 4 bytes of SHA256(SHA256(payload))
 *   encode payload||checksum in Base58 (Bitcoin alphabet)
 */
static const char b58_alphabet[] = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/* Big-endian binary → Base58 (Bitcoin alphabet). digits[] is little-endian. */
static char* bin_to_base58(const uint8_t* data, size_t datalen) {
    size_t   zeros = 0, i, j, digitslen = 1, outlen;
    uint8_t* digits;
    char *   out, *p;

    while (zeros < datalen && data[zeros] == 0)
        zeros++;

    digits = ckzalloc(datalen * 138 / 100 + 2);

    for (i = zeros; i < datalen; i++) {
        unsigned carry = data[i];

        for (j = 0; j < digitslen; j++) {
            carry += (unsigned)digits[j] << 8;
            digits[j] = carry % 58;
            carry /= 58;
        }
        while (carry) {
            digits[digitslen++] = carry % 58;
            carry /= 58;
        }
    }

    /* Skip trailing zero digits (high-order in little-endian array) */
    while (digitslen > 0 && digits[digitslen - 1] == 0)
        digitslen--;

    outlen = zeros + digitslen;
    out = ckalloc(outlen + 1);
    p = out;
    for (i = 0; i < zeros; i++)
        *p++ = '1';
    for (j = digitslen; j > 0; j--)
        *p++ = b58_alphabet[digits[j - 1]];
    *p = '\0';
    dealloc(digits);
    return out;
}

char* sv2_noise_authority_pubkey_b58(const struct sv2_noise_server_keys* keys) {
    uint8_t payload[2 + 32 + 4];
    uint8_t hash1[32], hash2[32];

    if (!keys)
        return NULL;
    /* LE u16 version = 1 → bytes [0x01, 0x00] */
    payload[0] = 0x01;
    payload[1] = 0x00;
    memcpy(payload + 2, keys->authority_xonly, 32);
    crypto_hash_sha256(hash1, payload, 34);
    crypto_hash_sha256(hash2, hash1, 32);
    memcpy(payload + 34, hash2, 4);
    return bin_to_base58(payload, 38);
}

/* Exposed for tests: encode raw 32-byte x-only authority pub to base58check. */
char* sv2_noise_authority_xonly_to_b58(const uint8_t xonly[32]) {
    struct sv2_noise_server_keys k;

    memset(&k, 0, sizeof(k));
    memcpy(k.authority_xonly, xonly, 32);
    return sv2_noise_authority_pubkey_b58(&k);
}

/* Base58 (Bitcoin alphabet) → big-endian bytes. Returns false on an invalid
 * character or if the result exceeds *outlen; on success *outlen is set to the
 * decoded length. Inverse of bin_to_base58. */
static bool base58_decode(const char* s, uint8_t* out, size_t* outlen) {
    size_t   slen, i, j, zeros = 0, byteslen = 1, total, n = 0;
    uint8_t* bytes;

    if (!s)
        return false;
    slen = strlen(s);
    if (!slen)
        return false;
    while (zeros < slen && s[zeros] == '1')
        zeros++;
    bytes = ckzalloc(slen);
    for (i = zeros; i < slen; i++) {
        const char* pos = strchr(b58_alphabet, s[i]);
        unsigned    carry;

        /* Reject any non-alphabet char (strchr matching the NUL is
         * impossible here since i < slen). */
        if (!pos || !s[i]) {
            dealloc(bytes);
            return false;
        }
        carry = (unsigned)(pos - b58_alphabet);
        for (j = 0; j < byteslen; j++) {
            carry += (unsigned)bytes[j] * 58;
            bytes[j] = (uint8_t)(carry & 0xff);
            carry >>= 8;
        }
        while (carry) {
            bytes[byteslen++] = (uint8_t)(carry & 0xff);
            carry >>= 8;
        }
    }
    total = zeros + byteslen;
    if (total > *outlen) {
        dealloc(bytes);
        return false;
    }
    /* bytes[] is little-endian; emit leading zeros then big-endian value */
    for (i = 0; i < zeros; i++)
        out[n++] = 0;
    for (j = byteslen; j > 0; j--)
        out[n++] = bytes[j - 1];
    *outlen = n;
    dealloc(bytes);
    return true;
}

/* Decode a base58check pool authority pubkey (spec 04 §4.7) into its 32-byte
 * x-only form: payload = LE_u16(1) || xonly(32) || sha256d-checksum(4). Returns
 * false on bad length, wrong version prefix, or checksum mismatch. */
bool sv2_noise_authority_b58_to_xonly(const char* b58, uint8_t xonly[32]) {
    uint8_t payload[64];
    uint8_t hash1[32], hash2[32];
    size_t  outlen = sizeof(payload);

    if (!b58 || !xonly)
        return false;
    if (!base58_decode(b58, payload, &outlen))
        return false;
    if (outlen != 2 + 32 + 4)
        return false;
    /* LE u16 version prefix must be 1 → bytes [0x01, 0x00] */
    if (payload[0] != 0x01 || payload[1] != 0x00)
        return false;
    crypto_hash_sha256(hash1, payload, 34);
    crypto_hash_sha256(hash2, hash1, 32);
    if (memcmp(payload + 34, hash2, 4) != 0)
        return false;
    memcpy(xonly, payload + 2, 32);
    return true;
}

#endif /* HAVE_SV2 */
