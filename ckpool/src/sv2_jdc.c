/*
 * Copyright 2026 Con Kolivas
 *
 * SV2 Job Declaration Client (JDC) — ckproxy side.
 *
 * Local block templates from the Bitcoin Core mining IPC:
 *   waitTipChanged (tip thread)  → queue a build
 *   createNewBlock               → header / coinbase / merkle path
 *   getBlock                     → the transaction set, split in place
 * yielding a struct sv2_jdc_template with everything a Full-Template
 * DeclareMiningJob and SetCustomMiningJob need.
 *
 * Those templates are declared to the pool's JDS over a second Noise session:
 * AllocateMiningJobToken for a token and the pool's payout script,
 * DeclareMiningJob per template, ProvideMissingTransactions answered from the
 * template's own transaction bytes. An accepted declare is handed to the
 * generator as a SetCustomMiningJob (it owns the mining connection); once the
 * pool acknowledges it, that job carries downstream work and shares, and a share
 * meeting the network target comes back here to be pushed upstream and submitted
 * to our own node.
 *
 * Threading: the tip waiter owns its own (thread affine) IPC context, exactly
 * as the pool's ipcnotify does. Template generation and declares run on the
 * jdctmpl ckmsgq; the JD session has its own recv thread. Neither ever runs on
 * the generator's mining receive path, so a createNewBlock, a getBlock or a
 * declare round trip cannot sit in front of jobs or shares.
 */

#include "config.h"

#ifdef HAVE_SV2

#include <string.h>
#include <time.h>
#include <unistd.h>

#include "ckpool.h"
#include "libckpool.h"
#include "sv2_cb.h"
#include "sv2_codec.h"
#include "sv2_jdc.h"
#include "sv2_noise.h"
#include "sv2_tx.h"
#include "sv2_types.h"

#ifdef HAVE_CAPNP
#include "mining_ipc.h"
#endif

bool sv2_jdc_configured(void) {
    int i;

    if (!ckpool.proxy || !ckpool.proxyjds)
        return false;
    for (i = 0; i < ckpool.proxies; i++) {
        if (ckpool.proxyjds[i])
            return true;
    }
    return false;
}

#ifdef HAVE_CAPNP

/* Template service connection, owned here rather than in ckpool.btc_template_svc:
 * that field belongs to the pool-mode stratifier path and proxy mode never
 * starts it. */
static mining_ipc_service* jdc_svc;
static ckmsgq_t*           jdc_buildq;
/* Published by the tip thread while it holds a connection, cleared once it has
 * disconnected, so sv2_jdc_stop() can wait for an orderly teardown. */
static mining_ipc_ctx* jdc_tip_ctx;

static mutex_t                  jdc_lock;
static pthread_once_t           jdc_lock_once = PTHREAD_ONCE_INIT;
static struct sv2_jdc_template* jdc_current;
static struct sv2_jdc_template* jdc_previous;
static uint64_t                 jdc_template_id;
static bool                     jdc_build_queued;
static time_t                   jdc_last_build;
/*
 * Tips we have built a template on, under jdc_lock. The generator classifies the
 * pool's SetNewPrevHash against this to run the work-source state machine,
 * and a tip only enters it once a template exists for it —
 * which is exactly the condition for being able to declare on it.
 */
static struct sv2_tip_ring jdc_tips;

/* Reason a build was queued, for the log line. */
struct jdc_build {
    bool tip_change;
    /* Not a build at all: the pool reached our tip, so the current template
     * became declarable without anything about it changing. */
    bool declare_only;
};

/*
 * The Job Declaration session: a second Noise connection alongside the
 * generator's mining one, owned by its own recv thread.
 *
 * Locking: send_lock serialises the outbound Noise CipherState (the recv thread
 * replies to ProvideMissingTransactions while the template thread sends
 * declares). jdc_lock covers the session state the two threads share — ready,
 * the token, and the pending declare.
 */
struct jdc_token {
    uint8_t token[SV2_MAX_JOB_TOKEN];
    uint8_t token_len;
    /* Pool payout outputs from AllocateMiningJobToken.Success, copied whole:
     * the decode aliases the frame, which is freed as soon as it is
     * dispatched. Kept in wire form because that is what the coinbase needs
     * back, every output of it and not just the one the reward lands in. */
    uint8_t  payouts[SV2_MAX_PAYOUT_BLOB];
    uint16_t payouts_len;
    bool     declared; /* a declare on this token succeeded */
};

/*
 * Everything an accepted declare needs to become a SetCustomMiningJob, built
 * once in sess_declare() so the bytes the JDS validated and the bytes the pool's
 * share validation reconstructs cannot diverge.
 *
 * dcb_* is the declared coinbase split (witness serialised when the template has
 * a commitment); lcb_* is the same transaction in legacy/txid form, which is
 * what downstream miners hash and what SetCustomMiningJob describes. Owned by
 * the pending declare; stolen by whoever consumes or replaces it.
 */
struct jdc_material {
    /* Reference held so the transactions stay alive for a
     * ProvideMissingTransactions reply, and the template (with its IPC handle)
     * for a block submit. Published under jdc_lock before the declare goes
     * out: the JDS can ask within a millisecond, on the session thread, while
     * the declare is still being sent from the template one. */
    struct sv2_jdc_template* t;
    uint8_t                  token[SV2_MAX_JOB_TOKEN];
    uint8_t                  token_len;
    uint8_t*                 outputs;
    uint16_t                 outputs_len;
    uint8_t *                dcb_prefix, *dcb_suffix;
    uint16_t                 dcb_prefix_len, dcb_suffix_len;
    uint8_t *                lcb_prefix, *lcb_suffix;
    uint16_t                 lcb_prefix_len, lcb_suffix_len;
    uint16_t                 hole_len;
};

/* A declare awaiting Success / Error / ProvideMissingTransactions. */
struct jdc_pending {
    bool                live;
    uint32_t            request_id;
    time_t              sent;
    struct jdc_material m;
};

static struct {
    int     proxy_id; /* proxy entry this session belongs to */
    char*   host;
    char*   port;
    char    user[SV2_MAX_STR_LEN + 1];
    uint8_t authority[32];

    int                  fd;
    sv2_noise_session_t* noise;
    mutex_t              send_lock;
    bool                 ready; /* SetupConnection.Success seen */
    uint32_t             next_request_id;

    uint8_t* rx;
    size_t   rx_len, rx_cap;
    time_t   last_rx;     /* last frame received */
    bool     tx_since_rx; /* sent something and had nothing back */

    bool               have_token;
    struct jdc_token   token;
    time_t             token_asked;
    struct jdc_pending pending;
    /*
     * Declare bookkeeping, all under jdc_lock.
     *
     * declared_id is the template whose work miners are actually on — never
     * worth declaring again. attempt_id is the template last tried: an attempt
     * that ended without live custom work stays eligible for another go once
     * retry_after passes, because otherwise a timeout or a transient rejection
     * would stall job declaration until the next template appeared, which on a
     * quiet tip is a whole refresh interval away.
     *
     * An accepted declare is only half the job, so it does not retire the
     * template either: it becomes custom_id with custom_live set, and a
     * SetCustomMiningJob the pool never accepts hands the template back to the
     * retry policy. custom_live suppresses only a re-declare of custom_id — a
     * newer template must not wait on it.
     */
    uint64_t declared_id;
    uint64_t attempt_id;
    time_t   retry_after;
    int      attempt_fails;
    bool     attempt_done; /* stop retrying attempt_id */
    bool     custom_live;  /* SetCustomMiningJob out on custom_id */
    uint64_t custom_id;
    time_t   custom_at; /* when it went out, for the backstop below */

    /*
     * The pool's tip, as its mining connection last announced it, pushed here
     * by the generator rather than read across from the connection: this is
     * the thread that decides whether to declare, and a torn read of a 32 byte
     * hash would declare against a tip that never existed.
     *
     * A declare is only legal on the pool's own tip — the JDS assembles
     * its candidate block on that prevhash and SetCustomMiningJob is checked
     * against it — so this gates the declare path, and nodeclare_id keeps the
     * "waiting for the pool" log to once per template.
     */
    bool     have_pool_tip;
    uint8_t  pool_prev[32];
    tv_t     pool_tip_tv; /* when that announcement arrived */
    uint64_t nodeclare_id;

    /*
     * Our own node's tip, stamped where the tip waiter observes it rather
     * than where the template built on it lands: a build is a full IPC round
     * trip, and folding it in would report propagation we did not lose.
     * Paired with pool_prev above only to time the race between the two.
     */
    bool    have_local_tip;
    uint8_t local_prev[32];
    tv_t    local_tip_tv;

    uint64_t declares, declare_ok, declare_err, provide_missing;
    uint64_t customs, custom_ok, custom_err, solutions;
} sess;

static void jdc_lock_init_once(void) {
    mutex_init(&jdc_lock);
}

static void jdc_declare_template(struct sv2_jdc_template* t);
static void jdc_declare_retry(void);
static bool may_declare_locked(uint64_t id, time_t now);
static void declare_failed_locked(int delay, bool done);
static void custom_failed_locked(int delay);

/* The store may be queried before (or without) sv2_jdc_start(). */
static void ensure_lock(void) {
    pthread_once(&jdc_lock_once, jdc_lock_init_once);
}

static void template_free(struct sv2_jdc_template* t) {
    if (!t)
        return;
    if (t->tmpl)
        mining_block_template_destroy(t->tmpl);
    mining_ipc_block_free(t->block);
    dealloc(t->txn);
    dealloc(t);
}

void sv2_jdc_template_put(struct sv2_jdc_template* t) {
    bool last;

    if (!t)
        return;
    ensure_lock();
    mutex_lock(&jdc_lock);
    last = (--t->refcount < 1);
    mutex_unlock(&jdc_lock);
    if (last)
        template_free(t);
}

struct sv2_jdc_template* sv2_jdc_template_ref(struct sv2_jdc_template* t) {
    if (!t)
        return NULL;
    ensure_lock();
    mutex_lock(&jdc_lock);
    t->refcount++;
    mutex_unlock(&jdc_lock);
    return t;
}

struct sv2_jdc_template* sv2_jdc_template_current(void) {
    struct sv2_jdc_template* t;

    ensure_lock();
    mutex_lock(&jdc_lock);
    t = jdc_current;
    if (t)
        t->refcount++;
    mutex_unlock(&jdc_lock);
    return t;
}

/*
 * Release declare material. Never called with jdc_lock held: dropping the
 * template reference takes it. Callers steal the material out of the pending
 * declare under the lock and clear their copy afterwards.
 */
static void material_clear(struct jdc_material* m) {
    dealloc(m->outputs);
    dealloc(m->dcb_prefix);
    dealloc(m->dcb_suffix);
    dealloc(m->lcb_prefix);
    dealloc(m->lcb_suffix);
    if (m->t) {
        sv2_jdc_template_put(m->t);
        m->t = NULL;
    }
    memset(m, 0, sizeof(*m));
}

