/*
 * Copyright 2026 Con Kolivas
 *
 * Phase 1 SV2 stratifier path: SetupConnection, open standard channels,
 * pool jobs, shares via real stratum instances (stats + block solve).
 */

#include "config.h"

#ifdef HAVE_SV2

#include <math.h>
#include <string.h>

#include "ckpool.h"
#include "libckpool.h"
#include "sha2.h"
#include "connector.h"
#include "stratifier.h"
#include "uthash.h"
#include "sv2_cb.h"
#include "sv2_codec.h"
#include "sv2_jd.h"
#include "sv2_strat.h"
#include "sv2_types.h"
#include "sv2_work.h"

struct sv2_client {
    UT_hash_handle hh;
    int64_t        client_id;
    bool           setup_ok;
    uint32_t       flags;
    uint32_t       next_channel_id;
    char           address[INET6_ADDRSTRLEN];
    int            server;
    /* refs: 1 while hashed; +1 per outstanding user outside the lock */
    int refs;
};

enum sv2_work_src {
    SV2_WORK_POOL = 0,
    SV2_WORK_CUSTOM = 1,
};

/* Batch successful shares into fewer SubmitShares.Success messages (spec
 * 5.3.13). Errors remain immediate. Flush on count, age, job change, or close. */
#define SV2_SUCCESS_BATCH_MAX 10
#define SV2_SUCCESS_BATCH_SECS 1.0

/* Recent jobs still accepted after tip update (parity with multi-workbase SV1). */
#define SV2_JOB_RING 8

/* Snapshot of an accepted custom job for share reconstruction */
struct sv2_custom_job {
    uint32_t job_id;
    uint32_t version;
    uint8_t  prev_hash[32]; /* as on wire (U256 LE / header-internal) */
    uint32_t min_ntime;
    uint32_t nbits;
    uint32_t coinbase_tx_version;
    uint8_t  coinbase_prefix[SV2_MAX_B0_255];
    uint8_t  coinbase_prefix_len;
    uint32_t coinbase_tx_input_nSequence;
    uint8_t* coinbase_tx_outputs;
    uint16_t coinbase_tx_outputs_len;
    uint32_t coinbase_tx_locktime;
    uint8_t  merkle_count;
    uint8_t  merkle_path[SV2_MAX_MERKLE_PATH][32];
    uint8_t  token[SV2_MAX_JOB_TOKEN];
    uint8_t  token_len;
};

/*
 * Share work for sshareq (same workers as SV1 mining.submit). Event thread
 * only decodes/looks up job and enqueues; hashing runs on sprocessor.
 */
struct sv2_share_job {
    int64_t                connector_id;
    int64_t                instance_id;
    int64_t                workbase_id;
    uint32_t               channel_id;
    uint32_t               sequence_number;
    uint32_t               ntime;
    uint32_t               nonce;
    uint32_t               version;
    double                 ch_diff;
    bool                   is_custom;
    bool                   is_standard;
    uint8_t                extranonce[32];
    uint8_t                extranonce_len;
    uint8_t                en1[16];
    uint8_t                en1_len;
    struct sv2_custom_job* custom; /* owned deep copy if is_custom */
};

struct sv2_job_slot {
    uint32_t job_id;
    int64_t  workbase_id;
    bool     used;
    bool     custom; /* SetCustomMiningJob job (not pool template) */
    /* Job nVersion as advertised (New*MiningJob / SetCustom). Used to
     * enforce §5.3.16 version_rolling_allowed on SubmitShares. */
    uint32_t version;
    bool     version_rolling; /* BIP320 / pool mask bits may differ */
    /* Owned copy of custom job material while this slot is live (custom).
     * JDC replaces custom jobs every few seconds; shares for the previous
     * job_id must still validate until the ring slot is overwritten. */
    struct sv2_custom_job* custom_job;
};

/* Full client_id (int64) + channel_id — no 32-bit truncation (collision risk). */
struct sv2_chan_key {
    int64_t  client_id;
    uint32_t channel_id;
};

struct sv2_channel {
    UT_hash_handle      hh;
    struct sv2_chan_key key; /* HASH key (memset padding when set) */
    int64_t             client_id;
    uint32_t            channel_id;
    int64_t             instance_id; /* stratum_instance for accounting */
    bool                standard;
    char                user_identity[SV2_MAX_STR_LEN + 1];
    double              diff;
    double              min_diff; /* highdiff-port floor (0 = none), see server_diff_floor() */
    uint8_t             enonce1[16];
    uint8_t             enonce1_len;
    uint16_t            extranonce_size; /* client-rolled bytes (extended only) */
    /* Downstream max_target (LE): SetTarget must not be easier than this */
    uint8_t                max_target[32];
    bool                   has_max_target;
    uint32_t               active_job_id;
    int64_t                workbase_id;
    uint32_t               job_seq;
    struct sv2_job_slot    jobs[SV2_JOB_RING];
    int                    job_ring_pos;
    enum sv2_work_src      work_src;
    struct sv2_custom_job* custom; /* latest custom (alias; owned by a slot) */
    /* refs: 1 while hashed; +1 per outstanding user outside the lock */
    int refs;
    /* Pending success batch (reset after each Success) */
    uint32_t batch_count;
    uint32_t batch_last_seq;
    uint64_t batch_shares_sum;
    tv_t     batch_start;
};

static struct sv2_client*  sv2_clients;
static struct sv2_channel* sv2_channels;
static mutex_t             sv2_lock;
static pthread_once_t      sv2_lock_once = PTHREAD_ONCE_INIT;

static void sv2_lock_init_once(void) {
    mutex_init(&sv2_lock);
}

static void ensure_lock(void) {
    pthread_once(&sv2_lock_once, sv2_lock_init_once);
}

static void flush_success_batch(struct sv2_channel* ch);
static void note_share_success(struct sv2_channel* ch, uint32_t sequence_number, uint64_t share_diff);
static void channel_clear_custom_locked(struct sv2_channel* ch);

static void chan_key_init(struct sv2_chan_key* k, int64_t client_id, uint32_t channel_id) {
    /* Zero padding so HASH key bytes are stable across platforms. */
    memset(k, 0, sizeof(*k));
    k->client_id = client_id;
    k->channel_id = channel_id;
}

/* Flush pending success batch for channel (no-op if empty). Takes sv2_lock
 * itself, which is not recursive — callers must NOT hold it. */
static void flush_success_batch(struct sv2_channel* ch);

/*
 * Caller holds sv2_lock. Drop one ref. Channel must already be unhashed when
 * the last ref drops. Returns ch for deferred free when refs hit zero (caller
 * must NOT free under lock / mid-HASH_ITER); NULL if still live.
 * Never unlocks sv2_lock (avoids uthash iteration UAF on drop paths).
 */
static struct sv2_channel* channel_unref_locked(struct sv2_channel* ch) {
    if (!ch)
        return NULL;
    if (--ch->refs > 0)
        return NULL;
    channel_clear_custom_locked(ch);
    return ch;
}

/* Flush any success batch then free channel (call outside sv2_lock). */
static void channel_finish_free(struct sv2_channel* ch) {
    if (!ch)
        return;
    flush_success_batch(ch);
    dealloc(ch);
}

/* Snapshot a live channel pointer for use outside the lock. */
static struct sv2_channel* channel_find_ref(int64_t client_id, uint32_t channel_id) {
    struct sv2_channel* ch;
    struct sv2_chan_key key;

    chan_key_init(&key, client_id, channel_id);
    ensure_lock();
    mutex_lock(&sv2_lock);
    HASH_FIND(hh, sv2_channels, &key, sizeof(key), ch);
    if (ch)
        ch->refs++;
    mutex_unlock(&sv2_lock);
    return ch;
}

static void channel_put(struct sv2_channel* ch) {
    struct sv2_channel* dead;

    if (!ch)
        return;
    ensure_lock();
    mutex_lock(&sv2_lock);
    dead = channel_unref_locked(ch);
    mutex_unlock(&sv2_lock);
    if (dead)
        channel_finish_free(dead);
}

static void free_custom_job(struct sv2_custom_job* cj) {
    if (!cj)
        return;
    dealloc(cj->coinbase_tx_outputs);
    dealloc(cj);
}

/* Record job_id → workbase_id (call with sv2_lock held).
 * For custom jobs, takes ownership of custom_job (may be NULL for pool jobs). */
static void channel_note_job_locked(
    struct sv2_channel*    ch,
    uint32_t               job_id,
    int64_t                workbase_id,
    bool                   custom,
    struct sv2_custom_job* custom_job,
    uint32_t               version,
    bool                   version_rolling) {
    struct sv2_job_slot* slot;

    ch->active_job_id = job_id;
    ch->workbase_id = workbase_id;
    slot = &ch->jobs[ch->job_ring_pos % SV2_JOB_RING];
    /* Overwriting a slot: free any previous custom material */
    if (slot->custom_job) {
        if (ch->custom == slot->custom_job)
            ch->custom = NULL;
        free_custom_job(slot->custom_job);
        slot->custom_job = NULL;
    }
    slot->job_id = job_id;
    slot->workbase_id = workbase_id;
    slot->used = true;
    slot->custom = custom;
    slot->version = version;
    slot->version_rolling = version_rolling;
    slot->custom_job = custom ? custom_job : NULL;
    if (custom && custom_job)
        ch->custom = custom_job;
    if (!custom)
        free_custom_job(custom_job);
    ch->job_ring_pos = (ch->job_ring_pos + 1) % SV2_JOB_RING;
}

