/*
 * Copyright 2026 Con Kolivas
 *
 * SV2 Job Declaration Server (JDS) — Phase 2.
 *
 * Full-Template mode (DECLARE_TX_DATA required):
 *   SetupConnection
 *   AllocateMiningJobToken / .Success
 *   DeclareMiningJob → local payout check → request missing txs →
 *     first OK on this JD session: queue checkBlock worker (off creceiver);
 *     later declares on the same session: skip IPC (local payout only)
 *   ProvideMissingTransactions.Success (fills tx cache)
 *
 * PushSolution / SetCustomMiningJob (mining path) come next.
 * checkBlock never runs on the connector creceiver thread (HOL block).
 * Payout: shared pool — 100% of spendable value to pool script; solo — miner
 * script funded (other spendable outs e.g. fee allowed). The shared rule is a
 * deviation from spec §6.4.3; see payout_rule_ok().
 */

#include "config.h"

#ifdef HAVE_SV2

#include <fcntl.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "ckpool.h"
#include "libckpool.h"
#include "generator.h"
#include "connector.h"
#include "uthash.h"
#include "sv2_codec.h"
#include "sv2_cb.h"
#include "sv2_jd.h"
#include "sv2_tx.h"
#include "sv2_types.h"
#include "sv2_work.h"

#include <sodium.h>

#ifdef HAVE_CAPNP
#include "mining_ipc.h"
#endif

#define SV2_JD_TOKEN_BYTES 16
#define SV2_JD_MAX_TOKENS_PER_MIN 20
#define SV2_JD_MAX_DECLARES_PER_MIN 30
/*
 * Token lifetime. JDC keeps allocated tokens in a FIFO and may pop an
 * older unused token minutes (or longer) after Allocate.Success. A short
 * TTL races with concurrent Allocate (which runs expire_old_tokens) and
 * yields invalid-mining-job-token → SRI JDC treats that as malicious and
 * tears down the session. Keep undeclared tokens alive long enough for
 * the client queue; declared tokens only need a rebuild window.
 */
#define SV2_JD_TOKEN_TTL_UNDECLARED_SECS 7200 /* 2h unused/queued */
#define SV2_JD_TOKEN_TTL_DECLARED_SECS 3600   /* 1h after checkBlock OK */
#define SV2_JD_MAX_TOKENS_GLOBAL 4096
#define SV2_JD_DEFAULT_ENONCE_LEN 8
#define SV2_JD_MAX_BLOCK_BYTES (4u * 1024u * 1024u)
#define SV2_JD_MAX_CACHE_TXS 65536
/* Byte budget for the above; a full block of transactions is well under it. */
#define SV2_JD_MAX_CACHE_BYTES (256u << 20) /* 256 MiB */
/*
 * Cap concurrent checkBlock IPC calls (async workers). Excess declares get
 * busy immediately so the creceiver thread never waits on bitcoind.
 */
#define SV2_JD_CHECKBLOCK_MAX_INFLIGHT 4
/* Max enonce-length checkBlock attempts per declare (derived, then conf). */
#define SV2_JD_ENONCE_MAX_TRIES 2
/* Recent declared tokens to try on PushSolution (no token on wire). */
#define SV2_JD_PUSH_TOKEN_CANDIDATES 8
/* Coalesce identical declare templates for this many seconds. */
#define SV2_JD_COALESCE_TTL_SECS 120
#define SV2_JD_COALESCE_MAX 256
/* Drop pending ProvideMissing waits that never complete. */
#define SV2_JD_PENDING_TTL_SECS 300
/* Bound in-flight ProvideMissing state (memory / incomplete declares). */
#define SV2_JD_MAX_PENDING_GLOBAL 128
#define SV2_JD_MAX_PENDING_PER_CLIENT 8
/* When tx cache is full, evict this many oldest entries before insert. */
#define SV2_JD_CACHE_EVICT_BATCH 256

static void fill_random(uint8_t* buf, size_t len) {
    /* libsodium is linked for SV2 Noise; never fall back to libc random(). */
    if (sodium_init() < 0) {
        LOGERR("SV2 JD sodium_init failed — token RNG unavailable");
        memset(buf, 0, len);
        return;
    }
    randombytes_buf(buf, len);
}

/* --- wtxid → raw tx cache --- */

struct sv2_tx_cache_ent {
    UT_hash_handle hh;
    uint8_t        wtxid[32];
    uint8_t*       raw;
    uint32_t       raw_len;
    time_t         created;
};

struct sv2_jd_client {
    UT_hash_handle hh;
    int64_t        client_id;
    bool           setup_ok;
    bool           full_template;
    /*
     * After one successful checkBlock on this JD TCP session (shared or
     * solo), further declares skip IPC; our local payout rule still runs.
     * Invalid later templates risk hashrate only; submit fails closed.
     * Reconnect → new session → one new full check.
     */
    bool checkblock_ok;
    char address[INET6_ADDRSTRLEN];
    int  server;
    /* This session's JD user identity has been logged once at NOTICE. The
     * identity is separate from the mining connection's user_identity and
     * nothing cross-checks them, so an operator needs to see both. */
    bool   user_logged;
    time_t alloc_window_start;
    int    alloc_count;
    time_t declare_window_start;
    int    declare_count;
    /* refs: 1 while hashed; +1 per outstanding user outside the lock */
    int refs;
};

/* Validated template identity → skip re-checkBlock for identical declares. */
struct sv2_jd_coalesce {
    UT_hash_handle hh;
    uint8_t        thash[32];
    uint8_t        enonce_len;
    time_t         created;
};

/* Pending declare waiting on missing txs or validation */
struct sv2_jd_pending {
    uint32_t request_id;
    int64_t  client_id;
    time_t   created;
    uint8_t  token[SV2_MAX_JOB_TOKEN];
    uint8_t  token_len;
    uint32_t version;
    uint8_t* coinbase_tx_prefix;
    uint16_t coinbase_tx_prefix_len;
    uint8_t* coinbase_tx_suffix;
    uint16_t coinbase_tx_suffix_len;
    uint8_t* wtxid_list; /* wtxid_count * 32 */
    uint16_t wtxid_count;
    /* Positions still missing (shrunk as cache fills) */
    uint16_t* missing;
    uint16_t  missing_count;
    bool      awaiting_missing;
};

struct sv2_jd_token {
    UT_hash_handle hh;
    uint8_t        token[SV2_MAX_JOB_TOKEN];
    uint8_t        token_len;
    int64_t        client_id;
    char           user_identifier[SV2_MAX_STR_LEN + 1];
    time_t         created;
    bool           declared; /* checkBlock accepted */
    uint32_t       version;
    uint16_t       wtxid_count;
    /*
     * Pool/solo payout scriptPubKey from Allocate (spec §6.4.3 first
     * output). Declare/SetCustom MUST fund this script (value > 0).
     * has_payout false when allocate sent empty outputs (no address).
     */
    bool     has_payout;
    uint8_t* payout_script;
    uint8_t  payout_script_len;
    /* Material to rebuild on PushSolution */
    uint8_t* coinbase_tx_prefix;
    uint16_t coinbase_tx_prefix_len;
    uint8_t* coinbase_tx_suffix;
    uint16_t coinbase_tx_suffix_len;
    uint8_t* wtxid_list;
    uint8_t  enonce_len; /* enonce size that passed checkBlock */
                         /*
                          * No copy of the checkBlock candidate is kept: PushSolution rebuilds
                          * from the prefix/suffix/wtxid material above, so retaining the block
                          * pinned up to SV2_JD_MAX_BLOCK_BYTES per token for the declared TTL
                          * with nothing ever reading it.
                          */
};

/* Forward decls (after struct; used before definition). */
static void free_token_locked(struct sv2_jd_token* t);

static struct sv2_jd_client*    jd_clients;
static struct sv2_jd_token*     jd_tokens;
static struct sv2_tx_cache_ent* tx_cache;
static struct sv2_jd_coalesce*  jd_coalesce;
static struct sv2_jd_pending*   pending_declares; /* simple linked list */
static int                      pending_n;
static mutex_t                  jd_lock;
static int                      jd_token_count;
static int                      tx_cache_count;
static uint64_t                 tx_cache_bytes;
static int                      jd_coalesce_count;
/* In-flight checkBlock calls (global); not under jd_lock during IPC wait. */
static mutex_t        jd_cb_lock;
static int            jd_checkblock_inflight;
static pthread_once_t jd_locks_once = PTHREAD_ONCE_INIT;
/* Async checkBlock workers — keeps creceiver off bitcoind cs_main. */
static ckmsgq_t*      jd_validate_q;
static pthread_once_t jd_validate_once = PTHREAD_ONCE_INIT;
/*
 * Async block submit — keeps creceiver off the synchronous submitblock RPC,
 * which blocks for the duration of the call and spins indefinitely while no
 * bitcoind is live. A found block must never stall the connector's single
 * receive thread: that halts reads for every client, SV1 and SV2 alike.
 * Deliberately its own queue rather than jd_validate_q, whose workers each sit
 * in a full ConnectBlock for seconds at a time.
 */
static ckmsgq_t*      jd_submit_q;
static pthread_once_t jd_submit_once = PTHREAD_ONCE_INIT;

struct jd_submit_job {
    uint8_t* block;
    size_t   blen;
    int64_t  client_id;
    char     who[SV2_MAX_STR_LEN + 1];
};

/* Lightweight operator metrics (process lifetime). */
static struct {
    uint64_t allocate_ok;
    uint64_t allocate_rate_limited;
    uint64_t declare_ok;
    uint64_t declare_error;
    uint64_t declare_rate_limited;
    uint64_t checkblock_busy;
    uint64_t checkblock_fail;
    uint64_t coalesce_hit;
    uint64_t provide_missing;
    uint64_t push_solution;
} jd_stats;

/*
 * Bumped from the creceiver and from the jdval/jdsubmit worker threads, so a
 * plain ++ is a data race that silently loses counts. Relaxed ordering: these
 * are independent gauges, nothing is published through them.
 */
#define JD_STAT_INC(field) __atomic_add_fetch(&jd_stats.field, 1, __ATOMIC_RELAXED)
#define JD_STAT_GET(field) __atomic_load_n(&jd_stats.field, __ATOMIC_RELAXED)

static void jd_locks_init_once(void) {
    mutex_init(&jd_lock);
    mutex_init(&jd_cb_lock);
}

static void ensure_lock(void) {
    pthread_once(&jd_locks_once, jd_locks_init_once);
}

/* Caller holds jd_lock. Return the pending array once it empties; removal only
 * memmoves down, so it would otherwise sit at its high water mark forever. */
static void pending_shrink_locked(void) {
    if (!pending_n && pending_declares) {
        dealloc(pending_declares);
        pending_declares = NULL;
    }
}

/* Hash of declare template identity + tip prevhash (must re-validate on tip). */
static void template_hash(const struct sv2_jd_pending* pend, uint8_t out[32]) {
    size_t   n = 4 + 32 + pend->coinbase_tx_prefix_len + pend->coinbase_tx_suffix_len + (size_t)pend->wtxid_count * 32;
    uint8_t *buf, *p;
    uint32_t le, tip_ver, tip_ntime, tip_nbits;
    uint8_t  prevhash_hdr[32];

    buf = ckalloc(n ? n : 1);
    p = buf;
    le = htole32(pend->version);
    memcpy(p, &le, 4);
    p += 4;
    /* Scope coalesce to current tip so we never skip checkBlock across tips */
    if (stratifier_sv2_tip_for_jd(&tip_ver, &tip_ntime, &tip_nbits, prevhash_hdr))
        memcpy(p, prevhash_hdr, 32);
    else
        memset(p, 0, 32);
    p += 32;
    if (pend->coinbase_tx_prefix_len) {
        memcpy(p, pend->coinbase_tx_prefix, pend->coinbase_tx_prefix_len);
        p += pend->coinbase_tx_prefix_len;
    }
    if (pend->coinbase_tx_suffix_len) {
        memcpy(p, pend->coinbase_tx_suffix, pend->coinbase_tx_suffix_len);
        p += pend->coinbase_tx_suffix_len;
    }
    if (pend->wtxid_count && pend->wtxid_list) {
        memcpy(p, pend->wtxid_list, (size_t)pend->wtxid_count * 32);
        p += (size_t)pend->wtxid_count * 32;
    }
    gen_hash(buf, out, (int)(p - buf));
    dealloc(buf);
}

void sv2_jd_on_tip_change(void) {
    struct sv2_jd_coalesce *c, *tmp;

    if (!sv2_jd_enabled())
        return;
    ensure_lock();
    mutex_lock(&jd_lock);
    HASH_ITER(hh, jd_coalesce, c, tmp) {
        HASH_DEL(jd_coalesce, c);
        dealloc(c);
    }
    jd_coalesce_count = 0;
    mutex_unlock(&jd_lock);
    LOGDEBUG("SV2 JD coalesce cache cleared on tip change");
}

static void coalesce_note_locked(const uint8_t thash[32], uint8_t enonce_len, time_t now) {
    struct sv2_jd_coalesce *c, *tmp;

    HASH_FIND(hh, jd_coalesce, thash, 32, c);
    if (c) {
        c->enonce_len = enonce_len;
        c->created = now;
        return;
    }
    /* Evict expired entries if full */
    if (jd_coalesce_count >= SV2_JD_COALESCE_MAX) {
        HASH_ITER(hh, jd_coalesce, c, tmp) {
            if (now - c->created > SV2_JD_COALESCE_TTL_SECS || jd_coalesce_count >= SV2_JD_COALESCE_MAX) {
                HASH_DEL(jd_coalesce, c);
                jd_coalesce_count--;
                dealloc(c);
                if (jd_coalesce_count < SV2_JD_COALESCE_MAX / 2)
                    break;
            }
        }
    }
    c = ckzalloc(sizeof(*c));
    memcpy(c->thash, thash, 32);
    c->enonce_len = enonce_len;
    c->created = now;
    HASH_ADD(hh, jd_coalesce, thash, 32, c);
    jd_coalesce_count++;
}