/*
 * Publish a freshly built template. The store keeps the current and the
 * previous one so a share or solve arriving just after a fee bump still finds
 * its material; anything needing a longer guarantee (a custom job live in the
 * proxy job ring) holds its own reference.
 */
static void template_store(struct sv2_jdc_template* t) {
    struct sv2_jdc_template* drop;

    ensure_lock();
    mutex_lock(&jdc_lock);
    t->id = ++jdc_template_id;
    t->refcount = 1; /* the store's own reference */
    drop = jdc_previous;
    jdc_previous = jdc_current;
    jdc_current = t;
    /* A fee bump on an unchanged tip is not a new tip; sv2_tip_push knows. */
    sv2_tip_push(&jdc_tips, t->prev_hash, t->nbits, t->ntime);
    mutex_unlock(&jdc_lock);

    if (drop)
        sv2_jdc_template_put(drop);
}

/* Hex of a 32 byte hash in display (reversed) order, for logs. */
static void hash_to_display(const uint8_t hash[32], char out[65]) {
    uint8_t rev[32];
    int     i;

    for (i = 0; i < 32; i++)
        rev[i] = hash[31 - i];
    __bin2hex(out, rev, 32);
}

/*
 * mining_tip.hash is the IPC BlockRef bytes hexed in place, i.e. internal
 * order — the reverse of the hash bitcoin-cli reports. Flip it so JDC log
 * lines are comparable with the node and with each other.
 */
static void tip_to_display(const char* tip_hex, char out[65]) {
    uint8_t bin[32];

    if (strlen(tip_hex) != 64 || !hex2bin(bin, tip_hex, 32)) {
        snprintf(out, 65, "%s", tip_hex);
        return;
    }
    hash_to_display(bin, out);
}

/*
 * How our node's view of a tip and the pool's raced, one line per tip. Called by
 * whichever of the two events lands second, so each tip reports exactly once and
 * neither side has to know which it was.
 *
 * ms is always local minus pool, whichever way round it was measured: positive
 * means the pool announced the tip before our node reached it, which is the
 * window a POOL_BRIDGE covers, and negative means our node got there first,
 * which is what a well connected one should normally show.
 */
static void log_tip_latency(const uint8_t prev[32], const int ms) {
    char tiphex[65];

    hash_to_display(prev, tiphex);
    if (ms >= 0) {
        LOGNOTICE(
            "JDC tip %s: pool announced it %dms before our node reached it "
            "(%+dms)",
            tiphex, ms, ms);
    } else {
        LOGNOTICE(
            "JDC tip %s: our node reached it %dms before the pool announced "
            "it (%+dms)",
            tiphex, -ms, ms);
    }
}

/*
 * A required coinbase output is the segwit commitment when its scriptPubKey is
 * OP_RETURN <push 36> aa21a9ed <32 byte hash>, i.e. the serialised output runs
 * value(8) scriptlen(1) 6a 24 aa 21 a9 ed <hash>. Same test build_ipc_workbase()
 * applies; anything else cannot be represented in a coinbase we build.
 */
static bool required_output_is_commitment(const uint8_t* ro, size_t rl) {
    return rl >= 11 + 36 && ro[9] == 0x6a && ro[10] == 0x24 && ro[11] == 0xaa && ro[12] == 0x21 && ro[13] == 0xa9 &&
           ro[14] == 0xed;
}

/* Copy the coinbase constraints out of an IPC template. False if the template
 * demands something a declared coinbase cannot express. */
static bool template_take_coinbase(struct sv2_jdc_template* t, const mining_coinbase* cb) {
    int i;

    if (cb->script_sig_prefix_len > sizeof(t->script_sig_prefix)) {
        LOGWARNING("JDC template script_sig_prefix %zu > %zu", cb->script_sig_prefix_len, sizeof(t->script_sig_prefix));
        return false;
    }
    t->cb_version = cb->version;
    t->cb_sequence = cb->sequence;
    t->cb_locktime = cb->lock_time;
    t->reward = cb->block_reward_remaining;
    memcpy(t->script_sig_prefix, cb->script_sig_prefix, cb->script_sig_prefix_len);
    t->script_sig_prefix_len = (uint8_t)cb->script_sig_prefix_len;
    /* BIP54: lock_time carries height - 1. */
    t->height = (int)cb->lock_time + 1;

    for (i = 0; i < cb->required_outputs_count; i++) {
        const uint8_t* ro = cb->required_output[i];
        size_t         rl = cb->required_output_len[i];

        if (!required_output_is_commitment(ro, rl)) {
            LOGWARNING(
                "JDC template requires an unrepresentable coinbase output "
                "(%zu bytes) — cannot declare this template",
                rl);
            return false;
        }
        if (t->have_commitment) {
            LOGWARNING("JDC template has multiple witness commitments — cannot declare");
            return false;
        }
        if (rl > sizeof(t->commitment)) {
            LOGWARNING("JDC template commitment output %zu > %zu", rl, sizeof(t->commitment));
            return false;
        }
        memcpy(t->commitment, ro, rl);
        t->commitment_len = (uint8_t)rl;
        t->have_commitment = true;
    }
    /*
     * The commitment was computed over this reserved value, so the coinbase
     * witness must carry it verbatim or the commitment no longer matches and
     * checkBlock rejects the declare. Without a commitment there is nothing
     * to commit to and the coinbase must be serialised legacy instead
     * (BIP141) — mirroring ipc_submit_block() in the stratifier.
     */
    if (t->have_commitment) {
        if (cb->witness_len != sizeof(t->witness_reserved)) {
            LOGWARNING(
                "JDC template witness reserved value is %zu bytes not %zu", cb->witness_len,
                sizeof(t->witness_reserved));
            return false;
        }
        memcpy(t->witness_reserved, cb->witness, sizeof(t->witness_reserved));
    }
    return true;
}

/*
 * Split the serialised block into its transactions, filling t->txn with
 * pointers into t->block (no per-transaction copies) plus each txid/wtxid.
 * vtx[0] is the node's own coinbase: it is not declared, but its txid is
 * returned so the merkle path can be checked against the header.
 */
static bool template_split_block(struct sv2_jdc_template* t, uint8_t cb_txid[32]) {
    const uint8_t *p = t->block + 80, *end = t->block + t->blocklen;
    uint64_t       ntx = 0;
    size_t         txlen = 0;
    int            i;

    if (!sv2_read_compact_size(&p, end, &ntx) || ntx < 1) {
        LOGWARNING("JDC template block has no transaction count");
        return false;
    }
    if (ntx - 1 > SV2_MAX_JD_TXNS) {
        LOGWARNING("JDC template has %" PRIu64 " transactions, over the %u declare limit", ntx - 1, SV2_MAX_JD_TXNS);
        return false;
    }
    if (!sv2_bitcoin_tx_parse(p, (size_t)(end - p), &txlen, cb_txid, NULL)) {
        LOGWARNING("JDC template coinbase transaction unparseable");
        return false;
    }
    p += txlen;

    t->txns = (int)(ntx - 1);
    t->txn = t->txns ? ckzalloc(sizeof(*t->txn) * t->txns) : NULL;
    for (i = 0; i < t->txns; i++) {
        struct sv2_jdc_tx* tx = &t->txn[i];

        if (!sv2_bitcoin_tx_parse(p, (size_t)(end - p), &txlen, tx->txid, tx->wtxid)) {
            LOGWARNING("JDC template transaction %d of %d unparseable", i, t->txns);
            return false;
        }
        tx->raw = p;
        tx->len = (uint32_t)txlen;
        t->txn_bytes += txlen;
        p += txlen;
    }
    if (p != end) {
        LOGWARNING("JDC template block has %zd trailing bytes after %d transactions", (ssize_t)(end - p), t->txns);
        return false;
    }
    return true;
}

/*
 * Cross-check the IPC merkle path against the transaction set: fold the node's
 * own coinbase txid up the path, and independently build the root from every
 * txid in the block. They must agree, which proves the path, the transaction
 * split and the txid byte order against each other — so a mis-parse cannot
 * reach a declare, where it would only surface as a checkBlock rejection.
 *
 * The header's own merkle root cannot be used for this: a template header
 * carries no root, because the coinbase is not final until we build it (Core
 * leaves the field zeroed, and build_ipc_workbase() likewise never reads it).
 */
static bool template_check_merkle(const struct sv2_jdc_template* t, const uint8_t cb_txid[32]) {
    uint8_t path_root[32], tree_root[32];
    /* clang-format off */
    uint8_t (*txids)[32]; /* clang-format before 20 drops the space */
    /* clang-format on */
    char a[65], b[65];
    int  i;

    sv2_merkle_root_from_path(cb_txid, t->merkle_path, t->merkles, path_root);

    txids = ckalloc(sizeof(*txids) * (size_t)(t->txns + 1));
    memcpy(txids[0], cb_txid, 32);
    for (i = 0; i < t->txns; i++)
        memcpy(txids[1 + i], t->txn[i].txid, 32);
    sv2_merkle_root_from_txids(txids, t->txns + 1, tree_root);
    dealloc(txids);

    if (!memcmp(path_root, tree_root, 32))
        return true;

    hash_to_display(path_root, a);
    hash_to_display(tree_root, b);
    LOGWARNING(
        "JDC template merkle path and transaction set disagree: path %s "
        "tree %s (%d steps, %d txns)",
        a, b, t->merkles, t->txns);
    return false;
}

/* Build one template from the IPC service. NULL on any failure; the caller
 * simply keeps the previous template and tries again on the next wake. */
static struct sv2_jdc_template* template_build(void) {
    unsigned char            branch[MINING_MAX_MERKLES][32];
    mining_block_template*   tmpl = NULL;
    struct sv2_jdc_template* t;
    unsigned char            header[80];
    uint8_t                  cb_txid[32];
    mining_coinbase          cb;
    int                      count = 0, i;
    uint32_t                 le;

    if (!jdc_svc || !mining_ipc_service_ready(jdc_svc))
        return NULL;
    if (mining_ipc_create_new_block(jdc_svc, &tmpl) || !tmpl)
        return NULL;

    t = ckzalloc(sizeof(*t));
    t->tmpl = tmpl;

    if (mining_ipc_template_header(tmpl, header) || mining_ipc_template_coinbase(tmpl, &cb) ||
        mining_ipc_template_merkle_path(tmpl, branch, &count)) {
        LOGWARNING("JDC failed to read template fields over IPC");
        goto fail;
    }
    if (count > SV2_JDC_MAX_MERKLES) {
        LOGWARNING(
            "JDC template has %d merkle steps, over the %d the SV1 notify "
            "path can carry",
            count, SV2_JDC_MAX_MERKLES);
        goto fail;
    }
    t->merkles = count;
    for (i = 0; i < count; i++)
        memcpy(t->merkle_path[i], branch[i], 32);