/* Resolve job_id to workbase / custom material (call with sv2_lock held). */
static bool channel_lookup_job_locked(
    struct sv2_channel*     ch,
    uint32_t                job_id,
    int64_t*                wb_out,
    bool*                   custom_out,
    struct sv2_custom_job** cj_out,
    uint32_t*               version_out,
    bool*                   rolling_out) {
    int i;

    if (cj_out)
        *cj_out = NULL;
    if (version_out)
        *version_out = 0;
    if (rolling_out)
        *rolling_out = false;
    for (i = 0; i < SV2_JOB_RING; i++) {
        if (ch->jobs[i].used && ch->jobs[i].job_id == job_id) {
            *wb_out = ch->jobs[i].workbase_id;
            if (custom_out)
                *custom_out = ch->jobs[i].custom;
            if (cj_out && ch->jobs[i].custom)
                *cj_out = ch->jobs[i].custom_job;
            if (version_out)
                *version_out = ch->jobs[i].version;
            if (rolling_out)
                *rolling_out = ch->jobs[i].version_rolling;
            return true;
        }
    }
    return false;
}

/*
 * Spec §5.3.16 / §5.3.15 / §5.3.18: SubmitShares carries full nVersion.
 * When version_rolling is false the client MUST use the job version as-is.
 * When true, only bits in ckpool.version_mask (BIP320 / pool policy) may
 * differ from the job version — same rule as SV1 invalid-version-mask.
 */
static bool sv2_submit_version_ok(uint32_t submitted, uint32_t job_version, bool version_rolling) {
    if (!version_rolling)
        return submitted == job_version;
    return ((submitted ^ job_version) & ~ckpool.version_mask) == 0;
}

/* Drop all custom jobs (tip change). Pool-template slots stay. */
static void channel_clear_custom_locked(struct sv2_channel* ch) {
    int i;

    ch->custom = NULL;
    ch->work_src = SV2_WORK_POOL;
    for (i = 0; i < SV2_JOB_RING; i++) {
        if (ch->jobs[i].custom_job) {
            free_custom_job(ch->jobs[i].custom_job);
            ch->jobs[i].custom_job = NULL;
        }
        if (ch->jobs[i].used && ch->jobs[i].custom)
            ch->jobs[i].used = false;
    }
}

/* Caller holds sv2_lock. Free client when last ref drops (must be unhashed). */
static void client_unref_locked(struct sv2_client* c) {
    if (!c)
        return;
    if (--c->refs > 0)
        return;
    dealloc(c);
}

/*
 * Look up or create a client and take a ref for use outside sv2_lock.
 * Caller must client_put() when done.
 */
static struct sv2_client* client_get(int64_t client_id, bool create) {
    struct sv2_client* c;

    ensure_lock();
    mutex_lock(&sv2_lock);
    HASH_FIND_I64(sv2_clients, &client_id, c);
    if (!c && create) {
        c = ckzalloc(sizeof(*c));
        c->client_id = client_id;
        c->next_channel_id = 1;
        c->refs = 1; /* hashed */
        HASH_ADD_I64(sv2_clients, client_id, c);
    }
    if (c)
        c->refs++;
    mutex_unlock(&sv2_lock);
    return c;
}

static void client_put(struct sv2_client* c) {
    if (!c)
        return;
    ensure_lock();
    mutex_lock(&sv2_lock);
    client_unref_locked(c);
    mutex_unlock(&sv2_lock);
}

void sv2_strat_drop_client(int64_t client_id) {
    struct sv2_client*   c;
    struct sv2_channel * ch, *tmp, *dead;
    struct sv2_channel** to_free = NULL;
    int                  nfree = 0, i;
    int64_t*             iids = NULL;
    int                  niids = 0;

    ensure_lock();
    mutex_lock(&sv2_lock);
    HASH_FIND_I64(sv2_clients, &client_id, c);
    if (c) {
        /* Unhash and drop the table ref; handlers may still hold refs. */
        HASH_DEL(sv2_clients, c);
        client_unref_locked(c);
    }
    /*
     * Two-phase free: under lock only HASH_DEL + unref. Last-ref channels
     * are collected and freed after unlock so unref never unlocks mid-ITER.
     */
    HASH_ITER(hh, sv2_channels, ch, tmp) {
        if (ch->client_id == client_id) {
            if (ch->instance_id) {
                iids = ckrealloc(iids, (niids + 1) * sizeof(*iids));
                iids[niids++] = ch->instance_id;
            }
            HASH_DEL(sv2_channels, ch);
            dead = channel_unref_locked(ch);
            if (dead) {
                to_free = ckrealloc(to_free, (nfree + 1) * sizeof(*to_free));
                to_free[nfree++] = dead;
            }
        }
    }
    mutex_unlock(&sv2_lock);
    for (i = 0; i < nfree; i++)
        channel_finish_free(to_free[i]);
    dealloc(to_free);
    for (i = 0; i < niids; i++)
        stratifier_sv2_close_session(iids[i]);
    dealloc(iids);
}

void sv2_strat_drop_all(void) {
    struct sv2_client *  c, *ctmp;
    struct sv2_channel * ch, *chtmp, *dead;
    struct sv2_channel** to_free = NULL;
    int                  nfree = 0, i;
    int64_t*             iids = NULL;
    int                  niids = 0;

    ensure_lock();
    mutex_lock(&sv2_lock);
    HASH_ITER(hh, sv2_channels, ch, chtmp) {
        if (ch->instance_id) {
            iids = ckrealloc(iids, (niids + 1) * sizeof(*iids));
            iids[niids++] = ch->instance_id;
        }
        HASH_DEL(sv2_channels, ch);
        dead = channel_unref_locked(ch);
        if (dead) {
            to_free = ckrealloc(to_free, (nfree + 1) * sizeof(*to_free));
            to_free[nfree++] = dead;
        }
    }
    HASH_ITER(hh, sv2_clients, c, ctmp) {
        HASH_DEL(sv2_clients, c);
        client_unref_locked(c);
    }
    mutex_unlock(&sv2_lock);
    for (i = 0; i < nfree; i++)
        channel_finish_free(to_free[i]);
    dealloc(to_free);
    for (i = 0; i < niids; i++)
        stratifier_sv2_close_session(iids[i]);
    dealloc(iids);
    if (niids)
        LOGNOTICE("SV2 drop_all: closed %d virtual session(s)", niids);
}

static uint8_t*
reply_frame(uint8_t msg_type, bool channel_msg, const uint8_t* payload, size_t payload_len, size_t* replylen) {
    uint16_t ext = channel_msg ? SV2_CHANNEL_MSG_BIT : 0;
    uint8_t* frame = NULL;

    if (!sv2_build_frame(ext, msg_type, payload, (uint32_t)payload_len, &frame, replylen))
        return NULL;
    return frame;
}

static void
queue_frame(int64_t client_id, uint8_t msg_type, bool channel_msg, const uint8_t* payload, size_t payload_len) {
    size_t   flen = 0;
    uint8_t* frame = reply_frame(msg_type, channel_msg, payload, payload_len, &flen);

    if (frame)
        connector_sv2_send_plain(client_id, frame, flen);
}

/* Flush pending success batch for channel (no-op if empty). Takes sv2_lock
 * itself, which is not recursive — callers must NOT hold it. */
static void flush_success_batch(struct sv2_channel* ch) {
    struct sv2_submit_shares_success ok;
    uint8_t                          pbuf[64];
    size_t                           plen = 0;
    int64_t                          client_id;
    uint32_t                         channel_id, last_seq, count;
    uint64_t                         shares_sum;

    if (!ch)
        return;
    ensure_lock();
    mutex_lock(&sv2_lock);
    if (!ch->batch_count) {
        mutex_unlock(&sv2_lock);
        return;
    }
    client_id = ch->client_id;
    channel_id = ch->channel_id;
    last_seq = ch->batch_last_seq;
    count = ch->batch_count;
    shares_sum = ch->batch_shares_sum;
    ch->batch_count = 0;
    /* Keep batch_last_seq as a session high-water mark so a post-flush
     * straggler with a lower seq cannot recede last_sequence_number. */
    ch->batch_shares_sum = 0;
    memset(&ch->batch_start, 0, sizeof(ch->batch_start));
    mutex_unlock(&sv2_lock);

    memset(&ok, 0, sizeof(ok));
    ok.channel_id = channel_id;
    ok.last_sequence_number = last_seq;
    ok.new_submits_accepted_count = count;
    ok.new_shares_sum = shares_sum;
    if (sv2_encode_submit_shares_success(pbuf, sizeof(pbuf), &plen, &ok))
        queue_frame(client_id, SV2_MSG_SUBMIT_SHARES_SUCCESS, true, pbuf, plen);
}

/* Record an accepted share; may send SubmitShares.Success when batch is full
 * or aged out. share_diff is the difficulty credited for this share. */
static void note_share_success(struct sv2_channel* ch, uint32_t sequence_number, uint64_t share_diff) {
    tv_t   now;
    double age;
    bool   do_flush = false;

    if (!ch)
        return;
    tv_time(&now);
    ensure_lock();
    mutex_lock(&sv2_lock);
    if (!ch->batch_count)
        copy_tv(&ch->batch_start, &now);
    ch->batch_count++;
    /* Highest accepted seq (sshareq can process shares out of order). */
    if (sequence_number > ch->batch_last_seq)
        ch->batch_last_seq = sequence_number;
    ch->batch_shares_sum += share_diff ? share_diff : 1;
    age = tvdiff(&now, &ch->batch_start);
    if (ch->batch_count >= SV2_SUCCESS_BATCH_MAX || age >= SV2_SUCCESS_BATCH_SECS)
        do_flush = true;
    mutex_unlock(&sv2_lock);
    if (do_flush)
        flush_success_batch(ch);
}

/* Compare LE U256 targets: >0 if a is easier (larger) than b. */
static int target_cmp_le(const uint8_t a[32], const uint8_t b[32]) {
    int i;

    for (i = 31; i >= 0; i--) {
        if (a[i] != b[i])
            return (int)a[i] - (int)b[i];
    }
    return 0;
}

/*
 * A server listening on an even-numbered port is a highdiff port: like SV1,
 * sessions on it float no lower than ckpool.highdiff. Returns the floor (0 if
 * not highdiff).
 */
static double server_diff_floor(int server) {
    if (ckpool.server_highdiff && server >= 0 && server < ckpool.serverurls && ckpool.server_highdiff[server])
        return (double)ckpool.highdiff;
    return 0.0;
}