static bool coalesce_lookup_locked(const uint8_t thash[32], uint8_t* enonce_len_out, time_t now) {
    struct sv2_jd_coalesce* c;

    HASH_FIND(hh, jd_coalesce, thash, 32, c);
    if (!c || now - c->created > SV2_JD_COALESCE_TTL_SECS)
        return false;
    if (enonce_len_out)
        *enonce_len_out = c->enonce_len;
    return true;
}

static bool checkblock_try_begin(void) {
    bool ok = false;

    ensure_lock();
    mutex_lock(&jd_cb_lock);
    if (jd_checkblock_inflight < SV2_JD_CHECKBLOCK_MAX_INFLIGHT) {
        jd_checkblock_inflight++;
        ok = true;
    }
    mutex_unlock(&jd_cb_lock);
    return ok;
}

static void checkblock_end(void) {
    ensure_lock();
    mutex_lock(&jd_cb_lock);
    if (jd_checkblock_inflight > 0)
        jd_checkblock_inflight--;
    mutex_unlock(&jd_cb_lock);
}

/* Caller holds jd_lock. Free client when last ref drops (must be unhashed). */
static void client_unref_locked(struct sv2_jd_client* c) {
    if (!c)
        return;
    if (--c->refs > 0)
        return;
    dealloc(c);
}

/*
 * Look up or create a client and take a ref for use outside jd_lock.
 * Caller must client_put() when done.
 */
static struct sv2_jd_client* client_get(int64_t client_id, bool create) {
    struct sv2_jd_client* c;

    ensure_lock();
    mutex_lock(&jd_lock);
    HASH_FIND_I64(jd_clients, &client_id, c);
    if (!c && create) {
        c = ckzalloc(sizeof(*c));
        c->client_id = client_id;
        c->refs = 1; /* hashed */
        HASH_ADD_I64(jd_clients, client_id, c);
    }
    if (c)
        c->refs++;
    mutex_unlock(&jd_lock);
    return c;
}

static void client_put(struct sv2_jd_client* c) {
    if (!c)
        return;
    ensure_lock();
    mutex_lock(&jd_lock);
    client_unref_locked(c);
    mutex_unlock(&jd_lock);
}

void sv2_jd_note_client(int64_t client_id, const char* address, int server) {
    struct sv2_jd_client* c = client_get(client_id, true);

    if (!c)
        return;
    if (address)
        snprintf(c->address, sizeof(c->address), "%s", address);
    c->server = server;
    client_put(c);
}

void sv2_jd_drop_client(int64_t client_id) {
    struct sv2_jd_client* c;
    struct sv2_jd_token * t, *tmp;
    int                   i;

    ensure_lock();
    mutex_lock(&jd_lock);
    HASH_FIND_I64(jd_clients, &client_id, c);
    if (c) {
        /* Unhash and drop the table ref; handlers may still hold refs. */
        HASH_DEL(jd_clients, c);
        client_unref_locked(c);
    }
    HASH_ITER(hh, jd_tokens, t, tmp) {
        if (t->client_id == client_id)
            free_token_locked(t);
    }
    for (i = 0; i < pending_n;) {
        if (pending_declares[i].client_id == client_id) {
            struct sv2_jd_pending* p = &pending_declares[i];

            dealloc(p->coinbase_tx_prefix);
            dealloc(p->coinbase_tx_suffix);
            dealloc(p->wtxid_list);
            dealloc(p->missing);
            memmove(&pending_declares[i], &pending_declares[i + 1], (pending_n - i - 1) * sizeof(*pending_declares));
            pending_n--;
            pending_shrink_locked();
        } else
            i++;
    }
    mutex_unlock(&jd_lock);
}

void sv2_jd_drop_all(void) {
    struct sv2_jd_client *c, *ctmp;
    struct sv2_jd_token * t, *tmp;
    int                   i, ncli = 0, ntok = 0;

    ensure_lock();
    mutex_lock(&jd_lock);
    HASH_ITER(hh, jd_clients, c, ctmp) {
        HASH_DEL(jd_clients, c);
        client_unref_locked(c);
        ncli++;
    }
    HASH_ITER(hh, jd_tokens, t, tmp) {
        free_token_locked(t);
        ntok++;
    }
    for (i = 0; i < pending_n; i++) {
        struct sv2_jd_pending* p = &pending_declares[i];

        dealloc(p->coinbase_tx_prefix);
        dealloc(p->coinbase_tx_suffix);
        dealloc(p->wtxid_list);
        dealloc(p->missing);
    }
    pending_n = 0;
    mutex_unlock(&jd_lock);
    if (ncli || ntok)
        LOGNOTICE("SV2 JD drop_all: clients=%d tokens=%d", ncli, ntok);
}

/* Caller holds jd_lock. */
static void free_token_locked(struct sv2_jd_token* t) {
    if (!t)
        return;
    HASH_DEL(jd_tokens, t);
    jd_token_count--;
    dealloc(t->payout_script);
    dealloc(t->coinbase_tx_prefix);
    dealloc(t->coinbase_tx_suffix);
    dealloc(t->wtxid_list);
    dealloc(t);
}

static bool token_user_match(const struct sv2_jd_token* t, const char* user) {
    const char* u = user ? user : "";

    return !strcmp(t->user_identifier, u);
}

/* Newest undeclared token for client + user_identifier (allocate re-issue). */
static struct sv2_jd_token* find_client_undeclared_token_locked(int64_t client_id, const char* user) {
    struct sv2_jd_token *t, *tmp, *best = NULL;

    HASH_ITER(hh, jd_tokens, t, tmp) {
        if (t->client_id != client_id || t->declared)
            continue;
        if (!token_user_match(t, user))
            continue;
        if (!best || t->created >= best->created)
            best = t;
    }
    return best;
}

/* Oldest token for client + user (declared or not) — fair re-issue under limits. */
static struct sv2_jd_token* find_client_oldest_token_locked(int64_t client_id, const char* user) {
    struct sv2_jd_token *t, *tmp, *oldest = NULL;

    HASH_ITER(hh, jd_tokens, t, tmp) {
        if (t->client_id != client_id)
            continue;
        if (!token_user_match(t, user))
            continue;
        if (!oldest || t->created < oldest->created)
            oldest = t;
    }
    return oldest;
}

/*
 * Free oldest *undeclared* token globally. Never touches declared tokens
 * (in-flight custom jobs). Only used when the table is full and this client
 * has no token of its own to re-issue.
 */
static bool free_oldest_undeclared_global_locked(void) {
    struct sv2_jd_token *t, *tmp, *oldest = NULL;

    HASH_ITER(hh, jd_tokens, t, tmp) {
        if (t->declared)
            continue;
        if (!oldest || t->created < oldest->created)
            oldest = t;
    }
    if (!oldest)
        return false;
    free_token_locked(oldest);
    return true;
}

static void expire_old_tokens_locked(time_t now) {
    struct sv2_jd_token *t, *tmp;
    int                  i, expired = 0;

    HASH_ITER(hh, jd_tokens, t, tmp) {
        time_t ttl = t->declared ? SV2_JD_TOKEN_TTL_DECLARED_SECS : SV2_JD_TOKEN_TTL_UNDECLARED_SECS;

        if (now - t->created > ttl) {
            expired++;
            free_token_locked(t);
        }
    }
    if (expired)
        LOGINFO("SV2 JD expired %d mining job token(s) (remain=%d)", expired, jd_token_count);
    /* Expire stale pending declares (client never sent missing txs) */
    for (i = 0; i < pending_n;) {
        if (now - pending_declares[i].created > SV2_JD_PENDING_TTL_SECS) {
            struct sv2_jd_pending* p = &pending_declares[i];

            LOGINFO("SV2 JD expiring stale pending declare client %" PRId64 " req=%u", p->client_id, p->request_id);
            dealloc(p->coinbase_tx_prefix);
            dealloc(p->coinbase_tx_suffix);
            dealloc(p->wtxid_list);
            dealloc(p->missing);
            memmove(&pending_declares[i], &pending_declares[i + 1], (pending_n - i - 1) * sizeof(*pending_declares));
            pending_n--;
            pending_shrink_locked();
        } else
            i++;
    }
}

static struct sv2_jd_token* find_token_locked(const uint8_t* tok, uint8_t len) {
    struct sv2_jd_token *t, *tmp;

    HASH_ITER(hh, jd_tokens, t, tmp) {
        if (t->token_len == len && !memcmp(t->token, tok, len))
            return t;
    }
    return NULL;
}

/* Caller holds jd_lock. */
static int pending_count_client_locked(int64_t client_id) {
    int i, n = 0;

    for (i = 0; i < pending_n; i++) {
        if (pending_declares[i].client_id == client_id)
            n++;
    }
    return n;
}

/* Caller holds jd_lock. True if another pending declare may be queued. */
static bool pending_may_add_locked(int64_t client_id) {
    if (pending_n >= SV2_JD_MAX_PENDING_GLOBAL)
        return false;
    if (pending_count_client_locked(client_id) >= SV2_JD_MAX_PENDING_PER_CLIENT)
        return false;
    return true;
}

/*
 * Caller holds jd_lock. Evict the n_evict oldest entries.
 *
 * uthash iterates in insertion order and created is stamped at insertion, so
 * the head of the table is the oldest entry — no search is needed. The former
 * rescan-per-eviction cost 256 full passes over up to SV2_JD_MAX_CACHE_TXS
 * entries under jd_lock, on the connector's receive thread, every time the
 * cache refilled to its cap.
 */
static void cache_evict_oldest_locked(int n_evict) {
    struct sv2_tx_cache_ent *e, *tmp;
    int                      i = 0;

    HASH_ITER(hh, tx_cache, e, tmp) {
        if (i++ >= n_evict)
            break;
        HASH_DEL(tx_cache, e);
        tx_cache_count--;
        tx_cache_bytes -= e->raw_len;
        dealloc(e->raw);
        dealloc(e);
    }
}

static void cache_put_tx(const uint8_t* raw, uint32_t raw_len) {
    struct sv2_tx_cache_ent* e;
    uint8_t                  wtxid[32];

    if (!raw || !raw_len)
        return;
    gen_hash((uchar*)raw, wtxid, (int)raw_len);
    HASH_FIND(hh, tx_cache, wtxid, 32, e);
    if (e)
        return;
    /*
     * Bound bytes as well as entries: a transaction runs to
     * SV2_MAX_TX_BYTES, so an entry cap alone leaves the cache byte
     * unbounded at 65536 * 4MiB. Evict until both budgets have room.
     */
    while ((tx_cache_count >= SV2_JD_MAX_CACHE_TXS || tx_cache_bytes + raw_len > SV2_JD_MAX_CACHE_BYTES) &&
           tx_cache_count > 0)
        cache_evict_oldest_locked(SV2_JD_CACHE_EVICT_BATCH);
    if (tx_cache_count >= SV2_JD_MAX_CACHE_TXS || tx_cache_bytes + raw_len > SV2_JD_MAX_CACHE_BYTES)
        return;
    e = ckzalloc(sizeof(*e));
    memcpy(e->wtxid, wtxid, 32);
    e->raw = ckalloc(raw_len);
    memcpy(e->raw, raw, raw_len);
    e->raw_len = raw_len;
    e->created = time(NULL);
    HASH_ADD(hh, tx_cache, wtxid, 32, e);
    tx_cache_count++;
    tx_cache_bytes += raw_len;
}

static struct sv2_tx_cache_ent* cache_get(const uint8_t wtxid[32]) {
    struct sv2_tx_cache_ent* e;

    HASH_FIND(hh, tx_cache, wtxid, 32, e);
    return e;
}

static uint8_t* reply_frame(uint8_t msg_type, const uint8_t* payload, size_t payload_len, size_t* replylen) {
    uint8_t* frame = NULL;

    if (!sv2_build_frame(0, msg_type, payload, (uint32_t)payload_len, &frame, replylen))
        return NULL;
    return frame;
}

/* --- Bitcoin helpers: coinbase parse, block assemble (see sv2_tx.c) --- */

/*
 * Derive the extranonce gap size between coinbase_tx_prefix and
 * coinbase_tx_suffix.
 *
 * SV2: full coinbase = prefix || extranonce || suffix. The scriptSig length
 * field lives in the prefix; the prefix ends part-way through scriptSig.
 * Remaining scriptSig bytes are the extranonce (suffix then continues with
 * sequence / outputs / optional witness / locktime).
 *
 * SRI JobFactory (DeclareMiningJob) serialises BIP141 marker+flag (0x00 0x01)
 * into the prefix — same layout as NewExtendedMiningJob before strip. Must
 * skip those two bytes or enonce derivation fails and every declare is
 * rejected as missing/unfunded pool payout.
 *
 * Trying a wrong length (e.g. nonce2length=8 when full enonce is en1+en2=12)
 * misaligns sequence/outputs → bitcoind Unserialize throws
 * ReadCompactSize(): size too large and checkBlock IPC fails hard.
 */
static bool derive_coinbase_enonce_len(const uint8_t* prefix, uint16_t prefix_len, uint8_t* enonce_len_out) {
    const uint8_t *p = prefix, *end = prefix + prefix_len;
    uint64_t       sslen;
    size_t         ss_in_prefix;

    if (!prefix || !enonce_len_out || prefix_len < 42)
        return false;
    /* version */
    p += 4;
    /* Optional BIP141 marker (0x00) + flag (non-zero, typically 0x01). */
    if (sv2_have_bytes(p, end, 2) && p[0] == 0x00 && p[1] != 0x00)
        p += 2;
    /* single coinbase input */
    if (p >= end || *p != 0x01)
        return false;
    p++;
    /* prevout (32 + 4) */
    if (!sv2_have_bytes(p, end, 36))
        return false;
    p += 36;
    if (!sv2_read_compact_size(&p, end, &sslen) || sslen > 10000)
        return false;
    ss_in_prefix = (size_t)(end - p);
    if (ss_in_prefix > sslen)
        return false;
    if (sslen - ss_in_prefix > 255)
        return false;
    *enonce_len_out = (uint8_t)(sslen - ss_in_prefix);
    return true;
}