    /* Header fields stay in wire (internal) order: prev_hash is then
     * directly comparable with a pool SetNewPrevHash. */
    memcpy(&le, header, 4);
    t->version = le32toh(le);
    memcpy(t->prev_hash, header + 4, 32);
    /* Zeroed in a template header — the coinbase is not final yet. Kept for
     * completeness; template_check_merkle() does not rely on it. */
    memcpy(t->merkle_root, header + 36, 32);
    memcpy(&le, header + 68, 4);
    t->ntime = le32toh(le);
    memcpy(&le, header + 72, 4);
    t->nbits = le32toh(le);

    if (!template_take_coinbase(t, &cb))
        goto fail;

    if (mining_ipc_template_block(tmpl, &t->block, &t->blocklen)) {
        LOGWARNING("JDC failed to fetch template block over IPC");
        goto fail;
    }
    if (t->blocklen > SV2_JDC_MAX_BLOCK_BYTES) {
        LOGWARNING("JDC template block %zu bytes exceeds the %u declare ceiling", t->blocklen, SV2_JDC_MAX_BLOCK_BYTES);
        goto fail;
    }
    if (!template_split_block(t, cb_txid))
        goto fail;
    if (!template_check_merkle(t, cb_txid))
        goto fail;
    return t;
fail:
    /* refcount is still 0: not published, so free it directly. */
    template_free(t);
    return NULL;
}

static void jdc_template_process(struct jdc_build* msg) {
    struct sv2_jdc_template* t;
    char                     prev[65];

    if (!msg)
        return;
    ensure_lock();
    if (msg->declare_only) {
        /*
         * The pool caught up to our tip. Nothing to build — the current
         * template is already on that tip — but it could not be declared
         * until now, and this runs off the pool's SetNewPrevHash rather
         * than a timer so the window costs no work.
         */
        jdc_declare_retry();
        goto out;
    }
    mutex_lock(&jdc_lock);
    jdc_build_queued = false;
    mutex_unlock(&jdc_lock);

    t = template_build();
    if (!t) {
        LOGNOTICE("JDC template generation failed (%s)", msg->tip_change ? "tip change" : "refresh");
        goto out;
    }
    template_store(t);

    hash_to_display(t->prev_hash, prev);
    LOGNOTICE(
        "JDC template %" PRIu64 " height %d prev %s: %d txns (%" PRIu64
        " bytes, "
        "block %zu), %d merkles, reward %" PRId64 ", ssig prefix %u, %s (%s)",
        t->id, t->height, prev, t->txns, t->txn_bytes, t->blocklen, t->merkles, t->reward, t->script_sig_prefix_len,
        t->have_commitment ? "segwit commitment" : "no commitment (legacy coinbase)",
        msg->tip_change ? "tip change" : "refresh");
    /* Declare it if the JD session is ready for one. Runs on this thread so a
     * declare (and its ProvideMissingTransactions round trip) never sits in
     * front of tip detection. */
    jdc_declare_template(t);
out:
    dealloc(msg);
}

/*
 * Queue a build. A tip change always goes; a fee-bump refresh is dropped when
 * one is already queued or the interval has not elapsed, so a slow (or failing)
 * createNewBlock cannot let requests pile up behind it.
 *
 * The interval is stamped here, on the attempt rather than on success: a
 * template we cannot use must not leave the interval permanently expired and
 * rebuild in a loop.
 */
static void queue_build(bool tip_change) {
    struct jdc_build* msg;
    time_t            now = time(NULL);
    bool              skip;

    ensure_lock();
    mutex_lock(&jdc_lock);
    skip = !tip_change && (jdc_build_queued || now - jdc_last_build < SV2_JDC_REFRESH_SECS);
    if (!skip) {
        jdc_build_queued = true;
        jdc_last_build = now;
    }
    mutex_unlock(&jdc_lock);
    if (skip)
        return;

    msg = ckzalloc(sizeof(*msg));
    msg->tip_change = tip_change;
    ckmsgq_add(jdc_buildq, msg);
}

/*
 * Declare the current template on the build worker. Called from the generator's
 * receive thread, which must not build a coinbase and block on a socket write
 * itself, and must not take the JD session's send lock in the middle of handling
 * a pool frame.
 */
static void queue_declare(void) {
    struct jdc_build* msg;

    if (!jdc_buildq)
        return;
    msg = ckzalloc(sizeof(*msg));
    msg->declare_only = true;
    ckmsgq_add(jdc_buildq, msg);
}

enum sv2_tip_rel
sv2_jdc_pool_tip(int proxy_id, const uint8_t prev[32], uint32_t nbits, uint32_t min_ntime, const char** why) {
    enum sv2_tip_reject rej = SV2_TIP_OK;
    bool                declare = false, report = false;
    enum sv2_tip_rel    rel;
    int                 latency = 0;
    tv_t                now;

    if (why)
        *why = NULL;
    /* jdc_buildq exists only once the local template provider is running, so
     * it also rules out the zeroed sess.proxy_id of a proxy without JD. */
    if (!prev || !jdc_buildq || proxy_id != sess.proxy_id)
        return SV2_TIP_NO_LOCAL;
    tv_time(&now);
    ensure_lock();
    mutex_lock(&jdc_lock);
    /*
     * Our node was already here, so this announcement closes the race and we
     * won it. Only on a tip the pool has not already given us: a repeat
     * SetNewPrevHash for the tip we hold is not a second race to report.
     */
    if ((!sess.have_pool_tip || memcmp(sess.pool_prev, prev, 32)) && sess.have_local_tip &&
        !memcmp(sess.local_prev, prev, 32)) {
        latency = ms_tvdiff(&sess.local_tip_tv, &now);
        report = true;
    }
    memcpy(sess.pool_prev, prev, 32);
    sess.pool_tip_tv = now;
    sess.have_pool_tip = true;
    rel = sv2_tip_relation(&jdc_tips, prev);
    if (rel == SV2_TIP_AHEAD)
        rej = sv2_tip_plausible(&jdc_tips, nbits, min_ntime);
    /*
     * The pool has reached the tip our current template is on, so a declare the
     * tip gate was refusing can go now. Only whether it is worth waking the
     * declare path is decided here: may_declare_locked stays the one gate, so
     * the pacing a genuine failure earned this template still applies.
     */
    declare = (rel == SV2_TIP_SAME && jdc_current && !memcmp(jdc_current->prev_hash, prev, 32));
    mutex_unlock(&jdc_lock);
    if (report)
        log_tip_latency(prev, latency);
    if (rej != SV2_TIP_OK && why)
        *why = sv2_tip_reject_str(rej);
    if (declare)
        queue_declare();
    return rel;
}

void sv2_jdc_pool_tip_clear(int proxy_id) {
    if (!jdc_buildq || proxy_id != sess.proxy_id)
        return;
    ensure_lock();
    mutex_lock(&jdc_lock);
    sess.have_pool_tip = false;
    memset(sess.pool_prev, 0, 32);
    /*
     * The local stamp is only meaningful as the other half of a race with this
     * connection's announcement. A reconnect re-announces the tip we already
     * hold, which is a resync and not a second race, so keeping the stamp would
     * report the whole outage as latency.
     */
    sess.have_local_tip = false;
    memset(sess.local_prev, 0, 32);
    mutex_unlock(&jdc_lock);
}

/* ===================================================================
 * Job Declaration session
 * =================================================================== */

/*
 * Split "[scheme://]host:port/<base58 authority>" into host, port and the
 * authority x-only pubkey. Same URL shape as an SV2 mining entry, and the same
 * rule: no key means no connection, since a JD session is never run
 * unauthenticated.
 */
static bool jds_parse_url(const char* url, char** host, char** port, uint8_t authority[32]) {
    static const char b58set[] = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    const char *      hostp, *path;
    char              hostport[256], b58[128];
    size_t            n;

    *host = NULL;
    *port = NULL;
    if (!url)
        return false;
    hostp = strstr(url, "//");
    hostp = hostp ? hostp + 2 : url;
    path = strchr(hostp, '/');
    if (!path || !path[1]) {
        LOGWARNING("JDC url %s has no authority key in its path", url);
        return false;
    }
    n = (size_t)(path - hostp);
    if (!n || n >= sizeof(hostport)) {
        LOGWARNING("JDC url %s has no usable host:port", url);
        return false;
    }
    memcpy(hostport, hostp, n);
    hostport[n] = '\0';

    path++;
    n = strspn(path, b58set);
    if (!n || n >= sizeof(b58)) {
        LOGWARNING("JDC url %s authority key is not base58", url);
        return false;
    }
    memcpy(b58, path, n);
    b58[n] = '\0';
    if (!sv2_noise_authority_b58_to_xonly(b58, authority)) {
        LOGWARNING("JDC url %s authority key invalid", url);
        return false;
    }
    if (!extract_sockaddr(hostport, host, port)) {
        LOGWARNING("JDC url %s host:port unparseable", url);
        return false;
    }
    return true;
}

/* Reassembly, bounded by the JD payload policy and returned to its initial
 * size when it drains (as the JDS side does for its own buffers). */
static bool sess_rx_append(const uint8_t* data, size_t n) {
    size_t max = sv2_rx_max_for_payload(SV2_MAX_JD_PAYLOAD);

    if (sess.rx_len + n > sess.rx_cap) {
        size_t ncap = sess.rx_cap ? sess.rx_cap : SV2_RX_CAP_INITIAL;

        while (ncap < sess.rx_len + n)
            ncap *= 2;
        if (ncap > max)
            return false;
        sess.rx = ckrealloc(sess.rx, ncap);
        sess.rx_cap = ncap;
    }
    memcpy(sess.rx + sess.rx_len, data, n);
    sess.rx_len += n;
    return true;
}

/* 1 = frame extracted (caller frees), 0 = need more, -1 = fatal. */
static int sess_rx_next(uint8_t** plain, size_t* plainlen) {
    size_t consumed = 0;
    int    rc;

    *plain = NULL;
    *plainlen = 0;
    rc = sv2_noise_decrypt_frame(sess.noise, sess.rx, sess.rx_len, &consumed, plain, plainlen);
    if (rc == 0) {
        memmove(sess.rx, sess.rx + consumed, sess.rx_len - consumed);
        sess.rx_len -= consumed;
        if (!sess.rx_len && sess.rx_cap > SV2_RX_CAP_INITIAL) {
            sess.rx = ckrealloc(sess.rx, SV2_RX_CAP_INITIAL);
            sess.rx_cap = SV2_RX_CAP_INITIAL;
        }
        return 1;
    }
    if (rc == -2)
        return -1;
    return 0;
}

/* Blocking read of one plaintext frame, for the handshake / setup phase. */
static int sess_readframe(uint8_t** plain, size_t* plainlen, float timeout) {
    uint8_t rbuf[SV2_RX_READ_CHUNK];

    while (42) {
        int rc = sess_rx_next(plain, plainlen);
        int r;

        if (rc != 0)
            return rc;
        if (wait_read_select(sess.fd, timeout) < 1)
            return 0;
        r = read(sess.fd, rbuf, sizeof(rbuf));
        if (r < 1)
            return -1;
        if (!sess_rx_append(rbuf, (size_t)r))
            return -1;
    }
}

