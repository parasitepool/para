/*
 * Copyright 2026 Con Kolivas
 *
 * SV2 per-connection reassembly + handshake rate limiting.
 */

#include "config.h"

#ifdef HAVE_SV2

#include <string.h>
#include <time.h>

#include "libckpool.h"
#include "sv2_conn.h"

static struct sv2_noise_server_keys g_sv2_keys;
static bool                         g_sv2_keys_ready;

/*
 * Per-IP handshake throttle: open-addressed table.
 * - Rate window: max SV2_HS_MAX_PER_MIN starts per IP per 60s.
 * - Global: max SV2_HS_MAX_GLOBAL concurrent in-flight handshakes.
 * - Bucket TTL: entries idle past SV2_HS_BUCKET_TTL are reclaimable so a
 *   multi-IP flood / natural churn cannot permanently fill the table.
 */
#define SV2_HS_BUCKETS 512
#define SV2_HS_MAX_PER_MIN 30
#define SV2_HS_MAX_GLOBAL 256
#define SV2_HS_WINDOW_SECS 60
#define SV2_HS_BUCKET_TTL 120 /* must be >= SV2_HS_WINDOW_SECS */

struct sv2_hs_bucket {
    char   addr[64];
    time_t window_start;
    int    count;
    bool   used;
};

static struct sv2_hs_bucket g_hs[SV2_HS_BUCKETS];
static mutex_t              g_hs_lock;
static int                  g_hs_global_inflight;
static pthread_once_t       g_hs_lock_once = PTHREAD_ONCE_INIT;

static void hs_lock_init_once(void) {
    mutex_init(&g_hs_lock);
}

static void ensure_hs_lock(void) {
    pthread_once(&g_hs_lock_once, hs_lock_init_once);
}

bool sv2_init_server_keys(const char* authority_path, const char* static_path) {
    if (g_sv2_keys_ready)
        return true;
    if (!sv2_noise_load_server_keys(&g_sv2_keys, authority_path, static_path, 365))
        return false;
    g_sv2_keys_ready = true;
    ensure_hs_lock();
    return true;
}

void sv2_clear_server_keys(void) {
    if (g_sv2_keys_ready) {
        sv2_noise_server_keys_clear(&g_sv2_keys);
        g_sv2_keys_ready = false;
    }
}

const struct sv2_noise_server_keys* sv2_get_server_keys(void) {
    return g_sv2_keys_ready ? &g_sv2_keys : NULL;
}

static unsigned hs_hash(const char* addr) {
    unsigned h = 5381;
    int      c;

    while ((c = (unsigned char)*addr++))
        h = ((h << 5) + h) + (unsigned)c;
    return h % SV2_HS_BUCKETS;
}

static void hs_claim_bucket(struct sv2_hs_bucket* b, const char* address_name, time_t now) {
    snprintf(b->addr, sizeof(b->addr), "%s", address_name);
    b->window_start = now;
    b->count = 0;
    b->used = true;
}

/* True if this entry has been idle long enough to forget the IP. */
static bool hs_bucket_stale(const struct sv2_hs_bucket* b, time_t now) {
    return b->used && now >= b->window_start + SV2_HS_BUCKET_TTL;
}

/*
 * Find matching bucket, free slot, or reclaim a stale one (linear probe).
 * Caller holds g_hs_lock. create=false: match only.
 */
static struct sv2_hs_bucket* hs_lookup(const char* address_name, bool create, time_t now) {
    unsigned              start = hs_hash(address_name);
    unsigned              i;
    struct sv2_hs_bucket* stale = NULL;

    for (i = 0; i < SV2_HS_BUCKETS; i++) {
        struct sv2_hs_bucket* b = &g_hs[(start + i) % SV2_HS_BUCKETS];

        if (b->used && strcmp(b->addr, address_name) == 0)
            return b;
        if (!b->used) {
            if (!create)
                return NULL;
            hs_claim_bucket(b, address_name, now);
            return b;
        }
        /* Remember first stale slot on the probe chain for reclaim. */
        if (create && !stale && hs_bucket_stale(b, now))
            stale = b;
    }
    if (create && stale) {
        hs_claim_bucket(stale, address_name, now);
        return stale;
    }
    return NULL;
}

/*
 * Atomically enforce per-IP rate + global inflight and reserve a slot.
 * Must run before sv2_conn_new() so ECDH cost is only paid after a grant.
 */
bool sv2_handshake_try_reserve(const char* address_name) {
    struct sv2_hs_bucket* b;
    time_t                now = time(NULL);
    bool                  ok = false;

    if (!address_name || !address_name[0])
        return false;
    ensure_hs_lock();
    mutex_lock(&g_hs_lock);
    if (g_hs_global_inflight >= SV2_HS_MAX_GLOBAL)
        goto out;
    b = hs_lookup(address_name, true, now);
    if (!b)
        goto out;
    if (now >= b->window_start + SV2_HS_WINDOW_SECS) {
        b->window_start = now;
        b->count = 0;
    }
    if (b->count >= SV2_HS_MAX_PER_MIN)
        goto out;
    b->count++;
    g_hs_global_inflight++;
    ok = true;
out:
    mutex_unlock(&g_hs_lock);
    return ok;
}