/*
 * Bitcoin txid = SHA256d of non-witness serialization.
 * Handles both legacy and BIP144 witness forms.
 */
/*
 * Snapshot wtxid → raw txs under jd_lock (caller may run without lock after).
 * On success *raws and *lens are ckalloc'd arrays of count entries (raws[i] copied).
 */
static bool
snapshot_txs_from_cache(const uint8_t* wtxid_list, uint16_t count, uint8_t*** raws_out, uint32_t** lens_out) {
    uint8_t** raws;
    uint32_t* lens;
    uint16_t  i;

    *raws_out = NULL;
    *lens_out = NULL;
    if (!count)
        return true;
    raws = ckzalloc(sizeof(uint8_t*) * count);
    lens = ckzalloc(sizeof(uint32_t) * count);

    ensure_lock();
    mutex_lock(&jd_lock);
    for (i = 0; i < count; i++) {
        struct sv2_tx_cache_ent* e = cache_get(wtxid_list + (size_t)i * 32);

        if (!e) {
            mutex_unlock(&jd_lock);
            for (i = 0; i < count; i++)
                dealloc(raws[i]);
            dealloc(raws);
            dealloc(lens);
            return false;
        }
        lens[i] = e->raw_len;
        raws[i] = ckalloc(e->raw_len);
        memcpy(raws[i], e->raw, e->raw_len);
    }
    mutex_unlock(&jd_lock);

    *raws_out = raws;
    *lens_out = lens;
    return true;
}

static void free_tx_snapshot(uint8_t** raws, uint32_t* lens, uint16_t count) {
    uint16_t i;

    if (raws) {
        for (i = 0; i < count; i++)
            dealloc(raws[i]);
        dealloc(raws);
    }
    dealloc(lens);
}

/*
 * Assemble candidate block for checkBlock.
 * Coinbase = prefix || zero_enonce[enonce_len] || suffix
 * Txs follow wtxid order from cache (snapshotted under lock).
 * Returns ckalloc'd block; *out_len set. NULL on failure.
 */
static uint8_t* assemble_candidate_block(const struct sv2_jd_pending* pend, uint8_t enonce_len, size_t* out_len) {
    uint32_t tip_ver, tip_ntime, tip_nbits;
    uint8_t  prevhash_hdr[32];
    uint8_t *coinbase, *block, *p;
    size_t   coinbase_len, body_len, total;
    uint16_t i;
    int      ntx;
    /* clang-format off */
    uint8_t (*txids)[32]; /* clang-format before 20 drops the space */
    /* clang-format on */
    uint8_t   merkle[32];
    uint32_t  le;
    uint8_t** tx_raws = NULL;
    uint32_t* tx_lens = NULL;

    *out_len = 0;
    if (!stratifier_sv2_tip_for_jd(&tip_ver, &tip_ntime, &tip_nbits, prevhash_hdr)) {
        LOGINFO("SV2 JD no tip available for checkBlock");
        return NULL;
    }

    /* Policy: rough size before assembling (weight ≈ 4*vsize; use raw bytes) */
    {
        size_t est = pend->coinbase_tx_prefix_len + enonce_len + pend->coinbase_tx_suffix_len + 80 + 9;

        if (est > SV2_JD_MAX_BLOCK_BYTES)
            return NULL;
    }

    if (!snapshot_txs_from_cache(pend->wtxid_list, pend->wtxid_count, &tx_raws, &tx_lens))
        return NULL;

    coinbase_len = pend->coinbase_tx_prefix_len + enonce_len + pend->coinbase_tx_suffix_len;
    coinbase = ckalloc(coinbase_len);
    memcpy(coinbase, pend->coinbase_tx_prefix, pend->coinbase_tx_prefix_len);
    if (enonce_len)
        memset(coinbase + pend->coinbase_tx_prefix_len, 0, enonce_len);
    memcpy(
        coinbase + pend->coinbase_tx_prefix_len + enonce_len, pend->coinbase_tx_suffix, pend->coinbase_tx_suffix_len);

    ntx = 1 + (int)pend->wtxid_count;
    txids = ckalloc(sizeof(*txids) * (size_t)ntx);
    if (!sv2_bitcoin_txid(coinbase, coinbase_len, txids[0])) {
        dealloc(coinbase);
        dealloc(txids);
        free_tx_snapshot(tx_raws, tx_lens, pend->wtxid_count);
        return NULL;
    }

    body_len = coinbase_len;
    for (i = 0; i < pend->wtxid_count; i++) {
        if (!sv2_bitcoin_txid(tx_raws[i], tx_lens[i], txids[1 + i])) {
            dealloc(coinbase);
            dealloc(txids);
            free_tx_snapshot(tx_raws, tx_lens, pend->wtxid_count);
            return NULL;
        }
        body_len += tx_lens[i];
    }
    sv2_merkle_root_from_txids(txids, ntx, merkle);
    dealloc(txids);

    total = 80 + sv2_compact_size_len((uint64_t)ntx) + body_len;
    if (total > SV2_JD_MAX_BLOCK_BYTES) {
        dealloc(coinbase);
        free_tx_snapshot(tx_raws, tx_lens, pend->wtxid_count);
        return NULL;
    }
    block = ckalloc(total);
    p = block;
    /* header: version (declare wins if non-zero), prevhash, merkle, ntime, nbits, nonce=0 */
    le = htole32(pend->version ? pend->version : tip_ver);
    memcpy(p, &le, 4);
    p += 4;
    memcpy(p, prevhash_hdr, 32);
    p += 32;
    memcpy(p, merkle, 32);
    p += 32;
    le = htole32(tip_ntime);
    memcpy(p, &le, 4);
    p += 4;
    le = htole32(tip_nbits);
    memcpy(p, &le, 4);
    p += 4;
    memset(p, 0, 4); /* nonce */
    p += 4;
    sv2_write_compact_size(&p, (uint64_t)ntx);
    memcpy(p, coinbase, coinbase_len);
    p += coinbase_len;
    dealloc(coinbase);
    for (i = 0; i < pend->wtxid_count; i++) {
        memcpy(p, tx_raws[i], tx_lens[i]);
        p += tx_lens[i];
    }
    free_tx_snapshot(tx_raws, tx_lens, pend->wtxid_count);
    *out_len = (size_t)(p - block);
    return block;
}

/*
 * Tip-race rejects from checkBlock. SRI JDC only treats "stale-chain-tip" as
 * non-fatal; anything else (e.g. bad-cb-height) tears down the whole session.
 * Map these so a block arriving mid-declare does not force solo fallback.
 */
static bool reason_is_stale_tip(const char* reason) {
    if (!reason || !reason[0])
        return false;
    if (!strcmp(reason, "stale-chain-tip") || !strcmp(reason, "bad-cb-height") ||
        /* A JDC whose node is ahead of ours declares a coinbase with a BIP54
         * nLockTime of its own height - 1, which is not final in a block at
         * our lower tip. A tip disagreement, not a malformed template. */
        !strcmp(reason, "bad-txns-nonfinal") || !strcmp(reason, "inconclusive-not-best-prevblk") ||
        !strcmp(reason, "prev-blk-not-found") || !strcmp(reason, "time-too-old") || !strcmp(reason, "time-too-new"))
        return true;
    if (strstr(reason, "not-best-prev") || strstr(reason, "bad-cb-height"))
        return true;
    return false;
}

static uint8_t* error_declare(uint32_t request_id, const char* code, size_t* replylen) {
    struct sv2_declare_mining_job_error err;
    uint8_t                             pbuf[512];
    size_t                              plen = 0;
    const char*                         out = code;

    /* Wire code JDC soft-fails on */
    if (reason_is_stale_tip(code))
        out = "stale-chain-tip";

    JD_STAT_INC(declare_error);
    if (out && !strcmp(out, "busy"))
        JD_STAT_INC(checkblock_busy);
    else if (out && !strcmp(out, "rate-limited"))
        JD_STAT_INC(declare_rate_limited);
    else if (
        out && (!strcmp(out, "invalid-template") || !strcmp(out, "block-assemble-failed") ||
                !strcmp(out, "validation-unavailable") || !strcmp(out, "stale-chain-tip")))
        JD_STAT_INC(checkblock_fail);

    memset(&err, 0, sizeof(err));
    err.request_id = request_id;
    snprintf(err.error_code, sizeof(err.error_code), "%s", out ? out : "invalid-template");
    /*
     * Remapping the code above keeps a JDC that only knows "stale-chain-tip"
     * from tearing its session down, but it must not cost the client what
     * bitcoind actually said. error_details is the field for exactly that, so
     * the real reason still goes on the wire, verbatim, whenever the code we
     * send is not the one we were given. Borrowed for the encode only; code
     * outlives this call in every caller.
     */
    if (out != code && code && code[0]) {
        err.error_details = (const uint8_t*)code;
        err.error_details_len = strlen(code);
    }
    if (!sv2_encode_declare_mining_job_error(pbuf, sizeof(pbuf), &plen, &err))
        return NULL;
    return reply_frame(SV2_MSG_DECLARE_MINING_JOB_ERROR, pbuf, plen, replylen);
}

/*
 * Mark token declared and attach rebuild material. Takes ownership of pend
 * coinbase/wtxid buffers. block may be NULL if coalesced (no stored full block).
 */
static bool token_accept_declare_locked(struct sv2_jd_pending* pend, uint8_t enonce_used) {
    struct sv2_jd_token* tok;

    tok = find_token_locked(pend->token, pend->token_len);
    if (!tok || tok->client_id != pend->client_id)
        return false;
    tok->declared = true;
    tok->version = pend->version;
    tok->wtxid_count = pend->wtxid_count;
    tok->enonce_len = enonce_used;
    dealloc(tok->coinbase_tx_prefix);
    dealloc(tok->coinbase_tx_suffix);
    dealloc(tok->wtxid_list);
    tok->coinbase_tx_prefix = pend->coinbase_tx_prefix;
    tok->coinbase_tx_prefix_len = pend->coinbase_tx_prefix_len;
    tok->coinbase_tx_suffix = pend->coinbase_tx_suffix;
    tok->coinbase_tx_suffix_len = pend->coinbase_tx_suffix_len;
    tok->wtxid_list = pend->wtxid_list;
    pend->coinbase_tx_prefix = NULL;
    pend->coinbase_tx_suffix = NULL;
    pend->wtxid_list = NULL;
    return true;
}

static uint8_t* success_declare(struct sv2_jd_pending* pend, size_t* replylen) {
    struct sv2_declare_mining_job_success ok;
    uint8_t                               pbuf[512];
    size_t                                plen = 0;

    memset(&ok, 0, sizeof(ok));
    ok.request_id = pend->request_id;
    ok.new_mining_job_token_len = pend->token_len;
    memcpy(ok.new_mining_job_token, pend->token, pend->token_len);
    if (!sv2_encode_declare_mining_job_success(pbuf, sizeof(pbuf), &plen, &ok))
        return error_declare(pend->request_id, "internal-error", replylen);
    return reply_frame(SV2_MSG_DECLARE_MINING_JOB_SUCCESS, pbuf, plen, replylen);
}

static void free_jd_pending_fields(struct sv2_jd_pending* p) {
    if (!p)
        return;
    dealloc(p->coinbase_tx_prefix);
    dealloc(p->coinbase_tx_suffix);
    dealloc(p->wtxid_list);
    dealloc(p->missing);
    p->coinbase_tx_prefix = NULL;
    p->coinbase_tx_suffix = NULL;
    p->wtxid_list = NULL;
    p->missing = NULL;
}

static void free_jd_pending(struct sv2_jd_pending* p) {
    if (!p)
        return;
    free_jd_pending_fields(p);
    dealloc(p);
}

/* Caller holds jd_lock. Drop one pending declare (e.g. after encode fail). */
static void remove_pending_declare_locked(int64_t client_id, uint32_t request_id) {
    int i;

    for (i = 0; i < pending_n; i++) {
        if (pending_declares[i].client_id != client_id || pending_declares[i].request_id != request_id)
            continue;
        free_jd_pending_fields(&pending_declares[i]);
        memmove(&pending_declares[i], &pending_declares[i + 1], (pending_n - i - 1) * sizeof(*pending_declares));
        pending_n--;
        pending_shrink_locked();
        return;
    }
}

/*
 * Fast path: identical tip-scoped template recently validated — no IPC.
 * Returns Success/Error frame, or NULL if full checkBlock is required.
 */
static uint8_t* try_coalesce_declare(struct sv2_jd_pending* pend, size_t* replylen) {
    uint8_t thash[32], thash2[32];
    uint8_t enonce_used = SV2_JD_DEFAULT_ENONCE_LEN;
    time_t  now = time(NULL);
    bool    hit = false;

    template_hash(pend, thash);
    ensure_lock();
    mutex_lock(&jd_lock);
    if (!coalesce_lookup_locked(thash, &enonce_used, now)) {
        mutex_unlock(&jd_lock);
        return NULL;
    }
    mutex_unlock(&jd_lock);

    /* Tip may have moved while unlocked — re-hash and re-lookup. */
    template_hash(pend, thash2);
    if (memcmp(thash, thash2, 32) != 0)
        return NULL;

    ensure_lock();
    mutex_lock(&jd_lock);
    if (coalesce_lookup_locked(thash2, &enonce_used, now)) {
        if (!token_accept_declare_locked(pend, enonce_used)) {
            mutex_unlock(&jd_lock);
            return error_declare(pend->request_id, "invalid-mining-job-token", replylen);
        }
        hit = true;
    }
    mutex_unlock(&jd_lock);

    if (!hit)
        return NULL;

    JD_STAT_INC(coalesce_hit);
    JD_STAT_INC(declare_ok);
    LOGINFO(
        "SV2 JD DeclareMiningJob coalesced client %" PRId64
        " req=%u "
        "(ok=%" PRIu64 " coalesce=%" PRIu64 ")",
        pend->client_id, pend->request_id, JD_STAT_GET(declare_ok), JD_STAT_GET(coalesce_hit));
    return success_declare(pend, replylen);
}