/* Encrypt and send one plaintext JD message. Single writer via send_lock. */
static bool sess_send(uint8_t msg_type, const uint8_t* pay, size_t paylen) {
    uint8_t *frame = NULL, *ct = NULL;
    size_t   flen = 0, ctlen = 0;
    bool     ret = false;

    if (sess.fd < 0 || !sess.noise)
        return false;
    if (!sv2_build_frame(0, msg_type, pay, (uint32_t)paylen, &frame, &flen))
        return false;
    mutex_lock(&sess.send_lock);
    if (sv2_noise_encrypt_frame(sess.noise, frame, flen, &ct, &ctlen))
        ret = (write_socket(sess.fd, ct, ctlen) == (int)ctlen);
    if (ret)
        sess.tx_since_rx = true;
    mutex_unlock(&sess.send_lock);
    dealloc(frame);
    dealloc(ct);
    return ret;
}

static void sess_disconnect(void) {
    struct jdc_material drop;

    mutex_lock(&jdc_lock);
    sess.ready = false;
    sess.have_token = false;
    sess.pending.live = false;
    /* A reconnected JDS has no record of anything we declared, so every
     * template — including one it previously accepted — is declarable again. */
    sess.declared_id = 0;
    sess.attempt_id = 0;
    sess.retry_after = 0;
    sess.attempt_fails = 0;
    sess.attempt_done = false;
    sess.custom_live = false;
    sess.custom_id = 0;
    drop = sess.pending.m;
    memset(&sess.pending.m, 0, sizeof(sess.pending.m));
    mutex_unlock(&jdc_lock);
    /* The pending declare's template reference is dropped outside the lock. */
    material_clear(&drop);
    if (sess.noise) {
        sv2_noise_session_free(sess.noise);
        sess.noise = NULL;
    }
    if (sess.fd >= 0) {
        close(sess.fd);
        sess.fd = -1;
    }
    dealloc(sess.rx);
    sess.rx_len = sess.rx_cap = 0;
}

/* TCP → Noise NX (certificate verified against the URL authority key). */
static bool sess_handshake(void) {
    uint8_t act1[64], act2[234];

    sess.noise = sv2_noise_client_session_new(sess.authority);
    if (!sess.noise) {
        LOGWARNING("JDC failed to create a Noise session");
        return false;
    }
    if (!sv2_noise_client_act1(sess.noise, act1) || write_socket(sess.fd, act1, 64) != 64) {
        LOGNOTICE("JDC failed to send handshake act1");
        return false;
    }
    if (wait_read_select(sess.fd, 15) < 1 || read_length(sess.fd, act2, sizeof(act2)) != (int)sizeof(act2)) {
        LOGNOTICE("JDC failed to read handshake act2");
        return false;
    }
    if (!sv2_noise_client_act2(sess.noise, act2, sizeof(act2))) {
        LOGWARNING("JDC JDS certificate verification failed");
        return false;
    }
    return true;
}

/*
 * SetupConnection for the Job Declaration protocol. DECLARE_TX_DATA is
 * mandatory: ckpool's JDS refuses a session without it, and Full-Template is
 * the only mode we implement.
 */
static bool sess_setup(void) {
    struct sv2_setup_connection sc;
    uint8_t                     buf[512];
    size_t                      plen = 0;

    memset(&sc, 0, sizeof(sc));
    sc.protocol = SV2_PROTOCOL_JOB_DECLARATION;
    sc.min_version = 2;
    sc.max_version = 2;
    sc.flags = SV2_JD_FLAG_DECLARE_TX_DATA;
    snprintf(sc.endpoint_host, sizeof(sc.endpoint_host), "%s", sess.host);
    sc.endpoint_port = (uint16_t)atoi(sess.port);
    snprintf(sc.vendor, sizeof(sc.vendor), "ckproxy");
    snprintf(sc.firmware, sizeof(sc.firmware), "%s", PACKAGE "/" VERSION);
    if (!sv2_encode_setup_connection(buf, sizeof(buf), &plen, &sc) || !sess_send(SV2_MSG_SETUP_CONNECTION, buf, plen)) {
        LOGNOTICE("JDC failed to send SetupConnection");
        return false;
    }
    while (42) {
        uint8_t*         f = NULL;
        size_t           fl = 0;
        struct sv2_frame fr;
        const uint8_t*   pay;

        if (sess_readframe(&f, &fl, 15) < 1) {
            LOGNOTICE("JDC no SetupConnection response");
            return false;
        }
        if (!sv2_decode_header(f, fl, &fr)) {
            dealloc(f);
            return false;
        }
        pay = f + SV2_FRAME_HEADER_LEN;
        if (fr.msg_type == SV2_MSG_SETUP_CONNECTION_SUCCESS) {
            struct sv2_setup_connection_success ok;
            bool good = sv2_decode_setup_connection_success(pay, fr.msg_length, &ok) && ok.used_version == 2;

            dealloc(f);
            if (!good) {
                LOGWARNING("JDC bad SetupConnection.Success");
                return false;
            }
            return true;
        }
        if (fr.msg_type == SV2_MSG_SETUP_CONNECTION_ERROR) {
            struct sv2_setup_connection_error err;

            memset(&err, 0, sizeof(err));
            sv2_decode_setup_connection_error(pay, fr.msg_length, &err);
            LOGWARNING("JDC SetupConnection.Error flags=0x%x: %s", err.flags, err.error_code);
            dealloc(f);
            return false;
        }
        dealloc(f);
    }
}

/* Ask for a mining job token. One outstanding request at a time. */
static void sess_allocate_token(void) {
    struct sv2_allocate_mining_job_token req;
    uint8_t                              buf[SV2_ALLOCATE_TOKEN_MAX_BYTES];
    size_t                               plen = 0;

    memset(&req, 0, sizeof(req));
    snprintf(req.user_identifier, sizeof(req.user_identifier), "%s", sess.user);
    mutex_lock(&jdc_lock);
    req.request_id = ++sess.next_request_id;
    sess.token_asked = time(NULL);
    mutex_unlock(&jdc_lock);

    if (!sv2_encode_allocate_mining_job_token(buf, sizeof(buf), &plen, &req) ||
        !sess_send(SV2_MSG_ALLOCATE_MINING_JOB_TOKEN, buf, plen)) {
        LOGNOTICE("JDC failed to send AllocateMiningJobToken");
        return;
    }
    LOGINFO("JDC AllocateMiningJobToken req=%u user=%s", req.request_id, sess.user);
}

/*
 * Build and send a DeclareMiningJob for template t.
 *
 * The coinbase pays the pool's advertised script the whole reward and leaves an
 * extranonce hole of exactly extranonce_prefix_len + extranonce_size, which is
 * what the JDS derives back out of the prefix.
 */
static bool sess_declare(struct sv2_jdc_template* t) {
    uint8_t                       obuf[SV2_MAX_B0_255 + SV2_JDC_MAX_OUTPUT + 16];
    struct jdc_material           m, drop;
    struct sv2_jdc_channel        chan;
    struct sv2_declare_mining_job decl;
    struct sv2_cb_spec            spec;
    uint8_t *                     wtxids = NULL, *buf = NULL;
    uint8_t                       payouts[SV2_MAX_PAYOUT_BLOB];
    size_t                        olen, need, plen = 0;
    uint16_t                      payouts_len;
    uint32_t                      request_id;
    bool                          sent = false, installed = false;
    int                           hole, i;

    if (!sv2_proxy_jd_channel(sess.proxy_id, &chan)) {
        LOGINFO("JDC no open mining channel on proxy %d yet, not declaring", sess.proxy_id);
        return false;
    }
    /* Declaring against a pool we are not currently mining would spend its
     * token budget on work nothing can use. */
    if (!chan.current) {
        LOGINFO("JDC proxy %d is not the current upstream, not declaring", sess.proxy_id);
        return false;
    }
    hole = (int)chan.extranonce_prefix_len + (int)chan.extranonce_size;
    /*
     * The full extranonce has to fit an SV2 B0_32: PushSolution carries it that
     * way, and so does the JDS's own rebuild of the block. A channel granting
     * more than 32 bytes of hole could be declared but never solved, so it is
     * refused here rather than at the point a block is lost.
     */
    if (hole < 1 || hole > SV2_MAX_B0_32) {
        LOGWARNING(
            "JDC channel extranonce hole %d (prefix %u + size %u) exceeds "
            "the %d bytes a solution can carry",
            hole, chan.extranonce_prefix_len, chan.extranonce_size, SV2_MAX_B0_32);
        return false;
    }

    memset(&m, 0, sizeof(m));
    memset(&drop, 0, sizeof(drop));
    m.hole_len = (uint16_t)hole;

    mutex_lock(&jdc_lock);
    if (!may_declare_locked(t->id, time(NULL))) {
        mutex_unlock(&jdc_lock);
        return false;
    }
    /*
     * Both server-side gates on the way to live custom work are against the
     * pool's tip: the JDS assembles its candidate block on that prevhash, and
     * SetCustomMiningJob is refused outright with stale-prev-hash. A local node
     * that is ahead — the normal case for a well connected one — must therefore
     * wait rather than spend the pool's checkBlock budget on a certain failure.
     * Nothing here touches the retry state: this is not an attempt.
     */
    if (!sess.have_pool_tip || memcmp(sess.pool_prev, t->prev_hash, 32)) {
        bool announce = sess.nodeclare_id != t->id;
        bool known = sess.have_pool_tip;

        sess.nodeclare_id = t->id;
        mutex_unlock(&jdc_lock);
        if (announce) {
            LOGNOTICE(
                "JDC template %" PRIu64
                " height %d is not on the pool's "
                "tip%s; waiting for the pool to announce it",
                t->id, t->height, known ? "" : " (none announced yet)");
        }
        return false;
    }
    m.token_len = sess.token.token_len;
    memcpy(m.token, sess.token.token, sess.token.token_len);
    payouts_len = sess.token.payouts_len;
    memcpy(payouts, sess.token.payouts, payouts_len);
    request_id = ++sess.next_request_id;
    sess.pending.live = true;
    sess.pending.request_id = request_id;
    sess.pending.sent = time(NULL);
    if (sess.attempt_id != t->id) {
        sess.attempt_id = t->id;
        sess.attempt_fails = 0;
        sess.attempt_done = false;
    }
    mutex_unlock(&jdc_lock);

    olen = sv2_cb_build_outputs(
        obuf, sizeof(obuf), payouts, payouts_len, (uint64_t)t->reward, t->have_commitment ? t->commitment : NULL,
        t->have_commitment ? t->commitment_len : 0);
    if (!olen) {
        LOGWARNING(
            "JDC failed to build coinbase outputs (payouts %u bytes, "
            "commitment %u bytes)",
            payouts_len, t->commitment_len);
        goto out;
    }
    m.outputs = ckalloc(olen);
    memcpy(m.outputs, obuf, olen);
    m.outputs_len = (uint16_t)olen;

    memset(&spec, 0, sizeof(spec));
    spec.version = t->cb_version;
    spec.nsequence = t->cb_sequence;
    spec.locktime = t->cb_locktime;
    spec.ssig_prefix = t->script_sig_prefix;
    spec.ssig_prefix_len = t->script_sig_prefix_len;
    spec.hole_len = (uint8_t)hole;
    spec.outputs = m.outputs;
    spec.outputs_len = m.outputs_len;
    spec.witness_reserved = t->have_commitment ? t->witness_reserved : NULL;
    if (!sv2_cb_declare_parts(&spec, &m.dcb_prefix, &m.dcb_prefix_len, &m.dcb_suffix, &m.dcb_suffix_len)) {
        LOGWARNING(
            "JDC cannot build a declarable coinbase: scriptSig %u+%d "
            "exceeds %d bytes",
            t->script_sig_prefix_len, hole, SV2_CB_MAX_SCRIPTSIG);
        goto out;
    }
    /*
     * The same transaction in legacy (txid) form. A merkle root is built over
     * txids, so this — not the witness serialisation above — is what downstream
     * miners hash, what SetCustomMiningJob describes, and what the pool
     * reconstructs to validate a share. Identical to the declared split on a
     * template without a witness commitment.
     */
    spec.witness_reserved = NULL;
    if (!sv2_cb_declare_parts(&spec, &m.lcb_prefix, &m.lcb_prefix_len, &m.lcb_suffix, &m.lcb_suffix_len)) {
        LOGWARNING("JDC failed to build the legacy coinbase split");
        goto out;
    }
    m.t = sv2_jdc_template_ref(t);

    /*
     * Publish the material before the declare goes out: the JDS asks for
     * transactions on the session thread within a millisecond, while this
     * thread is still inside sess_send().
     */
    mutex_lock(&jdc_lock);
    if (sess.pending.live && sess.pending.request_id == request_id) {
        drop = sess.pending.m;
        sess.pending.m = m;
        installed = true;
    }
    mutex_unlock(&jdc_lock);
    if (!installed) {
        /* The session dropped while this was being built. */
        LOGINFO("JDC declare req=%u abandoned before sending", request_id);
        goto out;
    }
    material_clear(&drop);

    if (t->txns) {
        wtxids = ckalloc((size_t)t->txns * 32);
        for (i = 0; i < t->txns; i++)
            memcpy(wtxids + (size_t)i * 32, t->txn[i].wtxid, 32);
    }

    memset(&decl, 0, sizeof(decl));
    decl.request_id = request_id;
    decl.mining_job_token_len = m.token_len;
    memcpy(decl.mining_job_token, m.token, m.token_len);
    decl.version = t->version;
    decl.coinbase_tx_prefix = m.dcb_prefix;
    decl.coinbase_tx_prefix_len = m.dcb_prefix_len;
    decl.coinbase_tx_suffix = m.dcb_suffix;
    decl.coinbase_tx_suffix_len = m.dcb_suffix_len;
    decl.wtxid_list = wtxids;
    decl.wtxid_count = (uint16_t)t->txns;

    need = sv2_declare_mining_job_encoded_size(&decl);
    buf = ckalloc(need);
    if (!sv2_encode_declare_mining_job(buf, need, &plen, &decl)) {
        LOGWARNING("JDC failed to encode DeclareMiningJob (%zu bytes)", need);
        goto out;
    }
    if (!sess_send(SV2_MSG_DECLARE_MINING_JOB, buf, plen)) {
        LOGNOTICE("JDC failed to send DeclareMiningJob req=%u", request_id);
        goto out;
    }
    sent = true;
    sess.declares++;
    LOGNOTICE(
        "JDC DeclareMiningJob req=%u template %" PRIu64
        " height %d: %d txns, "
        "coinbase %u+%d+%u, reward %" PRId64 ", frame %zu bytes",
        request_id, t->id, t->height, t->txns, m.dcb_prefix_len, hole, m.dcb_suffix_len, t->reward, plen);
out:
    dealloc(wtxids);
    dealloc(buf);
    /* Installed material belongs to the pending declare now. */
    if (!installed)
        material_clear(&m);
    if (!sent) {
        /* Never reached the wire: a prompt retry is free. */
        mutex_lock(&jdc_lock);
        declare_failed_locked(1, false);
        mutex_unlock(&jdc_lock);
    }
    return sent;
}