static void choose_channel_diff(float nominal_hr, const uint8_t max_target[32], double floor, double* diff_out) {
    double diff = (double)ckpool.startdiff;
    double mindiff = (double)ckpool.mindiff;
    double hr;
    double max_diff;

    /* Highdiff port raises the effective minimum (never below ckpool.mindiff). */
    if (floor > mindiff)
        mindiff = floor;

    /* Wire f32 is untrusted — reject non-finite / negative before math. */
    if (isfinite((double)nominal_hr) && nominal_hr > 0.0f)
        hr = (double)nominal_hr;
    else
        hr = 0.0;

    if (hr > 0) {
        /* ~6 shares/min: want = hr / 2^32 * 10 */
        double shares_per_sec = hr / 4294967296.0;
        double want = shares_per_sec * 10.0;

        if (isfinite(want) && want > 1.0)
            diff = want;
    }
    if (diff < mindiff)
        diff = mindiff;
    if (ckpool.maxdiff && diff > (double)ckpool.maxdiff)
        diff = (double)ckpool.maxdiff;
    /*
     * max_target is the easiest allowed target (largest U256) → minimum
     * allowed difficulty. Harder (higher diff / smaller target) is always
     * fine; never go easier than max_target.
     */
    max_diff = diff_from_target((uchar*)max_target);
    if (isfinite(max_diff) && max_diff > 0 && diff < max_diff)
        diff = max_diff;
    if (!isfinite(diff) || diff < 1.0)
        diff = 1.0;
    *diff_out = diff;
}

static void assign_enonce1(struct sv2_channel* ch, int64_t client_id, uint32_t ch_id) {
    uint32_t mixed;

    ch->enonce1_len = (uint8_t)ckpool.nonce1length;
    if (ch->enonce1_len < 2 || ch->enonce1_len > 8)
        ch->enonce1_len = 4;
    memset(ch->enonce1, 0, sizeof(ch->enonce1));
    /*
     * Spec §5.1 / §5.2: each channel must receive a unique extranonce
     * subset. Allocate from the same global counter SV1 new_enonce1 uses
     * so no two SV1 clients or SV2 channels can ever share a prefix on
     * the same workbase.
     */
    if (stratifier_sv2_alloc_enonce1(ch->enonce1, ch->enonce1_len))
        return;
    /* Stratifier not up yet — fall back to client/channel mix. Distinct
     * channels on one connection always differ: d ^ (d << 16) != 0. */
    mixed = (uint32_t)client_id ^ ch_id ^ (ch_id << 16);
    ch->enonce1[0] = (uint8_t)(mixed & 0xff);
    ch->enonce1[1] = (uint8_t)((mixed >> 8) & 0xff);
    if (ch->enonce1_len > 2) {
        ch->enonce1[2] = (uint8_t)((mixed >> 16) & 0xff);
        if (ch->enonce1_len > 3)
            ch->enonce1[3] = (uint8_t)((mixed >> 24) & 0xff);
    }
    if (ch->enonce1_len > 4) {
        /* Extra room: raw channel_id for stronger separation */
        ch->enonce1[4] = (uint8_t)(ch_id & 0xff);
        ch->enonce1[5] = (uint8_t)((ch_id >> 8) & 0xff);
        ch->enonce1[6] = (uint8_t)((ch_id >> 16) & 0xff);
        ch->enonce1[7] = (uint8_t)((ch_id >> 24) & 0xff);
    }
}

static void push_pool_job_standard(struct sv2_channel* ch, bool future_first) {
    struct sv2_new_mining_job    job;
    struct sv2_set_new_prev_hash ph;
    uint8_t                      pbuf[256];
    size_t                       plen = 0;
    uint32_t                     job_id, version, ntime, nbits;
    int64_t                      wb_id;

    memset(&job, 0, sizeof(job));
    memset(&ph, 0, sizeof(ph));
    if (!stratifier_sv2_merkle_root(ch->instance_id, job.merkle_root, &wb_id, &version, &ntime, &nbits, ph.prev_hash)) {
        LOGINFO("SV2 no work for ch %u client %" PRId64, ch->channel_id, ch->client_id);
        return;
    }

    ensure_lock();
    mutex_lock(&sv2_lock);
    job_id = ++ch->job_seq;
    if (!job_id)
        job_id = ++ch->job_seq;
    /* NewMiningJob: BIP320 bits may be rolled (mask is pool policy). */
    channel_note_job_locked(ch, job_id, wb_id, false, NULL, version, ckpool.version_mask != 0);
    mutex_unlock(&sv2_lock);

    job.channel_id = ch->channel_id;
    job.job_id = job_id;
    job.min_ntime_present = !future_first;
    job.min_ntime = ntime;
    job.version = version;

    if (!sv2_encode_new_mining_job(pbuf, sizeof(pbuf), &plen, &job))
        return;
    queue_frame(ch->client_id, SV2_MSG_NEW_MINING_JOB, true, pbuf, plen);

    if (future_first) {
        ph.channel_id = ch->channel_id;
        ph.job_id = job_id;
        ph.min_ntime = ntime;
        ph.nbits = nbits;
        if (!sv2_encode_set_new_prev_hash(pbuf, sizeof(pbuf), &plen, &ph))
            return;
        queue_frame(ch->client_id, SV2_MSG_SET_NEW_PREV_HASH, true, pbuf, plen);
    }

    LOGDEBUG(
        "SV2 standard job %u wb %" PRId64 " client %" PRId64 " ch %u future=%d", job_id, wb_id, ch->client_id,
        ch->channel_id, future_first);
}

/* Extended: NewExtendedMiningJob carries coinbase prefix/suffix + merkle path */
static void push_pool_job_extended(struct sv2_channel* ch, bool future_first) {
    struct sv2_work_snap               snap;
    struct sv2_new_extended_mining_job ej;
    struct sv2_set_new_prev_hash       ph;
    uint8_t*                           pbuf = NULL;
    size_t                             plen = 0, bufsz;
    uint32_t                           job_id;
    bool                               rolling;
    int                                i;

    if (!stratifier_sv2_snapshot_work(&snap, ch->instance_id)) {
        LOGINFO("SV2 no work for extended ch %u client %" PRId64, ch->channel_id, ch->client_id);
        return;
    }

    rolling = (ckpool.version_mask != 0);

    ensure_lock();
    mutex_lock(&sv2_lock);
    job_id = ++ch->job_seq;
    if (!job_id)
        job_id = ++ch->job_seq;
    channel_note_job_locked(ch, job_id, snap.wb_id, false, NULL, snap.version, rolling);
    mutex_unlock(&sv2_lock);

    memset(&ej, 0, sizeof(ej));
    ej.channel_id = ch->channel_id;
    ej.job_id = job_id;
    ej.min_ntime_present = !future_first;
    ej.min_ntime = snap.ntime;
    ej.version = snap.version;
    ej.version_rolling_allowed = rolling;
    /* Clamp: snap.merkles can exceed SV2_MAX_MERKLE_PATH in theory */
    ej.merkle_count = (uint8_t)(snap.merkles < SV2_MAX_MERKLE_PATH ? snap.merkles : SV2_MAX_MERKLE_PATH);
    for (i = 0; i < ej.merkle_count; i++)
        memcpy(ej.merkle_path[i], snap.merklebin[i], 32);
    ej.coinbase_tx_prefix = snap.coinb1;
    ej.coinbase_tx_prefix_len = (uint16_t)snap.coinb1len;
    ej.coinbase_tx_suffix = snap.coinb2;
    ej.coinbase_tx_suffix_len = (uint16_t)snap.coinb2len;

    bufsz = 64 + snap.coinb1len + snap.coinb2len + snap.merkles * 32 + 64;
    pbuf = ckalloc(bufsz);
    if (!sv2_encode_new_extended_mining_job(pbuf, bufsz, &plen, &ej)) {
        dealloc(pbuf);
        return;
    }
    queue_frame(ch->client_id, SV2_MSG_NEW_EXTENDED_MINING_JOB, true, pbuf, plen);
    dealloc(pbuf);

    /*
     * SetNewPrevHash only activates future jobs (min_ntime absent). For an
     * immediate job it must not be sent — clients reject SNPH when job_id
     * is not in future_jobs (e.g. tProxy JobIdNotFound).
     */
    if (future_first) {
        memset(&ph, 0, sizeof(ph));
        ph.channel_id = ch->channel_id;
        ph.job_id = job_id;
        memcpy(ph.prev_hash, snap.prevhash, 32);
        ph.min_ntime = snap.ntime;
        ph.nbits = snap.nbits;
        {
            uint8_t phbuf[80];
            size_t  phlen = 0;

            if (sv2_encode_set_new_prev_hash(phbuf, sizeof(phbuf), &phlen, &ph))
                queue_frame(ch->client_id, SV2_MSG_SET_NEW_PREV_HASH, true, phbuf, phlen);
        }
    }

    LOGDEBUG(
        "SV2 extended job %u wb %" PRId64 " client %" PRId64 " ch %u future=%d", job_id, snap.wb_id, ch->client_id,
        ch->channel_id, future_first);
}

static void push_pool_job(struct sv2_channel* ch, bool future_first) {
    /* New job invalidates old work; flush any pending success batch first
     * so sequence numbers stay meaningful to the client. */
    flush_success_batch(ch);
    if (ch->standard)
        push_pool_job_standard(ch, future_first);
    else
        push_pool_job_extended(ch, future_first);
}