/*
 * Run checkBlock on assembled candidate. Caller already reserved an inflight
 * slot via checkblock_try_begin(); this always calls checkblock_end().
 * Blocks the calling worker thread on IPC — never the creceiver.
 */
static uint8_t* finalize_declare_check(struct sv2_jd_pending* pend, size_t* replylen) {
    uint8_t* block = NULL;
    size_t   blen = 0;
    uint8_t  enonce_try[SV2_JD_ENONCE_MAX_TRIES];
    unsigned enonce_ntry = 0;
    uint8_t  enonce_used = SV2_JD_DEFAULT_ENONCE_LEN;
    uint8_t  thash[32];
    unsigned ti;
    int      valid = 0;
    char     reason[256] = "", debug[512] = "";
    bool     any_ipc = false;
    time_t   now = time(NULL);

    /* Tip identity for this validation; only coalesce-note if still same after. */
    template_hash(pend, thash);

    /*
     * Extranonce length: prefer coinbase-derived (SRI JDC = full en1+en2),
     * then conf en1+en2 if different. Cap at SV2_JD_ENONCE_MAX_TRIES — do
     * not walk a long fallback list (each try is a full ConnectBlock).
     */
    {
        uint8_t derived = 0;
        uint8_t conf_sum = 0;

        if (derive_coinbase_enonce_len(pend->coinbase_tx_prefix, pend->coinbase_tx_prefix_len, &derived)) {
            enonce_try[enonce_ntry++] = derived;
            LOGDEBUG("SV2 JD declare enonce_len derived=%u from coinbase", derived);
        }
        if (ckpool.nonce1length > 0 && ckpool.nonce2length > 0 && ckpool.nonce1length + ckpool.nonce2length <= 255)
            conf_sum = (uint8_t)(ckpool.nonce1length + ckpool.nonce2length);
        if (conf_sum && enonce_ntry < SV2_JD_ENONCE_MAX_TRIES) {
            unsigned u;
            bool     seen = false;

            for (u = 0; u < enonce_ntry; u++) {
                if (enonce_try[u] == conf_sum) {
                    seen = true;
                    break;
                }
            }
            if (!seen)
                enonce_try[enonce_ntry++] = conf_sum;
        }
        if (!enonce_ntry)
            enonce_try[enonce_ntry++] = SV2_JD_DEFAULT_ENONCE_LEN;
    }

    for (ti = 0; ti < enonce_ntry; ti++) {
        dealloc(block);
        block = assemble_candidate_block(pend, enonce_try[ti], &blen);
        if (!block)
            continue;
        enonce_used = enonce_try[ti];
#ifdef HAVE_CAPNP
        if (ckpool.btc_validation_svc && mining_ipc_service_connected(ckpool.btc_validation_svc)) {
            mining_block_check_options opts = mining_check_opts_template();

            if (mining_ipc_check_block(
                    ckpool.btc_validation_svc, block, blen, &opts, &valid, reason, sizeof(reason), debug,
                    sizeof(debug)) == 0) {
                any_ipc = true;
                if (valid)
                    break;
                LOGINFO("SV2 JD checkBlock reject enonce=%u: %s (%s)", enonce_try[ti], reason, debug);
                if (reason_is_stale_tip(reason))
                    break;
            } else {
                LOGWARNING(
                    "SV2 JD checkBlock IPC call failed "
                    "enonce=%u block_len=%zu — trying next",
                    enonce_try[ti], blen);
                JD_STAT_INC(checkblock_fail);
                valid = 0;
            }
        } else
#endif
        {
            /*
             * No validation IPC: fail closed.
             * Structural assembly is not consensus-valid proof —
             * never DeclareMiningJob.Success without checkBlock.
             * any_ipc stays false → error "validation-unavailable".
             */
            LOGWARNING(
                "SV2 JD checkBlock skipped (no validation svc) — "
                "rejecting declare (validation-unavailable)");
            valid = 0;
            break;
        }
    }

    if (!block) {
        checkblock_end();
        return error_declare(pend->request_id, "block-assemble-failed", replylen);
    }
    if (!valid) {
        const char* code;

        dealloc(block);
        checkblock_end();
        if (!any_ipc)
            code = "validation-unavailable";
        else if (reason[0])
            code = reason;
        else
            code = "invalid-template";
        return error_declare(pend->request_id, code, replylen);
    }

    /* checkBlock has passed; the candidate itself is of no further use. */
    dealloc(block);

    /*
     * Only coalesce-note if tip identity is unchanged since pre-IPC hash.
     * If the tip moved during checkBlock, still accept the token but do not
     * cache under the new tip (that would skip validation on the new tip).
     */
    {
        uint8_t thash_post[32];
        bool    tip_stable;

        template_hash(pend, thash_post);
        tip_stable = (memcmp(thash, thash_post, 32) == 0);

        ensure_lock();
        mutex_lock(&jd_lock);
        if (!token_accept_declare_locked(pend, enonce_used)) {
            mutex_unlock(&jd_lock);
            checkblock_end();
            return error_declare(pend->request_id, "invalid-mining-job-token", replylen);
        }
        if (tip_stable)
            coalesce_note_locked(thash, enonce_used, now);
        mutex_unlock(&jd_lock);
    }
    checkblock_end();

    JD_STAT_INC(declare_ok);
    /* Session self-tested; later declares skip checkBlock (shared + solo). */
    {
        struct sv2_jd_client* c;

        ensure_lock();
        mutex_lock(&jd_lock);
        HASH_FIND_I64(jd_clients, &pend->client_id, c);
        if (c)
            c->checkblock_ok = true;
        mutex_unlock(&jd_lock);
    }
    LOGNOTICE(
        "SV2 JD DeclareMiningJob checkBlock OK client %" PRId64 " req=%u wtxids=%u block=%zu enonce=%u (ok=%" PRIu64
        " fail=%" PRIu64 " busy=%" PRIu64 ") (session validated)",
        pend->client_id, pend->request_id, pend->wtxid_count, blen, enonce_used, JD_STAT_GET(declare_ok),
        JD_STAT_GET(checkblock_fail), JD_STAT_GET(checkblock_busy));
    return success_declare(pend, replylen);
}

/* Worker: run checkBlock off the creceiver and send the reply async. */
static void jd_validate_process(struct sv2_jd_pending* pend) {
    size_t   replylen = 0;
    uint8_t* reply;

    if (!pend)
        return;
    reply = finalize_declare_check(pend, &replylen);
    if (reply && replylen)
        connector_sv2_send_plain(pend->client_id, reply, replylen);
    else
        dealloc(reply);
    free_jd_pending(pend);
}

static void jd_validate_q_init(void) {
    /* Primary queue + N worker threads (shared lock) = max inflight. */
    jd_validate_q = create_ckmsgqs("jdval", (const void*)jd_validate_process, SV2_JD_CHECKBLOCK_MAX_INFLIGHT);
}

/* Worker: submit a solved block off the creceiver. Single thread, so two
 * solutions arriving together stay serialised as they were when inline. */
static void jd_submit_process(struct jd_submit_job* job) {
    bool accepted;

    if (!job)
        return;
    accepted = stratifier_sv2_submit_block_bin(job->block, job->blen, 0, job->who[0] ? job->who : NULL);
    LOGWARNING(
        "SV2 JD PushSolution client %" PRId64
        " block %zu bytes %s "
        "(push_total=%" PRIu64 ")",
        job->client_id, job->blen, accepted ? "ACCEPTED" : "rejected/error", JD_STAT_GET(push_solution));
    dealloc(job->block);
    dealloc(job);
}

static void jd_submit_q_init(void) {
    jd_submit_q = create_ckmsgq("jdsubmit", (const void*)jd_submit_process);
}

/*
 * Session already passed one checkBlock — accept declare without IPC.
 * Local prechecks (token, payout rule, caps) already ran. Derive enonce for
 * PushSolution rebuild; submit path still fails closed at the node.
 */
static uint8_t* session_accept_declare_skip_check(struct sv2_jd_pending* pend, size_t* replylen) {
    uint8_t enonce_used = SV2_JD_DEFAULT_ENONCE_LEN;

    if (!derive_coinbase_enonce_len(pend->coinbase_tx_prefix, pend->coinbase_tx_prefix_len, &enonce_used))
        enonce_used = SV2_JD_DEFAULT_ENONCE_LEN;

    ensure_lock();
    mutex_lock(&jd_lock);
    if (!token_accept_declare_locked(pend, enonce_used)) {
        mutex_unlock(&jd_lock);
        return error_declare(pend->request_id, "invalid-mining-job-token", replylen);
    }
    mutex_unlock(&jd_lock);

    JD_STAT_INC(declare_ok);
    LOGINFO(
        "SV2 JD DeclareMiningJob skip-check client %" PRId64 " req=%u enonce=%u (session already validated)",
        pend->client_id, pend->request_id, enonce_used);
    return success_declare(pend, replylen);
}

/*
 * Start validation for a declare that has all txs.
 *
 * Shared pool and solo:
 *   First successful checkBlock on this JD TCP session self-tests the client's
 *   JDC/template stack. Further declares on the same session skip checkBlock;
 *   every declare still enforces our local payout rule (shared: 100% spendable
 *   to pool script; solo: miner script funded, other outs OK). Amount/fees
 *   are not re-checked at the node.
 *   Invalid later templates risk hashrate only; submit fails closed.
 *   Reconnect → new session → one new full check. Tip change does not clear
 *   the session flag (coalesce cache is tip-scoped separately).
 *
 * Coalesce may complete synchronously. Otherwise checkBlock is queued and
 * *replylen stays 0 (reply later via connector_sv2_send_plain). On queue
 * success, steals buffer ownership from *pend_src.
 */
static uint8_t* start_declare_validation(struct sv2_jd_pending* pend_src, size_t* replylen) {
    struct sv2_jd_pending* pend;
    struct sv2_jd_client*  c;
    uint8_t*               coalesced;
    bool                   session_skip = false;

    *replylen = 0;
    coalesced = try_coalesce_declare(pend_src, replylen);
    if (coalesced)
        return coalesced;

    ensure_lock();
    mutex_lock(&jd_lock);
    HASH_FIND_I64(jd_clients, &pend_src->client_id, c);
    if (c && c->checkblock_ok)
        session_skip = true;
    mutex_unlock(&jd_lock);
    if (session_skip)
        return session_accept_declare_skip_check(pend_src, replylen);

    if (!checkblock_try_begin()) {
        LOGNOTICE(
            "SV2 JD checkBlock busy (inflight cap %d) client %" PRId64 " busy_total=%" PRIu64 " (first-session check)",
            SV2_JD_CHECKBLOCK_MAX_INFLIGHT, pend_src->client_id, JD_STAT_GET(checkblock_busy) + 1);
        return error_declare(pend_src->request_id, "busy", replylen);
    }

    /* Heap-copy pending for worker lifetime. */
    pend = ckzalloc(sizeof(*pend));
    *pend = *pend_src;
    /*
     * Validation does not need the ProvideMissing index list. Free it
     * here — both pend and pend_src must drop the pointer or we leak
     * once per declare that went through ProvideMissing.
     */
    dealloc(pend->missing);
    pend->missing = NULL;
    pend->missing_count = 0;
    pend->awaiting_missing = false;
    /* Steal buffers from source so caller does not free them. */
    pend_src->coinbase_tx_prefix = NULL;
    pend_src->coinbase_tx_suffix = NULL;
    pend_src->wtxid_list = NULL;
    pend_src->missing = NULL;

    pthread_once(&jd_validate_once, jd_validate_q_init);
    if (!jd_validate_q || !ckmsgq_add(jd_validate_q, pend)) {
        checkblock_end();
        free_jd_pending(pend);
        return error_declare(pend_src->request_id, "busy", replylen);
    }
    LOGDEBUG(
        "SV2 JD checkBlock queued client %" PRId64 " req=%u (first-session)", pend_src->client_id,
        pend_src->request_id);
    return NULL;
}

/* Build missing list from cache; returns count. Fills missing[] (caller-owned max). */
static uint16_t
collect_missing(const uint8_t* wtxid_list, uint16_t wtxid_count, uint16_t* missing, uint16_t max_missing) {
    uint16_t i, n = 0;

    for (i = 0; i < wtxid_count && n < max_missing; i++) {
        if (!cache_get(wtxid_list + (size_t)i * 32))
            missing[n++] = i;
    }
    return n;
}

static uint8_t* handle_setup(struct sv2_jd_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_setup_connection         sc;
    struct sv2_setup_connection_success ok;
    struct sv2_setup_connection_error   err;
    uint8_t                             pbuf[64];
    size_t                              plen = 0;

    if (!sv2_decode_setup_connection(payload, len, &sc))
        return NULL;
    if (sc.protocol != SV2_PROTOCOL_JOB_DECLARATION) {
        memset(&err, 0, sizeof(err));
        snprintf(err.error_code, sizeof(err.error_code), "unsupported-protocol");
        if (!sv2_encode_setup_connection_error(pbuf, sizeof(pbuf), &plen, &err))
            return NULL;
        return reply_frame(SV2_MSG_SETUP_CONNECTION_ERROR, pbuf, plen, replylen);
    }
    if (sc.min_version > 2 || sc.max_version < 2) {
        memset(&err, 0, sizeof(err));
        snprintf(err.error_code, sizeof(err.error_code), "unsupported-feature-flags");
        if (!sv2_encode_setup_connection_error(pbuf, sizeof(pbuf), &plen, &err))
            return NULL;
        return reply_frame(SV2_MSG_SETUP_CONNECTION_ERROR, pbuf, plen, replylen);
    }
    if (!(sc.flags & SV2_JD_FLAG_DECLARE_TX_DATA)) {
        memset(&err, 0, sizeof(err));
        err.flags = SV2_JD_FLAG_DECLARE_TX_DATA;
        snprintf(err.error_code, sizeof(err.error_code), "unsupported-feature-flags");
        if (!sv2_encode_setup_connection_error(pbuf, sizeof(pbuf), &plen, &err))
            return NULL;
        return reply_frame(SV2_MSG_SETUP_CONNECTION_ERROR, pbuf, plen, replylen);
    }

    c->setup_ok = true;
    c->full_template = true;
    memset(&ok, 0, sizeof(ok));
    ok.used_version = 2;
    if (!sv2_encode_setup_connection_success(pbuf, sizeof(pbuf), &plen, &ok))
        return NULL;
    LOGNOTICE("SV2 JD SetupConnection ok client %" PRId64 " vendor=%s full-template", c->client_id, sc.vendor);
    return reply_frame(SV2_MSG_SETUP_CONNECTION_SUCCESS, pbuf, plen, replylen);
}