/*
 * Whether template id may be declared now. Caller holds jdc_lock.
 *
 * The one gate: its callers only test cheap readiness, so the retry policy
 * lives here and cannot drift between the template thread and the session one.
 */
static bool may_declare_locked(uint64_t id, time_t now) {
    if (!sess.ready || !sess.have_token || sess.pending.live)
        return false;
    if (sess.declared_id == id)
        return false; /* already mining on it */
    if (sess.custom_live && sess.custom_id == id)
        return false; /* declared, awaiting the pool's SetCustom reply */
    if (sess.attempt_id == id) {
        if (sess.attempt_done)
            return false;
        if (now < sess.retry_after)
            return false;
    }
    return true;
}

/*
 * Record that the pending declare ended without a Success. Caller holds
 * jdc_lock. delay paces the next attempt on the same template; done stops
 * retrying it at all, for a reason another attempt cannot fix.
 */
static void declare_failed_locked(int delay, bool done) {
    sess.pending.live = false;
    if (done) {
        sess.attempt_done = true;
        return;
    }
    if (++sess.attempt_fails >= SV2_JDC_DECLARE_MAX_ATTEMPTS) {
        /* Something about this template keeps failing: wait for the next
         * one rather than keep spending the pool's checkBlock budget. */
        sess.attempt_done = true;
        return;
    }
    sess.retry_after = time(NULL) + delay;
}

/*
 * The declare was accepted but its custom job never went live. Caller holds
 * jdc_lock and has checked custom_live. The template goes back to the same
 * bounded retry policy a failed declare uses — deliberately sharing
 * attempt_fails with the declare stage, so declare/custom cannot alternate
 * failures indefinitely on one template.
 */
static void custom_failed_locked(int delay) {
    sess.custom_live = false;
    /* A newer template has already taken over the retry state. */
    if (sess.attempt_id != sess.custom_id)
        return;
    if (++sess.attempt_fails >= SV2_JDC_DECLARE_MAX_ATTEMPTS) {
        sess.attempt_done = true;
        return;
    }
    sess.retry_after = time(NULL) + delay;
}

/* Reply to ProvideMissingTransactions with the raw bytes of the positions the
 * JDS is missing, in the order it asked for them. */
static void sess_provide_missing(const uint8_t* pay, uint32_t plen) {
    struct sv2_provide_missing_transactions         req;
    struct sv2_provide_missing_transactions_success rep;
    struct sv2_jdc_template*                        t = NULL;
    uint8_t **                                      txs = NULL, *buf = NULL;
    uint32_t*                                       lens = NULL;
    size_t                                          need, elen = 0;
    uint16_t                                        i;

    if (!sv2_decode_provide_missing_transactions(pay, plen, &req)) {
        LOGNOTICE("JDC bad ProvideMissingTransactions");
        return;
    }
    sess.provide_missing++;
    mutex_lock(&jdc_lock);
    if (sess.pending.live && sess.pending.m.t && req.request_id == sess.pending.request_id) {
        t = sess.pending.m.t;
        t->refcount++;
    }
    mutex_unlock(&jdc_lock);
    if (!t) {
        LOGNOTICE("JDC ProvideMissingTransactions req=%u for no pending declare", req.request_id);
        goto out;
    }
    /* Positions index the wtxid list we sent, so anything outside it means
     * the JDS and we disagree about the declare — abandon it. */
    for (i = 0; i < req.unknown_count; i++) {
        if (req.unknown_tx_position_list[i] >= t->txns) {
            LOGWARNING(
                "JDC ProvideMissingTransactions position %u outside "
                "our %d transactions",
                req.unknown_tx_position_list[i], t->txns);
            goto out;
        }
    }

    memset(&rep, 0, sizeof(rep));
    rep.request_id = req.request_id;
    rep.tx_count = req.unknown_count;
    if (rep.tx_count) {
        txs = ckalloc(sizeof(*txs) * rep.tx_count);
        lens = ckalloc(sizeof(*lens) * rep.tx_count);
        for (i = 0; i < rep.tx_count; i++) {
            const struct sv2_jdc_tx* tx = &t->txn[req.unknown_tx_position_list[i]];

            /* Cast away const: the encoder only reads. */
            txs[i] = (uint8_t*)tx->raw;
            lens[i] = tx->len;
        }
        rep.transactions = txs;
        rep.tx_lens = lens;
    }
    need = sv2_provide_missing_tx_success_encoded_size(&rep);
    buf = ckalloc(need);
    if (!sv2_encode_provide_missing_transactions_success(buf, need, &elen, &rep)) {
        LOGWARNING("JDC failed to encode %u missing transactions (%zu bytes)", rep.tx_count, need);
        goto out;
    }
    if (!sess_send(SV2_MSG_PROVIDE_MISSING_TRANSACTIONS_SUCCESS, buf, elen)) {
        LOGNOTICE("JDC failed to send ProvideMissingTransactions.Success");
        goto out;
    }
    LOGNOTICE("JDC provided %u of %d transactions (%zu bytes) for req=%u", rep.tx_count, t->txns, elen, req.request_id);
out:
    dealloc(txs);
    dealloc(lens);
    dealloc(buf);
    sv2_jdc_template_put(t);
    sv2_provide_missing_transactions_free(&req);
}

/*
 * Turn an accepted declare into a SetCustomMiningJob on the mining connection,
 * which the generator owns. Consumes the material: it has already been stolen
 * out of the pending declare, and the generator copies whatever it keeps.
 *
 * Nothing goes downstream from here — that waits on the pool's
 * SetCustomMiningJob.Success (sv2_jdc_custom_result), because a notify for a
 * job_id the pool has not acknowledged only produces shares it will reject.
 */