/* Build SetTarget from channel state under sv2_lock (diff / max_target races). */
static void push_set_target(struct sv2_channel* ch) {
    struct sv2_set_target st;
    uint8_t               pbuf[64];
    uint8_t               wire_target[32];
    uint8_t               max_target[32];
    size_t                plen = 0;
    double                diff;
    bool                  has_max;
    uint32_t              channel_id;
    int64_t               client_id;

    if (!ch)
        return;
    ensure_lock();
    mutex_lock(&sv2_lock);
    diff = ch->diff;
    has_max = ch->has_max_target;
    if (has_max)
        memcpy(max_target, ch->max_target, 32);
    channel_id = ch->channel_id;
    client_id = ch->client_id;
    /*
     * Never return a target easier than the downstream max_target
     * (tProxy rejects SetTarget if target > UpdateChannel.maximum_target).
     * target_from_diff float round-trip can slightly overshoot.
     */
    target_from_diff(wire_target, diff);
    if (has_max && target_cmp_le(wire_target, max_target) > 0) {
        memcpy(wire_target, max_target, 32);
        diff = diff_from_target(max_target);
        if (!isfinite(diff) || diff < 1.0)
            diff = 1.0;
        ch->diff = diff;
    }
    mutex_unlock(&sv2_lock);

    memset(&st, 0, sizeof(st));
    st.channel_id = channel_id;
    memcpy(st.maximum_target, wire_target, 32);
    if (!sv2_encode_set_target(pbuf, sizeof(pbuf), &plen, &st))
        return;
    queue_frame(client_id, SV2_MSG_SET_TARGET, true, pbuf, plen);
}

static uint8_t* handle_setup(struct sv2_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_setup_connection         sc;
    struct sv2_setup_connection_success ok;
    struct sv2_setup_connection_error   err;
    uint8_t                             pbuf[64];
    size_t                              plen = 0;

    if (!sv2_decode_setup_connection(payload, len, &sc))
        return NULL;
    if (sc.protocol != SV2_PROTOCOL_MINING) {
        memset(&err, 0, sizeof(err));
        snprintf(err.error_code, sizeof(err.error_code), "unsupported-protocol");
        if (!sv2_encode_setup_connection_error(pbuf, sizeof(pbuf), &plen, &err))
            return NULL;
        return reply_frame(SV2_MSG_SETUP_CONNECTION_ERROR, false, pbuf, plen, replylen);
    }
    if (sc.min_version > 2 || sc.max_version < 2) {
        /* Spec §3.6.3: flags = 0 signals a non-flag condition. */
        memset(&err, 0, sizeof(err));
        snprintf(err.error_code, sizeof(err.error_code), "protocol-version-mismatch");
        if (!sv2_encode_setup_connection_error(pbuf, sizeof(pbuf), &plen, &err))
            return NULL;
        return reply_frame(SV2_MSG_SETUP_CONNECTION_ERROR, false, pbuf, plen, replylen);
    }
    /*
     * Spec §3.6.3: the Error MUST carry the full set of flags we do not
     * support, not just the first one found, so clients probing with all
     * flags set learn our capabilities in one round trip.
     * Work selection (SetCustomMiningJob) only when JD listen is enabled;
     * §5.3.1: MUST NOT send non-rolling jobs to a REQUIRES_VERSION_ROLLING
     * client, so refuse it when no version mask is configured.
     */
    {
        uint32_t bad_flags = 0;

        if ((sc.flags & SV2_FLAG_REQUIRES_WORK_SELECTION) && !sv2_jd_enabled())
            bad_flags |= SV2_FLAG_REQUIRES_WORK_SELECTION;
        if ((sc.flags & SV2_FLAG_REQUIRES_VERSION_ROLLING) && !ckpool.version_mask)
            bad_flags |= SV2_FLAG_REQUIRES_VERSION_ROLLING;
        if (bad_flags) {
            memset(&err, 0, sizeof(err));
            err.flags = bad_flags;
            snprintf(err.error_code, sizeof(err.error_code), "unsupported-feature-flags");
            if (!sv2_encode_setup_connection_error(pbuf, sizeof(pbuf), &plen, &err))
                return NULL;
            return reply_frame(SV2_MSG_SETUP_CONNECTION_ERROR, false, pbuf, plen, replylen);
        }
    }

    c->setup_ok = true;
    c->flags = sc.flags;
    memset(&ok, 0, sizeof(ok));
    ok.used_version = 2;
    if (!sv2_encode_setup_connection_success(pbuf, sizeof(pbuf), &plen, &ok))
        return NULL;
    LOGNOTICE(
        "SV2 SetupConnection ok client %" PRId64 " vendor=%s%s", c->client_id, sc.vendor,
        (sc.flags & SV2_FLAG_REQUIRES_WORK_SELECTION) ? " work-selection" : "");
    return reply_frame(SV2_MSG_SETUP_CONNECTION_SUCCESS, false, pbuf, plen, replylen);
}

static uint8_t* handle_open_standard(struct sv2_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_open_standard_channel         o;
    struct sv2_open_standard_channel_success ok;
    struct sv2_open_channel_error            err;
    struct sv2_channel*                      ch;
    uint8_t                                  pbuf[128];
    size_t                                   plen = 0;
    uint32_t                                 ch_id;
    double                                   diff, floor;
    int64_t                                  instance_id = 0;

    if (!c->setup_ok)
        return NULL;
    if (!sv2_decode_open_standard_channel(payload, len, &o))
        return NULL;

    floor = server_diff_floor(c->server);
    choose_channel_diff(o.nominal_hash_rate, o.max_target, floor, &diff);
    ch_id = c->next_channel_id++;
    if (!ch_id)
        ch_id = c->next_channel_id++;

    ch = ckzalloc(sizeof(*ch));
    ch->client_id = c->client_id;
    ch->channel_id = ch_id;
    ch->min_diff = floor;
    ch->standard = true;
    ch->diff = diff;
    memcpy(ch->max_target, o.max_target, 32);
    ch->has_max_target = true;
    assign_enonce1(ch, c->client_id, ch_id);
    ch->extranonce_size = 0;
    snprintf(ch->user_identity, sizeof(ch->user_identity), "%s", o.user_identity);
    chan_key_init(&ch->key, c->client_id, ch_id);

    if (!stratifier_sv2_open_session(
            c->client_id, ch_id, o.user_identity, c->address, c->server, ch->enonce1, ch->enonce1_len, diff,
            &instance_id)) {
        dealloc(ch);
        memset(&err, 0, sizeof(err));
        err.request_id = o.request_id;
        snprintf(err.error_code, sizeof(err.error_code), ckpool.btcsolo ? "invalid-username" : "unauthorized-worker");
        if (!sv2_encode_open_channel_error(pbuf, sizeof(pbuf), &plen, &err))
            return NULL;
        return reply_frame(SV2_MSG_OPEN_MINING_CHANNEL_ERROR, false, pbuf, plen, replylen);
    }
    ch->instance_id = instance_id;

    /*
     * refs = 2: table pin + open-handler pin. Drop can unhash the table
     * pin without freeing while we still run push_pool_job / SetTarget.
     */
    ensure_lock();
    mutex_lock(&sv2_lock);
    ch->refs = 2;
    HASH_ADD(hh, sv2_channels, key, sizeof(ch->key), ch);
    memset(&ok, 0, sizeof(ok));
    ok.request_id = o.request_id;
    ok.channel_id = ch_id;
    target_from_diff(ok.target, ch->diff);
    if (target_cmp_le(ok.target, ch->max_target) > 0) {
        memcpy(ok.target, ch->max_target, 32);
        ch->diff = diff_from_target(ch->max_target);
        if (!isfinite(ch->diff) || ch->diff < 1.0)
            ch->diff = 1.0;
    }
    ok.extranonce_prefix_len = ch->enonce1_len;
    memcpy(ok.extranonce_prefix, ch->enonce1, ch->enonce1_len);
    ok.group_channel_id = 0;
    mutex_unlock(&sv2_lock);

    if (!sv2_encode_open_standard_channel_success(pbuf, sizeof(pbuf), &plen, &ok)) {
        struct sv2_channel* dead = NULL;

        ensure_lock();
        mutex_lock(&sv2_lock);
        HASH_DEL(sv2_channels, ch);
        /* Drop table + handler pins; free after unlock. */
        (void)channel_unref_locked(ch);
        dead = channel_unref_locked(ch);
        mutex_unlock(&sv2_lock);
        if (dead)
            channel_finish_free(dead);
        stratifier_sv2_close_session(instance_id);
        return NULL;
    }

    LOGNOTICE(
        "SV2 OpenStandard ch=%u client %" PRId64 " inst %" PRId64 " user=%s diff=%.1f", ch_id, c->client_id,
        instance_id, o.user_identity, diff);

    /*
     * Open*.Success must hit the wire before jobs/targets.
     * queue_frame enqueues immediately; a returned reply is sent only after
     * this handler returns — which reorders messages and breaks clients
     * (e.g. official tProxy ChannelNotFound on early SetTarget).
     *
     * Spec §5.3.15: NewMiningJob MUST be the first message after the
     * channel has been successfully opened (Open.Success already carries
     * the initial target). Order: Success → future job + SNPH → SetTarget.
     */
    queue_frame(c->client_id, SV2_MSG_OPEN_STANDARD_MINING_CHANNEL_SUCCESS, false, pbuf, plen);
    push_pool_job(ch, true);
    push_set_target(ch);
    channel_put(ch); /* drop open-handler pin; table pin remains */
    *replylen = 0;
    return NULL;
}

static void free_share_job(struct sv2_share_job* job) {
    if (!job)
        return;
    free_custom_job(job->custom);
    dealloc(job);
}

static bool validate_custom_share(
    const struct sv2_custom_job* cj,
    const uint8_t*               enonce1,
    uint8_t                      en1len,
    const uint8_t*               extranonce,
    uint16_t                     enlen,
    uint32_t                     ntime,
    uint32_t                     nonce,
    uint32_t                     version,
    uchar                        hash[32],
    double*                      sdiff_out,
    char*                        err,
    size_t                       errsz);