/*
 * Spec §6.4.3: pool payout is the first Allocate output's scriptPubKey.
 * Extract it from CompactSize-prefixed outputs blob (count + outputs).
 */
static bool
extract_first_output_script(const uint8_t* outputs, uint16_t olen, uint8_t** script_out, uint8_t* script_len_out) {
    const uint8_t* script;
    uint8_t        slen;

    *script_out = NULL;
    *script_len_out = 0;
    /* Shared parse (sv2_cb.c) is zero-copy; tokens outlive the blob it points
     * into, so keep our own copy. */
    if (!sv2_cb_first_output_script(outputs, olen, &script, &slen))
        return false;
    *script_out = ckalloc(slen);
    memcpy(*script_out, script, slen);
    *script_len_out = slen;
    return true;
}

/*
 * Spendable-output accounting:
 *   value == 0 → not spendable reward (commitment OK under any script)
 *   value > 0 on payout_script → payout_value
 *   value > 0 elsewhere → other_value (diverted / fee / etc.)
 */
static void account_spendable_out(
    const uint8_t* payout,
    uint8_t        payout_len,
    const uint8_t* out_script,
    uint64_t       out_len,
    uint64_t       value,
    uint64_t*      payout_value,
    uint64_t*      other_value) {
    if (value == 0)
        return;
    if (out_len == payout_len && !memcmp(out_script, payout, payout_len))
        *payout_value += value;
    else
        *other_value += value;
}

/*
 * Minimum pool payout accepted in shared mode: the block subsidy.
 *
 * Spec §6.4.3 pins the payout *destination* but not the amount ("JDC MUST
 * allocate sats into the pool payout output", no figure given), and a coinbase
 * that claims less than it is entitled to is valid to consensus — so a template
 * paying the pool one satoshi and forfeiting the rest passes checkBlock, is
 * accepted by the network, and does NOT fail closed on submit. Per-template
 * fees cannot be known here, but the subsidy is deterministic from height, so
 * floor on that and let fee variance alone. Catches gross underpayment,
 * whether a broken JDC fee calculation or a deliberate burn.
 *
 * Uses the subsidy one block *ahead* of our own tip: subsidy is non-increasing
 * with height, so if a block arrives between our tip and the client's declare
 * we can never floor above the true subsidy for the height they built on. The
 * cost is accepting the post-halving amount for the one block before a halving.
 *
 * Returns 0 (no floor) in solo mode, or before we have a workbase to ask.
 */
static uint64_t pool_min_payout(void) {
    /* Regtest halves every 150 blocks, everything else every 210000. */
    const int interval = ckpool.regtest ? 150 : 210000;
    int       height = stratifier_sv2_tip_height();
    int       halvings;

    if (ckpool.btcsolo || height < 1)
        return 0;
    halvings = (height + 1) / interval;
    if (halvings >= 64)
        return 0;
    return (uint64_t)5000000000ULL >> halvings;
}

/*
 * Accept payout result for mode:
 *   shared pool — 100% of spendable value to pool script (other_value == 0),
 *                 and at least the block subsidy (see pool_min_payout)
 *   solo — miner payout script funded with value > 0; other outs (fee) OK
 *
 * Spec §6.4.3 requires of the client only that the pool payout output be funded
 * at all. The shared rule here is stricter than that, and deliberately so: the
 * spec's own remedy for a client that diverts reward is for the pool to "pay
 * proportionally smaller rewards for this job", which a share-accounted pool
 * has no means to do — there is no per-job reward knob to turn down. Refusing
 * the declare is the only lever we have, at the cost that the SV2 Job
 * Declaration rules only require the payout output be funded and do not
 * require 100% of spendable value to the pool — so what we refuse is stricter
 * than the protocol alone. DeclareMiningJob.Error also invites the client to
 * fall back to another pool. Solo pays the miner, so it needs no such rule.
 */
static bool payout_rule_ok(uint64_t payout_value, uint64_t other_value) {
    uint64_t min_payout;

    if (payout_value == 0)
        return false;
    if (ckpool.btcsolo)
        return true;
    if (other_value)
        return false;
    min_payout = pool_min_payout();
    if (unlikely(payout_value < min_payout)) {
        LOGWARNING(
            "SV2 JD coinbase pays pool %" PRIu64
            " sats, below subsidy floor "
            "%" PRIu64 " — refusing",
            payout_value, min_payout);
        return false;
    }
    return true;
}

/*
 * Walk CompactSize-prefixed output list (Allocate / SetCustomMiningJob shape).
 */
static bool
outputs_blob_funds_payout(const uint8_t* blob, uint16_t blob_len, const uint8_t* script, uint8_t script_len) {
    const uint8_t *p, *end;
    uint64_t       nout, i, slen, value;
    uint64_t       payout_value = 0, other_value = 0;

    if (!blob || !blob_len || !script || !script_len)
        return false;
    p = blob;
    end = blob + blob_len;
    if (!sv2_read_compact_size(&p, end, &nout) || nout > 10000)
        return false;
    for (i = 0; i < nout; i++) {
        if (!sv2_have_bytes(p, end, 8))
            return false;
        /* value LE */
        value = (uint64_t)p[0] | ((uint64_t)p[1] << 8) | ((uint64_t)p[2] << 16) | ((uint64_t)p[3] << 24) |
                ((uint64_t)p[4] << 32) | ((uint64_t)p[5] << 40) | ((uint64_t)p[6] << 48) | ((uint64_t)p[7] << 56);
        p += 8;
        if (!sv2_read_compact_size(&p, end, &slen) || !sv2_have_bytes(p, end, slen))
            return false;
        account_spendable_out(script, script_len, p, slen, value, &payout_value, &other_value);
        p += slen;
    }
    return payout_rule_ok(payout_value, other_value);
}

/*
 * Full coinbase (prefix||enonce||suffix), possibly BIP141 witness form.
 */
static bool
coinbase_tx_funds_payout(const uint8_t* tx, size_t len, const uint8_t* script, uint8_t script_len, bool* parse_ok) {
    const uint8_t *p, *end;
    uint64_t       i, nin, nout, n, value;
    uint64_t       payout_value = 0, other_value = 0;
    bool           witness = false;

    /* Lets the caller tell a structurally bad coinbase apart from a well
     * formed one that breaks the payout rule — they need different advice. */
    if (parse_ok)
        *parse_ok = false;
    if (!tx || len < 10 || !script || !script_len)
        return false;
    p = tx;
    end = tx + len;
    p += 4; /* version */
    if (sv2_have_bytes(p, end, 2) && p[0] == 0x00 && p[1] == 0x01) {
        witness = true;
        p += 2;
    }
    if (!sv2_read_compact_size(&p, end, &nin) || nin > 100000)
        return false;
    for (i = 0; i < nin; i++) {
        if (!sv2_have_bytes(p, end, 36))
            return false;
        p += 36;
        if (!sv2_read_compact_size(&p, end, &n) || n > UINT64_MAX - 4 || !sv2_have_bytes(p, end, n + 4))
            return false;
        p += n + 4;
    }
    if (!sv2_read_compact_size(&p, end, &nout) || nout > 100000)
        return false;
    for (i = 0; i < nout; i++) {
        if (!sv2_have_bytes(p, end, 8))
            return false;
        value = (uint64_t)p[0] | ((uint64_t)p[1] << 8) | ((uint64_t)p[2] << 16) | ((uint64_t)p[3] << 24) |
                ((uint64_t)p[4] << 32) | ((uint64_t)p[5] << 40) | ((uint64_t)p[6] << 48) | ((uint64_t)p[7] << 56);
        p += 8;
        if (!sv2_read_compact_size(&p, end, &n) || !sv2_have_bytes(p, end, n))
            return false;
        account_spendable_out(script, script_len, p, n, value, &payout_value, &other_value);
        p += n;
    }
    (void)witness;
    if (parse_ok)
        *parse_ok = true;
    return payout_rule_ok(payout_value, other_value);
}

/* Store Allocate first-output script on token (caller holds jd_lock). */
static void
token_set_payout_from_outputs_locked(struct sv2_jd_token* tok, const uint8_t* outputs, uint16_t outputs_len) {
    uint8_t* script = NULL;
    uint8_t  slen = 0;

    if (!tok)
        return;
    dealloc(tok->payout_script);
    tok->payout_script = NULL;
    tok->payout_script_len = 0;
    tok->has_payout = false;
    if (!outputs_len)
        return;
    if (!extract_first_output_script(outputs, outputs_len, &script, &slen))
        return;
    tok->payout_script = script;
    tok->payout_script_len = slen;
    tok->has_payout = true;
}

/*
 * Build coinbase_tx_outputs for AllocateMiningJobToken.Success:
 * CompactSize-prefixed array of consensus outputs. First output is the pool
 * (or solo user) payout script with 0 sats (spec 6.4.3).
 *
 * Solo has no pool payout, so the JD user identity is the only address we will
 * ever constrain the coinbase to. No identity, one bitcoind rejects, or no
 * bitcoind to ask means no constraint at all, leaving the payout to the JDC's
 * own config: falling back to btcaddress there would hand the miner a
 * constraint funding the pool, which in solo with no btcaddress configured is
 * the donation address (see server_alive()). Solo passes an unconstrained token
 * by design. why, when non-NULL, takes the reason no constraint was built.
 */
static uint16_t build_allocate_outputs(uint8_t* out, size_t outsz, const char* user_identifier, const char** why) {
    char        script[64], account[128];
    int         script_len = 0;
    uint8_t*    p = out;
    const char* addr = NULL;
    bool        script_flag = false, segwit = false;

    if (outsz < 2)
        return 0;

    if (ckpool.btcsolo) {
        bool   scr = false, seg = false;
        size_t n;

        if (!user_identifier || !user_identifier[0]) {
            if (why)
                *why = "no user identity sent";
            return 0;
        }
        /*
         * The account is everything before the first '.' or '_', which is
         * what generate_user() takes from a workername on the mining
         * connection. A JD client is asked to send the same identity there
         * as it authorises with, and that string
         * usually names a worker, so without this normalisation a perfectly
         * good address arrives here as "address.worker" and is rejected —
         * silently paying whatever the fallback is. No address encoding
         * contains either character, so a bare address passes untouched.
         */
        n = strcspn(user_identifier, "._");
        if (!n || n >= sizeof(account)) {
            if (why)
                *why = "identity has no usable account name";
            return 0;
        }
        memcpy(account, user_identifier, n);
        account[n] = '\0';
        if (!generator_checkaddr(account, &scr, &seg)) {
            if (why) {
                *why = generator_alive() ? "identity is not an address bitcoind accepts"
                                         : "no live bitcoind to validate identity";
            }
            return 0;
        }
        if (n != strlen(user_identifier)) {
            LOGINFO("SV2 JD identity %s pays its account %s", user_identifier, account);
        }
        addr = account;
        script_flag = scr;
        segwit = seg;
    } else if (ckpool.btcaddress) {
        addr = ckpool.btcaddress;
        script_flag = ckpool.script;
        segwit = ckpool.segwit;
    }
    if (!addr)
        return 0; /* empty outputs — JDC may still declare */

    script_len = address_to_txn(script, addr, script_flag, segwit);
    if (script_len < 1 || script_len > 40) {
        if (why)
            *why = "identity has no usable output script";
        return 0;
    }
    /* 1 output: value(8)=0 + script_len(1) + script */
    if (outsz < (size_t)(1 + 8 + 1 + script_len))
        return 0;
    *p++ = 0x01;     /* output count */
    memset(p, 0, 8); /* 0 sats */
    p += 8;
    *p++ = (uint8_t)script_len;
    memcpy(p, script, (size_t)script_len);
    p += script_len;
    return (uint16_t)(p - out);
}