static void jdc_custom_job(struct jdc_material* m) {
    struct sv2_jdc_custom_job cj;
    bool                      superseded;

    if (!m->t)
        goto out;
    /*
     * sess_declare() checked this template against the pool's tip before the
     * declare went out, but the answer took a round trip and the tip can move
     * inside it. The pool matches SetCustomMiningJob.prev_hash against its own
     * tip and refuses anything else with stale-prev-hash, so sending one now
     * buys a certain rejection and the round trip to hear it. Say nothing and
     * let the template built on the new tip take its place.
     *
     * Not a failure of this template — it was overtaken, not refused — so the
     * retry state is left alone, as sess_declare() leaves it when it declines
     * to declare for the same reason. Only custom_live is cleared, since no
     * reply is now coming for it: left set it would suppress a re-declare of
     * this same template in may_declare_locked() until the backstop in the
     * session loop timed it out.
     */
    mutex_lock(&jdc_lock);
    superseded = !sess.have_pool_tip || memcmp(sess.pool_prev, m->t->prev_hash, 32) != 0;
    if (superseded)
        sess.custom_live = false;
    mutex_unlock(&jdc_lock);
    if (superseded) {
        LOGNOTICE(
            "JDC template %" PRIu64
            " height %d is no longer on the pool's "
            "tip; sending no SetCustomMiningJob for it",
            m->t->id, m->t->height);
        material_clear(m);
        /*
         * Declare the template that replaced this one now, rather than
         * leaving it to the session loop: a loop iteration that read a
         * frame continues without reaching the housekeeping that calls
         * this, so it only runs after a full second of quiet — and we are
         * here off a frame. sess_declare() gates on the pool's tip itself,
         * so this costs nothing if the replacement is not built yet; the
         * tip-change build will declare it when it is.
         */
        jdc_declare_retry();
        return;
    }
    memset(&cj, 0, sizeof(cj));
    cj.proxy_id = sess.proxy_id;
    cj.t = m->t;
    cj.token = m->token;
    cj.token_len = m->token_len;
    cj.outputs = m->outputs;
    cj.outputs_len = m->outputs_len;
    cj.lcb_prefix = m->lcb_prefix;
    cj.lcb_prefix_len = m->lcb_prefix_len;
    cj.lcb_suffix = m->lcb_suffix;
    cj.lcb_suffix_len = m->lcb_suffix_len;
    cj.dcb_prefix = m->dcb_prefix;
    cj.dcb_prefix_len = m->dcb_prefix_len;
    cj.dcb_suffix = m->dcb_suffix;
    cj.dcb_suffix_len = m->dcb_suffix_len;
    cj.hole_len = m->hole_len;
    /* Outside jdc_lock: it takes the template store's reference, which is
     * that same lock. */
    if (sv2_proxy_set_custom_job(&cj)) {
        mutex_lock(&jdc_lock);
        sess.customs++;
        mutex_unlock(&jdc_lock);
    } else {
        /* Nothing was staged, so no result will come back: hand the template
         * to the retry policy here or it would never be declared again. */
        mutex_lock(&jdc_lock);
        if (sess.custom_live)
            custom_failed_locked(SV2_JDC_CUSTOM_RETRY_SECS);
        mutex_unlock(&jdc_lock);
        LOGNOTICE(
            "JDC no SetCustomMiningJob sent for template %" PRIu64 ", re-declaring in %ds", m->t->id,
            SV2_JDC_CUSTOM_RETRY_SECS);
    }
out:
    material_clear(m);
}

void sv2_jdc_custom_result(bool ok, uint32_t job_id, const char* code) {
    bool reallocate = false;

    ensure_lock();
    mutex_lock(&jdc_lock);
    if (ok) {
        sess.custom_ok++;
        /* Live custom work is what retires a template. */
        if (sess.custom_live) {
            sess.declared_id = sess.custom_id;
            sess.custom_live = false;
            if (sess.attempt_id == sess.custom_id)
                sess.attempt_fails = 0;
        }
    } else {
        sess.custom_err++;
        /*
         * The declare was accepted but its token has since expired (TTL is
         * 1 h declared). Chase a fresh one and let the current template be
         * declared again on it: without a token the custom job it built
         * cannot be reinstated.
         */
        if (code && !strcmp(code, "invalid-mining-job-token")) {
            sess.have_token = false;
            sess.declared_id = 0;
            sess.attempt_id = 0;
            sess.retry_after = 0;
            sess.attempt_fails = 0;
            sess.attempt_done = false;
            sess.custom_live = false;
            sess.custom_id = 0;
            reallocate = true;
        } else if (sess.custom_live) {
            /* Everything else — a timeout, a busy pool, a job the pool
             * would not take — is the declare's problem again, paced by
             * the same policy and picked up by jdc_declare_retry(). */
            custom_failed_locked(SV2_JDC_CUSTOM_RETRY_SECS);
        }
    }
    mutex_unlock(&jdc_lock);
    if (ok) {
        LOGNOTICE(
            "JDC custom job %u live (%" PRIu64 " ok / %" PRIu64
            " errors of "
            "%" PRIu64 " sent)",
            job_id, sess.custom_ok, sess.custom_err, sess.customs);
        return;
    }
    LOGWARNING(
        "JDC SetCustomMiningJob rejected: %s — %s", code ? code : "(no code)",
        reallocate ? "allocating a new token" : "re-declaring the template");
    if (reallocate)
        sess_allocate_token();
}

/*
 * The mining channel can no longer mine what we declared — its extranonce
 * layout moved under us, so the coinbase hole the JDS accepted is the wrong
 * size. Retire nothing: put the current template back on the declare path so a
 * fresh declare and SetCustomMiningJob are built for the new layout.
 */
void sv2_jdc_custom_dropped(const char* why) {
    ensure_lock();
    mutex_lock(&jdc_lock);
    if (!sess.declared_id && !sess.custom_live) {
        mutex_unlock(&jdc_lock);
        return;
    }
    sess.declared_id = 0;
    sess.custom_live = false;
    sess.custom_id = 0;
    sess.attempt_id = 0;
    sess.retry_after = 0;
    sess.attempt_fails = 0;
    sess.attempt_done = false;
    mutex_unlock(&jdc_lock);
    LOGNOTICE("JDC custom work dropped (%s), re-declaring the current template", why ? why : "no reason given");
}

void sv2_jdc_solved(const struct sv2_jdc_solution* sol) {
    struct sv2_push_solution ps;
    uint8_t                  buf[SV2_PUSH_SOLUTION_MAX_BYTES];
    size_t                   plen = 0;
    int                      accepted = 0;

    if (!sol || !sol->t)
        return;
    mutex_lock(&jdc_lock);
    sess.solutions++;
    mutex_unlock(&jdc_lock);

    /*
     * PushSolution first: it is one socket write, where the local submit is a
     * validating RPC that connects the block and can take a while. The pool
     * rebuilds the block from the material we declared, so it needs the full
     * extranonce — channel prefix included — and the header as mined.
     */
    memset(&ps, 0, sizeof(ps));
    if (sol->extranonce_len > sizeof(ps.extranonce)) {
        LOGWARNING("JDC solution extranonce %u bytes is unpushable", sol->extranonce_len);
    } else {
        memcpy(ps.extranonce, sol->extranonce, sol->extranonce_len);
        ps.extranonce_len = sol->extranonce_len;
        memcpy(ps.prev_hash, sol->prev_hash, 32);
        ps.nonce = sol->nonce;
        ps.ntime = sol->ntime;
        ps.nbits = sol->nbits;
        ps.version = sol->version;
        if (sv2_encode_push_solution(buf, sizeof(buf), &plen, &ps) && sess_send(SV2_MSG_PUSH_SOLUTION, buf, plen)) {
            LOGWARNING(
                "JDC PushSolution sent for height %d nonce %08x "
                "enonce %u bytes",
                sol->t->height, sol->nonce, sol->extranonce_len);
        } else
            LOGWARNING("JDC failed to send PushSolution for height %d", sol->t->height);
    }

    /*
     * And submit it ourselves, which needs no pool cooperation at all. The
     * coinbase is the declared serialisation with its extranonce hole filled:
     * witness form (marker/flag plus the reserved value) when the template has
     * a commitment, legacy when it has none, since BIP141 forbids a coinbase
     * witness on a block without one.
     */
    if (mining_ipc_submit_solution(
            sol->t->tmpl, sol->version, sol->ntime, sol->nonce, sol->coinbase, sol->coinbase_len, &accepted)) {
        LOGWARNING(
            "JDC local block submit height %d template %" PRIu64
            " failed on "
            "the mining IPC",
            sol->t->height, sol->t->id);
        return;
    }
    LOGWARNING(
        "JDC local block submit height %d template %" PRIu64
        " coinbase %zu "
        "bytes: %s by our node",
        sol->t->height, sol->t->id, sol->coinbase_len, accepted ? "ACCEPTED" : "rejected");
}

/*
 * A declare error the JDS raises for a tip race, or because it is momentarily
 * unable to validate. Neither is our bug and neither should tear anything down;
 * the next template simply tries again.
 */
static bool declare_error_transient(const char* code) {
    static const char* transient[] = {
        "stale-chain-tip", "busy", "rate-limited", "validation-unavailable", "block-assemble-failed", "internal-error",
        /*
         * Raw bitcoind reasons, for a JDS that does not remap them.
         * bad-txns-nonfinal is the one a local tip ahead of the pool's
         * actually produces: our coinbase carries a BIP54 nLockTime of
         * height - 1, so evaluated against the pool's lower tip it is not
         * yet final. The same template becomes valid once the pool catches
         * up, so it must be retried, not blamed on the coinbase.
         */
        "bad-cb-height", "bad-txns-nonfinal", "inconclusive-not-best-prevblk", "prev-blk-not-found", "time-too-old",
        "time-too-new", NULL};
    int i;

    if (!code || !code[0])
        return false;
    for (i = 0; transient[i]; i++) {
        if (!strcmp(code, transient[i]))
            return true;
    }
    return strstr(code, "not-best-prev") != NULL;
}