/* Immediate SubmitShares.Error from the event thread (pre-hash rejects). */
static uint8_t* submit_error_reply(uint32_t channel_id, uint32_t seq, const char* code, size_t* replylen) {
    struct sv2_submit_shares_error err;
    uint8_t                        pbuf[64];
    size_t                         plen = 0;

    memset(&err, 0, sizeof(err));
    err.channel_id = channel_id;
    err.sequence_number = seq;
    snprintf(err.error_code, sizeof(err.error_code), "%s", code ? code : "invalid-share");
    if (!sv2_encode_submit_shares_error(pbuf, sizeof(pbuf), &plen, &err))
        return NULL;
    return reply_frame(SV2_MSG_SUBMIT_SHARES_ERROR, true, pbuf, plen, replylen);
}

static bool enqueue_share_job(struct sv2_share_job* job) {
    if (stratifier_queue_share_work(true, job))
        return true;
    free_share_job(job);
    return false;
}

/*
 * sshareq worker: PoW + account + reply. Same threads as SV1 mining.submit.
 */
void sv2_strat_process_share_job(void* jobp) {
    struct sv2_share_job* job = jobp;
    struct sv2_channel*   ch;
    char                  ebuf[64];
    double                sdiff = 0;
    uint8_t               pbuf[64];
    size_t                plen = 0;

    if (!job)
        return;
    ebuf[0] = '\0';
    ch = channel_find_ref(job->connector_id, job->channel_id);

    if (job->is_custom) {
        uchar hash[32];
        bool  netdiff = false;

        if (!job->custom) {
            snprintf(ebuf, sizeof(ebuf), "invalid-job-id");
            goto err;
        }
        if (!validate_custom_share(
                job->custom, job->en1, job->en1_len, job->extranonce, job->extranonce_len, job->ntime, job->nonce,
                job->version, hash, &sdiff, ebuf, sizeof(ebuf)))
            goto err;
        if (!stratifier_sv2_account_share(
                job->instance_id, job->workbase_id, hash, sdiff, ebuf, sizeof(ebuf), &netdiff))
            goto err;
        if (netdiff && job->custom->token_len) {
            uint8_t* blk = NULL;
            size_t   blen = 0;
            uint8_t  full_en[32];
            uint8_t  full_len;

            full_len = (uint8_t)(job->en1_len + job->extranonce_len);
            if (full_len <= sizeof(full_en) && (unsigned)job->en1_len + job->extranonce_len == full_len) {
                memcpy(full_en, job->en1, job->en1_len);
                memcpy(full_en + job->en1_len, job->extranonce, job->extranonce_len);
                if (sv2_jd_rebuild_solved_block(
                        job->custom->token, job->custom->token_len, full_en, full_len, job->version, job->ntime,
                        job->nonce, job->custom->nbits, job->custom->prev_hash, &blk, &blen)) {
                    bool acc = stratifier_sv2_submit_block_bin(blk, blen, job->instance_id, NULL);

                    LOGWARNING(
                        "SV2 custom block solve client %" PRId64 " job %u %zu bytes enonce=%u %s", job->connector_id,
                        job->custom->job_id, blen, full_len, acc ? "ACCEPTED" : "rejected/error");
                    dealloc(blk);
                } else {
                    LOGWARNING(
                        "SV2 custom network-diff share but "
                        "block rebuild failed (token miss / "
                        "tx cache); PushSolution may still submit");
                }
            }
        }
        if (ch)
            note_share_success(ch, job->sequence_number, (uint64_t)job->ch_diff);
        channel_put(ch);
        free_share_job(job);
        return;
    }

    {
        char nonce2hex[64];

        if (job->is_standard || !job->extranonce_len) {
            if (!stratifier_sv2_submit_share(
                    job->instance_id, job->workbase_id, job->ntime, job->nonce, job->version, NULL, ebuf, sizeof(ebuf),
                    &sdiff))
                goto err;
        } else {
            if (job->extranonce_len * 2 >= sizeof(nonce2hex)) {
                snprintf(ebuf, sizeof(ebuf), "invalid-extranonce-size");
                goto err;
            }
            __bin2hex(nonce2hex, job->extranonce, job->extranonce_len);
            if (!stratifier_sv2_submit_share(
                    job->instance_id, job->workbase_id, job->ntime, job->nonce, job->version, nonce2hex, ebuf,
                    sizeof(ebuf), &sdiff))
                goto err;
        }
        if (ch)
            note_share_success(ch, job->sequence_number, (uint64_t)job->ch_diff);
        channel_put(ch);
        free_share_job(job);
        (void)sdiff;
        return;
    }

err:
    if (ch) {
        flush_success_batch(ch);
        channel_put(ch);
    }
    {
        struct sv2_submit_shares_error err;

        memset(&err, 0, sizeof(err));
        err.channel_id = job->channel_id;
        err.sequence_number = job->sequence_number;
        snprintf(err.error_code, sizeof(err.error_code), "%s", ebuf[0] ? ebuf : "invalid-share");
        if (sv2_encode_submit_shares_error(pbuf, sizeof(pbuf), &plen, &err))
            queue_frame(job->connector_id, SV2_MSG_SUBMIT_SHARES_ERROR, true, pbuf, plen);
    }
    free_share_job(job);
}

static uint8_t* handle_submit_standard(struct sv2_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_submit_shares_standard sub;
    struct sv2_channel*               ch;
    struct sv2_share_job*             job;
    int64_t                           instance_id = 0, workbase_id = 0;
    double                            ch_diff = 0;
    bool                              ok_job = false;
    uint32_t                          job_version = 0;
    bool                              version_rolling = false;

    *replylen = 0;
    if (!sv2_decode_submit_shares_standard(payload, len, &sub))
        return NULL;

    ch = channel_find_ref(c->client_id, sub.channel_id);
    if (!ch || !ch->standard || !ch->instance_id) {
        channel_put(ch);
        return submit_error_reply(sub.channel_id, sub.sequence_number, "invalid-channel-id", replylen);
    }

    ensure_lock();
    mutex_lock(&sv2_lock);
    ok_job = channel_lookup_job_locked(ch, sub.job_id, &workbase_id, NULL, NULL, &job_version, &version_rolling);
    instance_id = ch->instance_id;
    ch_diff = ch->diff;
    mutex_unlock(&sv2_lock);
    channel_put(ch);

    if (!ok_job)
        return submit_error_reply(sub.channel_id, sub.sequence_number, "invalid-job-id", replylen);
    if (!sv2_submit_version_ok(sub.version, job_version, version_rolling))
        return submit_error_reply(sub.channel_id, sub.sequence_number, "invalid-share", replylen);

    job = ckzalloc(sizeof(*job));
    job->connector_id = c->client_id;
    job->instance_id = instance_id;
    job->workbase_id = workbase_id;
    job->channel_id = sub.channel_id;
    job->sequence_number = sub.sequence_number;
    job->ntime = sub.ntime;
    job->nonce = sub.nonce;
    job->version = sub.version;
    job->ch_diff = ch_diff;
    job->is_standard = true;
    if (!enqueue_share_job(job))
        return submit_error_reply(sub.channel_id, sub.sequence_number, "internal-error", replylen);
    return NULL; /* reply sent async from sshareq */
}

static uint8_t* handle_open_extended(struct sv2_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_open_extended_channel         o;
    struct sv2_open_extended_channel_success ok;
    struct sv2_open_channel_error            err;
    struct sv2_channel*                      ch;
    uint8_t                                  pbuf[128];
    size_t                                   plen = 0;
    uint32_t                                 ch_id;
    double                                   diff, floor;
    int64_t                                  instance_id = 0;
    uint16_t                                 en2;

    if (!c->setup_ok)
        return NULL;
    /* Clients that required standard-only jobs cannot open extended */
    if (c->flags & SV2_FLAG_REQUIRES_STANDARD_JOBS) {
        memset(&err, 0, sizeof(err));
        if (!sv2_decode_open_extended_channel(payload, len, &o))
            return NULL;
        err.request_id = o.request_id;
        snprintf(err.error_code, sizeof(err.error_code), "unsupported-channel-flags");
        if (!sv2_encode_open_channel_error(pbuf, sizeof(pbuf), &plen, &err))
            return NULL;
        return reply_frame(SV2_MSG_OPEN_MINING_CHANNEL_ERROR, false, pbuf, plen, replylen);
    }
    if (!sv2_decode_open_extended_channel(payload, len, &o))
        return NULL;

    floor = server_diff_floor(c->server);
    choose_channel_diff(o.nominal_hash_rate, o.max_target, floor, &diff);

    /* Negotiate extranonce_size: at least client min, at most pool enonce2 */
    en2 = (uint16_t)ckpool.nonce2length;
    if (en2 < 2)
        en2 = 8;
    if (o.min_extranonce_size > en2) {
        memset(&err, 0, sizeof(err));
        err.request_id = o.request_id;
        snprintf(err.error_code, sizeof(err.error_code), "unsupported-min-extranonce-size");
        if (!sv2_encode_open_channel_error(pbuf, sizeof(pbuf), &plen, &err))
            return NULL;
        return reply_frame(SV2_MSG_OPEN_MINING_CHANNEL_ERROR, false, pbuf, plen, replylen);
    }
    /* Give full pool enonce2 space when client min is smaller (more headroom). */

    ch_id = c->next_channel_id++;
    if (!ch_id)
        ch_id = c->next_channel_id++;

    ch = ckzalloc(sizeof(*ch));
    ch->client_id = c->client_id;
    ch->channel_id = ch_id;
    ch->min_diff = floor;
    ch->standard = false;
    ch->diff = diff;
    memcpy(ch->max_target, o.max_target, 32);
    ch->has_max_target = true;
    ch->extranonce_size = en2;
    assign_enonce1(ch, c->client_id, ch_id);
    snprintf(ch->user_identity, sizeof(ch->user_identity), "%s", o.user_identity);
    chan_key_init(&ch->key, c->client_id, ch_id);

    if (!stratifier_sv2_open_session(
            c->client_id, ch_id, o.user_identity, c->address, c->server, ch->enonce1, ch->enonce1_len, diff,
            &instance_id)) {
        dealloc(ch);
        memset(&err, 0, sizeof(err));
        err.request_id = o.request_id;
        snprintf(err.error_code, sizeof(err.error_code), ckpool.btcsolo ? "invalid-username" : "unauthorized-worker");
        if (!sv2_encode_open_channel_error(pbuf, sizeof(pbuf), &plen, &err))
            return NULL;
        return reply_frame(SV2_MSG_OPEN_MINING_CHANNEL_ERROR, false, pbuf, plen, replylen);
    }
    ch->instance_id = instance_id;

    /* refs = 2: table pin + open-handler pin (see handle_open_standard). */
    ensure_lock();
    mutex_lock(&sv2_lock);
    ch->refs = 2;
    HASH_ADD(hh, sv2_channels, key, sizeof(ch->key), ch);
    memset(&ok, 0, sizeof(ok));
    ok.request_id = o.request_id;
    ok.channel_id = ch_id;
    target_from_diff(ok.target, ch->diff);
    if (target_cmp_le(ok.target, ch->max_target) > 0) {
        memcpy(ok.target, ch->max_target, 32);
        ch->diff = diff_from_target(ch->max_target);
        if (!isfinite(ch->diff) || ch->diff < 1.0)
            ch->diff = 1.0;
    }
    ok.extranonce_size = ch->extranonce_size;
    ok.extranonce_prefix_len = ch->enonce1_len;
    memcpy(ok.extranonce_prefix, ch->enonce1, ch->enonce1_len);
    ok.group_channel_id = 0;
    mutex_unlock(&sv2_lock);

    if (!sv2_encode_open_extended_channel_success(pbuf, sizeof(pbuf), &plen, &ok)) {
        struct sv2_channel* dead = NULL;

        ensure_lock();
        mutex_lock(&sv2_lock);
        HASH_DEL(sv2_channels, ch);
        (void)channel_unref_locked(ch);
        dead = channel_unref_locked(ch);
        mutex_unlock(&sv2_lock);
        if (dead)
            channel_finish_free(dead);
        stratifier_sv2_close_session(instance_id);
        return NULL;
    }

    LOGNOTICE(
        "SV2 OpenExtended ch=%u client %" PRId64 " inst %" PRId64 " user=%s diff=%.1f en2=%u", ch_id, c->client_id,
        instance_id, o.user_identity, diff, ch->extranonce_size);

    /*
     * Open*.Success before jobs/targets (see handle_open_standard).
     * Spec: New*MiningJob first after open; Open.Success already has target.
     */
    queue_frame(c->client_id, SV2_MSG_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS, false, pbuf, plen);
    /* Extended: first job future for SNPH activation consistency */
    push_pool_job(ch, true);
    push_set_target(ch);
    channel_put(ch); /* drop open-handler pin; table pin remains */
    *replylen = 0;
    return NULL;
}