static uint8_t* handle_allocate(struct sv2_jd_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_allocate_mining_job_token         req;
    struct sv2_allocate_mining_job_token_success ok;
    struct sv2_jd_token*                         tok = NULL;
    uint8_t                                      pbuf[512];
    uint8_t                                      outputs[64];
    uint16_t                                     outputs_len = 0;
    size_t                                       plen = 0;
    time_t                                       now = time(NULL);
    bool                                         limited = false;
    bool                                         reused = false;
    bool                                         per_min_hit, global_full;
    const char*                                  reuse_note = NULL;
    const char*                                  no_payout = NULL;

    if (!c->setup_ok || !c->full_template)
        return NULL;
    if (!sv2_decode_allocate_mining_job_token(payload, len, &req))
        return NULL;

    outputs_len = build_allocate_outputs(outputs, sizeof(outputs), req.user_identifier, &no_payout);
    if (!c->user_logged) {
        c->user_logged = true;
        LOGNOTICE(
            "SV2 JD client %" PRId64 " user identity %s%s%s", c->client_id,
            req.user_identifier[0] ? req.user_identifier : "(none)", no_payout ? ", no payout constraint: " : "",
            no_payout ? no_payout : "");
    }

    /*
     * Spec has AllocateMiningJobToken.Success only (no .Error). Never
     * silent-drop on limits — JDC waits on request_id for a Success.
     *
     * Under pressure: re-issue this client's own token (prefer undeclared;
     * else oldest declared/undeclared). Never mint past the per-min cap
     * and never free another client's declared tokens.
     */
    ensure_lock();
    mutex_lock(&jd_lock);
    expire_old_tokens_locked(now);

    if (!c->alloc_window_start || now >= c->alloc_window_start + 60) {
        c->alloc_window_start = now;
        c->alloc_count = 0;
    }
    per_min_hit = (c->alloc_count >= SV2_JD_MAX_TOKENS_PER_MIN);
    global_full = (jd_token_count >= SV2_JD_MAX_TOKENS_GLOBAL);

    if (per_min_hit || global_full) {
        struct sv2_jd_token *t, *tmp, *oldest_any = NULL;

        limited = true;
        /* 1) Newest undeclared for this client+user */
        tok = find_client_undeclared_token_locked(c->client_id, req.user_identifier);
        if (tok) {
            reused = true;
            reuse_note = "reissued undeclared token";
        } else {
            /* 2) Own oldest matching user (declared OK) */
            tok = find_client_oldest_token_locked(c->client_id, req.user_identifier);
            if (tok) {
                reused = true;
                reuse_note = tok->declared ? "reissued own declared token" : "reissued own token";
            }
        }
        if (!reused) {
            /* 3) Any own token (user_identifier changed mid-session) */
            HASH_ITER(hh, jd_tokens, t, tmp) {
                if (t->client_id != c->client_id)
                    continue;
                if (!oldest_any || t->created < oldest_any->created)
                    oldest_any = t;
            }
            if (oldest_any) {
                tok = oldest_any;
                reused = true;
                reuse_note = "reissued own token (user fallback)";
            }
        }
        if (reused) {
            /* Refresh TTL so re-handout is not near expiry. */
            tok->created = now;
            /* Outputs always match the token's bound user. */
            if (!token_user_match(tok, req.user_identifier))
                outputs_len = build_allocate_outputs(outputs, sizeof(outputs), tok->user_identifier, NULL);
            memset(&ok, 0, sizeof(ok));
            ok.request_id = req.request_id;
            ok.mining_job_token_len = tok->token_len;
            memcpy(ok.mining_job_token, tok->token, tok->token_len);
            ok.coinbase_tx_outputs = outputs_len ? outputs : NULL;
            ok.coinbase_tx_outputs_len = outputs_len;
        }
    }

    if (!reused) {
        /*
         * Mint only under per-min budget. Never free another client's
         * declared tokens; only drop global undeclared when table full.
         */
        if (per_min_hit) {
            /* No own token under per-min — cannot mint past cap. */
            mutex_unlock(&jd_lock);
            JD_STAT_INC(allocate_rate_limited);
            LOGWARNING(
                "SV2 JD AllocateMiningJobToken per-min limit client %" PRId64 " with no token to reissue",
                c->client_id);
            return NULL;
        }
        while (jd_token_count >= SV2_JD_MAX_TOKENS_GLOBAL && free_oldest_undeclared_global_locked())
            ;
        if (jd_token_count >= SV2_JD_MAX_TOKENS_GLOBAL) {
            mutex_unlock(&jd_lock);
            JD_STAT_INC(allocate_rate_limited);
            LOGWARNING(
                "SV2 JD AllocateMiningJobToken table full client %" PRId64
                " — no undeclared slot (not evicting declared)",
                c->client_id);
            return NULL;
        }
        c->alloc_count++;
        tok = ckzalloc(sizeof(*tok));
        tok->token_len = SV2_JD_TOKEN_BYTES;
        fill_random(tok->token, SV2_JD_TOKEN_BYTES);
        tok->client_id = c->client_id;
        snprintf(tok->user_identifier, sizeof(tok->user_identifier), "%s", req.user_identifier);
        tok->created = now;
        token_set_payout_from_outputs_locked(tok, outputs, outputs_len);
        HASH_ADD(hh, jd_tokens, token, SV2_JD_TOKEN_BYTES, tok);
        jd_token_count++;

        memset(&ok, 0, sizeof(ok));
        ok.request_id = req.request_id;
        ok.mining_job_token_len = tok->token_len;
        memcpy(ok.mining_job_token, tok->token, tok->token_len);
        ok.coinbase_tx_outputs = outputs_len ? outputs : NULL;
        ok.coinbase_tx_outputs_len = outputs_len;
    }
    mutex_unlock(&jd_lock);

    if (limited) {
        JD_STAT_INC(allocate_rate_limited);
        LOGNOTICE(
            "SV2 JD AllocateMiningJobToken limited client %" PRId64 " req=%u %s", c->client_id, req.request_id,
            reuse_note ? reuse_note : "handled");
    }
    if (!sv2_encode_allocate_mining_job_token_success(pbuf, sizeof(pbuf), &plen, &ok))
        return NULL;
    JD_STAT_INC(allocate_ok);
    LOGINFO(
        "SV2 JD allocated token client %" PRId64 " user=%s req=%u outputs=%u%s", c->client_id, req.user_identifier,
        req.request_id, outputs_len, reused ? " (reused)" : "");
    return reply_frame(SV2_MSG_ALLOCATE_MINING_JOB_TOKEN_SUCCESS, pbuf, plen, replylen);
}

static uint8_t* handle_declare(struct sv2_jd_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_declare_mining_job decl;
    struct sv2_jd_token*          tok;
    struct sv2_jd_pending         pend_local;
    uint16_t                      missing[SV2_MAX_JD_TXNS];
    uint16_t                      nmiss = 0;
    uint32_t                      req_id = 0;
    uint8_t                       pbuf[65536];
    size_t                        plen = 0;

    memset(&decl, 0, sizeof(decl));
    if (!c->setup_ok || !c->full_template)
        return NULL;
    if (!sv2_decode_declare_mining_job(payload, len, &decl))
        return NULL;
    req_id = decl.request_id;

    if (!decl.mining_job_token_len) {
        sv2_declare_mining_job_free(&decl);
        return error_declare(req_id, "invalid-mining-job-token", replylen);
    }
    if (!decl.coinbase_tx_prefix_len || !decl.coinbase_tx_suffix_len) {
        sv2_declare_mining_job_free(&decl);
        return error_declare(req_id, "invalid-coinbase", replylen);
    }
    if (decl.wtxid_count > SV2_MAX_JD_TXNS) {
        sv2_declare_mining_job_free(&decl);
        return error_declare(req_id, "too-many-transactions", replylen);
    }

    /* Per-client declare rate limit */
    {
        time_t now = time(NULL);
        bool   limited = false;

        ensure_lock();
        mutex_lock(&jd_lock);
        if (!c->declare_window_start || now >= c->declare_window_start + 60) {
            c->declare_window_start = now;
            c->declare_count = 0;
        }
        if (c->declare_count >= SV2_JD_MAX_DECLARES_PER_MIN)
            limited = true;
        else
            c->declare_count++;
        mutex_unlock(&jd_lock);
        if (limited) {
            sv2_declare_mining_job_free(&decl);
            LOGNOTICE("SV2 JD DeclareMiningJob rate-limited client %" PRId64, c->client_id);
            return error_declare(req_id, "rate-limited", replylen);
        }
    }

    ensure_lock();
    mutex_lock(&jd_lock);
    /* Expire only when allocating; do not drop tokens mid-declare path. */
    tok = find_token_locked(decl.mining_job_token, decl.mining_job_token_len);
    if (!tok || tok->client_id != c->client_id) {
        char     hex[33];
        unsigned n = decl.mining_job_token_len;

        if (n > 16)
            n = 16;
        __bin2hex(hex, decl.mining_job_token, n);
        LOGNOTICE(
            "SV2 JD DeclareMiningJob invalid token client %" PRId64 " req=%u tok_len=%u tok0=%s pool_tokens=%d%s",
            c->client_id, req_id, decl.mining_job_token_len, hex, jd_token_count,
            tok ? " (client_id mismatch)" : " (not found — expired or never issued)");
        mutex_unlock(&jd_lock);
        sv2_declare_mining_job_free(&decl);
        return error_declare(req_id, "invalid-mining-job-token", replylen);
    }
    /* Our rule: shared = 100% spendable to pool; solo = miner script funded. */
    if (tok->has_payout && tok->payout_script && tok->payout_script_len) {
        bool     derived = false, parse_ok = false;
        uint8_t  en_len = 0;
        uint8_t* cb = NULL;
        size_t   cb_len = 0;
        bool     funded = false;
        uint8_t* script;
        uint8_t  script_len = tok->payout_script_len;

        /* Copy payout script so we can unlock before parsing coinbase. */
        script = ckalloc(script_len);
        memcpy(script, tok->payout_script, script_len);
        mutex_unlock(&jd_lock);

        if (!derive_coinbase_enonce_len(decl.coinbase_tx_prefix, decl.coinbase_tx_prefix_len, &en_len)) {
            LOGNOTICE(
                "SV2 JD DeclareMiningJob coinbase prefix unparseable "
                "(enonce gap) client %" PRId64 " req=%u prefix_len=%u",
                c->client_id, req_id, decl.coinbase_tx_prefix_len);
        } else {
            derived = true;
            cb_len = (size_t)decl.coinbase_tx_prefix_len + en_len + decl.coinbase_tx_suffix_len;
            cb = ckalloc(cb_len);
            memcpy(cb, decl.coinbase_tx_prefix, decl.coinbase_tx_prefix_len);
            if (en_len)
                memset(cb + decl.coinbase_tx_prefix_len, 0, en_len);
            memcpy(cb + decl.coinbase_tx_prefix_len + en_len, decl.coinbase_tx_suffix, decl.coinbase_tx_suffix_len);
            funded = coinbase_tx_funds_payout(cb, cb_len, script, script_len, &parse_ok);
            dealloc(cb);
        }
        dealloc(script);
        if (!funded) {
            if (derived && !parse_ok) {
                /*
                 * The gap was derived but the assembled coinbase did
                 * not parse. derive_coinbase_enonce_len takes the
                 * entire scriptSig tail as the extranonce, so a JDC
                 * that leaves scriptSig bytes *after* the extranonce
                 * over-derives the gap and misaligns everything past
                 * it. Say that rather than blaming the payout rule,
                 * which otherwise reads as a 100% rejection rate for
                 * no visible reason.
                 */
                LOGNOTICE(
                    "SV2 JD DeclareMiningJob coinbase unparseable at "
                    "derived enonce len %u client %" PRId64
                    " req=%u — "
                    "extranonce must be the final bytes of the "
                    "coinbase scriptSig",
                    en_len, c->client_id, req_id);
            } else if (derived) {
                LOGNOTICE(
                    "SV2 JD DeclareMiningJob %s client %" PRId64 " req=%u",
                    ckpool.btcsolo ? "missing/unfunded miner payout" : "spendable payout not 100% to pool script",
                    c->client_id, req_id);
            }
            /* !derived already logged the prefix-unparseable case above. */
            sv2_declare_mining_job_free(&decl);
            /* Standard DeclareMiningJob.Error code (SRI invalid-coinbase-tx) */
            return error_declare(req_id, "invalid-coinbase-tx", replylen);
        }
    } else {
        bool no_payout = !ckpool.btcsolo;

        mutex_unlock(&jd_lock);
        /*
         * No payout constraint was advertised at Allocate, so the payout
         * rule has nothing to enforce. Solo miners own their own risk,
         * but for a shared pool this is the only thing standing between
         * a declared coinbase and reward redirection — fail closed rather
         * than open.
         * Only reachable if build_allocate_outputs() produced no script,
         * which needs a btcaddress that startup already validated.
         */
        if (unlikely(no_payout)) {
            LOGWARNING(
                "SV2 JD DeclareMiningJob no pool payout script on token "
                "client %" PRId64 " req=%u, refusing declare",
                c->client_id, req_id);
            sv2_declare_mining_job_free(&decl);
            return error_declare(req_id, "invalid-coinbase-tx", replylen);
        }
    }

    /* Steal owned buffers from decl into pending */
    memset(&pend_local, 0, sizeof(pend_local));
    pend_local.request_id = req_id;
    pend_local.client_id = c->client_id;
    pend_local.created = time(NULL);
    pend_local.token_len = decl.mining_job_token_len;
    memcpy(pend_local.token, decl.mining_job_token, decl.mining_job_token_len);
    pend_local.version = decl.version;
    pend_local.coinbase_tx_prefix = decl.coinbase_tx_prefix;
    pend_local.coinbase_tx_prefix_len = decl.coinbase_tx_prefix_len;
    pend_local.coinbase_tx_suffix = decl.coinbase_tx_suffix;
    pend_local.coinbase_tx_suffix_len = decl.coinbase_tx_suffix_len;
    pend_local.wtxid_list = decl.wtxid_list;
    pend_local.wtxid_count = decl.wtxid_count;
    decl.coinbase_tx_prefix = NULL;
    decl.coinbase_tx_suffix = NULL;
    decl.wtxid_list = NULL;
    sv2_declare_mining_job_free(&decl); /* free excess only */

    /*
     * No getrawtransaction prefetch: declare lists *wtxids*, but Core's
     * getrawtransaction is keyed by *txid*. For segwit they differ, so
     * almost every RPC misses and spams -5 "No such mempool transaction".
     * Full-Template path: ProvideMissingTransactions from the JDC, which
     * already has the raw txs from the template provider. Cache hits still
     * come from prior ProvideMissing on this tip.
     */
    ensure_lock();
    mutex_lock(&jd_lock);
    nmiss = collect_missing(pend_local.wtxid_list, pend_local.wtxid_count, missing, SV2_MAX_JD_TXNS);

    if (nmiss > 0) {
        struct sv2_provide_missing_transactions pm;

        if (!pending_may_add_locked(c->client_id)) {
            mutex_unlock(&jd_lock);
            dealloc(pend_local.coinbase_tx_prefix);
            dealloc(pend_local.coinbase_tx_suffix);
            dealloc(pend_local.wtxid_list);
            LOGNOTICE(
                "SV2 JD pending declare cap client %" PRId64 " (global=%d per_client max=%d)", c->client_id,
                SV2_JD_MAX_PENDING_GLOBAL, SV2_JD_MAX_PENDING_PER_CLIENT);
            return error_declare(req_id, "busy", replylen);
        }

        pend_local.missing = ckalloc(sizeof(uint16_t) * nmiss);
        memcpy(pend_local.missing, missing, sizeof(uint16_t) * nmiss);
        pend_local.missing_count = nmiss;
        pend_local.awaiting_missing = true;

        pending_declares = ckrealloc(pending_declares, (pending_n + 1) * sizeof(*pending_declares));
        pending_declares[pending_n++] = pend_local;
        mutex_unlock(&jd_lock);

        memset(&pm, 0, sizeof(pm));
        pm.request_id = req_id;
        pm.unknown_count = nmiss;
        pm.unknown_tx_position_list = missing;
        if (!sv2_encode_provide_missing_transactions(pbuf, sizeof(pbuf), &plen, &pm)) {
            /* Do not leave orphan pending until TTL. */
            ensure_lock();
            mutex_lock(&jd_lock);
            remove_pending_declare_locked(c->client_id, req_id);
            mutex_unlock(&jd_lock);
            return error_declare(req_id, "internal-error", replylen);
        }
        JD_STAT_INC(provide_missing);
        LOGINFO(
            "SV2 JD ProvideMissingTransactions client %" PRId64 " req=%u missing=%u (rounds=%" PRIu64 ")", c->client_id,
            req_id, nmiss, JD_STAT_GET(provide_missing));
        return reply_frame(SV2_MSG_PROVIDE_MISSING_TRANSACTIONS, pbuf, plen, replylen);
    }
    mutex_unlock(&jd_lock);

    /* All txs cached (or coinbase-only) — checkBlock async off creceiver */
    {
        uint8_t* ret = start_declare_validation(&pend_local, replylen);

        dealloc(pend_local.coinbase_tx_prefix);
        dealloc(pend_local.coinbase_tx_suffix);
        dealloc(pend_local.wtxid_list);
        return ret;
    }
}