static void sess_handle_frame(const uint8_t* frame, size_t flen) {
    struct sv2_frame fr;
    const uint8_t*   pay;
    uint32_t         plen;

    if (!sv2_decode_header(frame, flen, &fr))
        return;
    if (fr.msg_length > SV2_MAX_JD_PAYLOAD || flen < SV2_FRAME_HEADER_LEN + fr.msg_length)
        return;
    pay = frame + SV2_FRAME_HEADER_LEN;
    plen = fr.msg_length;

    switch (fr.msg_type) {
        case SV2_MSG_ALLOCATE_MINING_JOB_TOKEN_SUCCESS: {
            struct sv2_allocate_mining_job_token_success ok;
            char                                         payaddr[128], account[128];
            const uint8_t*                               script;
            uint8_t                                      slen = 0;
            size_t                                       alen;
            int                                          nouts, i;

            if (!sv2_decode_allocate_mining_job_token_success(pay, plen, &ok)) {
                LOGNOTICE("JDC bad AllocateMiningJobToken.Success");
                break;
            }
            if (!ok.mining_job_token_len) {
                LOGWARNING("JDC AllocateMiningJobToken.Success carries no token");
                break;
            }
            /*
             * Every output the pool names has to appear in our coinbase, so a
             * blob we cannot store whole is refused rather than honoured in
             * part: paying a subset of what the pool asked for is the one
             * outcome worse than not declaring at all.
             */
            nouts = sv2_cb_outputs_count(ok.coinbase_tx_outputs, ok.coinbase_tx_outputs_len);
            if (nouts < 1) {
                LOGWARNING(
                    "JDC AllocateMiningJobToken.Success has no usable "
                    "payout outputs (%u bytes of outputs)",
                    ok.coinbase_tx_outputs_len);
                break;
            }
            if (ok.coinbase_tx_outputs_len > sizeof(sess.token.payouts)) {
                LOGWARNING(
                    "JDC AllocateMiningJobToken.Success wants %d payout "
                    "outputs in %u bytes, over our %zu byte limit",
                    nouts, ok.coinbase_tx_outputs_len, sizeof(sess.token.payouts));
                break;
            }
            mutex_lock(&jdc_lock);
            memset(&sess.token, 0, sizeof(sess.token));
            sess.token.token_len = ok.mining_job_token_len;
            memcpy(sess.token.token, ok.mining_job_token, ok.mining_job_token_len);
            sess.token.payouts_len = ok.coinbase_tx_outputs_len;
            memcpy(sess.token.payouts, ok.coinbase_tx_outputs, ok.coinbase_tx_outputs_len);
            sess.have_token = true;
            mutex_unlock(&jdc_lock);
            LOGNOTICE(
                "JDC token allocated req=%u (%u bytes), %d payout output%s", ok.request_id, ok.mining_job_token_len,
                nouts, nouts == 1 ? "" : "s");
            /*
             * The payout scripts the pool hands back are what our coinbase has to
             * pay, so name the addresses they pay rather than their length: a solo
             * pool derives the first from the identity we declare with, and
             * anything else means the blocks we declare credit someone else.
             * Decoded against our own identity because a script does not name its
             * chain, and compared against the account part of it, which is all the
             * pool takes.
             */
            alen = strcspn(sess.user, "._");
            if (alen >= sizeof(account))
                alen = sizeof(account) - 1;
            memcpy(account, sess.user, alen);
            account[alen] = '\0';
            for (i = 0; i < nouts; i++) {
                uint64_t val = 0;

                if (!sv2_cb_output_at(ok.coinbase_tx_outputs, ok.coinbase_tx_outputs_len, i, &val, &script, &slen))
                    break;
                if (!txn_to_address(payaddr, sizeof(payaddr), script, slen, account)) {
                    char hex[SV2_MAX_B0_255 * 2 + 1];

                    __bin2hex(hex, script, slen);
                    LOGWARNING(
                        "JDC pool payout %d is a non-standard %u byte "
                        "script: %s",
                        i, slen, hex);
                } else if (i) {
                    /* Fixed amounts, unlike the first: whatever the pool
                     * set is what the coinbase pays them. */
                    LOGWARNING("JDC pool payout %d is %s for %" PRIu64 " sats", i, payaddr, val);
                } else if (!strcasecmp(payaddr, account)) {
                    LOGWARNING(
                        "JDC pool payout is %s, the identity we declare "
                        "as, and takes the remaining reward",
                        payaddr);
                } else {
                    LOGWARNING(
                        "JDC pool payout is %s, not the identity we "
                        "declare as (%s), and takes the remaining reward",
                        payaddr, account);
                }
            }
            /* A token is only useful with a template to declare. */
            queue_build(true);
            break;
        }
        case SV2_MSG_PROVIDE_MISSING_TRANSACTIONS:
            sess_provide_missing(pay, plen);
            break;
        case SV2_MSG_DECLARE_MINING_JOB_SUCCESS: {
            struct sv2_declare_mining_job_success ok;
            struct jdc_material                   m;
            bool                                  ours;

            if (!sv2_decode_declare_mining_job_success(pay, plen, &ok)) {
                LOGNOTICE("JDC bad DeclareMiningJob.Success");
                break;
            }
            memset(&m, 0, sizeof(m));
            mutex_lock(&jdc_lock);
            ours = sess.pending.live && ok.request_id == sess.pending.request_id;
            if (ours) {
                sess.pending.live = false;
                /*
                 * A template is retired by live custom work, not by an
                 * accepted declare, so this only moves it to the custom
                 * stage; attempt_fails carries over, keeping one bounded
                 * budget across both stages.
                 */
                sess.custom_live = true;
                sess.custom_id = sess.attempt_id;
                sess.custom_at = time(NULL);
                /* The material becomes this thread's, to turn into a
                 * SetCustomMiningJob below. */
                m = sess.pending.m;
                memset(&sess.pending.m, 0, sizeof(sess.pending.m));
                /*
                 * Success may hand back a new token; it is the one a
                 * SetCustomMiningJob must reference. ckpool returns the
                 * token that was declared on, so in practice it is the
                 * same one.
                 */
                if (ok.new_mining_job_token_len) {
                    sess.token.token_len = ok.new_mining_job_token_len;
                    memcpy(sess.token.token, ok.new_mining_job_token, ok.new_mining_job_token_len);
                    m.token_len = ok.new_mining_job_token_len;
                    memcpy(m.token, ok.new_mining_job_token, ok.new_mining_job_token_len);
                }
                sess.token.declared = true;
                sess.have_token = true;
            }
            mutex_unlock(&jdc_lock);
            if (!ours) {
                LOGNOTICE("JDC DeclareMiningJob.Success req=%u is not ours", ok.request_id);
                break;
            }
            sess.declare_ok++;
            LOGNOTICE(
                "JDC DeclareMiningJob.Success req=%u token %u bytes "
                "(%" PRIu64 " ok / %" PRIu64 " errors of %" PRIu64 " declares)",
                ok.request_id, ok.new_mining_job_token_len, sess.declare_ok, sess.declare_err, sess.declares);
            /* Consumes the material either way. */
            jdc_custom_job(&m);
            break;
        }
        case SV2_MSG_DECLARE_MINING_JOB_ERROR: {
            struct sv2_declare_mining_job_error err;
            bool                                ours, transient, bad_token, retry;
            char                                detail[128] = "";
            size_t                              detail_len;
            int                                 delay, attempt;

            if (!sv2_decode_declare_mining_job_error(pay, plen, &err)) {
                LOGNOTICE("JDC bad DeclareMiningJob.Error");
                break;
            }
            transient = declare_error_transient(err.error_code);
            bad_token = !strcmp(err.error_code, "invalid-mining-job-token");
            /*
             * Pace the retry by what the code means: a saturated or
             * rate-limited JDS wants time, a tip race wants another go at the
             * next opportunity, and a template we built wrong is not worth
             * re-sending at all.
             */
            if (!strcmp(err.error_code, "rate-limited"))
                delay = SV2_JDC_DECLARE_RATELIMIT_SECS;
            else if (!strcmp(err.error_code, "busy"))
                delay = 2;
            else
                delay = 1;

            mutex_lock(&jdc_lock);
            ours = sess.pending.live && err.request_id == sess.pending.request_id;
            if (ours) {
                /* A rejected token is spent; the retry needs a new one. */
                if (bad_token)
                    sess.have_token = false;
                declare_failed_locked(delay, !transient && !bad_token);
                retry = !sess.attempt_done;
                attempt = sess.attempt_fails + 1;
            }
            mutex_unlock(&jdc_lock);
            if (!ours)
                break;
            sess.declare_err++;
            /*
             * A JDS that remaps a raw validation reason onto a code we can act
             * on puts the original in error_details (ckpool's does). It is the
             * only thing that says which tip race or which malformed field, so
             * report it alongside the code rather than dropping it. Not NUL
             * terminated - it is a B0_64K of bytes - and untrusted, so bound it
             * and let the %.*s stop at the length the frame declared.
             */
            detail_len = err.error_details_len;
            /* Room for the " (" and ")" we wrap it in, and the terminator. */
            if (detail_len > sizeof(detail) - 4)
                detail_len = sizeof(detail) - 4;
            if (detail_len)
                snprintf(detail, sizeof(detail), " (%.*s)", (int)detail_len, (const char*)err.error_details);
            if (retry) {
                LOGNOTICE(
                    "JDC DeclareMiningJob.Error req=%u %s%s — retrying this "
                    "template in %ds (attempt %d)",
                    err.request_id, err.error_code, detail, delay, attempt);
            } else if (transient || bad_token) {
                LOGWARNING(
                    "JDC DeclareMiningJob.Error req=%u %s%s — giving up on "
                    "this template after %d attempts",
                    err.request_id, err.error_code, detail, SV2_JDC_DECLARE_MAX_ATTEMPTS);
            } else {
                LOGWARNING(
                    "JDC DeclareMiningJob.Error req=%u %s%s — our template "
                    "or coinbase is at fault, not retrying it",
                    err.request_id, err.error_code, detail);
            }
            /* Chase the replacement token now rather than on the idle timer. */
            if (bad_token)
                sess_allocate_token();
            break;
        }
        default:
            LOGDEBUG("JDC unhandled JD message 0x%02x", fr.msg_type);
            break;
    }
}

/* Read whatever is available and dispatch complete frames. */
static bool sess_service(void) {
    uint8_t rbuf[SV2_RX_READ_CHUNK];
    int     r = read(sess.fd, rbuf, sizeof(rbuf));

    if (r < 1)
        return false;
    if (!sess_rx_append(rbuf, (size_t)r))
        return false;
    while (42) {
        uint8_t* plain = NULL;
        size_t   plainlen = 0;
        int      rc = sess_rx_next(&plain, &plainlen);

        if (rc == 1) {
            sess.last_rx = time(NULL);
            sess.tx_since_rx = false;
            sess_handle_frame(plain, plainlen);
            dealloc(plain);
            continue;
        }
        if (rc == -1)
            return false;
        break;
    }
    return true;
}

static bool sess_connect(void) {
    sess.fd = connect_socket(sess.host, sess.port);
    if (sess.fd < 0) {
        LOGINFO("JDC failed to connect to JDS %s:%s", sess.host, sess.port);
        return false;
    }
    if (!sess_handshake() || !sess_setup()) {
        sess_disconnect();
        return false;
    }
    sess.last_rx = time(NULL);
    sess.tx_since_rx = false;
    mutex_lock(&jdc_lock);
    sess.ready = true;
    mutex_unlock(&jdc_lock);
    LOGWARNING("JDC job declaration session up to %s:%s", sess.host, sess.port);
    sess_allocate_token();
    return true;
}

/*
 * Session thread: keep the JD connection up, dispatch frames, and re-ask for a
 * token when we have none. Declares are sent from the template build queue,
 * which is why every send goes through send_lock.
 */