static uint8_t*
handle_set_custom_mining_job(struct sv2_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_set_custom_mining_job         req;
    struct sv2_set_custom_mining_job_success ok;
    struct sv2_set_custom_mining_job_error   err;
    struct sv2_channel*                      ch;
    struct sv2_custom_job*                   cj;
    uint8_t                                  pbuf[128];
    size_t                                   plen = 0;
    uint32_t                                 job_id;
    int64_t                                  wb_id = 0;
    uint32_t                                 tip_ver, tip_ntime, tip_nbits;
    uint8_t                                  tip_prev[32];
    uint32_t                                 err_ch = 0, err_req = 0;
    int                                      i;
    char                                     ecode[64];

    memset(&req, 0, sizeof(req));
    ecode[0] = '\0';

    if (!c->setup_ok)
        return NULL;
    if (!sv2_decode_set_custom_mining_job(payload, len, &req))
        return NULL;
    err_ch = req.channel_id;
    err_req = req.request_id;

    if (!(c->flags & SV2_FLAG_REQUIRES_WORK_SELECTION) || !sv2_jd_enabled()) {
        snprintf(ecode, sizeof(ecode), "unsupported-feature-flags");
        goto err;
    }

    ch = channel_find_ref(c->client_id, req.channel_id);
    if (!ch || ch->standard || !ch->instance_id) {
        /* Spec: extended or group only; standard ignored/rejected */
        snprintf(ecode, sizeof(ecode), "invalid-channel-id");
        channel_put(ch);
        goto err;
    }

    if (!req.mining_job_token_len || !sv2_jd_token_is_declared(req.mining_job_token, req.mining_job_token_len)) {
        snprintf(ecode, sizeof(ecode), "invalid-mining-job-token");
        channel_put(ch);
        goto err;
    }
    /* Spec §6.4.3: coinbase outputs must fund Allocate payout script. */
    if (!sv2_jd_token_outputs_fund_payout(
            req.mining_job_token, req.mining_job_token_len, req.coinbase_tx_outputs, req.coinbase_tx_outputs_len)) {
        snprintf(ecode, sizeof(ecode), "invalid-coinbase-tx-outputs");
        channel_put(ch);
        goto err;
    }

    /* Tip must match declared prev_hash (stale custom rejected after tip) */
    if (!stratifier_sv2_tip_for_jd(&tip_ver, &tip_ntime, &tip_nbits, tip_prev)) {
        snprintf(ecode, sizeof(ecode), "stale-prev-hash");
        channel_put(ch);
        goto err;
    }
    {
        uint8_t req_rev[32];
        int     j;

        for (j = 0; j < 32; j++)
            req_rev[j] = req.prev_hash[31 - j];
        if (memcmp(req.prev_hash, tip_prev, 32) != 0 && memcmp(req_rev, tip_prev, 32) != 0) {
            snprintf(ecode, sizeof(ecode), "stale-prev-hash");
            channel_put(ch);
            goto err;
        }
        /* Always store wire/header-internal prev for share hashing */
        memcpy(req.prev_hash, tip_prev, 32);
    }

    if (req.merkle_count > SV2_MAX_MERKLE_PATH) {
        snprintf(ecode, sizeof(ecode), "invalid-merkle-path");
        channel_put(ch);
        goto err;
    }

    {
        struct sv2_work_snap snap;

        if (stratifier_sv2_snapshot_work(&snap, ch->instance_id))
            wb_id = snap.wb_id;
    }

    cj = ckzalloc(sizeof(*cj));
    cj->version = req.version;
    memcpy(cj->prev_hash, req.prev_hash, 32);
    cj->min_ntime = req.min_ntime;
    cj->nbits = req.nbits;
    cj->coinbase_tx_version = req.coinbase_tx_version;
    cj->coinbase_prefix_len = req.coinbase_prefix_len;
    memcpy(cj->coinbase_prefix, req.coinbase_prefix, req.coinbase_prefix_len);
    cj->coinbase_tx_input_nSequence = req.coinbase_tx_input_nSequence;
    if (req.coinbase_tx_outputs_len) {
        cj->coinbase_tx_outputs = ckalloc(req.coinbase_tx_outputs_len);
        memcpy(cj->coinbase_tx_outputs, req.coinbase_tx_outputs, req.coinbase_tx_outputs_len);
        cj->coinbase_tx_outputs_len = req.coinbase_tx_outputs_len;
    }
    cj->coinbase_tx_locktime = req.coinbase_tx_locktime;
    cj->merkle_count = req.merkle_count;
    for (i = 0; i < req.merkle_count; i++)
        memcpy(cj->merkle_path[i], req.merkle_path[i], 32);
    cj->token_len = req.mining_job_token_len;
    memcpy(cj->token, req.mining_job_token, req.mining_job_token_len);

    ensure_lock();
    mutex_lock(&sv2_lock);
    /*
     * Do not wipe previous custom job_ids — JDC rotates SetCustom every few
     * seconds while in-flight shares still reference the prior job_id.
     * Ring slots retain custom material until overwritten (SV2_JOB_RING).
     */
    job_id = ++ch->job_seq;
    if (!job_id)
        job_id = ++ch->job_seq;
    cj->job_id = job_id;
    ch->work_src = SV2_WORK_CUSTOM;
    /* SetCustomMiningJob: BIP320 bits may be rolled (pool mask). */
    channel_note_job_locked(ch, job_id, wb_id, true, cj, cj->version, ckpool.version_mask != 0);
    mutex_unlock(&sv2_lock);

    memset(&ok, 0, sizeof(ok));
    ok.channel_id = err_ch;
    ok.request_id = err_req;
    ok.job_id = job_id;
    sv2_set_custom_mining_job_free(&req);
    channel_put(ch);

    if (!sv2_encode_set_custom_mining_job_success(pbuf, sizeof(pbuf), &plen, &ok))
        return NULL;
    LOGNOTICE("SV2 SetCustomMiningJob ok client %" PRId64 " ch=%u job=%u", c->client_id, ok.channel_id, job_id);
    return reply_frame(SV2_MSG_SET_CUSTOM_MINING_JOB_SUCCESS, true, pbuf, plen, replylen);

err:
    sv2_set_custom_mining_job_free(&req);
    memset(&err, 0, sizeof(err));
    err.channel_id = err_ch;
    err.request_id = err_req;
    snprintf(err.error_code, sizeof(err.error_code), "%s", ecode[0] ? ecode : "invalid-custom-job");
    if (!sv2_encode_set_custom_mining_job_error(pbuf, sizeof(pbuf), &plen, &err))
        return NULL;
    LOGINFO("SV2 SetCustomMiningJob error client %" PRId64 " %s", c->client_id, err.error_code);
    return reply_frame(SV2_MSG_SET_CUSTOM_MINING_JOB_ERROR, true, pbuf, plen, replylen);
}

/*
 * Build coinbase for a custom job: version || 1-in || null prevout ||
 * scriptSig(prefix||enonce1||extranonce) || nSequence || outputs || locktime.
 */