static uint8_t*
handle_provide_missing_success(struct sv2_jd_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_provide_missing_transactions_success ok;
    struct sv2_jd_pending                           pend_copy;
    bool                                            found = false;
    uint16_t                                        i;
    int                                             pi;

    if (!c->setup_ok)
        return NULL;
    if (!sv2_decode_provide_missing_transactions_success(payload, len, &ok))
        return NULL;

    /* Cache provided txs */
    ensure_lock();
    mutex_lock(&jd_lock);
    for (i = 0; i < ok.tx_count; i++)
        cache_put_tx(ok.transactions[i], ok.tx_lens[i]);

    /* Find matching pending declare */
    for (pi = 0; pi < pending_n; pi++) {
        if (pending_declares[pi].client_id == c->client_id && pending_declares[pi].request_id == ok.request_id &&
            pending_declares[pi].awaiting_missing) {
            pend_copy = pending_declares[pi];
            /* remove from list (shallow — we own copy of pointers) */
            memmove(&pending_declares[pi], &pending_declares[pi + 1], (pending_n - pi - 1) * sizeof(*pending_declares));
            pending_n--;
            pending_shrink_locked();
            found = true;
            break;
        }
    }
    mutex_unlock(&jd_lock);
    sv2_provide_missing_tx_success_free(&ok);

    if (!found) {
        LOGINFO(
            "SV2 JD ProvideMissing success with no pending declare client %" PRId64 " req=%u", c->client_id,
            ok.request_id);
        return NULL;
    }

    /* Re-check missing under cache */
    {
        uint16_t still[SV2_MAX_JD_TXNS];
        uint16_t nstill;

        ensure_lock();
        mutex_lock(&jd_lock);
        nstill = collect_missing(pend_copy.wtxid_list, pend_copy.wtxid_count, still, SV2_MAX_JD_TXNS);
        mutex_unlock(&jd_lock);

        if (nstill > 0) {
            struct sv2_provide_missing_transactions pm;
            uint8_t                                 pbuf[65536];
            size_t                                  plen = 0;

            dealloc(pend_copy.missing);
            pend_copy.missing = ckalloc(sizeof(uint16_t) * nstill);
            memcpy(pend_copy.missing, still, sizeof(uint16_t) * nstill);
            pend_copy.missing_count = nstill;
            pend_copy.awaiting_missing = true;

            ensure_lock();
            mutex_lock(&jd_lock);
            /*
             * Re-queue after providing more txs. Cap is soft here:
             * this pending was already removed from the list, so
             * allow re-insert even at the limit (same declare).
             */
            pending_declares = ckrealloc(pending_declares, (pending_n + 1) * sizeof(*pending_declares));
            pending_declares[pending_n++] = pend_copy;
            mutex_unlock(&jd_lock);

            memset(&pm, 0, sizeof(pm));
            pm.request_id = pend_copy.request_id;
            pm.unknown_count = nstill;
            pm.unknown_tx_position_list = still;
            if (!sv2_encode_provide_missing_transactions(pbuf, sizeof(pbuf), &plen, &pm)) {
                ensure_lock();
                mutex_lock(&jd_lock);
                remove_pending_declare_locked(c->client_id, pend_copy.request_id);
                mutex_unlock(&jd_lock);
                return error_declare(pend_copy.request_id, "internal-error", replylen);
            }
            return reply_frame(SV2_MSG_PROVIDE_MISSING_TRANSACTIONS, pbuf, plen, replylen);
        }
    }

    {
        uint8_t* ret = start_declare_validation(&pend_copy, replylen);

        dealloc(pend_copy.coinbase_tx_prefix);
        dealloc(pend_copy.coinbase_tx_suffix);
        dealloc(pend_copy.wtxid_list);
        dealloc(pend_copy.missing);
        return ret;
    }
}

/*
 * Snapshot of declared-token material for solved-block rebuild. Copied under
 * jd_lock so merkle/hash work can run without holding the lock.
 */
struct rebuild_snap {
    uint32_t  version;
    uint8_t   enonce_len;
    uint8_t*  coinbase_tx_prefix;
    uint16_t  coinbase_tx_prefix_len;
    uint8_t*  coinbase_tx_suffix;
    uint16_t  coinbase_tx_suffix_len;
    uint16_t  wtxid_count;
    uint8_t** tx_raws;
    uint32_t* tx_lens;
};

static void free_rebuild_snap(struct rebuild_snap* snap) {
    if (!snap)
        return;
    dealloc(snap->coinbase_tx_prefix);
    dealloc(snap->coinbase_tx_suffix);
    free_tx_snapshot(snap->tx_raws, snap->tx_lens, snap->wtxid_count);
    memset(snap, 0, sizeof(*snap));
}

/* Caller holds jd_lock. On failure *snap is zeroed. */
static bool snapshot_token_for_rebuild_locked(struct sv2_jd_token* tok, struct rebuild_snap* snap) {
    uint16_t i;

    memset(snap, 0, sizeof(*snap));
    if (!tok || !tok->declared || !tok->coinbase_tx_prefix)
        return false;

    snap->version = tok->version;
    snap->enonce_len = tok->enonce_len;
    snap->coinbase_tx_prefix_len = tok->coinbase_tx_prefix_len;
    snap->coinbase_tx_prefix = ckalloc(tok->coinbase_tx_prefix_len ? tok->coinbase_tx_prefix_len : 1);
    if (tok->coinbase_tx_prefix_len)
        memcpy(snap->coinbase_tx_prefix, tok->coinbase_tx_prefix, tok->coinbase_tx_prefix_len);
    snap->coinbase_tx_suffix_len = tok->coinbase_tx_suffix_len;
    if (tok->coinbase_tx_suffix_len) {
        snap->coinbase_tx_suffix = ckalloc(tok->coinbase_tx_suffix_len);
        memcpy(snap->coinbase_tx_suffix, tok->coinbase_tx_suffix, tok->coinbase_tx_suffix_len);
    }
    snap->wtxid_count = tok->wtxid_count;
    if (!tok->wtxid_count)
        return true;

    snap->tx_raws = ckzalloc(sizeof(uint8_t*) * tok->wtxid_count);
    snap->tx_lens = ckzalloc(sizeof(uint32_t) * tok->wtxid_count);
    for (i = 0; i < tok->wtxid_count; i++) {
        struct sv2_tx_cache_ent* e = cache_get(tok->wtxid_list + (size_t)i * 32);

        if (!e) {
            free_rebuild_snap(snap);
            return false;
        }
        snap->tx_lens[i] = e->raw_len;
        snap->tx_raws[i] = ckalloc(e->raw_len);
        memcpy(snap->tx_raws[i], e->raw, e->raw_len);
    }
    return true;
}

/* Assemble solved block from snap (no jd_lock). Returns ckalloc'd block. */
static uint8_t* assemble_solved_from_snap(
    const struct rebuild_snap* snap,
    const uint8_t*             extranonce,
    uint8_t                    extranonce_len,
    uint32_t                   version,
    uint32_t                   ntime,
    uint32_t                   nonce,
    uint32_t                   nbits,
    const uint8_t              prev_hash[32],
    size_t*                    out_len) {
    uint8_t *coinbase, *block, *p;
    size_t   coinbase_len, body_len, total;
    uint16_t i;
    int      ntx_i;
    /* clang-format off */
    uint8_t (*txids)[32]; /* clang-format before 20 drops the space */
    /* clang-format on */
    uint8_t  merkle[32];
    uint32_t le;
    uint8_t  enonce_len;

    *out_len = 0;
    if (!snap || !snap->coinbase_tx_prefix)
        return NULL;

    /*
     * Declare coinbase gap is snap->enonce_len (full en1+en2). Callers must
     * pass that many bytes (mining path: enonce1||submit_en; PushSolution:
     * full channel extranonce). Do not silently zero-fill a shorter buffer —
     * that builds a block that cannot match the share.
     */
    if (!extranonce || !extranonce_len || (snap->enonce_len && extranonce_len != snap->enonce_len)) {
        LOGDEBUG("SV2 JD assemble_solved: enonce len %u need %u", extranonce_len, snap->enonce_len);
        return NULL;
    }
    enonce_len = extranonce_len;

    coinbase_len = snap->coinbase_tx_prefix_len + enonce_len + snap->coinbase_tx_suffix_len;
    coinbase = ckalloc(coinbase_len);
    memcpy(coinbase, snap->coinbase_tx_prefix, snap->coinbase_tx_prefix_len);
    if (enonce_len) {
        if (extranonce)
            memcpy(coinbase + snap->coinbase_tx_prefix_len, extranonce, enonce_len);
        else
            memset(coinbase + snap->coinbase_tx_prefix_len, 0, enonce_len);
    }
    memcpy(
        coinbase + snap->coinbase_tx_prefix_len + enonce_len, snap->coinbase_tx_suffix, snap->coinbase_tx_suffix_len);

    ntx_i = 1 + (int)snap->wtxid_count;
    txids = ckalloc(sizeof(*txids) * (size_t)ntx_i);
    if (!sv2_bitcoin_txid(coinbase, coinbase_len, txids[0])) {
        dealloc(coinbase);
        dealloc(txids);
        return NULL;
    }
    body_len = coinbase_len;
    for (i = 0; i < snap->wtxid_count; i++) {
        if (!sv2_bitcoin_txid(snap->tx_raws[i], snap->tx_lens[i], txids[1 + i])) {
            dealloc(coinbase);
            dealloc(txids);
            return NULL;
        }
        body_len += snap->tx_lens[i];
    }
    sv2_merkle_root_from_txids(txids, ntx_i, merkle);
    dealloc(txids);

    total = 80 + sv2_compact_size_len((uint64_t)ntx_i) + body_len;
    if (total > SV2_JD_MAX_BLOCK_BYTES) {
        dealloc(coinbase);
        return NULL;
    }
    block = ckalloc(total);
    p = block;
    le = htole32(version ? version : snap->version);
    memcpy(p, &le, 4);
    p += 4;
    memcpy(p, prev_hash, 32);
    p += 32;
    memcpy(p, merkle, 32);
    p += 32;
    le = htole32(ntime);
    memcpy(p, &le, 4);
    p += 4;
    le = htole32(nbits);
    memcpy(p, &le, 4);
    p += 4;
    le = htole32(nonce);
    memcpy(p, &le, 4);
    p += 4;
    sv2_write_compact_size(&p, (uint64_t)ntx_i);
    memcpy(p, coinbase, coinbase_len);
    p += coinbase_len;
    dealloc(coinbase);
    for (i = 0; i < snap->wtxid_count; i++) {
        memcpy(p, snap->tx_raws[i], snap->tx_lens[i]);
        p += snap->tx_lens[i];
    }
    *out_len = (size_t)(p - block);
    return block;
}

bool sv2_jd_rebuild_solved_block(
    const uint8_t* token,
    uint8_t        token_len,
    const uint8_t* extranonce,
    uint8_t        extranonce_len,
    uint32_t       version,
    uint32_t       ntime,
    uint32_t       nonce,
    uint32_t       nbits,
    const uint8_t  prev_hash[32],
    uint8_t**      block_out,
    size_t*        block_len) {
    struct sv2_jd_token* tok;
    struct rebuild_snap  snap;
    uint8_t*             block = NULL;
    size_t               blen = 0;
    bool                 have = false;

    if (!token || !token_len || !block_out || !block_len || !prev_hash)
        return false;
    *block_out = NULL;
    *block_len = 0;

    ensure_lock();
    mutex_lock(&jd_lock);
    tok = find_token_locked(token, token_len);
    if (tok)
        have = snapshot_token_for_rebuild_locked(tok, &snap);
    mutex_unlock(&jd_lock);

    if (!have)
        return false;

    block =
        assemble_solved_from_snap(&snap, extranonce, extranonce_len, version, ntime, nonce, nbits, prev_hash, &blen);
    free_rebuild_snap(&snap);
    if (!block)
        return false;
    *block_out = block;
    *block_len = blen;
    return true;
}