static void* jdc_session(void __maybe_unused* arg) {
    int backoff = 5;

    pthread_detach(pthread_self());
    rename_proc("jdcsess");

    while (!ckpool.shutdown) {
        bool alive;

        if (!sess_connect()) {
            cksleep_ms(backoff * 1000);
            if (backoff < 60)
                backoff *= 2;
            continue;
        }
        backoff = 5;
        while (!ckpool.shutdown) {
            bool   want_token = false;
            time_t now;

            if (wait_read_select(sess.fd, 1) > 0) {
                if (!sess_service())
                    break;
                continue;
            }
            now = time(NULL);
            /*
             * A JDS that has stopped reading its socket looks exactly
             * like a slow one from here, so treat prolonged silence
             * after something we sent as a dead session and rebuild it
             * rather than keep declaring into it.
             */
            if (sess.tx_since_rx && now - sess.last_rx >= SV2_JDC_SILENCE_SECS) {
                LOGWARNING(
                    "JDC no response from JDS %s:%s for %ds, "
                    "dropping the session",
                    sess.host, sess.port, (int)(now - sess.last_rx));
                break;
            }
            /* Chase a token if we still have none, and time out a
             * declare the JDS never answered. */
            mutex_lock(&jdc_lock);
            if (!sess.have_token && now - sess.token_asked >= 10)
                want_token = true;
            if (sess.pending.live && now - sess.pending.sent >= SV2_JDC_DECLARE_TIMEOUT_SECS) {
                uint32_t req = sess.pending.request_id;

                declare_failed_locked(1, false);
                LOGNOTICE(
                    "JDC declare req=%u timed out after %ds, "
                    "retrying (attempt %d)",
                    req, SV2_JDC_DECLARE_TIMEOUT_SECS, sess.attempt_fails + 1);
            }
            /*
             * Backstop for a custom job whose result never comes back at
             * all — the mining connection reports its own timeout, but a
             * connection torn down with one staged reports nothing. Later
             * than that timeout so it does not race it.
             */
            if (sess.custom_live && now - sess.custom_at >= SV2_JDC_CUSTOM_TIMEOUT_SECS + 10) {
                LOGNOTICE(
                    "JDC custom job for template %" PRIu64
                    " was never "
                    "answered, re-declaring",
                    sess.custom_id);
                custom_failed_locked(1);
            }
            mutex_unlock(&jdc_lock);
            if (want_token)
                sess_allocate_token();
            else
                jdc_declare_retry();
        }
        alive = !ckpool.shutdown;
        sess_disconnect();
        if (alive)
            LOGWARNING(
                "JDC job declaration session to %s:%s dropped, "
                "reconnecting",
                sess.host, sess.port);
    }
    sess_disconnect();
    return NULL;
}

/* Called from the template build worker once a template is published. */
static void jdc_declare_template(struct sv2_jdc_template* t) {
    bool ready;

    mutex_lock(&jdc_lock);
    ready = may_declare_locked(t->id, time(NULL));
    mutex_unlock(&jdc_lock);
    if (!ready)
        return;
    sess_declare(t);
}

/*
 * Retry the current template from the session thread. The first templates after
 * startup usually cannot be declared — the mining channel is not open yet, or
 * the token has not arrived — and waiting a whole refresh interval for the next
 * one wastes the window. sess_declare de-duplicates on template id, so this is
 * a no-op once the current template has been declared.
 */
static void jdc_declare_retry(void) {
    struct sv2_jdc_template* t;
    bool                     ready;

    mutex_lock(&jdc_lock);
    ready = jdc_current && may_declare_locked(jdc_current->id, time(NULL));
    mutex_unlock(&jdc_lock);
    if (!ready)
        return;
    t = sv2_jdc_template_current();
    if (!t)
        return;
    sess_declare(t);
    sv2_jdc_template_put(t);
}

/*
 * Tip waiter. Structurally the same as the pool's ipcnotify(): capnp clients
 * are thread affine, so the connection is made and used only here, and a
 * dropped connection is recovered transparently. Each tip change, and each
 * refresh interval in between, queues a template build.
 */
static void* jdc_tipwaiter(void __maybe_unused* arg) {
    mining_ipc_ctx* ctx;
    mining_tip      tip = {};
    char            tiphex[65];
    bool            healthy = true;

    pthread_detach(pthread_self());
    rename_proc("jdctip");

    while (!(ctx = mining_ipc_connect(ckpool.ipcmining))) {
        if (ckpool.shutdown)
            return NULL;
        LOGWARNING("JDC failed to connect to mining IPC %s, retrying in 5s", ckpool.ipcmining);
        sleep(5);
    }
    jdc_tip_ctx = ctx;
    LOGWARNING("JDC connected to bitcoind mining IPC %s", ckpool.ipcmining);

    while (mining_ipc_get_tip(ctx, &tip)) {
        if (ckpool.shutdown)
            goto out;
        mining_ipc_try_obtain_mining(ctx);
        cksleep_ms(1000);
    }
    tip_to_display(tip.hash, tiphex);
    LOGNOTICE("JDC initial local tip %s height %d", tiphex, tip.height);
    queue_build(true);

    while (!ckpool.shutdown) {
        mining_tip new_tip = {};
        int        rc = mining_ipc_wait_tip_changed(ctx, tip.hash, 5.0, &new_tip);

        if (!rc) {
            if (memcmp(new_tip.hash, tip.hash, 64)) {
                uint8_t prevbin[32];
                bool    report = false;
                int     latency = 0;
                tv_t    now;

                /* Stamped before anything else this tip triggers. */
                tv_time(&now);
                tip_to_display(new_tip.hash, tiphex);
                LOGNOTICE("JDC local tip %s height %d", tiphex, new_tip.height);
                /* Same order as an SV2 prev_hash, so it compares
                 * against pool_prev without flipping. */
                if (strlen(new_tip.hash) == 64 && hex2bin(prevbin, new_tip.hash, 32)) {
                    ensure_lock();
                    mutex_lock(&jdc_lock);
                    /* The pool got here first: it has been
                     * waiting on us, so the race is ours to
                     * close. */
                    if (sess.have_pool_tip && !memcmp(sess.pool_prev, prevbin, 32)) {
                        latency = ms_tvdiff(&now, &sess.pool_tip_tv);
                        report = true;
                    }
                    memcpy(sess.local_prev, prevbin, 32);
                    sess.local_tip_tv = now;
                    sess.have_local_tip = true;
                    mutex_unlock(&jdc_lock);
                    if (report)
                        log_tip_latency(prevbin, latency);
                }
                tip = new_tip;
                queue_build(true);
                continue;
            }
            /*
             * Same tip: due a fee-bump refresh if the interval has
             * elapsed (queue_build decides). Normally this is the wait
             * timing out, but it also returns immediately while
             * bitcoind has not connected a block since it started (its
             * notification tip is unset and so never compares equal to
             * ours), so it cannot be relied on for pacing — sleep
             * before asking again or we spin at IPC speed.
             */
            queue_build(false);
            cksleep_ms(SV2_JDC_TIP_POLL_MS);
            continue;
        }

        /* Non-zero means the connection was lost. */
        if (mining_ipc_try_obtain_mining(ctx)) {
            if (healthy) {
                LOGWARNING("JDC lost connection to mining IPC %s, reconnecting", ckpool.ipcmining);
                healthy = false;
            }
            cksleep_ms(1000);
            continue;
        }
        if (!healthy) {
            healthy = true;
            if (!mining_ipc_get_tip(ctx, &tip))
                queue_build(true);
            tip_to_display(tip.hash, tiphex);
            LOGWARNING("JDC reconnected to mining IPC %s tip %s height %d", ckpool.ipcmining, tiphex, tip.height);
        }
    }
out:
    /*
     * capnp clients are thread affine: disconnect here, on the thread that
     * owns the connection, so bitcoind sees an orderly RPC-level shutdown
     * rather than a socket closed under an in-flight request. Clearing the
     * context afterwards releases sv2_jdc_stop().
     */
    LOGNOTICE("JDC disconnecting mining IPC on shutdown");
    mining_ipc_disconnect(ctx);
    jdc_tip_ctx = NULL;
    return NULL;
}

void sv2_jdc_start(void) {
    pthread_t pth;
    int       i;

    if (!sv2_jdc_configured())
        return;
    if (!ckpool.ipcmining) {
        LOGWARNING("JDC configured but no ipcmining socket; no local templates");
        return;
    }
    /*
     * One session, bound to the first entry that configures a jds. A failover
     * list may hold several JD-capable pools, but only the entry currently
     * supplying work can be declared to (sess_declare checks), and custom work
     * goes to one channel in v1.
     */
    sess.fd = -1;
    sess.proxy_id = -1;
    for (i = 0; i < ckpool.proxies; i++) {
        if (ckpool.proxyjds[i]) {
            sess.proxy_id = i;
            break;
        }
    }
    if (sess.proxy_id < 0)
        return;
    if (access(ckpool.ipcmining, F_OK)) {
        LOGWARNING(
            "JDC mining IPC socket %s is not present yet; the tip waiter "
            "will keep retrying",
            ckpool.ipcmining);
    }
    ensure_lock();
    jdc_svc = mining_ipc_service_connect(ckpool.ipcmining);
    if (!jdc_svc) {
        LOGWARNING("JDC failed to start the mining IPC template service on %s", ckpool.ipcmining);
        return;
    }
    LOGNOTICE("JDC started mining IPC template service on %s", ckpool.ipcmining);
    jdc_buildq = create_ckmsgq("jdctmpl", &jdc_template_process);
    create_pthread(&pth, jdc_tipwaiter, NULL);
    if (jds_parse_url(ckpool.proxyjds[sess.proxy_id], &sess.host, &sess.port, sess.authority)) {
        snprintf(
            sess.user, sizeof(sess.user), "%s", ckpool.proxyauth[sess.proxy_id] ? ckpool.proxyauth[sess.proxy_id] : "");
        mutex_init(&sess.send_lock);
        LOGNOTICE("JDC declaring to %s:%s as %s (proxy %d)", sess.host, sess.port, sess.user, sess.proxy_id);
        create_pthread(&pth, jdc_session, NULL);
    } else {
        LOGWARNING("JDC unusable jds url for proxy %d: no job declaration", sess.proxy_id);
    }
}

/*
 * Called from the main thread once ckpool.shutdown is set, before the process
 * exits. Drops the template service, then waits — bounded well above the tip
 * thread's own 5s wait window — for that thread to disconnect on its own
 * capnp-affine thread. Safe to call unconditionally.
 */
void sv2_jdc_stop(void) {
    mining_ipc_service* svc = jdc_svc;
    int                 waited = 0;

    if (svc) {
        jdc_svc = NULL;
        mining_ipc_service_disconnect(svc);
    }
    while (jdc_tip_ctx && waited++ < 200)
        cksleep_ms(50);
}

#else /* !HAVE_CAPNP */

/* Job declaration needs the mining IPC for templates; config parsing refuses
 * a jds entry in a build without it, so these can only be no-ops. */

void sv2_jdc_start(void) {}

void sv2_jdc_stop(void) {}

struct sv2_jdc_template* sv2_jdc_template_current(void) {
    return NULL;
}

struct sv2_jdc_template* sv2_jdc_template_ref(struct sv2_jdc_template __maybe_unused* t) {
    return NULL;
}

void sv2_jdc_template_put(struct sv2_jdc_template __maybe_unused* t) {}

void sv2_jdc_custom_dropped(const char __maybe_unused* why) {}

void sv2_jdc_custom_result(bool __maybe_unused ok, uint32_t __maybe_unused job_id, const char __maybe_unused* code) {}

void sv2_jdc_solved(const struct sv2_jdc_solution __maybe_unused* sol) {}

enum sv2_tip_rel sv2_jdc_pool_tip(
    int __maybe_unused           proxy_id,
    const uint8_t __maybe_unused prev[32],
    uint32_t __maybe_unused      nbits,
    uint32_t __maybe_unused      min_ntime,
    const char**                 why) {
    if (why)
        *why = NULL;
    /* No local chain to compare against, so the proxy stays on pool work. */
    return SV2_TIP_NO_LOCAL;
}

void sv2_jdc_pool_tip_clear(int __maybe_unused proxy_id) {}

#endif /* HAVE_CAPNP */

#endif /* HAVE_SV2 */