static size_t build_custom_coinbase(
    uint8_t*                     out,
    size_t                       outsz,
    const struct sv2_custom_job* cj,
    const uint8_t*               enonce1,
    uint8_t                      en1len,
    const uint8_t*               enonce,
    uint16_t                     enlen) {
    /* Shared with the JDC so the declared and custom shapes of one coinbase
     * cannot drift (sv2_cb.c); it also computes the scriptSig length wide and
     * rejects over 255 rather than relying on a wrap check. */
    return sv2_cb_assemble(
        out, outsz, cj->coinbase_tx_version, cj->coinbase_tx_input_nSequence, cj->coinbase_tx_locktime,
        cj->coinbase_prefix, cj->coinbase_prefix_len, enonce1, en1len, enonce, enlen, cj->coinbase_tx_outputs,
        cj->coinbase_tx_outputs_len);
}

/* Validate custom-job share PoW; fills hash and sdiff.
 * enonce1 is snapshotted under sv2_lock with the job (not live ch fields). */
static bool validate_custom_share(
    const struct sv2_custom_job* cj,
    const uint8_t*               enonce1,
    uint8_t                      en1len,
    const uint8_t*               extranonce,
    uint16_t                     enlen,
    uint32_t                     ntime,
    uint32_t                     nonce,
    uint32_t                     version,
    uchar                        hash[32],
    double*                      sdiff_out,
    char*                        err,
    size_t                       errsz) {
    uint8_t  coinbase[2048];
    size_t   cblen;
    uint8_t  merkle_root[32], merkle_sha[64], header[80], hash1[32];
    uint32_t le;
    int      i;

    /* Same ntime window as the pool-template path (wb->ntime32 + 7000):
     * bound rolling above min_ntime so a runaway ntime cannot be credited. */
    if (ntime < cj->min_ntime || ntime > cj->min_ntime + 7000) {
        snprintf(err, errsz, "invalid-ntime");
        return false;
    }
    cblen = build_custom_coinbase(coinbase, sizeof(coinbase), cj, enonce1, en1len, extranonce, enlen);
    if (!cblen) {
        snprintf(err, errsz, "invalid-coinbase");
        return false;
    }
    /* Coinbase txid then merkle path — natural SHA256d order = wire merkle */
    gen_hash(coinbase, merkle_root, (int)cblen);
    memcpy(merkle_sha, merkle_root, 32);
    for (i = 0; i < cj->merkle_count; i++) {
        memcpy(merkle_sha + 32, cj->merkle_path[i], 32);
        gen_hash(merkle_sha, merkle_root, 64);
        memcpy(merkle_sha, merkle_root, 32);
    }

    /*
     * Hash the Bitcoin *wire* header (not midstate + flip_80).
     * SetCustomMiningJob.prev_hash is SV2 U256 / header-internal (wire) order
     * — same as tip_for_jd. Putting it into a midstate slot and flip_80'ing
     * produced a wrong PoW hash → sdiff ~0 → difficulty-too-low while JDC
     * (wire hash) accepted the same share.
     */
    memset(header, 0, 80);
    le = htole32(version);
    memcpy(header, &le, 4);
    memcpy(header + 4, cj->prev_hash, 32);
    memcpy(header + 36, merkle_root, 32);
    le = htole32(ntime);
    memcpy(header + 68, &le, 4);
    le = htole32(cj->nbits);
    memcpy(header + 72, &le, 4);
    le = htole32(nonce);
    memcpy(header + 76, &le, 4);

    sha256(header, 80, hash1);
    sha256(hash1, 32, hash);
    *sdiff_out = diff_from_target(hash);
    return true;
}

static uint8_t* handle_submit_extended(struct sv2_client* c, const uint8_t* payload, uint32_t len, size_t* replylen) {
    struct sv2_submit_shares_extended sub;
    struct sv2_channel*               ch;
    struct sv2_share_job*             job;
    struct sv2_custom_job*            cj_copy = NULL;
    int64_t                           instance_id = 0, workbase_id = 0;
    double                            ch_diff = 0;
    uint16_t                          en_sz = 0;
    bool                              ok_job = false, is_custom = false;
    uint32_t                          job_version = 0;
    bool                              version_rolling = false;
    uint8_t                           en1_snap[16];
    uint8_t                           en1_len = 0;

    *replylen = 0;
    if (!sv2_decode_submit_shares_extended(payload, len, &sub))
        return NULL;

    ch = channel_find_ref(c->client_id, sub.base.channel_id);
    if (!ch || ch->standard || !ch->instance_id) {
        channel_put(ch);
        return submit_error_reply(sub.base.channel_id, sub.base.sequence_number, "invalid-channel-id", replylen);
    }

    ensure_lock();
    mutex_lock(&sv2_lock);
    {
        struct sv2_custom_job* cj_slot = NULL;

        ok_job = channel_lookup_job_locked(
            ch, sub.base.job_id, &workbase_id, &is_custom, &cj_slot, &job_version, &version_rolling);
        instance_id = ch->instance_id;
        ch_diff = ch->diff;
        en_sz = ch->extranonce_size;
        en1_len = ch->enonce1_len;
        if (en1_len > sizeof(en1_snap))
            en1_len = sizeof(en1_snap);
        if (en1_len)
            memcpy(en1_snap, ch->enonce1, en1_len);
        if (ok_job && is_custom && cj_slot) {
            cj_copy = ckzalloc(sizeof(*cj_copy));
            *cj_copy = *cj_slot;
            if (cj_slot->coinbase_tx_outputs_len) {
                cj_copy->coinbase_tx_outputs = ckalloc(cj_slot->coinbase_tx_outputs_len);
                memcpy(cj_copy->coinbase_tx_outputs, cj_slot->coinbase_tx_outputs, cj_slot->coinbase_tx_outputs_len);
            } else
                cj_copy->coinbase_tx_outputs = NULL;
        }
    }
    mutex_unlock(&sv2_lock);
    channel_put(ch);

    if (!ok_job) {
        free_custom_job(cj_copy);
        return submit_error_reply(sub.base.channel_id, sub.base.sequence_number, "invalid-job-id", replylen);
    }
    if (sub.extranonce_len != en_sz) {
        free_custom_job(cj_copy);
        return submit_error_reply(sub.base.channel_id, sub.base.sequence_number, "invalid-extranonce-size", replylen);
    }
    if (!sv2_submit_version_ok(sub.base.version, job_version, version_rolling)) {
        free_custom_job(cj_copy);
        return submit_error_reply(sub.base.channel_id, sub.base.sequence_number, "invalid-share", replylen);
    }
    if (is_custom && !cj_copy)
        return submit_error_reply(sub.base.channel_id, sub.base.sequence_number, "invalid-job-id", replylen);

    job = ckzalloc(sizeof(*job));
    job->connector_id = c->client_id;
    job->instance_id = instance_id;
    job->workbase_id = workbase_id;
    job->channel_id = sub.base.channel_id;
    job->sequence_number = sub.base.sequence_number;
    job->ntime = sub.base.ntime;
    job->nonce = sub.base.nonce;
    job->version = sub.base.version;
    job->ch_diff = ch_diff;
    job->is_custom = is_custom;
    job->is_standard = false;
    job->extranonce_len = sub.extranonce_len;
    if (sub.extranonce_len)
        memcpy(job->extranonce, sub.extranonce, sub.extranonce_len);
    job->en1_len = en1_len;
    if (en1_len)
        memcpy(job->en1, en1_snap, en1_len);
    job->custom = cj_copy;
    cj_copy = NULL;
    if (!enqueue_share_job(job))
        return submit_error_reply(sub.base.channel_id, sub.base.sequence_number, "internal-error", replylen);
    return NULL; /* reply sent async from sshareq */
}

/* Called from connector when client is first accepted — store address/server */
void sv2_strat_note_client(int64_t client_id, const char* address, int server) {
    struct sv2_client* c = client_get(client_id, true);

    if (!c)
        return;
    if (address)
        snprintf(c->address, sizeof(c->address), "%s", address);
    c->server = server;
    client_put(c);
}

void sv2_strat_set_instance_diff(int64_t instance_id, double diff) {
    struct sv2_channel * ch, *tmp;
    struct sv2_channel** list = NULL;
    int                  n = 0, i;

    if (instance_id < 1 || !isfinite(diff) || diff < 1.0)
        return;

    ensure_lock();
    mutex_lock(&sv2_lock);
    HASH_ITER(hh, sv2_channels, ch, tmp) {
        if (ch->instance_id != instance_id)
            continue;
        ch->diff = diff;
        ch->refs++;
        list = ckrealloc(list, (n + 1) * sizeof(*list));
        list[n++] = ch;
    }
    mutex_unlock(&sv2_lock);

    for (i = 0; i < n; i++) {
        push_set_target(list[i]);
        channel_put(list[i]);
    }
    dealloc(list);
    LOGINFO("SV2 SetTarget instance %" PRId64 " diff %.1f channels=%d", instance_id, diff, n);
}

void sv2_strat_clear_instance(int64_t instance_id) {
    struct sv2_channel *ch, *tmp;

    if (instance_id < 1)
        return;
    ensure_lock();
    mutex_lock(&sv2_lock);
    HASH_ITER(hh, sv2_channels, ch, tmp) {
        if (ch->instance_id == instance_id) {
            LOGINFO(
                "SV2 clear instance %" PRId64 " from ch %u client %" PRId64, instance_id, ch->channel_id,
                ch->client_id);
            ch->instance_id = 0;
            channel_clear_custom_locked(ch);
        }
    }
    mutex_unlock(&sv2_lock);
}