/*
 * True if wire header meets nbits compact target (wrong-template rebuilds fail).
 * nbits is the uint32 compact value as used on the SV2 wire / assemble path.
 */
static bool header_meets_nbits(const uint8_t* header80, uint32_t nbits) {
    uint8_t hash[32];
    uint8_t compact[4];
    double  sdiff, ndiff;

    if (!header80)
        return false;
    gen_hash((uchar*)header80, hash, 80);
    sdiff = diff_from_target(hash);
    /* diff_from_nbits wants BE compact bytes: size | mantissa */
    compact[0] = (uint8_t)((nbits >> 24) & 0xff);
    compact[1] = (uint8_t)((nbits >> 16) & 0xff);
    compact[2] = (uint8_t)((nbits >> 8) & 0xff);
    compact[3] = (uint8_t)(nbits & 0xff);
    ndiff = diff_from_nbits((char*)compact);
    return sdiff + 1e-9 >= ndiff;
}

static uint8_t* handle_push_solution(struct sv2_jd_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_push_solution sol;
    struct sv2_jd_token *    tok, *tmp;
    struct sv2_jd_token*     cands[SV2_JD_PUSH_TOKEN_CANDIDATES];
    int                      ncands = 0, i, j;
    uint8_t*                 block = NULL;
    size_t                   blen = 0;
    bool                     solved = false;
    char                     who[SV2_MAX_STR_LEN + 1];

    *replylen = 0;
    who[0] = '\0';
    if (!c->setup_ok)
        return NULL;
    if (!sv2_decode_push_solution(payload, len, &sol))
        return NULL;
    /* Count on arrival, before hand-off, so the submit worker's log of the
     * running total cannot race ahead of the message it belongs to. */
    JD_STAT_INC(push_solution);

    /*
     * PushSolution carries no token. Snapshot several recent declared
     * tokens and pick the rebuild whose header meets nbits (correct
     * merkle/PoW). Wrong-template rebuilds fail the PoW check.
     */
    ensure_lock();
    mutex_lock(&jd_lock);
    HASH_ITER(hh, jd_tokens, tok, tmp) {
        if (tok->client_id != c->client_id || !tok->declared || !tok->coinbase_tx_prefix)
            continue;
        /* Insert sorted by created descending (most recent first). */
        if (ncands < SV2_JD_PUSH_TOKEN_CANDIDATES) {
            cands[ncands++] = tok;
        } else if (tok->created > cands[ncands - 1]->created) {
            cands[ncands - 1] = tok;
        } else
            continue;
        for (i = ncands - 1; i > 0; i--) {
            if (cands[i]->created <= cands[i - 1]->created)
                break;
            tok = cands[i];
            cands[i] = cands[i - 1];
            cands[i - 1] = tok;
        }
    }
    /* Copy snaps under lock so tokens cannot vanish mid-rebuild. */
    {
        struct rebuild_snap snaps[SV2_JD_PUSH_TOKEN_CANDIDATES];
        int                 nsnaps = 0;
        char                whos[SV2_JD_PUSH_TOKEN_CANDIDATES][SV2_MAX_STR_LEN + 1];

        uint32_t pow_nbits = sol.nbits;
        uint32_t t_ver, t_ntime, t_nbits;
        uint8_t  t_prev[32];

        /*
         * Gate the proof of work on our own tip's nbits, not the value
         * the client sent: sol.nbits is attacker chosen, and an easy one
         * makes any non-solution pass this check and be submitted to
         * bitcoind for free. nbits only moves on a retarget boundary so
         * ours is stable across ordinary tip changes. Fall back to the
         * client's value only if we somehow have no tip to compare with.
         */
        if (stratifier_sv2_tip_for_jd(&t_ver, &t_ntime, &t_nbits, t_prev)) {
            if (unlikely(t_nbits != sol.nbits)) {
                LOGNOTICE(
                    "SV2 JD PushSolution client %" PRId64
                    " nbits %08x "
                    "differs from pool tip %08x, testing against tip",
                    c->client_id, sol.nbits, t_nbits);
            }
            pow_nbits = t_nbits;
        }

        memset(snaps, 0, sizeof(snaps));
        for (i = 0; i < ncands; i++) {
            if (!snapshot_token_for_rebuild_locked(cands[i], &snaps[nsnaps]))
                continue;
            whos[nsnaps][0] = '\0';
            if (cands[i]->user_identifier[0])
                snprintf(whos[nsnaps], sizeof(whos[nsnaps]), "%s", cands[i]->user_identifier);
            nsnaps++;
        }
        mutex_unlock(&jd_lock);

        for (i = 0; i < nsnaps; i++) {
            dealloc(block);
            block = assemble_solved_from_snap(
                &snaps[i], sol.extranonce, sol.extranonce_len, sol.version, sol.ntime, sol.nonce, sol.nbits,
                sol.prev_hash, &blen);
            if (!block || !header_meets_nbits(block, pow_nbits))
                continue;
            if (whos[i][0])
                snprintf(who, sizeof(who), "%s", whos[i]);
            /*
             * Hand off rather than submit here: this runs on the
             * connector's single receive thread and submitblock is
             * synchronous. Only a block that already met nbits above
             * can reach this, so the queue cannot be flooded.
             */
            pthread_once(&jd_submit_once, jd_submit_q_init);
            if (likely(jd_submit_q)) {
                struct jd_submit_job* job = ckzalloc(sizeof(*job));

                job->block = block;
                job->blen = blen;
                job->client_id = c->client_id;
                snprintf(job->who, sizeof(job->who), "%s", who);
                block = NULL; /* owned by the submit worker */
                ckmsgq_add(jd_submit_q, job);
            } else {
                /* Never lose a solved block to a missing queue. */
                LOGWARNING("SV2 JD no submit queue, submitting inline");
                stratifier_sv2_submit_block_bin(block, blen, 0, who[0] ? who : NULL);
            }
            solved = true;
            break;
        }
        for (j = 0; j < nsnaps; j++)
            free_rebuild_snap(&snaps[j]);
    }

    if (!solved) {
        LOGWARNING("SV2 JD PushSolution: no matching declared template client %" PRId64, c->client_id);
    } else {
        /* Outcome is logged by the submit worker once bitcoind replies. */
        LOGNOTICE(
            "SV2 JD PushSolution client %" PRId64
            " block %zu bytes queued "
            "for submit",
            c->client_id, blen);
    }
    dealloc(block);
    return NULL; /* no response message in protocol */
}

bool sv2_jd_enabled(void) {
    return ckpool.sv2jdurls > 0;
}

int sv2_jd_client_count(void) {
    int n;

    ensure_lock();
    mutex_lock(&jd_lock);
    n = (int)HASH_COUNT(jd_clients);
    mutex_unlock(&jd_lock);
    return n;
}

void sv2_jd_get_stats(struct sv2_jd_stats* out) {
    if (!out)
        return;
    memset(out, 0, sizeof(*out));
    out->allocate_ok = JD_STAT_GET(allocate_ok);
    out->allocate_rate_limited = JD_STAT_GET(allocate_rate_limited);
    out->declare_ok = JD_STAT_GET(declare_ok);
    out->declare_error = JD_STAT_GET(declare_error);
    out->declare_rate_limited = JD_STAT_GET(declare_rate_limited);
    out->checkblock_busy = JD_STAT_GET(checkblock_busy);
    out->checkblock_fail = JD_STAT_GET(checkblock_fail);
    out->coalesce_hit = JD_STAT_GET(coalesce_hit);
    out->provide_missing = JD_STAT_GET(provide_missing);
    out->push_solution = JD_STAT_GET(push_solution);

    ensure_lock();
    mutex_lock(&jd_lock);
    out->tokens = jd_token_count;
    out->tx_cache = tx_cache_count;
    out->tx_cache_bytes = tx_cache_bytes;
    out->pending_declares = pending_n;
    mutex_unlock(&jd_lock);
    mutex_lock(&jd_cb_lock);
    out->checkblock_inflight = jd_checkblock_inflight;
    mutex_unlock(&jd_cb_lock);
}

char* sv2_jd_stats_json(void) {
    struct sv2_jd_stats st;
    char*               s;

    if (!sv2_jd_enabled())
        return NULL;
    sv2_jd_get_stats(&st);
    /* Single JSON object line for pool.status */
    ASPRINTF(
        &s,
        "{\"sv2jd\":true,"
        "\"allocate_ok\":%" PRIu64 ",\"allocate_rate_limited\":%" PRIu64
        ","
        "\"declare_ok\":%" PRIu64 ",\"declare_error\":%" PRIu64
        ","
        "\"declare_rate_limited\":%" PRIu64
        ","
        "\"checkblock_busy\":%" PRIu64 ",\"checkblock_fail\":%" PRIu64
        ","
        "\"checkblock_inflight\":%d,"
        "\"coalesce_hit\":%" PRIu64 ",\"provide_missing\":%" PRIu64
        ","
        "\"push_solution\":%" PRIu64
        ","
        "\"tokens\":%d,\"tx_cache\":%d,\"tx_cache_bytes\":%" PRIu64
        ","
        "\"pending_declares\":%d}",
        st.allocate_ok, st.allocate_rate_limited, st.declare_ok, st.declare_error, st.declare_rate_limited,
        st.checkblock_busy, st.checkblock_fail, st.checkblock_inflight, st.coalesce_hit, st.provide_missing,
        st.push_solution, st.tokens, st.tx_cache, st.tx_cache_bytes, st.pending_declares);
    return s;
}

bool sv2_jd_token_is_declared(const uint8_t* token, uint8_t token_len) {
    struct sv2_jd_token* tok;
    bool                 ok = false;

    if (!token || !token_len)
        return false;
    ensure_lock();
    mutex_lock(&jd_lock);
    tok = find_token_locked(token, token_len);
    if (tok && tok->declared)
        ok = true;
    mutex_unlock(&jd_lock);
    return ok;
}

bool sv2_jd_token_outputs_fund_payout(
    const uint8_t* token,
    uint8_t        token_len,
    const uint8_t* outputs,
    uint16_t       outputs_len) {
    struct sv2_jd_token* tok;
    uint8_t*             script = NULL;
    uint8_t              script_len = 0;
    bool                 ok;

    if (!token || !token_len)
        return false;
    ensure_lock();
    mutex_lock(&jd_lock);
    tok = find_token_locked(token, token_len);
    if (!tok || !tok->declared) {
        mutex_unlock(&jd_lock);
        return false;
    }
    if (!tok->has_payout || !tok->payout_script || !tok->payout_script_len) {
        /*
         * No pool payout constraint advertised at Allocate. Solo miners
         * own their own payout risk, but a shared pool has nothing left
         * to stop these outputs paying somebody else — fail closed.
         */
        mutex_unlock(&jd_lock);
        if (unlikely(!ckpool.btcsolo)) {
            LOGWARNING(
                "SV2 JD SetCustomMiningJob no pool payout script on "
                "token, refusing custom job");
            return false;
        }
        return true;
    }
    script_len = tok->payout_script_len;
    script = ckalloc(script_len);
    memcpy(script, tok->payout_script, script_len);
    mutex_unlock(&jd_lock);

    ok = outputs_blob_funds_payout(outputs, outputs_len, script, script_len);
    dealloc(script);
    return ok;
}

uint8_t* sv2_jd_handle_frame(int64_t client_id, const uint8_t* frame, size_t framelen, size_t* replylen) {
    struct sv2_frame      fr;
    struct sv2_jd_client* c;
    const uint8_t*        payload;
    uint32_t              plen;
    uint8_t*              ret = NULL;

    *replylen = 0;
    if (!frame || framelen < SV2_FRAME_HEADER_LEN)
        return NULL;
    if (!sv2_decode_header(frame, framelen, &fr))
        return NULL;
    if (framelen < SV2_FRAME_HEADER_LEN + fr.msg_length)
        return NULL;
    /* Spec §3.4: unknown extension_type MUST be discarded and ignored */
    if (fr.extension_type & SV2_EXTENSION_MASK) {
        LOGDEBUG(
            "SV2 JD client %" PRId64 " ignoring extension 0x%04x msg 0x%02x", client_id, fr.extension_type,
            fr.msg_type);
        return NULL;
    }
    /* Policy cap below U24 max — large frames pin 2× payload on the conn. */
    if (fr.msg_length > SV2_MAX_JD_PAYLOAD) {
        LOGNOTICE(
            "SV2 JD client %" PRId64
            " frame type 0x%02x payload %u > "
            "JD cap %u — dropping frame",
            client_id, fr.msg_type, fr.msg_length, SV2_MAX_JD_PAYLOAD);
        return NULL;
    }
    payload = frame + SV2_FRAME_HEADER_LEN;
    plen = fr.msg_length;

    /* Ref held for entire handler so sender-thread drop cannot free c. */
    c = client_get(client_id, true);
    if (!c)
        return NULL;

    switch (fr.msg_type) {
        case SV2_MSG_SETUP_CONNECTION:
            ret = handle_setup(c, payload, plen, replylen);
            break;
        case SV2_MSG_ALLOCATE_MINING_JOB_TOKEN:
            ret = handle_allocate(c, payload, plen, replylen);
            break;
        case SV2_MSG_DECLARE_MINING_JOB:
            ret = handle_declare(c, payload, plen, replylen);
            break;
        case SV2_MSG_PROVIDE_MISSING_TRANSACTIONS_SUCCESS:
            ret = handle_provide_missing_success(c, payload, plen, replylen);
            break;
        case SV2_MSG_PUSH_SOLUTION:
            ret = handle_push_solution(c, payload, plen, replylen);
            break;
        default:
            LOGDEBUG("SV2 JD client %" PRId64 " msg 0x%02x", client_id, fr.msg_type);
            break;
    }
    client_put(c);
    return ret;
}

#endif /* HAVE_SV2 */