void sv2_handshake_release(void) {
    ensure_hs_lock();
    mutex_lock(&g_hs_lock);
    if (g_hs_global_inflight > 0)
        g_hs_global_inflight--;
    mutex_unlock(&g_hs_lock);
}

struct sv2_conn* sv2_conn_new(const struct sv2_noise_server_keys* keys) {
    struct sv2_conn* c;

    if (!keys)
        return NULL;
    c = ckzalloc(sizeof(*c));
    c->noise = sv2_noise_session_new(keys);
    if (!c->noise) {
        dealloc(c);
        return NULL;
    }
    c->rx_cap = SV2_RX_CAP_INITIAL;
    c->rx = ckalloc(c->rx_cap);
    /*
     * Mining default: wire size of a maximum policy payload plus one read.
     * JD overrides via sv2_conn_set_rx_max() to SV2_MAX_JD_PAYLOAD scale.
     */
    c->rx_max = sv2_rx_max_for_payload(SV2_MAX_MINING_PAYLOAD);
    c->hs_inflight = false; /* set by connector after try_reserve */
    return c;
}

void sv2_conn_set_rx_max(struct sv2_conn* c, size_t rx_max) {
    if (!c || rx_max < SV2_RX_CAP_INITIAL)
        return;
    c->rx_max = rx_max;
    /* Shrink logical ceiling if we already over-allocated (rare). */
    if (c->rx_cap > c->rx_max)
        c->rx_cap = c->rx_max;
}

void sv2_conn_free(struct sv2_conn* c) {
    if (!c)
        return;
    if (c->hs_inflight)
        sv2_handshake_release();
    sv2_noise_session_free(c->noise);
    dealloc(c->rx);
    dealloc(c);
}

bool sv2_conn_ready(const struct sv2_conn* c) {
    return c && c->noise && sv2_noise_handshake_complete(c->noise);
}

static bool rx_append(struct sv2_conn* c, const uint8_t* data, size_t len) {
    if (c->rx_len + len > c->rx_max)
        return false;
    if (c->rx_len + len > c->rx_cap) {
        size_t ncap = c->rx_cap;

        while (ncap < c->rx_len + len)
            ncap *= 2;
        if (ncap > c->rx_max)
            ncap = c->rx_max;
        if (ncap < c->rx_len + len)
            return false;
        c->rx = ckrealloc(c->rx, ncap);
        c->rx_cap = ncap;
    }
    memcpy(c->rx + c->rx_len, data, len);
    c->rx_len += len;
    return true;
}

bool sv2_conn_feed(
    struct sv2_conn* c,
    const uint8_t*   data,
    size_t           len,
    uint8_t**        reply,
    size_t*          reply_len,
    uint8_t***       frames,
    size_t**         frame_lens,
    size_t*          nframes) {
    *reply = NULL;
    *reply_len = 0;
    *frames = NULL;
    *frame_lens = NULL;
    *nframes = 0;

    if (!c || !data)
        return false;

    if (!sv2_noise_handshake_complete(c->noise)) {
        if (!sv2_noise_handshake_read(c->noise, data, len, reply, reply_len))
            return false;
        if (sv2_noise_handshake_complete(c->noise) && c->hs_inflight) {
            c->hs_inflight = false;
            sv2_handshake_release();
        }
        return true;
    }

    if (!rx_append(c, data, len))
        return false;

    /* Decrypt as many complete frames as possible */
    while (c->rx_len > 0) {
        uint8_t* plain = NULL;
        size_t   plainlen = 0, consumed = 0;
        int      rc = sv2_noise_decrypt_frame(c->noise, c->rx, c->rx_len, &consumed, &plain, &plainlen);

        if (rc == -1)
            break;
        if (rc == -2) {
            dealloc(plain);
            return false;
        }
        /* Grow frame arrays */
        {
            size_t    n = *nframes + 1;
            uint8_t** nf = ckrealloc(*frames, n * sizeof(uint8_t*));
            size_t*   nl = ckrealloc(*frame_lens, n * sizeof(size_t));

            *frames = nf;
            *frame_lens = nl;
            (*frames)[*nframes] = plain;
            (*frame_lens)[*nframes] = plainlen;
            (*nframes)++;
        }
        memmove(c->rx, c->rx + consumed, c->rx_len - consumed);
        c->rx_len -= consumed;
    }
    /*
     * Release an oversized buffer once it drains. rx_cap only ever grew, so
     * a peer that sent a single large frame — up to 8MiB on the JD path —
     * pinned that much for the life of the connection.
     */
    if (unlikely(!c->rx_len && c->rx_cap > SV2_RX_CAP_INITIAL)) {
        c->rx = ckrealloc(c->rx, SV2_RX_CAP_INITIAL);
        c->rx_cap = SV2_RX_CAP_INITIAL;
    }
    return true;
}

bool sv2_conn_encrypt(struct sv2_conn* c, const uint8_t* plain, size_t plainlen, uint8_t** out, size_t* outlen) {
    if (!c || !sv2_conn_ready(c))
        return false;
    return sv2_noise_encrypt_frame(c->noise, plain, plainlen, out, outlen);
}

#endif /* HAVE_SV2 */