uint8_t* sv2_strat_handle_frame(int64_t client_id, const uint8_t* frame, size_t framelen, size_t* replylen) {
    struct sv2_frame   fr;
    struct sv2_client* c;
    const uint8_t*     payload;
    uint32_t           plen;
    uint8_t*           ret = NULL;

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
            "SV2 client %" PRId64 " ignoring extension 0x%04x msg 0x%02x", client_id, fr.extension_type, fr.msg_type);
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
        case SV2_MSG_OPEN_STANDARD_MINING_CHANNEL:
            ret = handle_open_standard(c, payload, plen, replylen);
            break;
        case SV2_MSG_OPEN_EXTENDED_MINING_CHANNEL:
            ret = handle_open_extended(c, payload, plen, replylen);
            break;
        case SV2_MSG_SUBMIT_SHARES_STANDARD:
            ret = handle_submit_standard(c, payload, plen, replylen);
            break;
        case SV2_MSG_SUBMIT_SHARES_EXTENDED:
            ret = handle_submit_extended(c, payload, plen, replylen);
            break;
        case SV2_MSG_SET_CUSTOM_MINING_JOB:
            ret = handle_set_custom_mining_job(c, payload, plen, replylen);
            break;
        case SV2_MSG_UPDATE_CHANNEL: {
            /* Downstream (tProxy) requests new target from hashrate / max_target */
            uint32_t                        ch_id = 0;
            float                           nom_hr = 0;
            uint8_t                         max_t[32];
            const uint8_t *                 p = payload, *end = payload + plen;
            struct sv2_channel*             ch;
            struct sv2_update_channel_error uerr;
            double                          new_diff;
            uint8_t                         ebuf[64];
            size_t                          elen = 0;

            if (!sv2_read_u32(&p, end, &ch_id) || !sv2_read_f32(&p, end, &nom_hr) || !sv2_read_u256(&p, end, max_t)) {
                /* Malformed: cannot recover channel_id reliably */
                ret = NULL;
                break;
            }
            /* Spec: no response when accepted; Error only when invalid. */
            if (!isfinite(nom_hr) || nom_hr < 0.0f) {
                memset(&uerr, 0, sizeof(uerr));
                uerr.channel_id = ch_id;
                snprintf(uerr.error_code, sizeof(uerr.error_code), "invalid-nominal-hashrate");
                if (sv2_encode_update_channel_error(ebuf, sizeof(ebuf), &elen, &uerr))
                    ret = reply_frame(SV2_MSG_UPDATE_CHANNEL_ERROR, true, ebuf, elen, replylen);
                else
                    ret = NULL;
                break;
            }
            ch = channel_find_ref(client_id, ch_id);
            if (!ch || !ch->instance_id) {
                channel_put(ch);
                memset(&uerr, 0, sizeof(uerr));
                uerr.channel_id = ch_id;
                snprintf(uerr.error_code, sizeof(uerr.error_code), "invalid-channel-id");
                if (sv2_encode_update_channel_error(ebuf, sizeof(ebuf), &elen, &uerr))
                    ret = reply_frame(SV2_MSG_UPDATE_CHANNEL_ERROR, true, ebuf, elen, replylen);
                else
                    ret = NULL;
                break;
            }
            {
                uint8_t cur_target[32];
                double  old_diff;
                bool    apply;

                /*
                 * Refresh stored max_target from this request
                 * before SetTarget — tProxy compares our reply
                 * against UpdateChannel.maximum_target, not the
                 * open-time max. Also absorb float overshoot in
                 * target_from_diff via push_set_target clamp.
                 *
                 * Vardiff owns steady-state difficulty from
                 * observed shares. Re-deriving diff from
                 * nominal_hash_rate here on every UpdateChannel
                 * fights vardiff (different shares/min policies)
                 * and oscillates ~2.4x every few minutes through
                 * SRI JDC, flushing downstream work each swing.
                 * Only apply the hashrate-derived diff on gross
                 * (>4x) mismatch, e.g. a proxy losing or gaining
                 * most of its devices.
                 */
                choose_channel_diff(nom_hr, max_t, ch->min_diff, &new_diff);
                ensure_lock();
                mutex_lock(&sv2_lock);
                memcpy(ch->max_target, max_t, 32);
                ch->has_max_target = true;
                old_diff = ch->diff;
                apply = new_diff > old_diff * 4.0 || new_diff < old_diff / 4.0;
                if (apply)
                    ch->diff = new_diff;
                mutex_unlock(&sv2_lock);
                if (apply) {
                    double applied;

                    /* push_set_target may clamp ch->diff to max_target */
                    push_set_target(ch);
                    mutex_lock(&sv2_lock);
                    applied = ch->diff;
                    mutex_unlock(&sv2_lock);
                    stratifier_sv2_set_diff(ch->instance_id, applied);
                    LOGNOTICE(
                        "SV2 UpdateChannel ch=%u client %" PRId64 " hr=%.0f diff=%.1f (was %.1f)", ch_id, client_id,
                        (double)nom_hr, applied, old_diff);
                } else {
                    double applied;

                    /* Still honour a stricter maximum_target */
                    mutex_lock(&sv2_lock);
                    applied = ch->diff;
                    mutex_unlock(&sv2_lock);
                    target_from_diff(cur_target, applied);
                    if (target_cmp_le(cur_target, max_t) > 0) {
                        push_set_target(ch);
                        mutex_lock(&sv2_lock);
                        applied = ch->diff;
                        mutex_unlock(&sv2_lock);
                        stratifier_sv2_set_diff(ch->instance_id, applied);
                    }
                    LOGDEBUG(
                        "SV2 UpdateChannel ch=%u client %" PRId64 " hr=%.0f keeping vardiff %.1f (hint %.1f)", ch_id,
                        client_id, (double)nom_hr, old_diff, new_diff);
                }
            }
            channel_put(ch);
            ret = NULL;
            break;
        }
        case SV2_MSG_CLOSE_CHANNEL: {
            struct sv2_close_channel cl;
            struct sv2_channel*      ch;
            struct sv2_chan_key      key;
            int64_t                  iid = 0;

            if (sv2_decode_close_channel(payload, plen, &cl)) {
                struct sv2_channel* dead = NULL;

                chan_key_init(&key, client_id, cl.channel_id);
                ensure_lock();
                mutex_lock(&sv2_lock);
                HASH_FIND(hh, sv2_channels, &key, sizeof(key), ch);
                if (ch) {
                    iid = ch->instance_id;
                    HASH_DEL(sv2_channels, ch);
                    dead = channel_unref_locked(ch);
                }
                mutex_unlock(&sv2_lock);
                if (dead)
                    channel_finish_free(dead);
                if (iid)
                    stratifier_sv2_close_session(iid);
            }
            ret = NULL;
            break;
        }
        default:
            LOGDEBUG("SV2 client %" PRId64 " msg 0x%02x", client_id, fr.msg_type);
            break;
    }
    client_put(c);
    return ret;
}

void sv2_strat_on_work_update(bool clean) {
    struct sv2_channel * ch, *tmp;
    struct sv2_channel** list = NULL;
    struct sv2_client*   cl;
    int                  n = 0, i;
    uint8_t              tip_prev[32];
    uint32_t             tip_ver, tip_ntime, tip_nbits;
    static uint8_t       last_tip_prev[32];
    static bool          have_last_tip;
    bool                 tip_changed = true;

    (void)clean;

    /*
     * Only treat a real prevhash change as a tip switch. The stratifier also
     * calls this on every GBT refresh (~30s) with the same tip — wiping custom
     * jobs then causes invalid-job-id on in-flight JD shares.
     */
    if (stratifier_sv2_tip_for_jd(&tip_ver, &tip_ntime, &tip_nbits, tip_prev)) {
        if (have_last_tip && !memcmp(last_tip_prev, tip_prev, 32))
            tip_changed = false;
        memcpy(last_tip_prev, tip_prev, 32);
        have_last_tip = true;
    }

    if (tip_changed)
        sv2_jd_on_tip_change();

    ensure_lock();
    mutex_lock(&sv2_lock);
    HASH_ITER(hh, sv2_channels, ch, tmp) {
        bool work_selection = false;

        HASH_FIND_I64(sv2_clients, &ch->client_id, cl);
        if (cl && (cl->flags & SV2_FLAG_REQUIRES_WORK_SELECTION))
            work_selection = true;

        if (tip_changed)
            channel_clear_custom_locked(ch);

        /*
         * Work-selection / JD clients own templates via SetCustomMiningJob,
         * so skip the ~30s same-tip GBT refresh (that repeated SNPH/flush was
         * the original bug). But on a REAL tip change send a future
         * NewExtendedMiningJob + SetNewPrevHash so the JDC learns the pool's
         * new tip immediately (spec requires SNPH reference a known future
         * job, so we cannot send SNPH alone) and can adopt the pool job as
         * fallback if its own template provider lags. The stale custom job is
         * already cleared above; SNPH invalidating it is correct on a new tip.
         * One job+SNPH per tip (~per block) — negligible job-ring churn, and
         * work-selection channels already receive the same pair at open.
         */
        if (work_selection && !tip_changed)
            continue;

        ch->refs++;
        list = ckrealloc(list, (n + 1) * sizeof(*list));
        list[n++] = ch;
    }
    mutex_unlock(&sv2_lock);

    /*
     * Future job + SetNewPrevHash only on a real tip change. SNPH
     * invalidates every other job on the channel (spec 5.3.17), so
     * translators must flush downstream miners (SV1 clean_jobs=true),
     * discarding all in-flight work. Sending it on every ~30s GBT
     * refresh cost ~30% of real hashrate through SRI tProxy. Same-tip
     * refreshes send an immediate job (min_ntime set) which continues
     * against the last SetNewPrevHash context without a flush.
     */
    for (i = 0; i < n; i++) {
        if (list[i]->instance_id)
            push_pool_job(list[i], tip_changed);
        channel_put(list[i]);
    }
    dealloc(list);
}

/* Connected client/channel gauges for pool.status (one JSON object line). */
char* sv2_strat_stats_json(void) {
    int   clients, channels, jd_clients;
    char* s;

    if (!ckpool.sv2urls && !ckpool.sv2jdurls)
        return NULL;
    ensure_lock();
    mutex_lock(&sv2_lock);
    clients = (int)HASH_COUNT(sv2_clients);
    channels = (int)HASH_COUNT(sv2_channels);
    mutex_unlock(&sv2_lock);
    jd_clients = sv2_jd_client_count();
    ASPRINTF(&s, "{\"clients\":%d,\"channels\":%d,\"jdclients\":%d}", clients, channels, jd_clients);
    return s;
}

#endif /* HAVE_SV2 */
