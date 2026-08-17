/*
 * Copyright 2014-2020,2023,2025-2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#include "config.h"

#include <arpa/inet.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/types.h>
#include <dirent.h>
#include <fcntl.h>
#include <math.h>
#include <string.h>
#include <unistd.h>

#ifdef HAVE_ZMQ_H
#include <zmq.h>
#endif

#ifdef HAVE_CAPNP
#include "mining_ipc.h"
#endif

#ifdef HAVE_LIBPQ
#include <libpq-fe.h>
#endif

#include "ckpool.h"
#include "libckpool.h"
#include "bitcoin.h"
#include "sha2.h"
#include "stratifier.h"
#include "uthash.h"
#include "utlist.h"
#include "connector.h"
#include "generator.h"
#ifdef HAVE_SV2
#include "sv2_strat.h"
#include "sv2_jd.h"
#endif

/* Consistent across all pool instances */
static const char* workpadding =
    "000000800000000000000000000000000000000000000000000000000000000000000000000000000000000080020000";
static const char* scriptsig_header =
    "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff";
static uchar        scriptsig_header_bin[41];
static const double nonces = 4294967296;
/* Satoshi per bitcoin. The generation output is fixed at this value; see
 * generate_coinbase(). */
static const int64_t COIN = 100000000;

/* Add unaccounted shares when they arrive, remove them with each update of
 * rolling stats. */
struct pool_stats {
    tv_t start_time;
    ts_t last_update;

    int workers;
    int users;
    int disconnected;

    int remote_workers;
    int remote_users;

    /* Absolute shares stats */
    int64_t unaccounted_shares;
    int64_t accounted_shares;

    /* Cycle of 32 to determine which users to dump stats on */
    uint8_t userstats_cycle;

    /* Shares per second for 1/5/15/60 minute rolling averages */
    double sps1;
    double sps5;
    double sps15;
    double sps60;

    /* Diff shares stats */
    int64_t unaccounted_diff_shares;
    int64_t accounted_diff_shares;
    int64_t unaccounted_rejects;
    int64_t accounted_rejects;

    /* Diff shares per second for 1/5/15... minute rolling averages */
    double dsps1;
    double dsps5;
    double dsps15;
    double dsps60;
    double dsps360;
    double dsps1440;
    double dsps10080;

    double  network_diff;
    int64_t best_diff;
};

typedef struct pool_stats pool_stats_t;

typedef struct genwork workbase_t;

struct json_params {
    yyjson_mut_doc* doc;
    yyjson_mut_val* yymethod;
    yyjson_mut_val* yyparams;
    yyjson_mut_val* yyid_val;

    int64_t client_id;
};

typedef struct json_params json_params_t;

/* Shared sshareq item: SV1 JSON submit or SV2 binary share (same workers). */
struct shareq_item {
    bool  is_sv2;
    void* payload; /* json_params_t * or sv2 share job */
};

static void discard_json_params(json_params_t* jp);

/* Stratum json messages with their associated client id */
struct smsg {
    yyjson_mut_doc* doc;
    int64_t         client_id;
};

typedef struct smsg smsg_t;

struct userwb {
    UT_hash_handle hh;
    int64_t        id;

    uchar* coinb2bin;  // Coinb2 cointaining this user's address for generation
    char*  coinb2;
    int    coinb2len;  // Length of user coinb2
};

struct user_instance;
struct worker_instance;
struct stratum_instance;

typedef struct user_instance    user_instance_t;
typedef struct worker_instance  worker_instance_t;
typedef struct stratum_instance stratum_instance_t;

struct user_instance {
    UT_hash_handle hh;
    char           username[128];
    int            id;
    char*          secondaryuserid;
    bool           btcaddress;
    bool           script;
    bool           segwit;

    /* A linked list of all connected instances of this user */
    stratum_instance_t* clients;

    /* A linked list of all connected workers of this user */
    worker_instance_t* worker_instances;

    int            workers;
    int            remote_workers;
    char           txnbin[48];
    int            txnlen;
    struct userwb* userwbs; /* Protected by instance lock */

    double  best_diff; /* Best share found by this user */
    int64_t best_ever; /* Best share ever found by this user */

    int64_t shares;

    int64_t uadiff; /* Shares not yet accounted for in hashmeter */

    double dsps1;  /* Diff shares per second, 1 minute rolling average */
    double dsps5;  /* ... 5 minute ... */
    double dsps60; /* etc */
    double dsps1440;
    double dsps10080;
    tv_t   last_share;
    tv_t   last_decay;

    bool   authorised; /* Has this username ever been authorised? */
    time_t auth_time;
    time_t failed_authtime; /* Last time this username failed to authorise */
    int    auth_backoff;    /* How long to reject any auth attempts since last failure */
    bool   throttled;       /* Have we begun rejecting auth attempts */
};

/* Combined data from workers with the same workername */
struct worker_instance {
    user_instance_t* user_instance;
    char*            workername;

    /* Number of stratum instances attached as this one worker */
    int instance_count;

    worker_instance_t* next;
    worker_instance_t* prev;

    int64_t shares;

    int64_t uadiff; /* Shares not yet accounted for in hashmeter */

    double dsps1;
    double dsps5;
    double dsps60;
    double dsps1440;
    double dsps10080;
    tv_t   last_share;
    tv_t   last_decay;
    time_t start_time;

    double  best_diff; /* Best share found by this worker */
    int64_t best_ever; /* Best share ever found by this worker */
    int     mindiff;   /* User chosen mindiff */

    bool idle;
    bool notified_idle;
};

typedef struct stratifier_data sdata_t;

typedef struct proxy_base proxy_t;

/* Per client stratum instance == workers */
struct stratum_instance {
    UT_hash_handle hh;
    int64_t        id;

    /* Virtualid used as unique local id for passthrough clients */
    int64_t virtualid;

    stratum_instance_t* recycled_next;
    stratum_instance_t* recycled_prev;

    stratum_instance_t* user_next;
    stratum_instance_t* user_prev;

    stratum_instance_t* node_next;
    stratum_instance_t* node_prev;

    stratum_instance_t* remote_next;
    stratum_instance_t* remote_prev;

    /* Descriptive of ID number and passthrough if any */
    char identity[128];

    /* Reference count for when this instance is used outside of the
     * instance_lock */
    int ref;

    char     enonce1[36]; /* Fit up to 16 byte binary enonce1 */
    uchar    enonce1bin[16];
    char     enonce1var[20]; /* Fit up to 8 byte binary enonce1var */
    uint64_t enonce1_64;
    int      session_id;

    int64_t diff;               /* Current diff */
    int64_t old_diff;           /* Previous diff */
    int64_t diff_change_job_id; /* Last job_id we changed diff */

    int64_t uadiff; /* Shares not yet accounted for in hashmeter */

    double dsps1;  /* Diff shares per second, 1 minute rolling average */
    double dsps5;  /* ... 5 minute ... */
    double dsps60; /* etc */
    double dsps1440;
    double dsps10080;
    tv_t   ldc;  /* Last diff change */
    int    ssdc; /* Shares since diff change */
    tv_t   first_share;
    tv_t   last_share;
    tv_t   last_decay;
    time_t first_invalid;    /* Time of first invalid in run of non stale rejects */
    time_t upstream_invalid; /* As first_invalid but for upstream responses */
    time_t start_time;

    char address[INET6_ADDRSTRLEN];
    bool node; /* Is this a mining node */
    bool subscribed;
    bool authorising; /* In progress, protected by instance_lock */
    bool authorised;
    bool dropped;
    bool idle;
    int  reject; /* Indicator that this client is having a run of rejects
                  * or other problem and should be dropped lazily if
                  * this is set to 2 */

    int latency; /* Latency when on a mining node */

    bool   reconnect;         /* This client really needs to reconnect */
    time_t reconnect_request; /* The time we sent a reconnect message */

    user_instance_t*   user_instance;
    worker_instance_t* worker_instance;

    char* useragent;
    char* workername;
    char* password;
    bool  messages; /* Is this a client that understands stratum messages */
    int   user_id;
    int   server; /* Which server is this instance bound to */

    ckpool_t* ckp;

    time_t last_txns;         /* Last time this worker requested txn hashes */
    time_t disconnected_time; /* Time this instance disconnected */

    int64_t suggest_diff; /* Stratum client suggested diff */
    double  best_diff;    /* Best share found by this instance */

    sdata_t* sdata;      /* Which sdata this client is bound to */
    proxy_t* proxy;      /* Proxy this is bound to in proxy mode */
    int      proxyid;    /* Which proxy id  */
    int      subproxyid; /* Which subproxy */

    bool passthrough; /* Is this a passthrough */
    bool trusted;     /* Is this a trusted remote server */
    bool remote;      /* Is this a remote client on a trusted remote server */
    bool sv2;         /* Virtual SV2 mining session (no connector TCP client) */
};

struct share {
    UT_hash_handle hh;
    uchar          hash[32];
    int64_t        workbase_id;
};

typedef struct share share_t;

struct proxy_base {
    UT_hash_handle hh;
    UT_hash_handle sh;   /* For subproxy hashlist */
    proxy_t*       next; /* For retired subproxies */
    proxy_t*       prev;
    int            id;
    int            subid;

    /* Priority has the user id encoded in the high bits if it's not a
     * global proxy. */
    int64_t priority;

    bool global; /* Is this a global proxy */
    int  userid; /* Userid for non global proxies */

    double diff;

    char  baseurl[128];
    char  url[128];
    char  auth[128];
    char  pass[128];
    char  enonce1[32];
    uchar enonce1bin[16];
    int   enonce1constlen;
    int   enonce1varlen;

    int nonce2len;
    int enonce2varlen;

    bool subscribed;
    bool notified;

    int64_t clients;          /* Incrementing client count */
    int64_t max_clients;      /* Maximum number of clients per subproxy */
    int64_t bound_clients;    /* Currently actively bound clients */
    int64_t combined_clients; /* Total clients of all subproxies of a parent proxy */
    int64_t headroom;         /* Temporary variable when calculating how many more clients can bind */

    int      subproxy_count; /* Number of subproxies */
    proxy_t* parent;         /* Parent proxy of each subproxy */
    proxy_t* subproxies;     /* Hashlist of subproxies sorted by subid */
    sdata_t* sdata;          /* Unique stratifer data for each subproxy */
    bool     dead;
    bool     deleted;
};

typedef struct session session_t;

struct session {
    UT_hash_handle hh;
    int            session_id;
    uint64_t       enonce1_64;
    int64_t        client_id;
    int            userid;
    time_t         added;
    char           address[INET6_ADDRSTRLEN];
};

typedef struct txntable txntable_t;

struct txntable {
    UT_hash_handle hh;
    int            id;
    char           hash[68];
    char*          data;
    int            refcount;
    bool           seen;
};

/* Upper bound on the binary length of any single coinbase component (coinb1
 * or coinb2) accepted from remote/upstream sources. Coinbases are assembled
 * and hex encoded into buffers sized to their actual content, so this exists
 * only to keep those allocations bounded rather than to limit legitimate
 * pools; it is generous enough to cover large direct payout coinbases. */
#define MAX_COINBASE_LEN 8192

/* Upper bound on the number of transactions accepted in a block template.
 * This is the capacity of the fixed 16 entry merkle arrays (2^16 leaves) and
 * is far above any block a real chain can produce, existing only to keep the
 * merkle/hash size arithmetic and stack allocations bounded against a corrupt
 * or malicious template. */
#define MAX_GBT_TXNS 65535

#define ID_AUTH 0
#define ID_WORKINFO 1
#define ID_AGEWORKINFO 2
#define ID_SHARES 3
#define ID_SHAREERR 4
#define ID_POOLSTATS 5
#define ID_WORKERSTATS 6
#define ID_BLOCK 7
#define ID_ADDRAUTH 8
#define ID_HEARTBEAT 9

struct stratifier_data {
    ckpool_t* ckp;

    char txnbin[48];
    int  txnlen;
    char dontxnbin[48];
    int  dontxnlen;

    pool_stats_t stats;
    /* Protects changes to pool stats */
    mutex_t stats_lock;
    /* Protects changes to unaccounted pool stats */
    mutex_t uastats_lock;

    bool verbose;

    uint64_t enonce1_64;

    /* For protecting the txntable data */
    cklock_t txn_lock;

    /* For protecting the hashtable data */
    cklock_t workbase_lock;

    /* For the hashtable of all workbases */
    workbase_t* workbases;
    workbase_t* current_workbase;
    int         workbases_generated;
    txntable_t* txns;
    int64_t     txns_generated;

    /* Workbases from remote trusted servers */
    workbase_t* remote_workbases;

    /* Is this a node and unable to rebuild workinfos due to lack of txns */
    bool wbincomplete;

    /* Semaphore to serialise calls to add_base */
    sem_t update_sem;
    /* Time we last sent out a stratum update */
    time_t update_time;

    int64_t workbase_id;
    int64_t blockchange_id;
    int     session_id;
    char    lasthash[68];
    char    lastswaphash[68];

    ckmsgq_t* updateq;  // Generator base work updates
    ckmsgq_t* ssends;   // Stratum sends
    ckmsgq_t* srecvs;   // Stratum receives
    ckmsgq_t* sshareq;  // Stratum share sends
    ckmsgq_t* sauthq;   // Stratum authorisations
    ckmsgq_t* stxnq;    // Transaction requests

    int user_instance_id;

    stratum_instance_t* stratum_instances;
    stratum_instance_t* recycled_instances;
    stratum_instance_t* node_instances;
    stratum_instance_t* remote_instances;

    int64_t    stratum_generated;
    int64_t    disconnected_generated;
    int64_t    userwbs_generated;
    session_t* disconnected_sessions;

    user_instance_t* user_instances;

    /* Protects both stratum and user instances */
    cklock_t instance_lock;

    share_t* shares;
    mutex_t  share_lock;

    int64_t shares_generated;

    int      proxy_count; /* Total proxies generated (not necessarily still alive) */
    proxy_t* proxy;       /* Current proxy in use */
    proxy_t* proxies;     /* Hashlist of all proxies */
    mutex_t  proxy_lock;  /* Protects all proxy data */
    proxy_t* subproxy;    /* Which subproxy this sdata belongs to in proxy mode */

#ifdef HAVE_LIBPQ
    PGconn* pg_conn; /* Persistent postgres connection, NULL when down */
    mutex_t pg_lock; /* Protects pg_conn and all use of it */
#endif
};

/* PostgreSQL share, block and user logging.
 *
 * All statements are prepared once per connection and executed with bound
 * text parameters, so no value is ever interpolated into SQL. Every entry
 * point is a no-op unless --log-shares was given, and main() refuses that
 * flag unless a complete "database" config object produced a conninfo
 * string, so db_conn_str is non-NULL by the time anything here runs.
 *
 * Calls are synchronous and serialised on pg_lock; they sit in the share
 * path, so a slow database slows share logging rather than losing it. */
#ifdef HAVE_LIBPQ

struct db_stmt {
    const char* name;
    const char* sql;
};

static const struct db_stmt db_stmts[] = {
    {"insert_share",
     "INSERT INTO shares (blockheight, workinfoid, clientid, enonce1, nonce2, nonce, ntime, "
     "diff, sdiff, hash, result, reject_reason, error, errn, createdate, createby, createcode, "
     "createinet, workername, username, lnurl, address, agent) "
     "VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, "
     "$19, $20, $21, $22, $23)"},
    {"insert_user",
     "INSERT INTO users (username, btcaddress, lightning_address, workername, script, segwit, "
     "created_at) VALUES ($1, $2, $3, $4, $5, $6, NOW()) "
     "ON CONFLICT (username, lightning_address) DO UPDATE SET "
     "btcaddress = EXCLUDED.btcaddress, script = EXCLUDED.script, segwit = EXCLUDED.segwit, "
     "workername = EXCLUDED.workername, last_seen = NOW()"},
    {"insert_block",
     "INSERT INTO blocks (blockheight, blockhash, confirmed, workername, username, diff, "
     "coinbasevalue) VALUES ($1, $2, $3, $4, $5, $6, $7)"},
    {"update_block_confirmed", "UPDATE blocks SET confirmed = $1, rewards_processed = $2 WHERE blockhash = $3"},
    {"get_latest_block", "SELECT blockheight, blockhash, confirmed FROM blocks ORDER BY id DESC LIMIT 1"},
};

/* Drop the connection. Callers must hold pg_lock. */
static void __db_drop(sdata_t* sdata) {
    if (sdata->pg_conn) {
        PQfinish(sdata->pg_conn);
        sdata->pg_conn = NULL;
    }
}

/* Connect and prepare every statement. Callers must hold pg_lock. Returns
 * false with the connection dropped if any step fails. */
static bool __db_connect(sdata_t* sdata) {
    unsigned int i;

    if (sdata->pg_conn) {
        if (PQstatus(sdata->pg_conn) == CONNECTION_OK)
            return true;
        __db_drop(sdata);
    }

    sdata->pg_conn = PQconnectdb(ckpool.db_conn_str);
    if (PQstatus(sdata->pg_conn) != CONNECTION_OK) {
        LOGERR("Connection to database failed: %s", PQerrorMessage(sdata->pg_conn));
        __db_drop(sdata);
        return false;
    }

    for (i = 0; i < sizeof(db_stmts) / sizeof(db_stmts[0]); i++) {
        PGresult* res = PQprepare(sdata->pg_conn, db_stmts[i].name, db_stmts[i].sql, 0, NULL);

        if (PQresultStatus(res) != PGRES_COMMAND_OK) {
            LOGERR("Failed to prepare %s statement: %s", db_stmts[i].name, PQerrorMessage(sdata->pg_conn));
            PQclear(res);
            __db_drop(sdata);
            return false;
        }
        PQclear(res);
    }

    LOGDEBUG("Connected to database and prepared %zu statements", sizeof(db_stmts) / sizeof(db_stmts[0]));
    return true;
}

static void db_disconnect(sdata_t* sdata) {
    mutex_lock(&sdata->pg_lock);
    if (sdata->pg_conn)
        LOGINFO("Closing database connection");
    __db_drop(sdata);
    mutex_unlock(&sdata->pg_lock);
}

/* Execute a prepared statement, reconnecting once if the connection has
 * dropped. Returns a result the caller must PQclear, or NULL. The single
 * retry is deliberate: an unbounded retry loop here would stall the share
 * thread that called it. */
static PGresult* db_exec(sdata_t* sdata, const char* stmt, const char** values, int count) {
    PGresult* res = NULL;
    int       attempt;

    mutex_lock(&sdata->pg_lock);
    for (attempt = 0; attempt < 2; attempt++) {
        if (!__db_connect(sdata))
            break;

        res = PQexecPrepared(sdata->pg_conn, stmt, count, values, NULL, NULL, 0);
        if (PQresultStatus(res) == PGRES_COMMAND_OK || PQresultStatus(res) == PGRES_TUPLES_OK)
            break;

        LOGERR("Database statement %s failed: %s", stmt, PQerrorMessage(sdata->pg_conn));
        PQclear(res);
        res = NULL;

        /* Only a dropped connection is worth retrying; a rejected
         * statement will be rejected again. */
        if (PQstatus(sdata->pg_conn) == CONNECTION_OK)
            break;
        __db_drop(sdata);
    }
    mutex_unlock(&sdata->pg_lock);

    return res;
}

static void
db_insert_user(sdata_t* sdata, user_instance_t* user, const char* lightning_address, const char* workername) {
    const char* values[6];
    PGresult*   res;

    values[0] = user->username;
    values[1] = user->btcaddress ? "TRUE" : "FALSE";
    values[2] = lightning_address; /* NULL becomes SQL NULL */
    values[3] = workername;
    values[4] = user->script ? "TRUE" : "FALSE";
    values[5] = user->segwit ? "TRUE" : "FALSE";

    res = db_exec(sdata, "insert_user", values, 6);
    if (res) {
        LOGDEBUG("Recorded user %s in database", user->username);
        PQclear(res);
    } else
        LOGERR("Failed to record user %s in database", user->username);
}

/* Log one share. Takes the assembled sharelog document rather than the
 * workbase, which the caller has already released by this point. */
static void db_log_share(sdata_t* sdata, yyjson_mut_doc* doc, const int height) {
    yyjson_mut_val* val = yyjson_mut_doc_get_root(doc);
    char            height_str[16], clientid_str[32], workinfoid_str[32];
    char            diff_str[32], sdiff_str[32], errn_str[32];
    const char*     values[23];
    PGresult*       res;

    snprintf(height_str, sizeof(height_str), "%d", height);
    snprintf(
        workinfoid_str, sizeof(workinfoid_str), "%" PRId64,
        (int64_t)yyjson_mut_get_sint(yyjson_mut_obj_get(val, "workinfoid")));
    snprintf(
        clientid_str, sizeof(clientid_str), "%" PRId64,
        (int64_t)yyjson_mut_get_sint(yyjson_mut_obj_get(val, "clientid")));
    snprintf(diff_str, sizeof(diff_str), "%f", yyjson_mut_get_num(yyjson_mut_obj_get(val, "diff")));
    snprintf(sdiff_str, sizeof(sdiff_str), "%f", yyjson_mut_get_num(yyjson_mut_obj_get(val, "sdiff")));
    snprintf(errn_str, sizeof(errn_str), "%" PRId64, (int64_t)yyjson_mut_get_sint(yyjson_mut_obj_get(val, "errn")));

    values[0] = height_str;
    values[1] = workinfoid_str;
    values[2] = clientid_str;
    values[3] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "enonce1"));
    values[4] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "nonce2"));
    values[5] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "nonce"));
    values[6] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "ntime"));
    values[7] = diff_str;
    values[8] = sdiff_str;
    values[9] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "hash"));
    values[10] = yyjson_mut_get_bool(yyjson_mut_obj_get(val, "result")) ? "TRUE" : "FALSE";
    values[11] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "reject-reason"));
    values[12] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "error"));
    values[13] = errn_str;
    values[14] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "createdate"));
    values[15] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "createby"));
    values[16] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "createcode"));
    values[17] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "createinet"));
    values[18] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "workername"));
    values[19] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "username"));
    values[20] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "lnurl"));
    values[21] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "address"));
    values[22] = yyjson_mut_get_str(yyjson_mut_obj_get(val, "agent"));

    res = db_exec(sdata, "insert_share", values, 23);
    if (res)
        PQclear(res);
    else
        LOGDEBUG("Failed to record share %s in database", workinfoid_str);
}

/* Convert the internal block hash to the big endian form bitcoind displays.
 * process_block() byte swaps each 32 bit word in place, so reversing the
 * order of those words completes the full byte reversal. */
static bool block_hash_be(char* be_hash, const size_t be_size, const char* hash) {
    size_t len = hash ? strlen(hash) : 0;
    size_t i;

    if (unlikely(len != 64 || be_size < len + 1)) {
        LOGWARNING("Invalid block hash length %zu, not recording", len);
        return false;
    }
    for (i = 0; i < len; i += 8)
        memcpy(be_hash + len - i - 8, hash + i, 8);
    be_hash[len] = '\0';
    return true;
}

static void db_add_block(
    sdata_t*      sdata,
    const int     height,
    const char*   hash,
    const bool    confirmed,
    const char*   workername,
    const char*   username,
    const double  diff,
    const int64_t coinbasevalue) {
    char        height_str[16], diff_str[32], coinbasevalue_str[32], be_hash[65];
    const char* values[7];
    PGresult*   res;

    if (!block_hash_be(be_hash, sizeof(be_hash), hash))
        return;

    snprintf(height_str, sizeof(height_str), "%d", height);
    snprintf(diff_str, sizeof(diff_str), "%f", diff);
    snprintf(coinbasevalue_str, sizeof(coinbasevalue_str), "%" PRId64, coinbasevalue);

    values[0] = height_str;
    values[1] = be_hash;
    values[2] = confirmed ? "TRUE" : "FALSE";
    values[3] = workername ?: "";
    values[4] = username ?: "";
    values[5] = diff_str;
    values[6] = coinbasevalue_str;

    res = db_exec(sdata, "insert_block", values, 7);
    if (res) {
        LOGNOTICE("Recorded block %s at height %d in database", be_hash, height);
        PQclear(res);
    } else
        LOGERR("Failed to record block %s in database", be_hash);
}

static void
db_update_block_status(sdata_t* sdata, const char* hash, const bool confirmed, const bool rewards_processed) {
    const char* values[3];
    char        be_hash[65];
    PGresult*   res;

    if (!block_hash_be(be_hash, sizeof(be_hash), hash))
        return;

    values[0] = confirmed ? "TRUE" : "FALSE";
    values[1] = rewards_processed ? "TRUE" : "FALSE";
    values[2] = be_hash;

    res = db_exec(sdata, "update_block_confirmed", values, 3);
    if (res) {
        LOGNOTICE("Updated block %s (confirmed %s)", be_hash, confirmed ? "true" : "false");
        PQclear(res);
    } else
        LOGERR("Failed to update block %s status", be_hash);
}

/* Log the most recent block on record, as a startup sanity check that the
 * schema is reachable and readable. */
static void db_check_blocks(sdata_t* sdata) {
    PGresult* res = db_exec(sdata, "get_latest_block", NULL, 0);

    if (!res) {
        LOGWARNING("Could not read latest block from database");
        return;
    }
    if (PQntuples(res) > 0) {
        LOGNOTICE(
            "Latest block in database: height %s hash %s confirmed %s", PQgetvalue(res, 0, 0), PQgetvalue(res, 0, 1),
            PQgetvalue(res, 0, 2));
    } else
        LOGNOTICE("No blocks recorded in database yet");
    PQclear(res);
}

static void db_init(sdata_t* sdata) {
    mutex_init(&sdata->pg_lock);
    if (!ckpool.logshares_db)
        return;
    mutex_lock(&sdata->pg_lock);
    if (!__db_connect(sdata)) {
        mutex_unlock(&sdata->pg_lock);
        LOGWARNING("Failed to connect to database at startup, will retry on demand");
        return;
    }
    mutex_unlock(&sdata->pg_lock);
    LOGNOTICE("Connected to database");
    db_check_blocks(sdata);
}

#else /* HAVE_LIBPQ */

/* Built without libpq. main() refuses --log-shares in that case, so these
 * are never reached with logshares_db set. */
static inline void db_init(sdata_t __maybe_unused* sdata) {}
static inline void db_disconnect(sdata_t __maybe_unused* sdata) {}
static inline void db_insert_user(
    sdata_t __maybe_unused*         sdata,
    user_instance_t __maybe_unused* user,
    const char __maybe_unused*      lightning_address,
    const char __maybe_unused*      workername) {}
static inline void
db_log_share(sdata_t __maybe_unused* sdata, yyjson_mut_doc __maybe_unused* doc, const int __maybe_unused height) {}
static inline void db_add_block(
    sdata_t __maybe_unused*      sdata,
    const int __maybe_unused     height,
    const char __maybe_unused*   hash,
    const bool __maybe_unused    confirmed,
    const char __maybe_unused*   workername,
    const char __maybe_unused*   username,
    const double __maybe_unused  diff,
    const int64_t __maybe_unused coinbasevalue) {}
static inline void db_update_block_status(
    sdata_t __maybe_unused*    sdata,
    const char __maybe_unused* hash,
    const bool __maybe_unused  confirmed,
    const bool __maybe_unused  rewards_processed) {}

#endif /* HAVE_LIBPQ */

/* Priority levels for generator messages */
#define GEN_LAX 0
#define GEN_NORMAL 1
#define GEN_PRIORITY 2

/* For storing a set of messages within another lock, allowing us to dump them
 * to the log outside of lock */
static void add_msg_entry(char_entry_t** entries, char** buf) {
    char_entry_t* entry;

    if (!*buf)
        return;
    entry = ckalloc(sizeof(char_entry_t));
    entry->buf = *buf;
    *buf = NULL;
    DL_APPEND(*entries, entry);
}

static void notice_msg_entries(char_entry_t** entries) {
    char_entry_t *entry, *tmpentry;

    DL_FOREACH_SAFE(*entries, entry, tmpentry) {
        DL_DELETE(*entries, entry);
        LOGNOTICE("%s", entry->buf);
        free(entry->buf);
        free(entry);
    }
}

static void info_msg_entries(char_entry_t** entries) {
    char_entry_t *entry, *tmpentry;

    DL_FOREACH_SAFE(*entries, entry, tmpentry) {
        DL_DELETE(*entries, entry);
        LOGINFO("%s", entry->buf);
        free(entry->buf);
        free(entry);
    }
}

static const int witnessdata_size = 36;  // commitment header + hash

/* Minimal CScriptNum encoding of block height exactly as Bitcoin Core expects */
static int ser_bip34_height(uint8_t* buf, uint32_t height) {
    if (height == 0) {
        buf[0] = 0x00; /* OP_0 */
        return 1;
    }
    if (height <= 16) {
        buf[0] = 0x50 + (uint8_t)height; /* OP_1 (0x51) … OP_16 (0x60) */
        return 1;
    }

    /* General case: shortest little-endian bytes + push length */
    uint8_t  tmp[5];
    int      nlen = 0;
    uint32_t h = height;
    do {
        tmp[nlen++] = h & 0xff;
        h >>= 8;
    } while (h);

    if (tmp[nlen - 1] & 0x80)
        tmp[nlen++] = 0x00; /* sign byte for positive number */

    buf[0] = (uint8_t)nlen; /* push length */
    memcpy(buf + 1, tmp, nlen);
    return nlen + 1;
}

static void generate_coinbase(workbase_t* wb) {
    uint64_t u64, g64, d64 = 0;
    uint32_t u32;
    sdata_t* sdata = ckpool.sdata;
    char     header[272];
    int      len, ofs = 0;
    ts_t     now;

    /* Set fixed length coinb1 arrays to be more than enough */
    wb->coinb1 = ckzalloc(256);
    wb->coinb1bin = ckzalloc(128);

    /* Strings in wb should have been zero memset prior. Generate binary
     * templates first, then convert to hex */
    memcpy(wb->coinb1bin, scriptsig_header_bin, 41);
    ofs += 41;  // Fixed header length;

    ofs++;  // Script length is filled in at the end @wb->coinb1bin[41];

    /* Put block height at start of template */
    if (unlikely(ckpool.regtest))
        len = ser_bip34_height(wb->coinb1bin + ofs, wb->height);
    else
        len = ser_number(wb->coinb1bin + ofs, wb->height);
    ofs += len;

    /* Followed by flag */
    len = strlen(wb->flags) / 2;
    wb->coinb1bin[ofs++] = len;
    hex2bin(wb->coinb1bin + ofs, wb->flags, len);
    ofs += len;

    /* Followed by timestamp */
    ts_realtime(&now);
    len = ser_number(wb->coinb1bin + ofs, now.tv_sec);
    ofs += len;

    /* Followed by our unique randomiser based on the nsec timestamp */
    len = ser_number(wb->coinb1bin + ofs, now.tv_nsec);
    ofs += len;

    wb->enonce1varlen = ckpool.nonce1length;
    wb->enonce2varlen = ckpool.nonce2length;
    wb->coinb1bin[ofs++] = wb->enonce1varlen + wb->enonce2varlen;

    wb->coinb1len = ofs;

    len = wb->coinb1len - 41;

    len += wb->enonce1varlen;
    len += wb->enonce2varlen;

    wb->coinb2bin = ckzalloc(512);
    wb->coinb2len = 0;
    if (ckpool.btcsig) {
        int siglen = strlen(ckpool.btcsig);

        LOGDEBUG("Len %d sig %s", siglen, ckpool.btcsig);
        if (siglen) {
            wb->coinb2bin[wb->coinb2len++] = siglen;
            memcpy(wb->coinb2bin + wb->coinb2len, ckpool.btcsig, siglen);
            wb->coinb2len += siglen;
        }
    }
    len += wb->coinb2len;

    wb->coinb1bin[41] = len - 1; /* Set the length now */
    __bin2hex(wb->coinb1, wb->coinb1bin, wb->coinb1len);
    LOGDEBUG("Coinb1: %s", wb->coinb1);
    /* Coinbase 1 complete */

    /* BIP54 requires the coinbase's nSequence be anything but the maximum value. */
    memcpy(wb->coinb2bin + wb->coinb2len, "\xff\xff\xff\xfe", 4);
    wb->coinb2len += 4;

    /* Generation value. The generation output pays a fixed COIN (in solo
     * mode that output is the miner's own address) and everything above it
     * goes to the pool address in coinb3. A template worth COIN or less
     * pays out as a single output rather than underflowing the remainder,
     * which matters on low value test chains now and on mainnet once the
     * subsidy drops below COIN. */
    g64 = wb->coinbasevalue;
    d64 = 0;
    if (g64 > (uint64_t)COIN) {
        g64 = COIN;
        d64 = wb->coinbasevalue - COIN;
    }
    /* The output count must match what is actually serialised below. */
    wb->coinb2bin[wb->coinb2len++] = (d64 ? 2 : 1) + wb->insert_witness;

    u64 = htole64(g64);
    memcpy(&wb->coinb2bin[wb->coinb2len], &u64, sizeof(uint64_t));
    wb->coinb2len += sizeof(uint64_t);  // 8

    /* Coinb2 address goes here, takes up 23~25 bytes + 1 byte for length */

    wb->coinb3len = 0;
    wb->coinb3bin = ckzalloc(256 + wb->insert_witness * (8 + witnessdata_size + 2));

    if (d64) {
        u64 = htole64(d64);
        memcpy(wb->coinb3bin, &u64, sizeof(uint64_t));
        wb->coinb3len += sizeof(uint64_t);  // 8

        wb->coinb3bin[wb->coinb3len++] = sdata->dontxnlen;
        memcpy(wb->coinb3bin + wb->coinb3len, sdata->dontxnbin, sdata->dontxnlen);
        wb->coinb3len += sdata->dontxnlen;
    }

    if (wb->insert_witness) {
        // 0 value
        wb->coinb3len += 8;

        wb->coinb3bin[wb->coinb3len++] = witnessdata_size + 2;  // total scriptPubKey size
        wb->coinb3bin[wb->coinb3len++] = 0x6a;                  // OP_RETURN
        wb->coinb3bin[wb->coinb3len++] = witnessdata_size;

        hex2bin(&wb->coinb3bin[wb->coinb3len], wb->witnessdata, witnessdata_size);
        wb->coinb3len += witnessdata_size;
    }

    /* Set nLockTime to block height minus 1 as per BIP 54. */
    u32 = htole32(wb->height - 1);
    memcpy(&wb->coinb3bin[wb->coinb3len], &u32, sizeof(uint32_t));
    wb->coinb3len += sizeof(uint32_t);  // 4

    if (!ckpool.btcsolo) {
        int   coinbase_len, offset = 0;
        char *coinbase, *cb;

        /* Append the generation address and coinb3 in !solo mode */
        wb->coinb2bin[wb->coinb2len++] = sdata->txnlen;
        memcpy(wb->coinb2bin + wb->coinb2len, sdata->txnbin, sdata->txnlen);
        wb->coinb2len += sdata->txnlen;
        memcpy(wb->coinb2bin + wb->coinb2len, wb->coinb3bin, wb->coinb3len);
        wb->coinb2len += wb->coinb3len;
        wb->coinb3len = 0;
        dealloc(wb->coinb3bin);

        /* Set this only once */
        if (unlikely(!ckpool.coinbase_valid)) {
            char* cbstr;

            /* We have enough to test the validity of the coinbase here */
            coinbase_len = wb->coinb1len + ckpool.nonce1length + ckpool.nonce2length + wb->coinb2len;
            coinbase = ckzalloc(coinbase_len);
            memcpy(coinbase, wb->coinb1bin, wb->coinb1len);
            offset += wb->coinb1len;
            /* Space for nonce1 and 2 */
            offset += ckpool.nonce1length + ckpool.nonce2length;
            memcpy(coinbase + offset, wb->coinb2bin, wb->coinb2len);
            offset += wb->coinb2len;
            cb = bin2hex(coinbase, offset);
            LOGDEBUG("Coinbase txn %s", cb);
            free(coinbase);
            cbstr = generator_checktxn(cb);
            if (cbstr) {
                LOGNOTICE("Coinbase transaction confirmed valid");
                LOGDEBUG("%s", cbstr);
                free(cbstr);
            } else {
                /* This is a fatal error */
                LOGEMERG("Coinbase failed valid transaction check, aborting!");
                exit(1);
            }
            free(cb);
            ckpool.coinbase_valid = true;
            LOGWARNING("Mining from any incoming username to address %s", ckpool.btcaddress);
            LOGWARNING("Generation output fixed at %" PRId64 " satoshi, remainder to %s", COIN, ckpool.donaddress);
        }
    } else if (unlikely(!ckpool.coinbase_valid)) {
        /* Create a sample coinbase to test its validity in solo mode */
        int   coinbase_len, offset = 0;
        char *coinbase, *cb;
        char* cbstr;

        coinbase_len = wb->coinb1len + ckpool.nonce1length + ckpool.nonce2length + wb->coinb2len + sdata->txnlen +
                       wb->coinb3len + 1;
        coinbase = ckzalloc(coinbase_len);
        memcpy(coinbase, wb->coinb1bin, wb->coinb1len);
        offset += wb->coinb1len;
        offset += ckpool.nonce1length + ckpool.nonce2length;
        memcpy(coinbase + offset, wb->coinb2bin, wb->coinb2len);
        offset += wb->coinb2len;
        coinbase[offset] = sdata->txnlen;
        offset += 1;
        memcpy(coinbase + offset, sdata->txnbin, sdata->txnlen);
        offset += sdata->txnlen;
        memcpy(coinbase + offset, wb->coinb3bin, wb->coinb3len);
        offset += wb->coinb3len;
        cb = bin2hex(coinbase, offset);
        LOGDEBUG("Coinbase txn %s", cb);
        free(coinbase);
        cbstr = generator_checktxn(cb);
        if (cbstr) {
            LOGNOTICE("Coinbase transaction confirmed valid");
            LOGDEBUG("%s", cbstr);
            free(cbstr);
        } else {
            /* This is a fatal error */
            LOGEMERG("Coinbase failed valid transaction check, aborting!");
            exit(1);
        }
        free(cb);
        ckpool.coinbase_valid = true;
        LOGWARNING("Mining solo to any incoming valid BTC address username");
        LOGWARNING("Generation output fixed at %" PRId64 " satoshi, remainder to %s", COIN, ckpool.donaddress);
    }

    /* Set this just for node compatibility, though it's unused */
    wb->coinb2 = bin2hex(wb->coinb2bin, wb->coinb2len);
    LOGDEBUG("Coinb2: %s", wb->coinb2);
    /* Coinbases 2 +/- 3 templates complete */

    snprintf(
        header, 270, "%s%s%s%s%s%s%s", wb->bbversion, wb->prevhash,
        "0000000000000000000000000000000000000000000000000000000000000000", wb->ntime, wb->nbit, "00000000", /* nonce */
        workpadding);
    header[224] = 0;
    LOGDEBUG("Header: %s", header);
    hex2bin(wb->headerbin, header, 112);
}

static void stratum_broadcast_update(sdata_t* sdata, const workbase_t* wb, bool clean);
static void stratum_broadcast_updates(sdata_t* sdata, bool clean);

static void clear_userwb(sdata_t* sdata, int64_t id) {
    user_instance_t *instance, *tmp;

    ck_wlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->user_instances, instance, tmp) {
        struct userwb* userwb;

        HASH_FIND_I64(instance->userwbs, &id, userwb);
        if (!userwb)
            continue;
        HASH_DEL(instance->userwbs, userwb);
        free(userwb->coinb2bin);
        free(userwb->coinb2);
        free(userwb);
    }
    ck_wunlock(&sdata->instance_lock);
}

static void clear_workbase(workbase_t* wb) {
    if (ckpool.btcsolo)
        clear_userwb(ckpool.sdata, wb->id);
    free(wb->flags);
    free(wb->txn_data);
    free(wb->txn_hashes);
    free(wb->logdir);
    free(wb->coinb1bin);
    free(wb->coinb1);
    free(wb->coinb2bin);
    free(wb->coinb2);
    free(wb->coinb3bin);
    if (wb->yymerkle_doc)
        yyjson_mut_doc_free(wb->yymerkle_doc);
    if (wb->gbtdoc)
        yyjson_doc_free(wb->gbtdoc);
#ifdef HAVE_CAPNP
    if (wb->tmpl)
        mining_block_template_destroy(wb->tmpl);
#endif
    free(wb);
}

/* Remove all shares with a workbase id less than wb_id for block changes */
static void purge_share_hashtable(sdata_t* sdata, const int64_t wb_id) {
    share_t *share, *tmp;
    int      purged = 0;

    mutex_lock(&sdata->share_lock);
    HASH_ITER(hh, sdata->shares, share, tmp) {
        if (share->workbase_id < wb_id) {
            HASH_DEL(sdata->shares, share);
            dealloc(share);
            purged++;
        }
    }
    mutex_unlock(&sdata->share_lock);

    if (purged)
        LOGINFO("Cleared %d shares from share hashtable", purged);
}

/* Remove all shares with a workbase id == wb_id being discarded */
static void age_share_hashtable(sdata_t* sdata, const int64_t wb_id) {
    share_t *share, *tmp;
    int      aged = 0;

    mutex_lock(&sdata->share_lock);
    HASH_ITER(hh, sdata->shares, share, tmp) {
        if (share->workbase_id == wb_id) {
            HASH_DEL(sdata->shares, share);
            dealloc(share);
            aged++;
        }
    }
    mutex_unlock(&sdata->share_lock);

    if (aged)
        LOGINFO("Aged %d shares from share hashtable", aged);
}

/* Append a bulk list already created to the ssends list */
static void ssend_bulk_append(sdata_t* sdata, ckmsg_t* bulk_send, const int messages) {
    ckmsgq_t* ssends = sdata->ssends;

    mutex_lock(ssends->lock);
    ssends->messages += messages;
    DL_CONCAT(ssends->msgs, bulk_send);
    pthread_cond_signal(ssends->cond);
    mutex_unlock(ssends->lock);
}

/* As ssend_bulk_append but for high priority messages to be put at the front
 * of the list. */
static void ssend_bulk_prepend(sdata_t* sdata, ckmsg_t* bulk_send, const int messages) {
    ckmsgq_t* ssends = sdata->ssends;
    ckmsg_t*  tmp;

    mutex_lock(ssends->lock);
    tmp = ssends->msgs;
    ssends->msgs = bulk_send;
    ssends->messages += messages;
    DL_CONCAT(ssends->msgs, tmp);
    pthread_cond_signal(ssends->cond);
    mutex_unlock(ssends->lock);
}

/* Send a yyjson doc msg to an upstream trusted remote server */
static void upstream_yyjson(yyjson_mut_doc* doc) {
    char* msg = yyjson_mut_write(doc, YYJSON_WRITE_NEWLINE_AT_END, NULL);

    /* Connector absorbs and frees msg */
    connector_upstream_msg(msg);
}

/* Upstream a yyjson doc msgtype, duplicating the doc */
static void upstream_yydoc_msgtype(yyjson_mut_doc* doc, const int msg_type) {
    yyjson_mut_doc* copy = yyjson_mut_doc_mut_copy(doc, &ckyyalc);
    yyjson_mut_val* root = yyjson_mut_doc_get_root(copy);

    yyjson_mut_obj_add_strcpy(copy, root, "method", stratum_msgs[msg_type]);
    upstream_yyjson(copy);
    yyjson_mut_doc_free(copy);
}

static void send_node_workinfo(sdata_t* sdata, const workbase_t* wb) {
    yyjson_mut_doc*     wb_doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val*     wb_val = yyjson_mut_obj(wb_doc);
    stratum_instance_t* client;
    ckmsg_t*            bulk_send = NULL;
    int                 messages = 0;

    yyjson_mut_doc_set_root(wb_doc, wb_val);

    yyjson_mut_obj_add_int(wb_doc, wb_val, "jobid", wb->mapped_id);
    yyjson_mut_obj_add_strcpy(wb_doc, wb_val, "target", wb->target);
    yyjson_mut_obj_add_real(wb_doc, wb_val, "diff", wb->diff);
    yyjson_mut_obj_add_int(wb_doc, wb_val, "version", wb->version);
    yyjson_mut_obj_add_int(wb_doc, wb_val, "curtime", wb->curtime);
    yyjson_mut_obj_add_strcpy(wb_doc, wb_val, "prevhash", wb->prevhash);
    yyjson_mut_obj_add_strcpy(wb_doc, wb_val, "ntime", wb->ntime);
    yyjson_mut_obj_add_strcpy(wb_doc, wb_val, "bbversion", wb->bbversion);
    yyjson_mut_obj_add_strcpy(wb_doc, wb_val, "nbit", wb->nbit);
    yyjson_mut_obj_add_int(wb_doc, wb_val, "coinbasevalue", wb->coinbasevalue);
    yyjson_mut_obj_add_int(wb_doc, wb_val, "height", wb->height);
    yyjson_mut_obj_add_strcpy(wb_doc, wb_val, "flags", wb->flags);
    yyjson_mut_obj_add_int(wb_doc, wb_val, "txns", wb->txns);
    yyjson_mut_obj_add_strcpy(wb_doc, wb_val, "txn_hashes", wb->txn_hashes);
    yyjson_mut_obj_add_int(wb_doc, wb_val, "merkles", wb->merkles);
    yyjson_mut_obj_add_val(
        wb_doc, wb_val, "merklehash", yyjson_mut_val_mut_copy(wb_doc, yyjson_mut_doc_get_root(wb->yymerkle_doc)));
    yyjson_mut_obj_add_strcpy(wb_doc, wb_val, "coinb1", wb->coinb1);
    yyjson_mut_obj_add_int(wb_doc, wb_val, "enonce1varlen", wb->enonce1varlen);
    yyjson_mut_obj_add_int(wb_doc, wb_val, "enonce2varlen", wb->enonce2varlen);
    yyjson_mut_obj_add_int(wb_doc, wb_val, "coinb1len", wb->coinb1len);
    yyjson_mut_obj_add_int(wb_doc, wb_val, "coinb2len", wb->coinb2len);
    yyjson_mut_obj_add_strcpy(wb_doc, wb_val, "coinb2", wb->coinb2);

    ck_rlock(&sdata->instance_lock);
    DL_FOREACH2(sdata->node_instances, client, node_next) {
        yyjson_mut_doc* doc = yyjson_mut_doc_mut_copy(wb_doc, &ckyyalc);
        yyjson_mut_val* root = yyjson_mut_doc_get_root(doc);
        ckmsg_t*        client_msg;
        smsg_t*         msg;

        yyjson_mut_obj_add_strcpy(doc, root, "node.method", stratum_msgs[SM_WORKINFO]);
        client_msg = ckalloc(sizeof(ckmsg_t));
        msg = ckzalloc(sizeof(smsg_t));
        msg->doc = doc;
        msg->client_id = client->id;
        client_msg->data = msg;
        DL_APPEND(bulk_send, client_msg);
        messages++;
    }
    DL_FOREACH2(sdata->remote_instances, client, remote_next) {
        yyjson_mut_doc* doc = yyjson_mut_doc_mut_copy(wb_doc, &ckyyalc);
        yyjson_mut_val* root = yyjson_mut_doc_get_root(doc);
        ckmsg_t*        client_msg;
        smsg_t*         msg;

        yyjson_mut_obj_add_strcpy(doc, root, "method", stratum_msgs[SM_WORKINFO]);
        client_msg = ckalloc(sizeof(ckmsg_t));
        msg = ckzalloc(sizeof(smsg_t));
        msg->doc = doc;
        msg->client_id = client->id;
        client_msg->data = msg;
        DL_APPEND(bulk_send, client_msg);
        messages++;
    }
    ck_runlock(&sdata->instance_lock);

    if (ckpool.remote)
        upstream_yydoc_msgtype(wb_doc, SM_WORKINFO);

    yyjson_mut_doc_free(wb_doc);

    if (bulk_send) {
        LOGINFO("Sending workinfo to mining nodes");
        ssend_bulk_append(sdata, bulk_send, messages);
    }
}

static yyjson_mut_doc* generate_workinfo(const workbase_t* wb, const char* func) {
    yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val* val = yyjson_mut_obj(doc);
    char            cdfield[64];

    yyjson_mut_doc_set_root(doc, val);

    sprintf(cdfield, "%lu,%lu", wb->gentime.tv_sec, wb->gentime.tv_nsec);

    yyjson_mut_obj_add_int(doc, val, "workinfoid", wb->id);
    yyjson_mut_obj_add_strcpy(doc, val, "poolinstance", ckpool.name);
    yyjson_mut_obj_add_strcpy(doc, val, "transactiontree", wb->txn_hashes);
    yyjson_mut_obj_add_strcpy(doc, val, "prevhash", wb->prevhash);
    yyjson_mut_obj_add_strcpy(doc, val, "coinbase1", wb->coinb1);
    yyjson_mut_obj_add_strcpy(doc, val, "coinbase2", wb->coinb2);
    yyjson_mut_obj_add_strcpy(doc, val, "version", wb->bbversion);
    yyjson_mut_obj_add_strcpy(doc, val, "ntime", wb->ntime);
    yyjson_mut_obj_add_strcpy(doc, val, "bits", wb->nbit);
    yyjson_mut_obj_add_int(doc, val, "reward", wb->coinbasevalue);
    yyjson_mut_obj_add_val(
        doc, val, "merklehash", yyjson_mut_val_mut_copy(doc, yyjson_mut_doc_get_root(wb->yymerkle_doc)));
    yyjson_mut_obj_add_strcpy(doc, val, "createdate", cdfield);
    yyjson_mut_obj_add_strcpy(doc, val, "createby", "code");
    yyjson_mut_obj_add_strcpy(doc, val, "createcode", func);
    yyjson_mut_obj_add_strcpy(doc, val, "createinet", ckpool.serverurl[0]);
    return doc;
}

static void send_workinfo(sdata_t* sdata, const workbase_t* wb) {
    if (!ckpool.proxy)
        send_node_workinfo(sdata, wb);
}

/* Entered with instance_lock held, make sure wb can't be pulled from us */
static void __generate_userwb(sdata_t* sdata, workbase_t* wb, user_instance_t* user) {
    struct userwb* userwb;
    int64_t        id = wb->id;

    /* Make sure this user doesn't have this userwb already */
    HASH_FIND_I64(user->userwbs, &id, userwb);
    if (unlikely(userwb))
        return;

    sdata->userwbs_generated++;
    userwb = ckzalloc(sizeof(struct userwb));
    userwb->id = id;
    userwb->coinb2bin = ckalloc(wb->coinb2len + 1 + user->txnlen + wb->coinb3len);
    memcpy(userwb->coinb2bin, wb->coinb2bin, wb->coinb2len);
    userwb->coinb2len = wb->coinb2len;
    userwb->coinb2bin[userwb->coinb2len++] = user->txnlen;
    memcpy(userwb->coinb2bin + userwb->coinb2len, user->txnbin, user->txnlen);
    userwb->coinb2len += user->txnlen;
    memcpy(userwb->coinb2bin + userwb->coinb2len, wb->coinb3bin, wb->coinb3len);
    userwb->coinb2len += wb->coinb3len;
    userwb->coinb2 = bin2hex(userwb->coinb2bin, userwb->coinb2len);
    HASH_ADD_I64(user->userwbs, id, userwb);
}

static void generate_userwbs(sdata_t* sdata, workbase_t* wb) {
    user_instance_t *instance, *tmp;

    ck_wlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->user_instances, instance, tmp) {
        if (!instance->btcaddress)
            continue;
        __generate_userwb(sdata, wb, instance);
    }
    ck_wunlock(&sdata->instance_lock);
}

/* Add a new workbase to the table of workbases. Sdata is the global data in
 * pool mode but unique to each subproxy in proxy mode */
static void add_base(sdata_t* sdata, workbase_t* wb, bool* new_block) {
    sdata_t*      ckp_sdata = ckpool.sdata;
    pool_stats_t* stats = &sdata->stats;
    /* Per-sdata last value: proxy mode has one sdata per subproxy. Comparing
     * against the pool-wide stat re-logged every notify when upstreams are
     * on different networks (mainnet + testnet thrashing the global). */
    double      old_diff = stats->network_diff;
    workbase_t *tmp, *tmpa;
    bool        update_global;
    int         len, ret;

    ts_realtime(&wb->gentime);
    /* Stats network_diff is not protected by lock but is not a critical
     * value. Share validation and block-solve checks always use
     * wb->network_diff / current_workbase->network_diff on the client's
     * bound sdata, so mixed-network proxies remain correct. */
    wb->network_diff = diff_from_nbits(wb->headerbin + 72);
    if (wb->network_diff < 1)
        wb->network_diff = 1;
    stats->network_diff = wb->network_diff;

    /* Pool hashrate-%-of-network reporting: only the preferred parent
     * updates the shared copy so multi-network configs do not thrash it. */
    update_global = !ckpool.proxy;
    if (ckpool.proxy && sdata->subproxy) {
        proxy_t* cur;

        mutex_lock(&ckp_sdata->proxy_lock);
        cur = ckp_sdata->proxy;
        update_global = !cur || sdata->subproxy->parent == cur;
        mutex_unlock(&ckp_sdata->proxy_lock);
    }
    if (update_global)
        ckp_sdata->stats.network_diff = wb->network_diff;

    if (wb->network_diff != old_diff) {
        if (ckpool.proxy && sdata->subproxy)
            LOGWARNING(
                "Network diff set to %.1f (proxy %d:%d)", wb->network_diff, sdata->subproxy->id,
                sdata->subproxy->subid);
        else
            LOGWARNING("Network diff set to %.1f", wb->network_diff);
    }
    len = strlen(ckpool.logdir) + 8 + 1 + 16 + 1;
    wb->logdir = ckzalloc(len);

    /* In proxy mode, the wb->id is received in the notify update and
     * we set workbase_id from it. In server mode the stratifier is
     * setting the workbase_id */
    ck_wlock(&sdata->workbase_lock);
    ckp_sdata->workbases_generated++;
    if (!ckpool.proxy)
        wb->mapped_id = wb->id = sdata->workbase_id++;
    else
        sdata->workbase_id = wb->id;
    if (strncmp(wb->prevhash, sdata->lasthash, 64)) {
        char bin[32], swap[32];

        *new_block = true;
        memcpy(sdata->lasthash, wb->prevhash, 65);
        hex2bin(bin, sdata->lasthash, 32);
        swap_256(swap, bin);
        __bin2hex(sdata->lastswaphash, swap, 32);
        sdata->blockchange_id = wb->id;
    }
    if (*new_block && ckpool.logshares_file) {
        sprintf(wb->logdir, "%s%08x/", ckpool.logdir, wb->height);
        ret = mkdir(wb->logdir, 0750);
        if (unlikely(ret && errno != EEXIST))
            LOGERR("Failed to create log directory %s", wb->logdir);
    }
    sprintf(wb->idstring, "%016lx", wb->id);
    if (ckpool.logshares_file)
        sprintf(wb->logdir, "%s%08x/%s", ckpool.logdir, wb->height, wb->idstring);

    HASH_ADD_I64(sdata->workbases, id, wb);
    if (sdata->current_workbase)
        tv_time(&sdata->current_workbase->retired);
    sdata->current_workbase = wb;

    /* Is this long enough to ensure we don't dereference a workbase
     * immediately? Should be unless clock changes 10 minutes so we use
     * ts_realtime */
    HASH_ITER(hh, sdata->workbases, tmp, tmpa) {
        if (HASH_COUNT(sdata->workbases) < 3)
            break;
        if (wb == tmp)
            continue;
        if (tmp->readcount)
            continue;
        /*  Age old workbases older than 10 minutes old */
        if (tmp->gentime.tv_sec < wb->gentime.tv_sec - 600) {
            HASH_DEL(sdata->workbases, tmp);
            ck_wunlock(&sdata->workbase_lock);

            /* Drop lock to avoid recursive locks */
            age_share_hashtable(sdata, tmp->id);
            clear_workbase(tmp);

            ck_wlock(&sdata->workbase_lock);
        }
    }
    ck_wunlock(&sdata->workbase_lock);

    /* This wb can't be pulled out from under us so no workbase lock is
     * required to generate_userwbs */
    if (ckpool.btcsolo)
        generate_userwbs(sdata, wb);

    if (*new_block)
        purge_share_hashtable(sdata, wb->id);

    if (!ckpool.passthrough)
        send_workinfo(sdata, wb);
}

static void broadcast_ping(sdata_t* sdata);

#define REFCOUNT_REMOTE 20
#define REFCOUNT_LOCAL 10
#define REFCOUNT_RETURNED 5

/* Submit the transactions in node/remote mode so the local btcd has all the
 * transactions that will go into the next blocksolve. */
static void submit_transaction(const char* hash) {
    char* buf;

    if (unlikely(!ckpool.generator_ready))
        return;
    ASPRINTF(&buf, "submittxn:%s", hash);
    send_proc(ckpool.generator, buf);
    free(buf);
}

/* Build a hashlist of all transactions, allowing us to compare with the list of
 * existing transactions to determine which need to be propagated */
static bool add_txn(sdata_t* sdata, txntable_t** txns, const char* hash, const char* data, bool local) {
    bool        found = false;
    txntable_t* txn;

    /* Don't waste our time with a transaction hashlist if we don't have
     * any trusted or node servers configured */
    if (!ckpool.trusted && !ckpool.nodeserver)
        return found;

    /* Look for transactions we already know about and increment their
     * refcount if we're still using them. */
    ck_wlock(&sdata->txn_lock);
    HASH_FIND_STR(sdata->txns, hash, txn);
    if (txn) {
        /* If we already have this in our transaction table but haven't
         * seen it in a while, it is reappearing in work and we should
         * propagate it again in update_txns. */
        if (txn->refcount > REFCOUNT_RETURNED)
            found = true;
        if (!local)
            txn->refcount = REFCOUNT_REMOTE;
        else if (txn->refcount < REFCOUNT_LOCAL)
            txn->refcount = REFCOUNT_LOCAL;
        txn->seen = true;
    }
    ck_wunlock(&sdata->txn_lock);

    if (found)
        return false;

    txn = ckzalloc(sizeof(txntable_t));
    memcpy(txn->hash, hash, 65);
    if (local)
        txn->data = strdup(data);
    else {
        /* Get the data from our local bitcoind as a way of confirming it
         * already knows about this transaction. */
        txn->data = generator_get_txn(hash);
        if (!txn->data) {
            /* If our local bitcoind hasn't seen this transaction,
             * submit it for mempools to be ~synchronised */
            submit_transaction(data);
            txn->data = strdup(data);
        }
    }

    txn->seen = true;
    if (!local || ckpool.node)
        txn->refcount = REFCOUNT_REMOTE;
    else
        txn->refcount = REFCOUNT_LOCAL;
    HASH_ADD_STR(*txns, hash, txn);

    return true;
}

static void send_node_transactions(sdata_t* sdata, yyjson_mut_doc* txn_doc) {
    stratum_instance_t* client;
    ckmsg_t*            bulk_send = NULL;
    ckmsg_t*            client_msg;
    yyjson_mut_val*     root;
    yyjson_mut_doc*     doc;
    int                 messages = 0;
    smsg_t*             msg;

    ck_rlock(&sdata->instance_lock);
    DL_FOREACH2(sdata->node_instances, client, node_next) {
        doc = yyjson_mut_doc_mut_copy(txn_doc, &ckyyalc);
        root = yyjson_mut_doc_get_root(doc);
        yyjson_mut_obj_add_strcpy(doc, root, "node.method", stratum_msgs[SM_TRANSACTIONS]);
        client_msg = ckalloc(sizeof(ckmsg_t));
        msg = ckzalloc(sizeof(smsg_t));
        msg->doc = doc;
        msg->client_id = client->id;
        client_msg->data = msg;
        DL_APPEND(bulk_send, client_msg);
        messages++;
    }
    DL_FOREACH2(sdata->remote_instances, client, remote_next) {
        doc = yyjson_mut_doc_mut_copy(txn_doc, &ckyyalc);
        root = yyjson_mut_doc_get_root(doc);
        yyjson_mut_obj_add_strcpy(doc, root, "method", stratum_msgs[SM_TRANSACTIONS]);
        client_msg = ckalloc(sizeof(ckmsg_t));
        msg = ckzalloc(sizeof(smsg_t));
        msg->doc = doc;
        msg->client_id = client->id;
        client_msg->data = msg;
        DL_APPEND(bulk_send, client_msg);
        messages++;
    }
    ck_runlock(&sdata->instance_lock);

    if (ckpool.remote)
        upstream_yydoc_msgtype(txn_doc, SM_TRANSACTIONS);

    if (bulk_send) {
        LOGINFO("Sending transactions to mining nodes");
        ssend_bulk_append(sdata, bulk_send, messages);
    }
}

static void submit_transaction_array(yyjson_mut_val* arr) {
    yyjson_mut_val*     arr_val;
    yyjson_mut_arr_iter iter;

    yyjson_mut_arr_iter_init(arr, &iter);
    while ((arr_val = yyjson_mut_arr_iter_next(&iter)) != NULL)
        submit_transaction(yyjson_mut_get_str(arr_val));
}

static void clear_txn(txntable_t* txn) {
    free(txn->data);
    free(txn);
}

static void update_txns(sdata_t* sdata, txntable_t* txns, bool local) {
    yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val* txn_array = yyjson_mut_arr(doc);
    yyjson_mut_val* purged_txns = yyjson_mut_arr(doc);
    int             added = 0, purged = 0;
    txntable_t *    tmp, *tmpa;

    /* Find which transactions have their refcount decremented to zero
     * and remove them. */
    ck_wlock(&sdata->txn_lock);
    HASH_ITER(hh, sdata->txns, tmp, tmpa) {
        if (tmp->seen) {
            tmp->seen = false;
            continue;
        }
        if (tmp->refcount-- > 0)
            continue;
        HASH_DEL(sdata->txns, tmp);
        yyjson_mut_arr_add_strcpy(doc, purged_txns, tmp->data);
        clear_txn(tmp);
        purged++;
    }
    /* Add the new transactions to the transaction table */
    HASH_ITER(hh, txns, tmp, tmpa) {
        yyjson_mut_val* txn_val;
        txntable_t*     found;

        HASH_DEL(txns, tmp);
        /* Propagate transaction here */
        txn_val = yyjson_mut_pack_val(doc, "{ss,ss}", "hash", tmp->hash, "data", tmp->data);
        yyjson_mut_arr_append(txn_array, txn_val);

        /* Check one last time this txn hasn't already been added in the
         * interim. This can happen in add_txn intentionally for a
         * transaction that has reappeared. */
        HASH_FIND_STR(sdata->txns, tmp->hash, found);
        if (found) {
            clear_txn(tmp);
            continue;
        }

        /* Move to the sdata transaction table */
        HASH_ADD_STR(sdata->txns, hash, tmp);
        sdata->txns_generated++;
        added++;
    }
    ck_wunlock(&sdata->txn_lock);

    if (added) {
        yyjson_mut_val* root = yyjson_mut_obj(doc);

        yyjson_mut_obj_add_val(doc, root, "transaction", txn_array);
        yyjson_mut_doc_set_root(doc, root);
        send_node_transactions(sdata, doc);
    }

    /* Submit transactions to bitcoind again when we're purging them in
     * case they've been removed from its mempool as well and we need them
     * again in the future for a remote workinfo that hasn't forgotten
     * about them. */
    if (purged && ckpool.nodeservers)
        submit_transaction_array(purged_txns);
    yyjson_mut_doc_free(doc);

    if (added || purged) {
        LOGINFO("Stratifier added %d %stransactions and purged %d", added, local ? "" : "remote ", purged);
    }
}

/* Distill down a set of transactions into an efficient tree arrangement for
 * stratum messages and fast work assembly. */
static txntable_t* wb_merkle_bin_txns(sdata_t* sdata, workbase_t* wb, yyjson_val* txn_array, bool local) {
    int             i, j, binleft, binlen;
    txntable_t*     txns = NULL;
    yyjson_val*     arr_val;
    yyjson_mut_val* arr;
    uchar*          hashbin;
    char*           fname = NULL;
    FILE*           fp = NULL;

    if (ckpool.logtxns) {
        ASPRINTF(&fname, "%s/pool/pool.txns", ckpool.logdir);
        fp = fopen(fname, "we");
        if (!fp)
            LOGERR("Failed to fopen %s", fname);
        dealloc(fname);
    }

    wb->txns = yyjson_arr_size(txn_array);
    wb->merkles = 0;
    /* Guard against a corrupt transaction count that would overflow the
     * size arithmetic below, oversize the hashbin stack allocation or
     * exceed the fixed size merkle arrays. Fall through as a zero
     * transaction template rather than deriving work from garbage. */
    if (unlikely(wb->txns < 0 || wb->txns > MAX_GBT_TXNS)) {
        LOGWARNING("Invalid transaction count %d in wb_merkle_bin_txns, ignoring transactions", wb->txns);
        wb->txns = 0;
    }
    binlen = wb->txns * 32 + 32;
    hashbin = alloca(binlen + 32);
    memset(hashbin, 0, 32);
    binleft = binlen / 32;
    if (wb->txns) {
        int         len = 1, ofs = 0;
        const char* txn;

        for (i = 0; i < wb->txns; i++) {
            arr_val = yyjson_arr_get(txn_array, i);
            txn = yyjson_get_str(yyjson_obj_get(arr_val, "data"));
            if (!txn) {
                LOGWARNING("Failed to find transaction data in wb_merkle_bin_txns");
                goto out;
            }
            len += strlen(txn);
        }

        wb->txn_data = ckzalloc(len + 1);
        wb->txn_hashes = ckzalloc(wb->txns * 65 + 1);
        memset(wb->txn_hashes, 0x20, wb->txns * 65);  // Spaces

        for (i = 0; i < wb->txns; i++) {
            const char *txid, *hash;
            char        binswap[32];

            arr_val = yyjson_arr_get(txn_array, i);

            // Post-segwit, txid returns the tx hash without witness data
            txid = yyjson_get_str(yyjson_obj_get(arr_val, "txid"));
            hash = yyjson_get_str(yyjson_obj_get(arr_val, "hash"));
            if (!txid)
                txid = hash;
            /* Both are fixed 64 hex char values, txid gets copied into
             * the fixed width txn_hashes and hash is used as a fixed
             * length lookup key and copied into a fixed buffer in
             * add_txn, so reject a corrupt or missing value. */
            if (unlikely(!txid || strlen(txid) != 64)) {
                LOGERR("Missing or invalid txid for transaction in wb_merkle_bins");
                goto out;
            }
            if (!hash)
                hash = txid;
            else if (unlikely(strlen(hash) != 64)) {
                LOGERR("Invalid transaction hash in wb_merkle_bins");
                goto out;
            }
            if (fp)
                fprintf(fp, "%s\n", txid);
            txn = yyjson_get_str(yyjson_obj_get(arr_val, "data"));
            add_txn(sdata, &txns, hash, txn, local);
            len = strlen(txn);
            memcpy(wb->txn_data + ofs, txn, len);
            ofs += len;
            if (!hex2bin(binswap, txid, 32)) {
                LOGERR("Failed to hex2bin hash in gbt_merkle_bins");
                goto out;
            }
            memcpy(wb->txn_hashes + i * 65, txid, 64);
            bswap_256(hashbin + 32 + 32 * i, binswap);
        }
    } else
        wb->txn_hashes = ckzalloc(1);
    if (wb->yymerkle_doc)
        yyjson_mut_doc_free(wb->yymerkle_doc);
    wb->yymerkle_doc = yyjson_mut_doc_new(&ckyyalc);
    arr = yyjson_mut_arr(wb->yymerkle_doc);
    yyjson_mut_doc_set_root(wb->yymerkle_doc, arr);

    if (binleft > 1) {
        while (42) {
            if (binleft == 1)
                break;
            /* The transaction count is bounded to keep this within the
             * fixed size merkle arrays but guard the write regardless. */
            if (unlikely(wb->merkles >= 16)) {
                LOGWARNING("Merkle count overflow in wb_merkle_bin_txns");
                goto out;
            }
            memcpy(&wb->merklebin[wb->merkles][0], hashbin + 32, 32);
            __bin2hex(&wb->merklehash[wb->merkles][0], &wb->merklebin[wb->merkles][0], 32);
            yyjson_mut_arr_add_str(wb->yymerkle_doc, arr, &wb->merklehash[wb->merkles][0]);
            LOGDEBUG("MerkleHash %d %s", wb->merkles, &wb->merklehash[wb->merkles][0]);
            wb->merkles++;
            if (binleft % 2) {
                memcpy(hashbin + binlen, hashbin + binlen - 32, 32);
                binlen += 32;
                binleft++;
            }
            for (i = 32, j = 64; j < binlen; i += 32, j += 64)
                gen_hash(hashbin + j, hashbin + i, 64);
            binleft /= 2;
            binlen = binleft * 32;
        }
    }
    LOGNOTICE("Stored %s workbase with %d transactions", local ? "local" : "remote", wb->txns);
out:
    if (fp)
        fclose(fp);
    return txns;
}

static const unsigned char witness_nonce[32] = {0};
static const int           witness_nonce_size = sizeof(witness_nonce);
static const unsigned char witness_header[] = {0xaa, 0x21, 0xa9, 0xed};
static const int           witness_header_size = sizeof(witness_header);

static void gbt_witness_data(workbase_t* wb, yyjson_val* txn_array) {
    int         i, binlen, txncount = yyjson_arr_size(txn_array);
    const char* hash;
    yyjson_val* arr_val;
    uchar*      hashbin;

    /* Guard against a corrupt transaction count that would overflow the
     * size arithmetic or oversize the hashbin stack allocation. */
    if (unlikely(txncount < 0 || txncount > MAX_GBT_TXNS)) {
        LOGWARNING("Invalid transaction count %d in gbt_witness_data", txncount);
        return;
    }
    binlen = txncount * 32 + 32;
    hashbin = alloca(binlen + 32);
    memset(hashbin, 0, 32);

    for (i = 0; i < txncount; i++) {
        char binswap[32];

        arr_val = yyjson_arr_get(txn_array, i);
        hash = yyjson_get_str(yyjson_obj_get(arr_val, "hash"));
        if (unlikely(!hash)) {
            LOGERR("Hash missing for transaction");
            return;
        }
        if (!hex2bin(binswap, hash, 32)) {
            LOGERR("Failed to hex2bin hash in gbt_witness_data");
            return;
        }
        bswap_256(hashbin + 32 + 32 * i, binswap);
    }

    // Build merkle root (copied from libblkmaker)
    for (txncount++; txncount > 1; txncount /= 2) {
        if (txncount % 2) {
            // Odd number, duplicate the last
            memcpy(hashbin + 32 * txncount, hashbin + 32 * (txncount - 1), 32);
            txncount++;
        }
        for (i = 0; i < txncount; i += 2) {
            // We overlap input and output here, on the first pair
            gen_hash(hashbin + 32 * i, hashbin + 32 * (i / 2), 64);
        }
    }

    memcpy(hashbin + 32, &witness_nonce, witness_nonce_size);
    gen_hash(hashbin, hashbin + witness_header_size, 32 + witness_nonce_size);
    memcpy(hashbin, witness_header, witness_header_size);
    __bin2hex(wb->witnessdata, hashbin, 32 + witness_header_size);
    wb->insert_witness = true;
}

#ifdef HAVE_CAPNP
/* Build a workbase from the mining IPC interface (createNewBlock) instead of
 * getblocktemplate. Populates the same fields the GBT path does so that
 * generate_coinbase() and all downstream stratum handling work unchanged; the
 * template handle is retained for submitSolution. Returns NULL on any failure
 * so the caller can fall back to GBT. */
static workbase_t* build_ipc_workbase(void) {
    unsigned char          branch[MINING_MAX_MERKLES][32];
    unsigned char          header[80], rev[32], swap[32];
    mining_block_template* tmpl = NULL;
    yyjson_mut_val*        arr;
    mining_coinbase        cb;
    workbase_t*            wb;
    int                    count = 0, i;
    uint32_t               v;

    if (mining_ipc_create_new_block(ckpool.btc_template_svc, &tmpl))
        return NULL;
    if (mining_ipc_template_header(tmpl, header) || mining_ipc_template_coinbase(tmpl, &cb) ||
        mining_ipc_template_merkle_path(tmpl, branch, &count) || count > 16) {
        mining_block_template_destroy(tmpl);
        return NULL;
    }

    wb = ckzalloc(sizeof(workbase_t));
    wb->ckp = &ckpool;
    wb->ipc = true;
    wb->tmpl = tmpl;

    /* Header-derived fields in ckpool's stratum-display format. */
    memcpy(&v, header, 4);
    wb->version = v;
    snprintf(wb->bbversion, 9, "%08x", v);
    memcpy(&v, header + 68, 4);
    wb->curtime = v;
    wb->ntime32 = v;
    snprintf(wb->ntime, 9, "%08x", v);
    memcpy(&v, header + 72, 4);
    snprintf(wb->nbit, 9, "%08x", v);

    /* prevhash: the header carries it in internal byte order; reverse to the
     * big-endian form gen_gbtbase() receives from RPC, then word-swap. */
    for (i = 0; i < 32; i++)
        rev[i] = header[4 + 31 - i];
    swap_256(swap, rev);
    __bin2hex(wb->prevhash, swap, 32);

    /* Coinbase-derived fields. */
    wb->coinbasevalue = cb.block_reward_remaining;
    wb->height = (int)cb.lock_time + 1; /* BIP54: lock_time = height - 1 */
    wb->flags = strdup("");

    /* Locate the segwit witness commitment among the required outputs and
     * extract its 36-byte witnessdata so generate_coinbase() rebuilds the
     * identical output. A required output is the commitment when its
     * scriptPubKey is OP_RETURN <push 36> aa21a9ed <32 byte hash>, i.e.
     * output layout value(8) scriptlen(1) 6a 24 aa 21 a9 ed <hash>, so bytes
     * 9..14 are 6a 24 aa 21 a9 ed and the 36-byte witnessdata starts at 11.
     * ckpool can only represent the commitment in its coinbase, so any other
     * required output means the template must be built from getblocktemplate
     * instead. */
    for (i = 0; i < cb.required_outputs_count; i++) {
        const unsigned char* ro = cb.required_output[i];
        size_t               rl = cb.required_output_len[i];

        if (rl >= 11 + 36 && ro[9] == 0x6a && ro[10] == 0x24 && ro[11] == 0xaa && ro[12] == 0x21 && ro[13] == 0xa9 &&
            ro[14] == 0xed) {
            if (unlikely(wb->insert_witness)) {
                LOGWARNING("IPC template has multiple witness commitments, using getblocktemplate");
                clear_workbase(wb);
                return NULL;
            }
            __bin2hex(wb->witnessdata, ro + 11, 36);
            wb->insert_witness = true;
        } else {
            LOGWARNING("IPC template requires an unrepresentable coinbase output, using getblocktemplate");
            clear_workbase(wb);
            return NULL;
        }
    }
    /* The witness commitment in the required output was computed over this
     * reserved value, and ipc_submit_block can only serialise a 32 byte one.
     * Silently leaving it zeroed on a length mismatch would build a block
     * that only fails at submit time, on a solve, so fall back to
     * getblocktemplate instead. Without a commitment nothing commits to it. */
    if (likely(cb.witness_len == sizeof(wb->coinbase_witness)))
        memcpy(wb->coinbase_witness, cb.witness, sizeof(wb->coinbase_witness));
    else if (wb->insert_witness) {
        LOGWARNING(
            "IPC template witness reserved value is %zu bytes not %zu, using getblocktemplate", cb.witness_len,
            sizeof(wb->coinbase_witness));
        clear_workbase(wb);
        return NULL;
    }

    /* Merkle branch straight from the template — no mempool hashing. */
    wb->yymerkle_doc = yyjson_mut_doc_new(&ckyyalc);
    arr = yyjson_mut_arr(wb->yymerkle_doc);
    yyjson_mut_doc_set_root(wb->yymerkle_doc, arr);
    wb->merkles = count;
    for (i = 0; i < count; i++) {
        memcpy(&wb->merklebin[i][0], branch[i], 32);
        __bin2hex(&wb->merklehash[i][0], branch[i], 32);
        yyjson_mut_arr_add_str(wb->yymerkle_doc, arr, &wb->merklehash[i][0]);
    }

    /* submitSolution reconstructs the block server-side, so no local
     * transaction data is required. */
    wb->txns = 0;
    wb->txn_hashes = ckzalloc(1);

    LOGINFO("Generated IPC block template for height %d, %d merkles", wb->height, count);
    return wb;
}
#endif

/* This function assumes it will only receive a valid json gbt base template
 * since checking should have been done earlier, and creates the base template
 * for generating work templates. This is a ckmsgq so all uses of this function
 * are serialised. */
static void block_update(int* prio) {
    bool        new_block = false, ret = false;
    const char* witnessdata_check;
    sdata_t*    sdata = ckpool.sdata;
    yyjson_val* txn_array;
    txntable_t* txns = NULL;
    int         retries = 0;
    workbase_t* wb = NULL;

#ifdef HAVE_CAPNP
    /* Prefer the mining IPC interface for block templates when enabled and
     * ready, falling back to getblocktemplate below on any failure. */
    if (ckpool.btc_template_svc && mining_ipc_service_ready(ckpool.btc_template_svc)) {
        wb = build_ipc_workbase();
        if (unlikely(!wb))
            LOGWARNING("IPC template generation failed, falling back to getblocktemplate");
    }
#endif
    if (!wb) {
    retry:
        wb = generator_getbase();
        if (unlikely(!wb)) {
            if (retries++ < 5 || *prio == GEN_PRIORITY) {
                LOGWARNING("Generator returned failure in update_base, retry #%d", retries);
                goto retry;
            }
            LOGWARNING("Generator failed in update_base after retrying");
            goto out;
        }
        if (unlikely(retries))
            LOGWARNING("Generator succeeded in update_base after retrying");

        txn_array = yyjson_obj_get(wb->gbtroot, "transactions");
        txns = wb_merkle_bin_txns(sdata, wb, txn_array, true);

        wb->insert_witness = false;

        witnessdata_check = yyjson_get_str(yyjson_obj_get(wb->gbtroot, "default_witness_commitment"));
        if (likely(witnessdata_check)) {
            LOGDEBUG("Default witness commitment present, adding witness data");
            gbt_witness_data(wb, txn_array);
            // Verify against the pre-calculated value if it exists. Skip the size/OP_RETURN bytes.
            if (wb->insert_witness && safecmp(witnessdata_check + 4, wb->witnessdata) != 0)
                LOGERR("Witness from btcd: %s. Calculated Witness: %s", witnessdata_check + 4, wb->witnessdata);
        }
    }

    generate_coinbase(wb);

    add_base(sdata, wb, &new_block);

    if (new_block)
        LOGNOTICE("Block hash changed to %s", sdata->lastswaphash);
    if (ckpool.btcsolo)
        stratum_broadcast_updates(sdata, new_block);
    else
        stratum_broadcast_update(sdata, wb, new_block);
    ret = true;
    LOGINFO("Broadcast updated stratum base");
    /* Update transactions after stratum broadcast to not delay
     * propagation. */
    if (likely(txns))
        update_txns(sdata, txns, true);
    /* Reset the update time to avoid stacked low priority notifies. Bring
     * forward the next notify in case of a new block. */
    sdata->update_time = time(NULL);
    if (new_block)
        sdata->update_time -= ckpool.update_interval / 2;
out:

    cksem_post(&sdata->update_sem);

    /* Send a ping to miners if we fail to get a base to keep them
     * connected while bitcoind recovers(?) */
    if (unlikely(!ret)) {
        LOGINFO("Broadcast ping due to failed stratum base update");
        broadcast_ping(sdata);
    }
    free(prio);
}

#define SSEND_PREPEND 0
#define SSEND_APPEND 1

/* Downstream a json message to all remote servers except for the one matching
 * client_id */
static void downstream_yydoc(sdata_t* sdata, yyjson_mut_doc* val, const int64_t client_id, const int prio) {
    stratum_instance_t* client;
    ckmsg_t*            bulk_send = NULL;
    int                 messages = 0;

    ck_rlock(&sdata->instance_lock);
    DL_FOREACH2(sdata->remote_instances, client, remote_next) {
        ckmsg_t* client_msg;
        smsg_t*  msg;

        /* Don't send remote workinfo back to same remote */
        if (client->id == client_id)
            continue;
        client_msg = ckalloc(sizeof(ckmsg_t));
        msg = ckzalloc(sizeof(smsg_t));
        msg->doc = yyjson_mut_doc_mut_copy(val, &ckyyalc);
        msg->client_id = client->id;
        client_msg->data = msg;
        DL_APPEND(bulk_send, client_msg);
        messages++;
    }
    ck_runlock(&sdata->instance_lock);

    if (bulk_send) {
        LOGINFO("Sending json to %d remote servers", messages);
        switch (prio) {
            case SSEND_PREPEND:
                ssend_bulk_prepend(sdata, bulk_send, messages);
                break;
            case SSEND_APPEND:
                ssend_bulk_append(sdata, bulk_send, messages);
                break;
        }
    }
}

/* Find any transactions that are missing from our transaction table during
 * rebuild_txns by requesting their data from another server. */
/* Takes a doc whose root is an array of the missing hashes, wrapping it into
 * a request object. Does not free the doc. */
static void request_txns(sdata_t* sdata, yyjson_mut_doc* doc) {
    yyjson_mut_val* val = yyjson_mut_obj(doc);

    yyjson_mut_obj_add_val(doc, val, "hash", yyjson_mut_doc_get_root(doc));
    yyjson_mut_doc_set_root(doc, val);
    if (ckpool.remote)
        upstream_yydoc_msgtype(doc, SM_REQTXNS);
    else if (ckpool.node) {
        /* Nodes have no way to signal upstream pool yet */
    } else {
        /* We don't know which remote sent the transaction hash so ask
         * all of them for it */
        yyjson_mut_obj_add_strcpy(doc, val, "method", stratum_msgs[SM_REQTXNS]);
        downstream_yydoc(sdata, doc, 0, SSEND_APPEND);
    }
}

/* Rebuilds transactions from txnhashes to be able to construct wb_merkle_bins
 * on remote workbases */
static bool rebuild_txns(sdata_t* sdata, workbase_t* wb) {
    const char*     hashes = wb->txn_hashes;
    yyjson_mut_val *txn_array, *missing_txns;
    yyjson_mut_doc *doc, *mdoc;
    char            hash[68] = {};
    bool            ret = false;
    txntable_t*     txns;
    int             i, len = 0;

    /* We'll only see this on testnet now */
    if (unlikely(!wb->txns)) {
        ret = true;
        goto out;
    }
    if (likely(hashes))
        len = strlen(hashes);
    if (!hashes || !len)
        goto out;

    /* Use 64 bit arithmetic here as a remotely supplied txn count could
     * otherwise overflow the multiplication and defeat this check */
    if (unlikely((int64_t)wb->txns * 65 > len)) {
        LOGERR("Truncated transactions in rebuild_txns only %d long", len);
        goto out;
    }
    ret = true;
    doc = yyjson_mut_doc_new(&ckyyalc);
    txn_array = yyjson_mut_arr(doc);
    mdoc = yyjson_mut_doc_new(&ckyyalc);
    missing_txns = yyjson_mut_arr(mdoc);
    yyjson_mut_doc_set_root(mdoc, missing_txns);

    for (i = 0; i < wb->txns; i++) {
        yyjson_mut_val* txn_val = NULL;
        txntable_t*     txn;
        char*           data;

        memcpy(hash, hashes + i * 65, 64);

        ck_wlock(&sdata->txn_lock);
        HASH_FIND_STR(sdata->txns, hash, txn);
        if (likely(txn)) {
            txn->refcount = REFCOUNT_REMOTE;
            txn->seen = true;
            txn_val = yyjson_mut_pack_val(doc, "{ss,ss}", "hash", hash, "data", txn->data);
            yyjson_mut_arr_append(txn_array, txn_val);
        }
        ck_wunlock(&sdata->txn_lock);

        if (likely(txn_val))
            continue;
        /* See if we can find it in our local bitcoind */
        data = generator_get_txn(hash);
        if (!data) {
            yyjson_mut_arr_add_strcpy(mdoc, missing_txns, hash);
            ret = false;
            continue;
        }

        /* We've found it, let's add it to the table */
        ck_wlock(&sdata->txn_lock);
        /* One last check in case it got added while we dropped the lock */
        HASH_FIND_STR(sdata->txns, hash, txn);
        if (likely(!txn)) {
            txn = ckzalloc(sizeof(txntable_t));
            memcpy(txn->hash, hash, 65);
            txn->data = data;
            HASH_ADD_STR(sdata->txns, hash, txn);
            sdata->txns_generated++;
        } else {
            free(data);
        }
        txn->refcount = REFCOUNT_REMOTE;
        txn->seen = true;
        txn_val = yyjson_mut_pack_val(doc, "{ss,ss}", "hash", hash, "data", txn->data);
        yyjson_mut_arr_append(txn_array, txn_val);
        ck_wunlock(&sdata->txn_lock);
    }

    if (ret) {
        yyjson_doc* idoc;

        wb->incomplete = false;
        LOGINFO("Rebuilt txns into workbase with %d transactions", i);
        /* This structure is regenerated so free its ram */
        dealloc(wb->txn_hashes);
        yyjson_mut_doc_set_root(doc, txn_array);
        idoc = yyjson_mut_doc_imut_copy(doc, &ckyyalc);
        txns = wb_merkle_bin_txns(sdata, wb, yyjson_doc_get_root(idoc), false);
        yyjson_doc_free(idoc);
        if (likely(txns))
            update_txns(sdata, txns, false);
    } else {
        if (!sdata->wbincomplete) {
            sdata->wbincomplete = true;
            if (ckpool.proxy)
                LOGWARNING("Unable to rebuild transactions to create workinfo, ignore displayed hashrate");
        }
        LOGINFO("Failed to find all txns in rebuild_txns");
        request_txns(sdata, mdoc);
    }

    yyjson_mut_doc_free(doc);
    yyjson_mut_doc_free(mdoc);
out:
    return ret;
}

/* Remote workbases are keyed by the combined values of wb->id and
 * wb->client_id to prevent collisions in the unlikely event two remote
 * servers are generating the same workbase ids. */
static void __add_to_remote_workbases(sdata_t* sdata, workbase_t* wb) {
    HASH_ADD(hh, sdata->remote_workbases, id, sizeof(int64_t) * 2, wb);
}

static void add_remote_base(sdata_t* sdata, workbase_t* wb) {
    stratum_instance_t* client;
    ckmsg_t*            bulk_send = NULL;
    workbase_t *        tmp, *tmpa;
    yyjson_mut_doc*     val;
    yyjson_mut_val*     vroot;
    int                 messages = 0;
    int64_t             skip;

    ts_realtime(&wb->gentime);

    ck_wlock(&sdata->workbase_lock);
    sdata->workbases_generated++;
    wb->mapped_id = sdata->workbase_id++;
    HASH_ITER(hh, sdata->remote_workbases, tmp, tmpa) {
        if (HASH_COUNT(sdata->remote_workbases) < 3)
            break;
        if (wb == tmp)
            continue;
        if (tmp->readcount)
            continue;
        /*  Age old workbases older than 10 minutes old */
        if (tmp->gentime.tv_sec < wb->gentime.tv_sec - 600) {
            HASH_DEL(sdata->remote_workbases, tmp);
            ck_wunlock(&sdata->workbase_lock);

            clear_workbase(tmp);

            ck_wlock(&sdata->workbase_lock);
        }
    }
    __add_to_remote_workbases(sdata, wb);
    ck_wunlock(&sdata->workbase_lock);

    val = generate_workinfo(wb, __func__);
    vroot = yyjson_mut_doc_get_root(val);

    /* Set jobid with mapped id for other nodes and remotes */
    yyjson_mut_obj_put(vroot, yyjson_mut_str(val, "jobid"), yyjson_mut_sint(val, wb->mapped_id));

    /* Replace workinfoid to mapped id */
    yyjson_mut_obj_put(vroot, yyjson_mut_str(val, "workinfoid"), yyjson_mut_sint(val, wb->mapped_id));

    /* Strip unnecessary fields and add extra fields needed */
    yyjson_mut_obj_put(vroot, yyjson_mut_str(val, "txns"), yyjson_mut_int(val, wb->txns));
    yyjson_mut_obj_put(vroot, yyjson_mut_str(val, "txn_hashes"), yyjson_mut_strcpy(val, wb->txn_hashes));
    yyjson_mut_obj_put(vroot, yyjson_mut_str(val, "merkles"), yyjson_mut_int(val, wb->merkles));

    skip = subclient(wb->client_id);

    /* Send a copy of this to all OTHER remote trusted servers as well */
    ck_rlock(&sdata->instance_lock);
    DL_FOREACH2(sdata->remote_instances, client, remote_next) {
        yyjson_mut_doc* doc;
        yyjson_mut_val* root;
        ckmsg_t*        client_msg;
        smsg_t*         msg;

        /* Don't send remote workinfo back to the source remote */
        if (client->id == wb->client_id)
            continue;
        doc = yyjson_mut_doc_mut_copy(val, &ckyyalc);
        root = yyjson_mut_doc_get_root(doc);
        yyjson_mut_obj_add_strcpy(doc, root, "method", stratum_msgs[SM_WORKINFO]);
        client_msg = ckalloc(sizeof(ckmsg_t));
        msg = ckzalloc(sizeof(smsg_t));
        msg->doc = doc;
        msg->client_id = client->id;
        client_msg->data = msg;
        DL_APPEND(bulk_send, client_msg);
        messages++;
    }
    DL_FOREACH2(sdata->node_instances, client, node_next) {
        yyjson_mut_doc* doc;
        yyjson_mut_val* root;
        ckmsg_t*        client_msg;
        smsg_t*         msg;

        /* Don't send node workinfo back to the source node */
        if (client->id == skip)
            continue;
        doc = yyjson_mut_doc_mut_copy(val, &ckyyalc);
        root = yyjson_mut_doc_get_root(doc);
        yyjson_mut_obj_add_strcpy(doc, root, "node.method", stratum_msgs[SM_WORKINFO]);
        client_msg = ckalloc(sizeof(ckmsg_t));
        msg = ckzalloc(sizeof(smsg_t));
        msg->doc = doc;
        msg->client_id = client->id;
        client_msg->data = msg;
        DL_APPEND(bulk_send, client_msg);
        messages++;
    }
    ck_runlock(&sdata->instance_lock);

    yyjson_mut_doc_free(val);

    if (bulk_send) {
        LOGINFO("Sending remote workinfo to %d other remote servers", messages);
        ssend_bulk_append(sdata, bulk_send, messages);
    }
}

static void add_node_base(yyjson_mut_val* val, bool trusted, int64_t client_id) {
    workbase_t* wb = ckzalloc(sizeof(workbase_t));
    sdata_t*    sdata = ckpool.sdata;
    bool        new_block = false;
    char        header[272];

    /* This is the client id if this workbase came from a remote trusted
     * server. */
    wb->client_id = client_id;

    /* Some of these fields are empty when running as a remote trusted
     * server receiving other workinfos from the upstream pool */
    yyjson_mut_obj_int64cpy(&wb->id, val, "jobid");
    yyjson_mut_obj_strncpy(wb->target, val, "target", sizeof(wb->target));
    yyjson_mut_obj_dblcpy(&wb->diff, val, "diff");
    yyjson_mut_obj_uintcpy(&wb->version, val, "version");
    yyjson_mut_obj_uintcpy(&wb->curtime, val, "curtime");
    yyjson_mut_obj_strncpy(wb->prevhash, val, "prevhash", sizeof(wb->prevhash));
    yyjson_mut_obj_strncpy(wb->ntime, val, "ntime", sizeof(wb->ntime));
    sscanf(wb->ntime, "%x", &wb->ntime32);
    yyjson_mut_obj_strncpy(wb->bbversion, val, "bbversion", sizeof(wb->bbversion));
    yyjson_mut_obj_strncpy(wb->nbit, val, "nbit", sizeof(wb->nbit));
    yyjson_mut_obj_uint64cpy(&wb->coinbasevalue, val, "coinbasevalue");
    yyjson_mut_obj_intcpy(&wb->height, val, "height");
    yyjson_mut_obj_strdup(&wb->flags, val, "flags");

    yyjson_mut_obj_intcpy(&wb->txns, val, "txns");
    yyjson_mut_obj_strdup(&wb->txn_hashes, val, "txn_hashes");
    if (!ckpool.proxy) {
        /* This is a workbase from a trusted remote */
        yyjson_mut_obj_intcpy(&wb->merkles, val, "merkles");
        /* merklehash and merklebin are fixed size arrays of 16
         * entries so reject rather than overrun them */
        if (unlikely(wb->merkles < 0 || wb->merkles > 16)) {
            LOGWARNING("Node base with invalid merkles %d", wb->merkles);
            clear_workbase(wb);
            return;
        }
        if (!rebuild_txns(sdata, wb))
            wb->incomplete = true;
    } else {
        if (!rebuild_txns(sdata, wb)) {
            clear_workbase(wb);
            return;
        }
    }
    yyjson_mut_obj_strdup(&wb->coinb1, val, "coinb1");
    yyjson_mut_obj_intcpy(&wb->coinb1len, val, "coinb1len");
    yyjson_mut_obj_strdup(&wb->coinb2, val, "coinb2");
    yyjson_mut_obj_intcpy(&wb->coinb2len, val, "coinb2len");
    /* Bound the coinbase allocations against absurd values */
    if (unlikely(
            wb->coinb1len < 0 || wb->coinb2len < 0 || wb->coinb1len > MAX_COINBASE_LEN ||
            wb->coinb2len > MAX_COINBASE_LEN)) {
        LOGWARNING("Node base with invalid coinb1len %d coinb2len %d", wb->coinb1len, wb->coinb2len);
        clear_workbase(wb);
        return;
    }
    wb->coinb1bin = ckzalloc(wb->coinb1len);
    hex2bin(wb->coinb1bin, wb->coinb1, wb->coinb1len);
    wb->coinb2bin = ckzalloc(wb->coinb2len);
    hex2bin(wb->coinb2bin, wb->coinb2, wb->coinb2len);
    yyjson_mut_obj_intcpy(&wb->enonce1varlen, val, "enonce1varlen");
    yyjson_mut_obj_intcpy(&wb->enonce2varlen, val, "enonce2varlen");
    /* These are used to size buffers when reconstructing the coinbase and
     * nonce2, and can only ever be configured in the 2~8 range, so reject
     * anything outside the possible extranonce sizes */
    if (unlikely(wb->enonce1varlen < 0 || wb->enonce1varlen > 8 || wb->enonce2varlen < 0 || wb->enonce2varlen > 8)) {
        LOGWARNING("Node base with invalid enonce1varlen %d enonce2varlen %d", wb->enonce1varlen, wb->enonce2varlen);
        clear_workbase(wb);
        return;
    }
    ts_realtime(&wb->gentime);

    snprintf(
        header, 270, "%s%s%s%s%s%s%s", wb->bbversion, wb->prevhash,
        "0000000000000000000000000000000000000000000000000000000000000000", wb->ntime, wb->nbit, "00000000", /* nonce */
        workpadding);
    header[224] = 0;
    LOGDEBUG("Header: %s", header);
    hex2bin(wb->headerbin, header, 112);

    /* If this is from a remote trusted server or an upstream server, add
     * it to the remote_workbases hashtable */
    if (trusted)
        add_remote_base(sdata, wb);
    else
        add_base(sdata, wb, &new_block);

    if (new_block)
        LOGNOTICE("Block hash changed to %s", sdata->lastswaphash);
}

/* Calculate share diff and fill in hash and swap. Need to hold workbase read count */
static double share_diff(
    char*             coinbase,
    const uchar*      enonce1bin,
    const workbase_t* wb,
    const char*       nonce2,
    const uint32_t    ntime32,
    uint32_t          version_mask,
    const char*       nonce,
    uchar*            hash,
    uchar*            swap,
    int*              cblen) {
    unsigned char merkle_root[32], merkle_sha[64];
    uint32_t *    data32, *swap32, benonce32;
    uchar         hash1[32];
    char          data[80];
    int           i;

    memcpy(coinbase, wb->coinb1bin, wb->coinb1len);
    *cblen = wb->coinb1len;
    memcpy(coinbase + *cblen, enonce1bin, wb->enonce1constlen + wb->enonce1varlen);
    *cblen += wb->enonce1constlen + wb->enonce1varlen;
    hex2bin(coinbase + *cblen, nonce2, wb->enonce2varlen);
    *cblen += wb->enonce2varlen;
    memcpy(coinbase + *cblen, wb->coinb2bin, wb->coinb2len);
    *cblen += wb->coinb2len;

    gen_hash((uchar*)coinbase, merkle_root, *cblen);
    memcpy(merkle_sha, merkle_root, 32);
    for (i = 0; i < wb->merkles; i++) {
        memcpy(merkle_sha + 32, &wb->merklebin[i], 32);
        gen_hash(merkle_sha, merkle_root, 64);
        memcpy(merkle_sha, merkle_root, 32);
    }
    data32 = (uint32_t*)merkle_sha;
    swap32 = (uint32_t*)merkle_root;
    flip_32(swap32, data32);

    /* Copy the cached header binary and insert the merkle root */
    memcpy(data, wb->headerbin, 80);
    memcpy(data + 36, merkle_root, 32);

    /* Update nVersion when version_mask is in use */
    if (version_mask) {
        version_mask = htobe32(version_mask);
        data32 = (uint32_t*)data;
        *data32 |= version_mask;
    }

    /* Insert the nonce value into the data */
    hex2bin(&benonce32, nonce, 4);
    data32 = (uint32_t*)(data + 64 + 12);
    *data32 = benonce32;

    /* Insert the ntime value into the data */
    data32 = (uint32_t*)(data + 68);
    *data32 = htobe32(ntime32);

    /* Hash the share */
    data32 = (uint32_t*)data;
    swap32 = (uint32_t*)swap;
    flip_80(swap32, data32);
    sha256(swap, 80, hash1);
    sha256(hash1, 32, hash);

    /* Calculate the diff of the share here */
    return diff_from_target(hash);
}

static void add_remote_blockdata(
    yyjson_mut_doc* doc,
    yyjson_mut_val* val,
    const int       cblen,
    const char*     coinbase,
    const uchar*    data) {
    char* buf;

    yyjson_mut_obj_add_strcpy(doc, val, "name", ckpool.name);
    yyjson_mut_obj_add_int(doc, val, "cblen", cblen);
    buf = bin2hex(coinbase, cblen);
    yyjson_mut_obj_add_strcpy(doc, val, "coinbasehex", buf);
    free(buf);
    buf = bin2hex(data, 80);
    yyjson_mut_obj_add_strcpy(doc, val, "swaphex", buf);
    free(buf);
}

/* Entered with workbase readcount, grabs instance_lock. client_id is where the
 * block originated. */
static void send_nodes_block(sdata_t* sdata, yyjson_mut_doc* block_doc, const int64_t client_id) {
    stratum_instance_t* client;
    ckmsg_t*            bulk_send = NULL;
    int                 messages = 0;
    int64_t             skip;

    /* Don't send the block back to a remote node if that's where it was
     * found. */
    skip = subclient(client_id);

    ck_rlock(&sdata->instance_lock);
    DL_FOREACH2(sdata->node_instances, client, node_next) {
        yyjson_mut_doc* doc;
        yyjson_mut_val* root;
        ckmsg_t*        client_msg;
        smsg_t*         msg;

        if (client->id == skip)
            continue;
        doc = yyjson_mut_doc_mut_copy(block_doc, &ckyyalc);
        root = yyjson_mut_doc_get_root(doc);
        yyjson_mut_obj_add_strcpy(doc, root, "node.method", stratum_msgs[SM_BLOCK]);
        client_msg = ckalloc(sizeof(ckmsg_t));
        msg = ckzalloc(sizeof(smsg_t));
        msg->doc = doc;
        msg->client_id = client->id;
        client_msg->data = msg;
        DL_APPEND(bulk_send, client_msg);
        messages++;
    }
    ck_runlock(&sdata->instance_lock);

    if (bulk_send) {
        LOGNOTICE("Sending block to %d mining nodes", messages);
        ssend_bulk_prepend(sdata, bulk_send, messages);
    }
}

/* Entered with workbase readcount. */
static void send_node_block(
    sdata_t*       sdata,
    const char*    enonce1,
    const char*    nonce,
    const char*    nonce2,
    const uint32_t ntime32,
    const uint32_t version_mask,
    const int64_t  jobid,
    const double   diff,
    const int64_t  client_id,
    const char*    coinbase,
    const int      cblen,
    const uchar*   data) {
    if (sdata->node_instances) {
        yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
        yyjson_mut_val* val = yyjson_mut_obj(doc);

        yyjson_mut_doc_set_root(doc, val);
        yyjson_mut_obj_add_strcpy(doc, val, "enonce1", enonce1);
        yyjson_mut_obj_add_strcpy(doc, val, "nonce", nonce);
        yyjson_mut_obj_add_strcpy(doc, val, "nonce2", nonce2);
        yyjson_mut_obj_add_uint(doc, val, "ntime32", ntime32);
        yyjson_mut_obj_add_uint(doc, val, "version_mask", version_mask);
        yyjson_mut_obj_add_int(doc, val, "jobid", jobid);
        yyjson_mut_obj_add_real(doc, val, "diff", diff);
        add_remote_blockdata(doc, val, cblen, coinbase, data);
        send_nodes_block(sdata, doc, client_id);
        yyjson_mut_doc_free(doc);
    }
}

/* Process a block into a message for the generator to submit. Must hold
 * workbase readcount */
static char* process_block(
    const workbase_t* wb,
    const char*       coinbase,
    const int         cblen,
    const uchar*      data,
    const uchar*      hash,
    uchar*            flip32,
    char*             blockhash) {
    char *gbt_block, *hexcoinbase, varint[12];
    int   txns = wb->txns + 1;

    flip_32(flip32, hash);
    __bin2hex(blockhash, flip32, 32);

    /* Message format: "data". Size the buffers to the actual coinbase
     * length: 80 byte header + up to 5 byte txn count varint + coinbase,
     * all hex encoded, plus room for the null terminator. Transaction
     * data beyond that is appended via realloc_strcat which grows the
     * buffer as needed. */
    hexcoinbase = alloca(cblen * 2 + 1);
    gbt_block = ckzalloc(80 * 2 + sizeof(varint) * 2 + cblen * 2 + 1);
    __bin2hex(gbt_block, data, 80);
    if (txns < 0xfd) {
        uint8_t val8 = txns;

        __bin2hex(varint, (const unsigned char*)&val8, 1);
    } else if (txns <= 0xffff) {
        uint16_t val16 = htole16(txns);

        strcat(gbt_block, "fd");
        __bin2hex(varint, (const unsigned char*)&val16, 2);
    } else {
        uint32_t val32 = htole32(txns);

        strcat(gbt_block, "fe");
        __bin2hex(varint, (const unsigned char*)&val32, 4);
    }
    strcat(gbt_block, varint);
    __bin2hex(hexcoinbase, coinbase, cblen);
    strcat(gbt_block, hexcoinbase);
    if (wb->txns)
        realloc_strcat(&gbt_block, wb->txn_data);
    return gbt_block;
}

/* Submit block data locally, absorbing and freeing gbt_block */
static bool local_block_submit(char* gbt_block, const uchar* flip32, int height) {
    bool  ret = generator_submitblock(gbt_block);
    char  heighthash[68] = {}, rhash[68] = {};
    uchar swap256[32];

    free(gbt_block);
    swap_256(swap256, flip32);
    __bin2hex(rhash, swap256, 32);
    generator_preciousblock(rhash);

    /* Check failures that may be inconclusive but were submitted via other
     * means or accepted due to precious block call. */
    if (!ret) {
        /* If the block is accepted locally, it means we may have
         * displaced a known block, and are now working on this fork.
         * This makes the most sense since if we solve the next block,
         * it validates this one as the best chain, orphaning the other
         * block. In the case of mainnet, it means we have found a stale
         * block and are trying to force ours ahead of the other. In
         * a low diff environment we may have successive blocks, and
         * this will be the last one solved locally. Trying to optimise
         * regtest/testnet will optimise against the mainnet case. */
        if (generator_get_blockhash(height, heighthash)) {
            ret = !strncmp(rhash, heighthash, 64);
            LOGWARNING(
                "Hash for forced possibly stale block, height %d confirms block was %s", height,
                ret ? "ACCEPTED" : "REJECTED");
        }
    }
    return ret;
}

static workbase_t* get_workbase(sdata_t* sdata, const int64_t id) {
    workbase_t* wb;

    ck_wlock(&sdata->workbase_lock);
    HASH_FIND_I64(sdata->workbases, &id, wb);
    if (wb)
        wb->readcount++;
    ck_wunlock(&sdata->workbase_lock);

    return wb;
}

static workbase_t* __find_remote_workbase(sdata_t* sdata, const int64_t id, const int64_t client_id) {
    int64_t     lookup[2] = {id, client_id};
    workbase_t* wb;

    HASH_FIND(hh, sdata->remote_workbases, lookup, sizeof(int64_t) * 2, wb);
    return wb;
}

static workbase_t* get_remote_workbase(sdata_t* sdata, const int64_t id, const int64_t client_id) {
    workbase_t* wb;

    ck_wlock(&sdata->workbase_lock);
    wb = __find_remote_workbase(sdata, id, client_id);
    if (wb) {
        if (wb->incomplete)
            wb = NULL;
        else
            wb->readcount++;
    }
    ck_wunlock(&sdata->workbase_lock);

    return wb;
}

static void put_workbase(sdata_t* sdata, workbase_t* wb) {
    ck_wlock(&sdata->workbase_lock);
    wb->readcount--;
    ck_wunlock(&sdata->workbase_lock);
}

#define put_remote_workbase(sdata, wb) put_workbase(sdata, wb)

static void block_solve(yyjson_mut_doc* doc);
static void block_reject(yyjson_mut_doc* doc);

static void submit_node_block(sdata_t* sdata, yyjson_mut_val* val) {
    char *          coinbase = NULL, *enonce1 = NULL, *nonce = NULL, *nonce2 = NULL, *gbt_block, *coinbasehex, *swaphex;
    uchar *         enonce1bin = NULL, hash[32], swap[80], flip32[32];
    uint32_t        ntime32, version_mask = 0;
    char            blockhash[68], cdfield[64];
    int             enonce1len, cblen = 0;
    workbase_t*     wb = NULL;
    yyjson_mut_doc* doc;
    double          diff;
    ts_t            ts_now;
    int64_t         id;
    bool            ret;

    if (unlikely(!yyjson_mut_obj_get_string(&enonce1, val, "enonce1"))) {
        LOGWARNING("Failed to get enonce1 from node method block");
        goto out;
    }
    if (unlikely(!yyjson_mut_obj_get_string(&nonce, val, "nonce"))) {
        LOGWARNING("Failed to get nonce from node method block");
        goto out;
    }
    if (unlikely(!yyjson_mut_obj_get_string(&nonce2, val, "nonce2"))) {
        LOGWARNING("Failed to get nonce2 from node method block");
        goto out;
    }
    if (unlikely(!yyjson_mut_obj_get_uint32(&ntime32, val, "ntime32"))) {
        LOGWARNING("Failed to get ntime32 from node method block");
        goto out;
    }
    if (unlikely(!yyjson_mut_obj_get_int64(&id, val, "jobid"))) {
        LOGWARNING("Failed to get jobid from node method block");
        goto out;
    }
    if (unlikely(!yyjson_mut_obj_get_double(&diff, val, "diff"))) {
        LOGWARNING("Failed to get diff from node method block");
        goto out;
    }

    if (!yyjson_mut_obj_get_uint32(&version_mask, val, "version_mask")) {
        /* No version mask is not fatal, assume it to be zero */
        LOGINFO("No version mask in node method block");
    }

    LOGWARNING("Possible upstream block solve diff %lf !", diff);

    ts_realtime(&ts_now);
    sprintf(cdfield, "%lu,%lu", ts_now.tv_sec, ts_now.tv_nsec);

    wb = get_workbase(sdata, id);
    if (unlikely(!wb)) {
        LOGWARNING("Failed to find workbase with jobid %" PRId64 " in node method block", id);
        goto out;
    }

    /* Get parameters if upstream pool supports them with new format */
    yyjson_mut_obj_get_string(&coinbasehex, val, "coinbasehex");
    yyjson_mut_obj_get_int(&cblen, val, "cblen");
    yyjson_mut_obj_get_string(&swaphex, val, "swaphex");
    /* cblen is the full assembled coinbase which is bounded by its two
     * components, so reject anything larger as corrupt or malicious */
    if (coinbasehex && swaphex && cblen > 0 && cblen <= 2 * MAX_COINBASE_LEN) {
        uchar hash1[32];

        coinbase = alloca(cblen);
        /* Reject corrupt hex rather than hashing uninitialised
         * alloca tails into a submitted block. */
        if (unlikely(!hex2bin(coinbase, coinbasehex, cblen) || !hex2bin(swap, swaphex, 80))) {
            LOGWARNING("Invalid hex in node method block for jobid %" PRId64, id);
            put_workbase(sdata, wb);
            goto out;
        }
        sha256(swap, 80, hash1);
        sha256(hash1, 32, hash);
    } else {
        /* Rebuild the old way if we can if the upstream pool is using
         * the old format only */
        enonce1len = wb->enonce1constlen + wb->enonce1varlen;
        enonce1bin = alloca(enonce1len);
        if (unlikely(!hex2bin(enonce1bin, enonce1, enonce1len))) {
            LOGWARNING("Invalid enonce1 hex in node method block for jobid %" PRId64, id);
            put_workbase(sdata, wb);
            goto out;
        }
        coinbase = alloca(wb->coinb1len + wb->enonce1constlen + wb->enonce1varlen + wb->enonce2varlen + wb->coinb2len);
        /* Fill in the hashes */
        share_diff(coinbase, enonce1bin, wb, nonce2, ntime32, version_mask, nonce, hash, swap, &cblen);
    }

    /* Now we have enough to assemble a block */
    gbt_block = process_block(wb, coinbase, cblen, swap, hash, flip32, blockhash);
    ret = local_block_submit(gbt_block, flip32, wb->height);

    doc = yyjson_mut_pack(
        "{si,ss,ss,sI,ss,ss,si,ss,sI,sf,ss,ss,ss,ss}", "height", wb->height, "blockhash", blockhash, "confirmed", "n",
        "workinfoid", wb->id, "enonce1", enonce1, "nonce2", nonce2, "version_mask", version_mask, "nonce", nonce,
        "reward", wb->coinbasevalue, "diff", diff, "createdate", cdfield, "createby", "code", "createcode", __func__,
        "createinet", ckpool.serverurl[0]);
    put_workbase(sdata, wb);

    if (ret)
        block_solve(doc);
    else
        block_reject(doc);

    yyjson_mut_doc_free(doc);
out:
    free(nonce2);
    free(nonce);
    free(enonce1);
}

static void update_base(sdata_t* sdata, const int prio) {
    int* uprio;

    /* All uses of block_update are serialised so if we have more
     * update_base calls waiting there is no point servicing them unless
     * they are high priority. */
    if (prio < GEN_PRIORITY) {
        /* Don't queue another routine update if one is already in
         * progress. */
        if (cksem_trywait(&sdata->update_sem)) {
            LOGINFO("Skipped lowprio update base");
            return;
        }
    } else
        cksem_wait(&sdata->update_sem);

    uprio = ckalloc(sizeof(int));
    *uprio = prio;
    ckmsgq_add(sdata->updateq, uprio);
}

/* Instead of removing the client instance, we add it to a list of recycled
 * clients allowing us to reuse it instead of callocing a new one */
static void __kill_instance(sdata_t* sdata, stratum_instance_t* client) {
    if (client->proxy) {
        client->proxy->bound_clients--;
        client->proxy->parent->combined_clients--;
    }
    free(client->workername);
    free(client->password);
    free(client->useragent);
    memset(client, 0, sizeof(stratum_instance_t));
    DL_APPEND2(sdata->recycled_instances, client, recycled_prev, recycled_next);
}

/* Called with instance_lock held. Note stats.users is protected by
 * instance lock to avoid recursive locking. */
static void __inc_worker(sdata_t* sdata, user_instance_t* user, worker_instance_t* worker) {
    sdata->stats.workers++;
    if (!user->workers++)
        sdata->stats.users++;
    worker->instance_count++;
}

static void __dec_worker(sdata_t* sdata, user_instance_t* user, worker_instance_t* worker) {
    sdata->stats.workers--;
    if (!--user->workers)
        sdata->stats.users--;
    worker->instance_count--;
}

static void __disconnect_session(sdata_t* sdata, const stratum_instance_t* client) {
    time_t     now_t = time(NULL);
    session_t *session, *tmp;

    /* Opportunity to age old sessions */
    HASH_ITER(hh, sdata->disconnected_sessions, session, tmp) {
        if (now_t - session->added > 600) {
            HASH_DEL(sdata->disconnected_sessions, session);
            dealloc(session);
            sdata->stats.disconnected--;
        }
    }

    if (!client->enonce1_64 || !client->user_instance || !client->authorised)
        return;
    HASH_FIND_INT(sdata->disconnected_sessions, &client->session_id, session);
    if (session)
        return;
    session = ckalloc(sizeof(session_t));
    session->enonce1_64 = client->enonce1_64;
    session->session_id = client->session_id;
    session->client_id = client->id;
    session->userid = client->user_id;
    session->added = now_t;
    strcpy(session->address, client->address);
    HASH_ADD_INT(sdata->disconnected_sessions, session_id, session);
    sdata->stats.disconnected++;
    sdata->disconnected_generated++;
}

/* Removes a client instance we know is on the stratum_instances list and from
 * the user client list if it's been placed on it */
static void __del_client(sdata_t* sdata, stratum_instance_t* client) {
    user_instance_t* user = client->user_instance;

    HASH_DEL(sdata->stratum_instances, client);
    if (user) {
        DL_DELETE2(user->clients, client, user_prev, user_next);
        __dec_worker(sdata, user, client->worker_instance);
    }
}

static void connector_drop_client(const int64_t id) {
    char buf[256];

    LOGDEBUG("Stratifier requesting connector drop client %" PRId64, id);
    snprintf(buf, 255, "dropclient=%" PRId64, id);
    send_proc(ckpool.connector, buf);
}

static void drop_allclients(void) {
    stratum_instance_t *client, *tmp;
    sdata_t*            sdata = ckpool.sdata;
    int                 kills = 0;

#ifdef HAVE_SV2
    /*
     * Virtual SV2 sessions are not connector TCP clients: tearing them
     * down via connector_drop_client is a no-op for Noise parents and
     * left the channel map pointing at dead instance_ids. Drop mining
     * + JD tables first (closes virtual sessions), then kill instances.
     */
    sv2_strat_drop_all();
    sv2_jd_drop_all();
#endif

    ck_wlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->stratum_instances, client, tmp) {
        int64_t client_id = client->id;
        bool    was_sv2 = false;

#ifdef HAVE_SV2
        was_sv2 = client->sv2;
#endif
        if (!client->ref) {
            __del_client(sdata, client);
            __kill_instance(sdata, client);
        } else
            client->dropped = true;
        kills++;
        /* Virtual SV2 instances have no connector fd — skip. */
        if (!was_sv2)
            connector_drop_client(client_id);
    }
    sdata->stats.users = sdata->stats.workers = 0;
    ck_wunlock(&sdata->instance_lock);

    if (kills)
        LOGNOTICE("Dropped %d instances for dropall request", kills);
}

/* Copy only the relevant parts of the master sdata for each subproxy */
static sdata_t* duplicate_sdata(const sdata_t* sdata) {
    sdata_t* dsdata = ckzalloc(sizeof(sdata_t));

    /* Copy the transaction binaries for workbase creation */
    memcpy(dsdata->txnbin, sdata->txnbin, 40);
    memcpy(dsdata->dontxnbin, sdata->dontxnbin, 40);

    /* Use the same work queues for all subproxies */
    dsdata->ssends = sdata->ssends;
    dsdata->srecvs = sdata->srecvs;
    dsdata->sshareq = sdata->sshareq;
    dsdata->sauthq = sdata->sauthq;
    dsdata->stxnq = sdata->stxnq;

    /* Give the sbuproxy its own workbase list and lock */
    cklock_init(&dsdata->workbase_lock);
    cksem_init(&dsdata->update_sem);
    cksem_post(&dsdata->update_sem);
    return dsdata;
}

static int64_t prio_sort(proxy_t* a, proxy_t* b) {
    return (a->priority - b->priority);
}

/* Masked increment */
static int64_t masked_inc(int64_t value, int64_t mask) {
    value &= ~mask;
    value++;
    value |= mask;
    return value;
}

/* Priority values can be sparse, they do not need to be sequential */
static void __set_proxy_prio(sdata_t* sdata, proxy_t* proxy, int64_t priority) {
    proxy_t *tmpa, *tmpb, *exists = NULL;
    int64_t  mask, next_prio = 0;

    /* Encode the userid as the high bits in priority */
    mask = proxy->userid;
    mask <<= 32;
    priority |= mask;

    /* See if the priority is already in use */
    HASH_ITER(hh, sdata->proxies, tmpa, tmpb) {
        if (tmpa->priority > priority)
            break;
        if (tmpa->priority == priority) {
            exists = tmpa;
            next_prio = masked_inc(priority, mask);
            break;
        }
    }
    /* See if we need to push the priority of everything after exists up */
    HASH_ITER(hh, exists, tmpa, tmpb) {
        if (tmpa->priority > next_prio)
            break;
        tmpa->priority = masked_inc(tmpa->priority, mask);
        next_prio++;
    }
    proxy->priority = priority;
    HASH_SORT(sdata->proxies, prio_sort);
}

static proxy_t* __generate_proxy(sdata_t* sdata, const int id) {
    proxy_t* proxy = ckzalloc(sizeof(proxy_t));

    proxy->parent = proxy;
    proxy->id = id;
    proxy->sdata = duplicate_sdata(sdata);
    proxy->sdata->subproxy = proxy;
    proxy->sdata->verbose = true;
    /* subid == 0 on parent proxy */
    HASH_ADD(sh, proxy->subproxies, subid, sizeof(int), proxy);
    proxy->subproxy_count++;
    HASH_ADD_INT(sdata->proxies, id, proxy);
    /* Set the initial priority to impossibly high initially as the userid
     * has yet to be inherited and the priority should be set only after
     * all the proxy details are finalised. */
    proxy->priority = 0x00FFFFFFFFFFFFFF;
    HASH_SORT(sdata->proxies, prio_sort);
    sdata->proxy_count++;
    return proxy;
}

static proxy_t* __generate_subproxy(sdata_t* sdata, proxy_t* proxy, const int subid) {
    proxy_t* subproxy = ckzalloc(sizeof(proxy_t));

    subproxy->parent = proxy;
    subproxy->id = proxy->id;
    subproxy->subid = subid;
    HASH_ADD(sh, proxy->subproxies, subid, sizeof(int), subproxy);
    proxy->subproxy_count++;
    subproxy->sdata = duplicate_sdata(sdata);
    subproxy->sdata->subproxy = subproxy;
    return subproxy;
}

static proxy_t* __existing_proxy(const sdata_t* sdata, const int id) {
    proxy_t* proxy;

    HASH_FIND_INT(sdata->proxies, &id, proxy);
    return proxy;
}

static proxy_t* existing_proxy(sdata_t* sdata, const int id) {
    proxy_t* proxy;

    mutex_lock(&sdata->proxy_lock);
    proxy = __existing_proxy(sdata, id);
    mutex_unlock(&sdata->proxy_lock);

    return proxy;
}

/* Find proxy by id number, generate one if none exist yet by that id */
static proxy_t* __proxy_by_id(sdata_t* sdata, const int id) {
    proxy_t* proxy = __existing_proxy(sdata, id);

    if (unlikely(!proxy)) {
        proxy = __generate_proxy(sdata, id);
        LOGNOTICE("Stratifier added new proxy %d", id);
    }

    return proxy;
}

static proxy_t* __existing_subproxy(proxy_t* proxy, const int subid) {
    proxy_t* subproxy;

    HASH_FIND(sh, proxy->subproxies, &subid, sizeof(int), subproxy);
    return subproxy;
}

static proxy_t* __subproxy_by_id(sdata_t* sdata, proxy_t* proxy, const int subid) {
    proxy_t* subproxy = __existing_subproxy(proxy, subid);

    if (!subproxy) {
        subproxy = __generate_subproxy(sdata, proxy, subid);
        LOGINFO("Stratifier added new subproxy %d:%d", proxy->id, subid);
    }
    return subproxy;
}

static proxy_t* subproxy_by_id(sdata_t* sdata, const int id, const int subid) {
    proxy_t *proxy, *subproxy;

    mutex_lock(&sdata->proxy_lock);
    proxy = __proxy_by_id(sdata, id);
    subproxy = __subproxy_by_id(sdata, proxy, subid);
    mutex_unlock(&sdata->proxy_lock);

    return subproxy;
}

static proxy_t* existing_subproxy(sdata_t* sdata, const int id, const int subid) {
    proxy_t *proxy, *subproxy = NULL;

    mutex_lock(&sdata->proxy_lock);
    proxy = __existing_proxy(sdata, id);
    if (proxy)
        subproxy = __existing_subproxy(proxy, subid);
    mutex_unlock(&sdata->proxy_lock);

    return subproxy;
}

static void check_userproxies(sdata_t* sdata, proxy_t* proxy, const int userid);

static void set_proxy_prio(sdata_t* sdata, proxy_t* proxy, const int priority) {
    mutex_lock(&sdata->proxy_lock);
    __set_proxy_prio(sdata, proxy, priority);
    mutex_unlock(&sdata->proxy_lock);

    if (!proxy->global)
        check_userproxies(sdata, proxy, proxy->userid);
}

/* Free client slots on one proxy. max_clients can be INT64_MAX/2 when
 * enonce1varlen >= 8; never return a negative free count. */
static int64_t proxy_free_slots(const proxy_t* proxy) {
    if (proxy->clients >= proxy->max_clients)
        return 0;
    return proxy->max_clients - proxy->clients;
}

/* Saturating add so summing several large max_clients values cannot overflow. */
static void add_headroom(int64_t* headroom, int64_t add) {
    if (add <= 0)
        return;
    if (*headroom >= INT64_MAX - add)
        *headroom = INT64_MAX;
    else
        *headroom += add;
}

/* Set proxy to the current proxy and calculate how much headroom it has */
static int64_t current_headroom(sdata_t* sdata, proxy_t** proxy) {
    proxy_t *subproxy, *tmp;
    int64_t  headroom = 0;

    mutex_lock(&sdata->proxy_lock);
    *proxy = sdata->proxy;
    if (!*proxy)
        goto out_unlock;
    HASH_ITER(sh, (*proxy)->subproxies, subproxy, tmp) {
        if (subproxy->dead)
            continue;
        add_headroom(&headroom, proxy_free_slots(subproxy));
    }
out_unlock:
    mutex_unlock(&sdata->proxy_lock);

    return headroom;
}

/* Returns the headroom available for more clients of the best alive user proxy
 * for userid. */
static int64_t best_userproxy_headroom(sdata_t* sdata, const int userid) {
    proxy_t *proxy, *subproxy, *tmp, *subtmp;
    int64_t  headroom = 0;

    mutex_lock(&sdata->proxy_lock);
    HASH_ITER(hh, sdata->proxies, proxy, tmp) {
        bool alive = false;

        if (proxy->userid < userid)
            continue;
        if (proxy->userid > userid)
            break;
        HASH_ITER(sh, proxy->subproxies, subproxy, subtmp) {
            if (subproxy->dead)
                continue;
            alive = true;
            add_headroom(&headroom, proxy_free_slots(subproxy));
        }
        /* Proxies are ordered by priority so first available will be
         * the best priority */
        if (alive)
            break;
    }
    mutex_unlock(&sdata->proxy_lock);

    return headroom;
}

static void reconnect_client(sdata_t* sdata, stratum_instance_t* client);

static void generator_recruit(const int proxyid, const int recruits) {
    char buf[256];

    sprintf(buf, "recruit=%d:%d", proxyid, recruits);
    LOGINFO("Stratifer requesting %d more subproxies of proxy %d from generator", recruits, proxyid);
    send_proc(ckpool.generator, buf);
}

/* Find how much headroom we have and connect up to that many clients that are
 * not currently on this pool, recruiting more slots to switch more clients
 * later on lazily. Only reconnect clients bound to global proxies. */
static void reconnect_global_clients(sdata_t* sdata) {
    stratum_instance_t *client, *tmpclient;
    int                 reconnects = 0;
    int64_t             headroom;
    proxy_t*            proxy;

    headroom = current_headroom(sdata, &proxy);
    if (!proxy)
        return;

    ck_rlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->stratum_instances, client, tmpclient) {
        if (client->dropped)
            continue;
        if (!client->authorised)
            continue;
        /* Is this client bound to a dead proxy? */
        if (!client->reconnect) {
            /* This client is bound to a user proxy */
            if (client->proxy->userid)
                continue;
            if (client->proxyid == proxy->id)
                continue;
        }
        if (headroom-- < 1)
            continue;
        reconnects++;
        reconnect_client(sdata, client);
    }
    ck_runlock(&sdata->instance_lock);

    if (reconnects) {
        LOGINFO("%d clients flagged for reconnect to global proxy %d", reconnects, proxy->id);
    }
    if (headroom < 0)
        generator_recruit(proxy->id, -headroom);
}

static bool __subproxies_alive(proxy_t* proxy) {
    proxy_t *subproxy, *tmp;
    bool     alive = false;

    HASH_ITER(sh, proxy->subproxies, subproxy, tmp) {
        if (!subproxy->dead) {
            alive = true;
            break;
        }
    }
    return alive;
}

/* Iterate over the current global proxy list and see if the current one is
 * the highest priority alive one. Proxies are sorted by priority so the first
 * available will be highest priority. Uses ckp sdata */
static void check_bestproxy(sdata_t* sdata) {
    proxy_t *proxy, *tmp;
    int      changed_id = -1;

    mutex_lock(&sdata->proxy_lock);
    if (sdata->proxy && !__subproxies_alive(sdata->proxy))
        sdata->proxy = NULL;
    HASH_ITER(hh, sdata->proxies, proxy, tmp) {
        if (!__subproxies_alive(proxy))
            continue;
        if (!proxy->global)
            break;
        if (proxy != sdata->proxy) {
            sdata->proxy = proxy;
            changed_id = proxy->id;
        }
        break;
    }
    mutex_unlock(&sdata->proxy_lock);

    if (changed_id != -1)
        LOGNOTICE("Stratifier setting active proxy to %d", changed_id);
}

static proxy_t* best_proxy(sdata_t* sdata) {
    proxy_t* proxy;

    mutex_lock(&sdata->proxy_lock);
    proxy = sdata->proxy;
    mutex_unlock(&sdata->proxy_lock);

    return proxy;
}

static void check_globalproxies(sdata_t* sdata, proxy_t* proxy) {
    proxy_t* best;

    check_bestproxy(sdata);
    best = best_proxy(sdata);
    /* All globals may be dead during failover — no best to compare. */
    if (best && proxy->parent == best->parent)
        reconnect_global_clients(sdata);
}

static void check_proxy(sdata_t* sdata, proxy_t* proxy) {
    if (proxy->global)
        check_globalproxies(sdata, proxy);
    else
        check_userproxies(sdata, proxy, proxy->userid);
}

static void dead_proxyid(sdata_t* sdata, const int id, const int subid, const bool replaced, const bool deleted) {
    stratum_instance_t *client, *tmp;
    int                 reconnects = 0, proxyid = 0;
    int64_t             headroom;
    proxy_t*            proxy;

    proxy = existing_subproxy(sdata, id, subid);
    if (proxy) {
        proxy->dead = true;
        proxy->deleted = deleted;
        if (!replaced && proxy->global)
            check_bestproxy(sdata);
    }
    LOGINFO("Stratifier dropping clients from proxy %d:%d", id, subid);
    headroom = current_headroom(sdata, &proxy);
    if (proxy)
        proxyid = proxy->id;

    ck_rlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->stratum_instances, client, tmp) {
        if (client->proxyid != id || client->subproxyid != subid)
            continue;
        /* Clients could remain connected to a dead connection here
         * but should be picked up when we recruit enough slots after
         * another notify. */
        if (headroom-- < 1) {
            client->reconnect = true;
            continue;
        }
        reconnects++;
        reconnect_client(sdata, client);
    }
    ck_runlock(&sdata->instance_lock);

    if (reconnects) {
        LOGINFO("%d clients flagged to reconnect from dead proxy %d:%d", reconnects, id, subid);
    }
    /* When a proxy dies, recruit more of the global proxies for them to
     * fail over to in case user proxies are unavailable. */
    if (headroom < 0)
        generator_recruit(proxyid, -headroom);
}

static void update_subscribe(const char* cmd) {
    sdata_t *   sdata = ckpool.sdata, *dsdata;
    int         id = 0, subid = 0, userid = 0;
    proxy_t *   proxy, *old = NULL;
    yyjson_val* val;
    yyjson_doc* doc;
    const char* buf;
    bool        global;

    if (unlikely(strlen(cmd) < 11)) {
        LOGWARNING("Received zero length string for subscribe in update_subscribe");
        return;
    }
    buf = cmd + 10;
    LOGDEBUG("Update subscribe: %s", buf);
    doc = yyjson_read(buf, strlen(buf), 0);
    if (unlikely(!doc)) {
        LOGWARNING("Failed to json decode subscribe response in update_subscribe %s", buf);
        return;
    }
    val = yyjson_doc_get_root(doc);
    if (unlikely(!yyjson_obj_get_int(&id, val, "proxy"))) {
        LOGWARNING("Failed to json decode proxy value in update_subscribe %s", buf);
        goto out_free;
    }
    if (unlikely(!yyjson_obj_get_int(&subid, val, "subproxy"))) {
        LOGWARNING("Failed to json decode subproxy value in update_subscribe %s", buf);
        goto out_free;
    }
    if (unlikely(!yyjson_obj_get_bool(&global, val, "global"))) {
        LOGWARNING("Failed to json decode global value in update_subscribe %s", buf);
        goto out_free;
    }
    if (!global) {
        if (unlikely(!yyjson_obj_get_int(&userid, val, "userid"))) {
            LOGWARNING("Failed to json decode userid value in update_subscribe %s", buf);
            goto out_free;
        }
    }

    if (!subid)
        LOGNOTICE("Got updated subscribe for proxy %d", id);
    else
        LOGINFO("Got updated subscribe for proxy %d:%d", id, subid);

    /* Is this a replacement for an existing proxy id? */
    old = existing_subproxy(sdata, id, subid);
    if (old) {
        dead_proxyid(sdata, id, subid, true, false);
        proxy = old;
        proxy->dead = false;
    } else /* This is where all new proxies are created */
        proxy = subproxy_by_id(sdata, id, subid);
    proxy->global = global;
    proxy->userid = userid;
    proxy->subscribed = true;
    proxy->diff = ckpool.startdiff;
    memset(proxy->baseurl, 0, 128);
    memset(proxy->url, 0, 128);
    memset(proxy->auth, 0, 128);
    memset(proxy->pass, 0, 128);
    strncpy(proxy->baseurl, yyjson_get_str(yyjson_obj_get(val, "baseurl")), 127);
    strncpy(proxy->url, yyjson_get_str(yyjson_obj_get(val, "url")), 127);
    strncpy(proxy->auth, yyjson_get_str(yyjson_obj_get(val, "auth")), 127);
    strncpy(proxy->pass, yyjson_get_str(yyjson_obj_get(val, "pass")), 127);

    dsdata = proxy->sdata;

    ck_wlock(&dsdata->workbase_lock);
    /* Length is checked by generator */
    strcpy(proxy->enonce1, yyjson_get_str(yyjson_obj_get(val, "enonce1")));
    proxy->enonce1constlen = strlen(proxy->enonce1) / 2;
    hex2bin(proxy->enonce1bin, proxy->enonce1, proxy->enonce1constlen);
    proxy->nonce2len = yyjson_get_sint(yyjson_obj_get(val, "nonce2len"));
    /*
     * Split generator-presented nonce2len (en1var+en2 only; const pad is
     * already in enonce1) into per-client en1var + miner en2.
     * Must not map nonce2len==8 → en1var=0 (max_clients=1); use classic
     * auto-split for ≤8 and en2=8 first only when nonce2len > 8.
     */
    if (ckpool.nonce2length) {
        int en2 = ckpool.nonce2length;

        if (en2 > 8)
            en2 = 8;
        if (en2 > proxy->nonce2len)
            en2 = proxy->nonce2len;
        if (en2 < 0)
            en2 = 0;
        proxy->enonce2varlen = en2;
        proxy->enonce1varlen = proxy->nonce2len - en2;
    } else if (proxy->nonce2len > 8) {
        /* Large grant: en2=8 (SV1-safe), rest → en1var */
        proxy->enonce2varlen = 8;
        proxy->enonce1varlen = proxy->nonce2len - 8;
    } else if (proxy->nonce2len > 7) {
        /* Classic U=8: 4 unique + 4 miner */
        proxy->enonce1varlen = 4;
        proxy->enonce2varlen = proxy->nonce2len - 4;
    } else if (proxy->nonce2len > 5) {
        proxy->enonce1varlen = 2;
        proxy->enonce2varlen = proxy->nonce2len - 2;
    } else if (proxy->nonce2len > 3) {
        proxy->enonce1varlen = 1;
        proxy->enonce2varlen = proxy->nonce2len - 1;
    } else if (proxy->nonce2len > 0) {
        proxy->enonce1varlen = 0;
        proxy->enonce2varlen = proxy->nonce2len;
    } else {
        proxy->enonce2varlen = 0;
        proxy->enonce1varlen = 0;
    }
    /* enonce1_64 only supplies 8 unique bytes */
    if (proxy->enonce1varlen > 8) {
        proxy->enonce1varlen = 8;
        proxy->enonce2varlen = proxy->nonce2len - proxy->enonce1varlen;
    }
    /* The combined const + var extranonce1 is written into a fixed 16 byte
     * enonce1bin buffer per client, so a hostile or buggy upstream that
     * grants a long const (up to 15) plus var could otherwise overflow it.
     * The generator caps const at 15 in isolation but nothing bounds the
     * sum. Give var only the room left after const, handing the remainder
     * to the miner facing enonce2. */
    if (proxy->enonce1constlen + proxy->enonce1varlen > (int)sizeof(proxy->enonce1bin)) {
        proxy->enonce1varlen = (int)sizeof(proxy->enonce1bin) - proxy->enonce1constlen;
        if (proxy->enonce1varlen < 0)
            proxy->enonce1varlen = 0;
        proxy->enonce2varlen = proxy->nonce2len - proxy->enonce1varlen;
        LOGWARNING(
            "Clamped oversize extranonce1 from upstream proxy %d:%d to const %d var %d", id, subid,
            proxy->enonce1constlen, proxy->enonce1varlen);
    }
    if (proxy->enonce1varlen > 0 && proxy->enonce1varlen < 8)
        proxy->max_clients = 1ll << (proxy->enonce1varlen * 8);
    else if (proxy->enonce1varlen >= 8)
        proxy->max_clients = INT64_MAX / 2;
    else
        proxy->max_clients = 1;
    proxy->clients = 0;
    ck_wunlock(&dsdata->workbase_lock);

    if (subid) {
        LOGINFO(
            "Upstream pool %s %d:%d extranonce2 length %d, max proxy clients %" PRId64, proxy->url, id, subid,
            proxy->nonce2len, proxy->max_clients);
    } else {
        LOGNOTICE(
            "Upstream pool %s %d extranonce2 length %d, max proxy clients %" PRId64, proxy->url, id, proxy->nonce2len,
            proxy->max_clients);
    }
    if (ckpool.nonce2length && proxy->enonce2varlen != ckpool.nonce2length)
        LOGWARNING(
            "Only able to set nonce2len %d of requested %d on proxy %d:%d", proxy->enonce2varlen, ckpool.nonce2length,
            id, subid);
    yyjson_doc_free(doc);

    /* Set the priority on a new proxy now that we have all the fields
     * filled in to push it to its correct priority position in the
     * hashlist. */
    if (!old)
        set_proxy_prio(sdata, proxy, id);

    check_proxy(sdata, proxy);
    return;
out_free:
    yyjson_doc_free(doc);
}

/* Find the highest priority alive proxy belonging to userid and recruit extra
 * subproxies. */
static void recruit_best_userproxy(sdata_t* sdata, const int userid, const int recruits) {
    proxy_t *proxy, *subproxy, *tmp, *subtmp;
    int      id = -1;

    mutex_lock(&sdata->proxy_lock);
    HASH_ITER(hh, sdata->proxies, proxy, tmp) {
        if (proxy->userid < userid)
            continue;
        if (proxy->userid > userid)
            break;
        HASH_ITER(sh, proxy->subproxies, subproxy, subtmp) {
            if (subproxy->dead)
                continue;
            id = proxy->id;
        }
    }
    mutex_unlock(&sdata->proxy_lock);

    if (id != -1)
        generator_recruit(id, recruits);
}

/* Check how much headroom the userid proxies have and reconnect any clients
 * that are not bound to it that should be */
static void check_userproxies(sdata_t* sdata, proxy_t* proxy, const int userid) {
    int64_t             headroom = best_userproxy_headroom(sdata, userid);
    stratum_instance_t *client, *tmpclient;
    int                 reconnects = 0;

    ck_rlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->stratum_instances, client, tmpclient) {
        if (client->dropped)
            continue;
        if (!client->authorised)
            continue;
        if (client->user_id != userid)
            continue;
        /* Is the client already bound to a proxy of its own userid of
         * a higher priority than this one. */
        if (client->proxy->userid == userid && client->proxy->parent->priority <= proxy->parent->priority)
            continue;
        if (headroom-- < 1)
            continue;
        reconnects++;
        reconnect_client(sdata, client);
    }
    ck_runlock(&sdata->instance_lock);

    if (reconnects) {
        LOGINFO("%d clients flagged for reconnect to user %d proxies", reconnects, userid);
    }
    if (headroom < 0)
        recruit_best_userproxy(sdata, userid, -headroom);
}

static void update_notify(const char* cmd) {
    sdata_t *       sdata = ckpool.sdata, *dsdata;
    bool            new_block = false, clean;
    int             i, id = 0, subid = 0;
    yyjson_mut_val* merkle_arr;
    yyjson_val *    val, *merkle_val;
    char            header[272];
    const char*     buf;
    yyjson_doc*     doc;
    proxy_t*        proxy;
    workbase_t*     wb;

    if (unlikely(strlen(cmd) < 8)) {
        LOGWARNING("Zero length string passed to update_notify");
        return;
    }
    buf = cmd + 7; /* "notify=" */
    LOGDEBUG("Update notify: %s", buf);

    doc = yyjson_read(buf, strlen(buf), 0);
    if (unlikely(!doc)) {
        LOGWARNING("Failed to json decode in update_notify");
        return;
    }
    val = yyjson_doc_get_root(doc);
    yyjson_obj_get_int(&id, val, "proxy");
    yyjson_obj_get_int(&subid, val, "subproxy");
    proxy = existing_subproxy(sdata, id, subid);
    if (unlikely(!proxy || !proxy->subscribed)) {
        LOGINFO("No valid proxy %d:%d subscription to update notify yet", id, subid);
        goto out;
    }
    LOGINFO("Got updated notify for proxy %d:%d", id, subid);

    wb = ckzalloc(sizeof(workbase_t));
    wb->proxy = true;

    yyjson_obj_int64cpy(&wb->id, val, "jobid");
    yyjson_obj_strncpy(wb->prevhash, val, "prevhash", sizeof(wb->prevhash));
    yyjson_obj_intcpy(&wb->coinb1len, val, "coinb1len");
    /* Bound the coinbase allocations against absurd values */
    if (unlikely(wb->coinb1len < 1 || wb->coinb1len > MAX_COINBASE_LEN)) {
        LOGWARNING("Proxy %d:%d notify with invalid coinb1len %d", id, subid, wb->coinb1len);
        clear_workbase(wb);
        goto out;
    }
    /* get_sernumber reads up to 5 bytes at offset 42 of coinb1bin so pad
     * the zeroed allocation to keep the read in bounds for undersized
     * coinbases such as low height regtest ones */
    wb->coinb1bin = ckzalloc(wb->coinb1len < 47 ? 47 : wb->coinb1len);
    wb->coinb1 = ckalloc(wb->coinb1len * 2 + 1);
    yyjson_obj_strncpy(wb->coinb1, val, "coinbase1", wb->coinb1len * 2 + 1);
    hex2bin(wb->coinb1bin, wb->coinb1, wb->coinb1len);
    wb->height = get_sernumber(wb->coinb1bin + 42);
    yyjson_obj_strdup(&wb->coinb2, val, "coinbase2");
    wb->coinb2len = strlen(wb->coinb2) / 2;
    /* As with coinb1len above, bound the coinbase allocations */
    if (unlikely(wb->coinb2len > MAX_COINBASE_LEN)) {
        LOGWARNING("Proxy %d:%d notify with invalid coinb2len %d", id, subid, wb->coinb2len);
        clear_workbase(wb);
        goto out;
    }
    wb->coinb2bin = ckalloc(wb->coinb2len);
    hex2bin(wb->coinb2bin, wb->coinb2, wb->coinb2len);
    merkle_val = yyjson_obj_get(val, "merklehash");
    wb->merkles = yyjson_arr_size(merkle_val);
    /* merklehash is a fixed size array so reject rather than overflow it */
    if (unlikely(wb->merkles > 16)) {
        LOGWARNING("Proxy %d:%d notify with %d merkles exceeds max of 16", id, subid, wb->merkles);
        clear_workbase(wb);
        goto out;
    }
    wb->yymerkle_doc = yyjson_mut_doc_new(&ckyyalc);
    merkle_arr = yyjson_mut_arr(wb->yymerkle_doc);
    yyjson_mut_doc_set_root(wb->yymerkle_doc, merkle_arr);
    for (i = 0; i < wb->merkles; i++) {
        const char* merkle = yyjson_get_str(yyjson_arr_get(merkle_val, i));

        /* Each merkle hash is a fixed 64 hex char (32 byte) value. Reject
         * anything else to avoid overflowing merklehash on copy or
         * overreading it in hex2bin. */
        if (unlikely(!merkle || strlen(merkle) != 64)) {
            LOGWARNING("Proxy %d:%d notify with invalid merkle hash", id, subid);
            clear_workbase(wb);
            goto out;
        }
        strcpy(&wb->merklehash[i][0], merkle);
        hex2bin(&wb->merklebin[i][0], &wb->merklehash[i][0], 32);
        yyjson_mut_arr_add_str(wb->yymerkle_doc, merkle_arr, &wb->merklehash[i][0]);
    }
    yyjson_obj_strncpy(wb->bbversion, val, "bbversion", sizeof(wb->bbversion));
    yyjson_obj_strncpy(wb->nbit, val, "nbit", sizeof(wb->nbit));
    yyjson_obj_strncpy(wb->ntime, val, "ntime", sizeof(wb->ntime));
    sscanf(wb->ntime, "%x", &wb->ntime32);
    clean = yyjson_is_true(yyjson_obj_get(val, "clean"));
    ts_realtime(&wb->gentime);
    snprintf(
        header, 270, "%s%s%s%s%s%s%s", wb->bbversion, wb->prevhash,
        "0000000000000000000000000000000000000000000000000000000000000000", wb->ntime, wb->nbit, "00000000", /* nonce */
        workpadding);
    header[224] = 0;
    LOGDEBUG("Header: %s", header);
    hex2bin(wb->headerbin, header, 112);
    wb->txn_hashes = ckzalloc(1);

    dsdata = proxy->sdata;

    ck_rlock(&dsdata->workbase_lock);
    strcpy(wb->enonce1const, proxy->enonce1);
    wb->enonce1constlen = proxy->enonce1constlen;
    memcpy(wb->enonce1constbin, proxy->enonce1bin, wb->enonce1constlen);
    wb->enonce1varlen = proxy->enonce1varlen;
    wb->enonce2varlen = proxy->enonce2varlen;
    wb->diff = proxy->diff;
    ck_runlock(&dsdata->workbase_lock);

    add_base(dsdata, wb, &new_block);
    if (new_block) {
        if (subid)
            LOGINFO("Block hash on proxy %d:%d changed to %s", id, subid, dsdata->lastswaphash);
        else
            LOGNOTICE("Block hash on proxy %d changed to %s", id, dsdata->lastswaphash);
    }

    check_proxy(sdata, proxy);
    clean |= new_block;
    LOGINFO("Proxy %d:%d broadcast updated stratum notify with%s clean", id, subid, clean ? "" : "out");
    stratum_broadcast_update(dsdata, wb, clean);
out:
    yyjson_doc_free(doc);
}

static void stratum_send_diff(sdata_t* sdata, const stratum_instance_t* client);

static void update_diff(const char* cmd) {
    sdata_t *           sdata = ckpool.sdata, *dsdata;
    stratum_instance_t *client, *tmp;
    double              old_diff, diff;
    int                 id = 0, subid = 0;
    const char*         buf;
    yyjson_val*         val;
    yyjson_doc*         doc;
    proxy_t*            proxy;

    if (unlikely(strlen(cmd) < 6)) {
        LOGWARNING("Zero length string passed to update_diff");
        return;
    }
    buf = cmd + 5; /* "diff=" */
    LOGDEBUG("Update diff: %s", buf);

    doc = yyjson_read(buf, strlen(buf), 0);
    if (unlikely(!doc)) {
        LOGWARNING("Failed to json decode in update_diff");
        return;
    }
    val = yyjson_doc_get_root(doc);
    yyjson_obj_get_int(&id, val, "proxy");
    yyjson_obj_get_int(&subid, val, "subproxy");
    yyjson_obj_dblcpy(&diff, val, "diff");
    yyjson_doc_free(doc);

    LOGINFO("Got updated diff for proxy %d:%d", id, subid);
    proxy = existing_subproxy(sdata, id, subid);
    if (!proxy) {
        LOGINFO("No existing subproxy %d:%d to update diff", id, subid);
        return;
    }

    /* We only really care about integer diffs so clamp the lower limit to
     * 1 or it will round down to zero. */
    if (unlikely(diff < 1))
        diff = 1;

    dsdata = proxy->sdata;

    if (unlikely(!dsdata->current_workbase)) {
        LOGINFO("No current workbase to update diff yet");
        return;
    }

    ck_wlock(&dsdata->workbase_lock);
    old_diff = proxy->diff;
    dsdata->current_workbase->diff = proxy->diff = diff;
    ck_wunlock(&dsdata->workbase_lock);

    if (old_diff < diff)
        return;

    /* If the diff has dropped, iterate over all the clients and check
     * they're at or below the new diff, and update it if not. */
    ck_rlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->stratum_instances, client, tmp) {
        if (client->proxyid != id)
            continue;
        if (client->subproxyid != subid)
            continue;
        if (client->diff > diff) {
            client->diff = diff;
            stratum_send_diff(sdata, client);
        }
    }
    ck_runlock(&sdata->instance_lock);
}

#if 0
static void generator_drop_proxy(const int64_t id, const int subid)
{
	char msg[256];

	sprintf(msg, "dropproxy=%ld:%d", id, subid);
	send_proc(ckpool.generator,msg);
}
#endif

/* Free proxy resources. Returns false if any workbase still has readcount
 * (share/auth path holds it) — proxy must remain reachable for a later reap.
 * When true, proxy and its sdata are freed and must not be used. */
static bool free_proxy(proxy_t* proxy) {
    sdata_t*    dsdata = proxy->sdata;
    workbase_t *pending = NULL, *wb, *tmpwb;
    bool        busy = false;

    /* Delete any shares in the proxy's hashtable. */
    if (dsdata) {
        share_t *share, *tmpshare;

        mutex_lock(&dsdata->share_lock);
        HASH_ITER(hh, dsdata->shares, share, tmpshare) {
            HASH_DEL(dsdata->shares, share);
            dealloc(share);
        }
        mutex_unlock(&dsdata->share_lock);

        /* Honour readcount the same way aging does — never clear a wb
         * still held across unlocks by a share/auth path. Check and
         * detach idle wbs under the same lock as get_workbase. */
        ck_wlock(&dsdata->workbase_lock);
        HASH_ITER(hh, dsdata->workbases, wb, tmpwb) {
            if (wb->readcount) {
                busy = true;
                continue;
            }
            HASH_DEL(dsdata->workbases, wb);
            if (dsdata->current_workbase == wb)
                dsdata->current_workbase = NULL;
            wb->hh.next = pending;
            pending = wb;
        }
        ck_wunlock(&dsdata->workbase_lock);

        while (pending) {
            wb = pending;
            pending = (workbase_t*)wb->hh.next;
            wb->hh.next = NULL;
            clear_workbase(wb);
        }

        if (busy)
            return false;
    }

    free(proxy->sdata);
    free(proxy);
    return true;
}

/* Remove subproxies that are flagged dead. Then see if there
 * are any retired proxies that no longer have any other subproxies and reap
 * those. */
static void reap_proxies(sdata_t* sdata) {
    proxy_t *proxy, *proxytmp, *subproxy, *subtmp;
    int      dead = 0;

    if (!ckpool.proxy)
        return;

    mutex_lock(&sdata->proxy_lock);
    HASH_ITER(hh, sdata->proxies, proxy, proxytmp) {
        HASH_ITER(sh, proxy->subproxies, subproxy, subtmp) {
            if (!subproxy->bound_clients && !subproxy->dead) {
                /* Reset the counter to reuse this proxy */
                subproxy->clients = 0;
                continue;
            }
            if (proxy == subproxy)
                continue;
            if (subproxy->bound_clients)
                continue;
            if (!subproxy->dead)
                continue;
            if (unlikely(!subproxy->subid)) {
                LOGWARNING(
                    "Unexepectedly found proxy %d:%d as subproxy of %d:%d", subproxy->id, subproxy->subid, proxy->id,
                    proxy->subid);
                continue;
            }
            if (unlikely(subproxy == sdata->proxy)) {
                LOGWARNING("Unexepectedly found proxy %d:%d as current", subproxy->id, subproxy->subid);
                continue;
            }
            /* Unlink first so concurrent lookups miss it; re-add if
             * free_proxy defers because workbases are still held. */
            HASH_DELETE(sh, proxy->subproxies, subproxy);
            proxy->subproxy_count--;
            if (!free_proxy(subproxy)) {
                HASH_ADD(sh, proxy->subproxies, subid, sizeof(int), subproxy);
                proxy->subproxy_count++;
                continue;
            }
            dead++;
        }
        /* Should we reap the parent proxy too?*/
        if (!proxy->deleted || proxy->subproxy_count > 1 || proxy->bound_clients)
            continue;
        if (sdata->proxy == proxy)
            sdata->proxy = NULL;
        HASH_DELETE(sh, proxy->subproxies, proxy);
        HASH_DELETE(hh, sdata->proxies, proxy);
        if (!free_proxy(proxy)) {
            /* Parent cannot die while workbases busy — put back. */
            HASH_ADD_INT(sdata->proxies, id, proxy);
            HASH_ADD(sh, proxy->subproxies, subid, sizeof(int), proxy);
            if (!sdata->proxy && proxy->global && !proxy->dead)
                sdata->proxy = proxy;
        }
    }
    mutex_unlock(&sdata->proxy_lock);

    if (dead)
        LOGINFO("Stratifier discarded %d dead proxies", dead);
}

/* Enter with instance_lock held */
static stratum_instance_t* __instance_by_id(sdata_t* sdata, const int64_t id) {
    stratum_instance_t* client;

    HASH_FIND_I64(sdata->stratum_instances, &id, client);
    return client;
}

static stratum_instance_t* instance_by_id(sdata_t* sdata, const int64_t id) {
    stratum_instance_t* client;

    ck_rlock(&sdata->instance_lock);
    client = __instance_by_id(sdata, id);
    ck_runlock(&sdata->instance_lock);

    return client;
}

/* Increase the reference count of instance */
static void __inc_instance_ref(stratum_instance_t* client) {
    client->ref++;
}

/* Find an __instance_by_id and increase its reference count allowing us to
 * use this instance outside of instance_lock without fear of it being
 * dereferenced. Does not return dropped clients still on the list. */
static inline stratum_instance_t* ref_instance_by_id(sdata_t* sdata, const int64_t id) {
    stratum_instance_t* client;

    ck_wlock(&sdata->instance_lock);
    client = __instance_by_id(sdata, id);
    if (client) {
        if (unlikely(client->dropped))
            client = NULL;
        else
            __inc_instance_ref(client);
    }
    ck_wunlock(&sdata->instance_lock);

    return client;
}

static void __drop_client(sdata_t* sdata, stratum_instance_t* client, bool lazily, char** msg) {
    user_instance_t* user = client->user_instance;

    if (unlikely(client->node))
        DL_DELETE2(sdata->node_instances, client, node_prev, node_next);
    else if (unlikely(client->trusted))
        DL_DELETE2(sdata->remote_instances, client, remote_prev, remote_next);

    if (client->workername) {
        if (user) {
            /* No message anywhere if throttled, too much flood and
             * these only can be LOGNOTICE messages.
             */
            if (!user->throttled) {
                ASPRINTF(
                    msg, "Dropped client %s %s user %s worker %s %s", client->identity, client->address, user->username,
                    client->workername, lazily ? "lazily" : "");
            }
        } else {
            ASPRINTF(
                msg, "Dropped client %s %s no user worker %s %s", client->identity, client->address, client->workername,
                lazily ? "lazily" : "");
        }
    } else {
        /* Workerless client. Too noisy to log them all */
    }
    __del_client(sdata, client);
    __kill_instance(sdata, client);
}

static int __dec_instance_ref(stratum_instance_t* client) {
    return --client->ref;
}

/* Decrease the reference count of instance. */
static void
_dec_instance_ref(sdata_t* sdata, stratum_instance_t* client, const char* file, const char* func, const int line) {
    char_entry_t* entries = NULL;
    bool          dropped = false;
    char*         msg = NULL;
    int           ref;

    ck_wlock(&sdata->instance_lock);
    ref = __dec_instance_ref(client);
    /* See if there are any instances that were dropped that could not be
     * moved due to holding a reference and drop them now. */
    if (unlikely(client->dropped && !ref)) {
        dropped = true;
        __drop_client(sdata, client, true, &msg);
        if (msg)
            add_msg_entry(&entries, &msg);
    }
    ck_wunlock(&sdata->instance_lock);

    if (entries)
        notice_msg_entries(&entries);
    /* This should never happen */
    if (unlikely(ref < 0))
        LOGERR("Instance ref count dropped below zero from %s %s:%d", file, func, line);

    if (dropped)
        reap_proxies(sdata);
}

#define dec_instance_ref(sdata, instance) _dec_instance_ref(sdata, instance, __FILE__, __func__, __LINE__)

/* If we have a no longer used stratum instance in the recycled linked list,
 * use that, otherwise calloc a fresh one. */
static stratum_instance_t* __recruit_stratum_instance(sdata_t* sdata) {
    stratum_instance_t* client = sdata->recycled_instances;

    if (client)
        DL_DELETE2(sdata->recycled_instances, client, recycled_prev, recycled_next);
    else {
        client = ckzalloc(sizeof(stratum_instance_t));
        sdata->stratum_generated++;
    }
    return client;
}

/* Enter with write instance_lock held, drops and grabs it again */
static stratum_instance_t* __stratum_add_instance(int64_t id, const char* address, int server) {
    sdata_t*            sdata = ckpool.sdata;
    stratum_instance_t* client;
    int64_t             pass_id;

    client = __recruit_stratum_instance(sdata);
    ck_wunlock(&sdata->instance_lock);

    /* Fake a share time at startup to prevent client being dropped for
     * being idle. */
    client->start_time = client->last_share.tv_sec = time(NULL);

    client->id = id;
    client->session_id = ++sdata->session_id;
    snprintf(client->address, sizeof(client->address), "%s", address);
    /* Sanity check to not overflow lookup in ckpool.serverurl[]. A negative
     * index would read before the allocation in the trusted[]/nodeserver[]
     * checks as well, so bound both ends. */
    if (server < 0 || server >= ckpool.serverurls)
        server = 0;
    client->server = server;
    client->diff = client->old_diff = ckpool.startdiff;
    if (ckpool.server_highdiff && ckpool.server_highdiff[server]) {
        client->suggest_diff = ckpool.highdiff;
        if (client->suggest_diff > client->diff)
            client->diff = client->old_diff = client->suggest_diff;
    }
    tv_time(&client->ldc);
    /* Points to ckp sdata in ckpool mode, but is changed later in proxy
     * mode . */
    client->sdata = sdata;
    if ((pass_id = subclient(id))) {
        stratum_instance_t* remote = instance_by_id(sdata, pass_id);

        id &= 0xffffffffll;
        if (remote && remote->node) {
            client->latency = remote->latency;
            LOGINFO("Client %s inherited node latency of %d", client->identity, client->latency);
            sprintf(client->identity, "node:%" PRId64 " subclient:%" PRId64, pass_id, id);
        } else if (remote && remote->trusted) {
            sprintf(client->identity, "remote:%" PRId64 " subclient:%" PRId64, pass_id, id);
        } else { /* remote->passthrough remaining */
            sprintf(client->identity, "passthrough:%" PRId64 " subclient:%" PRId64, pass_id, id);
        }
        client->virtualid = connector_newclientid();
    } else {
        sprintf(client->identity, "%" PRId64, id);
        client->virtualid = id;
    }

    ck_wlock(&sdata->instance_lock);
    HASH_ADD_I64(sdata->stratum_instances, id, client);
    return client;
}

static uint64_t disconnected_sessionid_exists(sdata_t* sdata, const int session_id, const int64_t id) {
    session_t* session;
    int64_t    old_id = 0;
    uint64_t   ret = 0;

    ck_wlock(&sdata->instance_lock);
    HASH_FIND_INT(sdata->disconnected_sessions, &session_id, session);
    if (!session)
        goto out_unlock;
    HASH_DEL(sdata->disconnected_sessions, session);
    sdata->stats.disconnected--;
    ret = session->enonce1_64;
    old_id = session->client_id;
    dealloc(session);
out_unlock:
    ck_wunlock(&sdata->instance_lock);

    if (ret)
        LOGINFO("Reconnecting old instance %" PRId64 " to instance %" PRId64, old_id, id);
    return ret;
}

static inline bool client_active(stratum_instance_t* client) {
    return (client->authorised && !client->dropped);
}

static inline bool remote_server(stratum_instance_t* client) {
    return (client->node || client->passthrough || client->trusted);
}

/* Ask the connector asynchronously to send us dropclient commands if this
 * client no longer exists. */
static void connector_test_client(const int64_t id) {
    char buf[256];

    LOGDEBUG("Stratifier requesting connector test client %" PRId64, id);
    snprintf(buf, 255, "testclient=%" PRId64, id);
    send_proc(ckpool.connector, buf);
}

/* For creating a list of sends without locking that can then be concatenated
 * to the stratum_sends list. Minimises locking and avoids taking recursive
 * locks. Sends only to sdata bound clients (everyone in ckpool) */
static void stratum_broadcast(sdata_t* sdata, yyjson_mut_doc* doc, const int msg_type) {
    sdata_t*            ckp_sdata = ckpool.sdata;
    stratum_instance_t *client, *tmp;
    ckmsg_t*            bulk_send = NULL;
    yyjson_mut_val*     root;
    int                 messages = 0;

    if (unlikely(!doc)) {
        LOGERR("Sent null json to stratum_yybroadcast");
        return;
    }

    if (ckpool.node) {
        yyjson_mut_doc_free(doc);
        return;
    }

    root = yyjson_mut_doc_get_root(doc);
    if (unlikely(!root)) {
        LOGERR("Failed to get root in stratum_yybroadcast");
        yyjson_mut_doc_free(doc);
        return;
    }

    ck_rlock(&ckp_sdata->instance_lock);
    HASH_ITER(hh, ckp_sdata->stratum_instances, client, tmp) {
        ckmsg_t* client_msg;
        smsg_t*  msg;

        if (sdata != ckp_sdata && client->sdata != sdata)
            continue;

        if (!client_active(client) || remote_server(client))
            continue;
        /* SV2 sessions are not connector clients; work is pushed via
         * sv2_strat_on_work_update. SV1 notify would fail send and
         * incorrectly drop the virtual instance. */
        if (client->sv2)
            continue;

        /* Only send messages to whitelisted clients */
        if (msg_type == SM_MSG && !client->messages)
            continue;

        client_msg = ckalloc(sizeof(ckmsg_t));
        msg = ckzalloc(sizeof(smsg_t));
        if (subclient(client->id))
            yyjson_mut_obj_add_str(doc, root, "node.method", stratum_msgs[msg_type]);
        msg->doc = yyjson_mut_doc_mut_copy(doc, &ckyyalc);
        msg->client_id = client->id;
        client_msg->data = msg;
        DL_APPEND(bulk_send, client_msg);
        messages++;
    }
    ck_runlock(&ckp_sdata->instance_lock);

    yyjson_mut_doc_free(doc);

    if (likely(bulk_send))
        ssend_bulk_append(sdata, bulk_send, messages);
}

static void stratum_add_yysend(sdata_t* sdata, yyjson_mut_doc* doc, const int64_t client_id, const int msg_type)

{
    int64_t remote_id;
    smsg_t* msg;

    if (ckpool.node) {
        /* Node shouldn't be sending any messages as it only uses the
         * stratifier for monitoring activity. */
        yyjson_mut_doc_free(doc);
        return;
    }

    if ((remote_id = subclient(client_id))) {
        stratum_instance_t* remote = ref_instance_by_id(sdata, remote_id);
        yyjson_mut_val*     root;

        if (unlikely(!remote)) {
            yyjson_mut_doc_free(doc);
            return;
        }
        root = yyjson_mut_doc_get_root(doc);
        if (remote->trusted)
            yyjson_mut_obj_add_str(doc, root, "method", stratum_msgs[msg_type]);
        else /* Both remote->node and remote->passthrough */
            yyjson_mut_obj_add_str(doc, root, "node.method", stratum_msgs[msg_type]);
        dec_instance_ref(sdata, remote);
    }

    LOGDEBUG("Sending stratum message %s", stratum_msgs[msg_type]);
    msg = ckzalloc(sizeof(smsg_t));
    msg->doc = doc;
    msg->client_id = client_id;
    if (likely(ckmsgq_add(sdata->ssends, msg)))
        return;
    yyjson_mut_doc_free(doc);
    free(msg);
}

static void drop_client(sdata_t* sdata, const int64_t id) {
    char_entry_t*       entries = NULL;
    stratum_instance_t* client;
    char*               msg = NULL;
    bool                was_sv2 = false;

    LOGINFO("Stratifier asked to drop client %" PRId64, id);

    ck_wlock(&sdata->instance_lock);
    client = __instance_by_id(sdata, id);
    if (client) {
        was_sv2 = client->sv2;
        __disconnect_session(sdata, client);
        /* If the client is still holding a reference, don't drop them
         * now but wait till the reference is dropped */
        if (!client->ref) {
            __drop_client(sdata, client, false, &msg);
            if (msg)
                add_msg_entry(&entries, &msg);
        } else
            client->dropped = true;
    }
    ck_wunlock(&sdata->instance_lock);

#ifdef HAVE_SV2
    if (was_sv2)
        sv2_strat_clear_instance(id);
#endif
    if (entries)
        notice_msg_entries(&entries);
    reap_proxies(sdata);
}

static void stratum_broadcast_message(sdata_t* sdata, const char* msg) {
    yyjson_mut_doc* doc;

    doc = yyjson_mut_pack("{snsss[s]}", "id", "method", "client.show_message", "params", msg);
    stratum_broadcast(sdata, doc, SM_MSG);
}

/* Send a generic reconnect to all clients without parameters to make them
 * reconnect to the same server. */
static void request_reconnect(sdata_t* sdata, const char* cmd) {
    char *              port = strdupa(cmd), *url = NULL;
    stratum_instance_t *client, *tmp;
    yyjson_mut_doc*     doc;

    strsep(&port, ":");
    if (port)
        url = strsep(&port, ",");
    if (url && port) {
        doc = yyjson_mut_pack("{snsss[ssi]}", "id", "method", "client.reconnect", "params", url, port, 0);
    } else
        doc = yyjson_mut_pack("{snsss[]}", "id", "method", "client.reconnect", "params");
    stratum_broadcast(sdata, doc, SM_RECONNECT);

    /* Tag all existing clients as dropped now so they can be removed
     * lazily */
    ck_wlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->stratum_instances, client, tmp) {
        client->dropped = true;
    }
    ck_wunlock(&sdata->instance_lock);
}

static void reset_bestshares(sdata_t* sdata) {
    user_instance_t *   user, *tmpuser;
    stratum_instance_t *client, *tmp;

    /* Can do this unlocked since it's just zeroing the values */
    sdata->stats.accounted_diff_shares = sdata->stats.accounted_shares = sdata->stats.accounted_rejects = 0;
    sdata->stats.best_diff = 0;

    ck_rlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->stratum_instances, client, tmp) {
        client->best_diff = 0;
    }
    HASH_ITER(hh, sdata->user_instances, user, tmpuser) {
        worker_instance_t* worker;

        user->best_diff = 0;
        DL_FOREACH(user->worker_instances, worker) {
            worker->best_diff = 0;
        }
    }
    ck_runlock(&sdata->instance_lock);
}

static user_instance_t* get_user(sdata_t* sdata, const char* username);

static user_instance_t* user_by_workername(sdata_t* sdata, const char* workername) {
    char *           username = strdupa(workername), *ignore;
    user_instance_t* user;

    ignore = username;
    strsep(&ignore, "._");

    /* Find the user first */
    user = get_user(sdata, username);
    return user;
}

static worker_instance_t* get_worker(sdata_t* sdata, user_instance_t* user, const char* workername);

static yyjson_mut_doc* worker_stats(const worker_instance_t* worker) {
    char            suffix1[16], suffix5[16], suffix60[16], suffix1440[16], suffix10080[16];
    yyjson_mut_doc* doc;
    double          ghs;

    ghs = worker->dsps1 * nonces;
    suffix_string(ghs, suffix1, 16, 0);

    ghs = worker->dsps5 * nonces;
    suffix_string(ghs, suffix5, 16, 0);

    ghs = worker->dsps60 * nonces;
    suffix_string(ghs, suffix60, 16, 0);

    ghs = worker->dsps1440 * nonces;
    suffix_string(ghs, suffix1440, 16, 0);

    ghs = worker->dsps10080 * nonces;
    suffix_string(ghs, suffix10080, 16, 0);

    doc = yyjson_mut_pack(
        "{ss,ss,ss,ss,ss}", "hashrate1m", suffix1, "hashrate5m", suffix5, "hashrate1hr", suffix60, "hashrate1d",
        suffix1440, "hashrate7d", suffix10080);
    return doc;
}

static yyjson_mut_doc* user_stats(const user_instance_t* user) {
    char            suffix1[16], suffix5[16], suffix60[16], suffix1440[16], suffix10080[16];
    yyjson_mut_doc* doc;
    double          ghs;

    ghs = user->dsps1 * nonces;
    suffix_string(ghs, suffix1, 16, 0);

    ghs = user->dsps5 * nonces;
    suffix_string(ghs, suffix5, 16, 0);

    ghs = user->dsps60 * nonces;
    suffix_string(ghs, suffix60, 16, 0);

    ghs = user->dsps1440 * nonces;
    suffix_string(ghs, suffix1440, 16, 0);

    ghs = user->dsps10080 * nonces;
    suffix_string(ghs, suffix10080, 16, 0);

    doc = yyjson_mut_pack(
        "{ss,ss,ss,ss,ss,sI,sI}", "hashrate1m", suffix1, "hashrate5m", suffix5, "hashrate1hr", suffix60, "hashrate1d",
        suffix1440, "hashrate7d", suffix10080, "shares", user->shares, "authorised", user->auth_time);
    return doc;
}

/* Adjust workinfo id to virtual value for remote trusted workinfos */
static void remap_workinfo_id(sdata_t* sdata, yyjson_mut_doc* doc, yyjson_mut_val* val, const int64_t client_id) {
    int64_t     mapped_id, id;
    workbase_t* wb;

    yyjson_mut_obj_get_int64(&id, val, "workinfoid");

    ck_rlock(&sdata->workbase_lock);
    wb = __find_remote_workbase(sdata, id, client_id);
    if (likely(wb))
        mapped_id = wb->mapped_id;
    else
        mapped_id = id;
    ck_runlock(&sdata->workbase_lock);

    /* Replace value with mapped id */
    yyjson_mut_obj_put(val, yyjson_mut_str(doc, "workinfoid"), yyjson_mut_sint(doc, mapped_id));
}

static void block_share_summary(sdata_t* sdata) {
    double bdiff, sdiff;

    if (unlikely(!sdata->current_workbase || !sdata->current_workbase->network_diff))
        return;

    sdiff = sdata->stats.accounted_diff_shares;
    bdiff = sdiff / sdata->current_workbase->network_diff * 100;
    LOGWARNING("Block solved after %.0lf shares at %.1f%% diff", sdiff, bdiff);
}

static void block_solve(yyjson_mut_doc* doc) {
    yyjson_mut_val* val = yyjson_mut_doc_get_root(doc);
    char *          msg, *workername = NULL, *protocol = NULL;
    sdata_t*        sdata = ckpool.sdata;
    char            cdfield[64], pbuf[32] = {};
    const char*     blockhash;
    int64_t         reward = 0;
    double          diff = 0;
    int             height = 0;
    ts_t            ts_now;

    ts_realtime(&ts_now);
    sprintf(cdfield, "%lu,%lu", ts_now.tv_sec, ts_now.tv_nsec);

    yyjson_mut_obj_put(val, yyjson_mut_str(doc, "confirmed"), yyjson_mut_strcpy(doc, "1"));
    yyjson_mut_obj_put(val, yyjson_mut_str(doc, "createdate"), yyjson_mut_strcpy(doc, cdfield));
    yyjson_mut_obj_put(val, yyjson_mut_str(doc, "createcode"), yyjson_mut_strcpy(doc, __func__));
    yyjson_mut_obj_get_int(&height, val, "height");
    yyjson_mut_obj_get_double(&diff, val, "diff");
    yyjson_mut_obj_get_string(&workername, val, "workername");
    /* Absent from blocks solved by a pool old enough not to send it, and
     * from upstream solves that have no miner of ours to attribute. */
    yyjson_mut_obj_get_string(&protocol, val, "protocol");
    if (protocol)
        snprintf(pbuf, sizeof(pbuf), " on %s", protocol);
    blockhash = yyjson_mut_get_str(yyjson_mut_obj_get(val, "blockhash"));
    yyjson_mut_obj_get_int64(&reward, val, "reward");

    if (!workername) {
        ASPRINTF(&msg, "Block solved by %s!", ckpool.name);
        LOGWARNING("Solved and confirmed block!");
        if (ckpool.logshares_db)
            db_add_block(sdata, height, blockhash, true, "", "", diff, reward);
    } else {
        yyjson_mut_doc *   user_val, *worker_val;
        worker_instance_t* worker;
        user_instance_t*   user;
        char*              s;

        ASPRINTF(&msg, "Block %d solved by %s @ %s!", height, workername, ckpool.name);
        LOGWARNING("Solved and confirmed block %d by %s%s", height, workername, pbuf);
        user = user_by_workername(sdata, workername);
        worker = get_worker(sdata, user, workername);

        if (ckpool.logshares_db) {
            db_add_block(sdata, height, blockhash, true, workername, user->username, diff, reward);
        }

        ck_rlock(&sdata->instance_lock);
        user_val = user_stats(user);
        worker_val = worker_stats(worker);
        ck_runlock(&sdata->instance_lock);

        s = yyjson_mut_write(user_val, 0, NULL);
        yyjson_mut_doc_free(user_val);
        LOGWARNING("User %s:%s", user->username, s);
        dealloc(s);
        s = yyjson_mut_write(worker_val, 0, NULL);
        yyjson_mut_doc_free(worker_val);
        LOGWARNING("Worker %s:%s", workername, s);
        dealloc(s);
    }
    stratum_broadcast_message(sdata, msg);
    free(msg);

    free(workername);

    block_share_summary(sdata);
    reset_bestshares(sdata);
}

static void block_reject(yyjson_mut_doc* doc) {
    yyjson_mut_val* val = yyjson_mut_doc_get_root(doc);
    const char*     blockhash;
    int             height = 0;

    yyjson_mut_obj_get_int(&height, val, "height");
    blockhash = yyjson_mut_get_str(yyjson_mut_obj_get(val, "blockhash"));
    if (ckpool.logshares_db && blockhash)
        db_update_block_status(ckpool.sdata, blockhash, false, false);

    LOGWARNING("Submitted, but had block %d rejected", height);
}

/* Some upstream pools (like p2pool) don't update stratum often enough and
 * miners disconnect if they don't receive regular communication so send them
 * a ping at regular intervals */
static void broadcast_ping(sdata_t* sdata) {
    yyjson_mut_doc* doc;

    doc = yyjson_mut_pack("{s:[],s:i,s:s}", "params", "id", 42, "method", "mining.ping");

    stratum_broadcast(sdata, doc, SM_PING);
}

static yyjson_mut_val* ckmsgq_stats(ckmsgq_t* ckmsgq, const int size, yyjson_mut_doc* doc) {
    int64_t  memsize, generated;
    ckmsg_t* msg;
    int      objects;

    mutex_lock(ckmsgq->lock);
    DL_COUNT(ckmsgq->msgs, msg, objects);
    generated = ckmsgq->messages;
    mutex_unlock(ckmsgq->lock);

    memsize = (sizeof(ckmsg_t) + size) * objects;
    return yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
}

char* stratifier_stats(void* data) {
    yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val *root = yyjson_mut_obj(doc), *subval;
    int64_t         memsize, generated;
    sdata_t*        sdata = data;
    int             objects;
    char*           buf;

    yyjson_mut_doc_set_root(doc, root);

    ck_rlock(&sdata->workbase_lock);
    objects = HASH_COUNT(sdata->workbases);
    memsize = SAFE_HASH_OVERHEAD(sdata->workbases) + sizeof(workbase_t) * objects;
    generated = sdata->workbases_generated;
    subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "workbases", subval);
    objects = HASH_COUNT(sdata->remote_workbases);
    memsize = SAFE_HASH_OVERHEAD(sdata->remote_workbases) + sizeof(workbase_t) * objects;
    ck_runlock(&sdata->workbase_lock);

    subval = yyjson_mut_pack_val(doc, "{si,sI}", "count", objects, "memory", memsize);
    yyjson_mut_obj_add_val(doc, root, "remote_workbases", subval);

    ck_rlock(&sdata->instance_lock);
    if (ckpool.btcsolo) {
        user_instance_t *user, *tmpuser;
        int              subobjects;

        objects = 0;
        memsize = 0;
        HASH_ITER(hh, sdata->user_instances, user, tmpuser) {
            subobjects = HASH_COUNT(user->userwbs);
            objects += subobjects;
            memsize += SAFE_HASH_OVERHEAD(user->userwbs) + sizeof(struct userwb) * subobjects;
        }
        generated = sdata->userwbs_generated;
        subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
        yyjson_mut_obj_add_val(doc, root, "userwbs", subval);
    }

    objects = HASH_COUNT(sdata->user_instances);
    memsize = SAFE_HASH_OVERHEAD(sdata->user_instances) + sizeof(stratum_instance_t) * objects;
    subval = yyjson_mut_pack_val(doc, "{si,sI}", "count", objects, "memory", memsize);
    yyjson_mut_obj_add_val(doc, root, "users", subval);

    objects = HASH_COUNT(sdata->stratum_instances);
    memsize = SAFE_HASH_OVERHEAD(sdata->stratum_instances);
    generated = sdata->stratum_generated;
    subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "clients", subval);

    objects = sdata->stats.disconnected;
    generated = sdata->disconnected_generated;
    memsize = SAFE_HASH_OVERHEAD(sdata->disconnected_sessions);
    memsize += sizeof(session_t) * sdata->stats.disconnected;
    subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "disconnected", subval);
    ck_runlock(&sdata->instance_lock);

    mutex_lock(&sdata->share_lock);
    generated = sdata->shares_generated;
    objects = HASH_COUNT(sdata->shares);
    memsize = SAFE_HASH_OVERHEAD(sdata->shares) + sizeof(share_t) * objects;
    mutex_unlock(&sdata->share_lock);

    subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "shares", subval);

    ck_rlock(&sdata->txn_lock);
    objects = HASH_COUNT(sdata->txns);
    memsize = SAFE_HASH_OVERHEAD(sdata->txns) + sizeof(txntable_t) * objects;
    generated = sdata->txns_generated;
    subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "transactions", subval);
    ck_runlock(&sdata->txn_lock);

    subval = ckmsgq_stats(sdata->ssends, sizeof(smsg_t), doc);
    yyjson_mut_obj_add_val(doc, root, "ssends", subval);
    /* Don't know exactly how big the string is so just count the pointer for now */
    subval = ckmsgq_stats(sdata->srecvs, sizeof(char*), doc);
    yyjson_mut_obj_add_val(doc, root, "srecvs", subval);
    subval = ckmsgq_stats(sdata->stxnq, sizeof(json_params_t), doc);
    yyjson_mut_obj_add_val(doc, root, "stxnq", subval);

    buf = yyjson_mut_write(doc, 0, NULL);
    yyjson_mut_doc_free(doc);
    LOGNOTICE("Stratifier stats: %s", buf);
    return buf;
}

/* Send a single client a reconnect request, setting the time we sent the
 * request so we can drop the client lazily if it hasn't reconnected on its
 * own more than one minute later if we call reconnect again */
static void reconnect_client(sdata_t* sdata, stratum_instance_t* client) {
    yyjson_mut_doc* doc;

    /* Already requested? */
    if (client->reconnect_request) {
        if (time(NULL) - client->reconnect_request >= 60)
            connector_drop_client(client->id);
        return;
    }
    client->reconnect_request = time(NULL);
    doc = yyjson_mut_pack("{snsss[]}", "id", "method", "client.reconnect", "params");
    stratum_add_yysend(sdata, doc, client->id, SM_RECONNECT);
}

static void dead_proxy(sdata_t* sdata, const char* buf) {
    int id = 0, subid = 0;

    sscanf(buf, "deadproxy=%d:%d", &id, &subid);
    dead_proxyid(sdata, id, subid, false, false);
    reap_proxies(sdata);
}

static void del_proxy(sdata_t* sdata, const char* buf) {
    int id = 0, subid = 0;

    sscanf(buf, "delproxy=%d:%d", &id, &subid);
    dead_proxyid(sdata, id, subid, false, true);
    reap_proxies(sdata);
}

static void reconnect_client_id(sdata_t* sdata, const int64_t client_id) {
    stratum_instance_t* client;

    client = ref_instance_by_id(sdata, client_id);
    if (!client) {
        LOGINFO("reconnect_client_id failed to find client %" PRId64, client_id);
        return;
    }
    client->reconnect = true;
    reconnect_client(sdata, client);
    dec_instance_ref(sdata, client);
}

/* API commands */

static yyjson_mut_val* userinfo(yyjson_mut_doc* doc, const user_instance_t* user) {
    return yyjson_mut_pack_val(
        doc, "{ss,si,si,sf,sf,sf,sf,sf,sf,sI}", "user", user->username, "id", user->id, "workers", user->workers,
        "bestdiff", user->best_diff, "dsps1", user->dsps1, "dsps5", user->dsps5, "dsps60", user->dsps60, "dsps1440",
        user->dsps1440, "dsps10080", user->dsps10080, "lastshare", user->last_share.tv_sec);
}

static void getuser(sdata_t* sdata, const char* buf, int* sockd) {
    yyjson_mut_doc*  res = NULL;
    char*            username = NULL;
    user_instance_t* user;
    yyjson_doc*      val = NULL;
    yyjson_read_err  err_val;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    if (!yyjson_obj_get_string(&username, yyjson_doc_get_root(val), "user")) {
        res = yyjson_errormsg("Failed to find user key");
        goto out;
    }
    if (!strlen(username)) {
        res = yyjson_errormsg("Zero length user key");
        goto out;
    }
    user = get_user(sdata, username);
    res = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_doc_set_root(res, userinfo(res, user));
out:
    if (val)
        yyjson_doc_free(val);
    free(username);
    send_api_yyresponse(res, *sockd);
}

static void userclients(sdata_t* sdata, const char* buf, int* sockd) {
    yyjson_mut_doc *    doc = yyjson_mut_doc_new(&ckyyalc), *res = NULL;
    yyjson_mut_val *    root, *client_arr;
    stratum_instance_t* client;
    char*               username = NULL;
    user_instance_t*    user;
    yyjson_doc*         val = NULL;
    yyjson_read_err     err_val;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    if (!yyjson_obj_get_string(&username, yyjson_doc_get_root(val), "user")) {
        res = yyjson_errormsg("Failed to find user key");
        goto out;
    }
    if (!strlen(username)) {
        res = yyjson_errormsg("Zero length user key");
        goto out;
    }
    user = get_user(sdata, username);
    client_arr = yyjson_mut_arr(doc);

    ck_rlock(&sdata->instance_lock);
    DL_FOREACH2(user->clients, client, user_next) {
        yyjson_mut_arr_add_int(doc, client_arr, client->id);
    }
    ck_runlock(&sdata->instance_lock);

    root = yyjson_mut_pack_val(doc, "{ss,so}", "user", username, "clients", client_arr);
    yyjson_mut_doc_set_root(doc, root);
    res = doc;
    doc = NULL;
out:
    if (doc)
        yyjson_mut_doc_free(doc);
    if (val)
        yyjson_doc_free(val);
    free(username);
    send_api_yyresponse(res, *sockd);
}

static void workerclients(sdata_t* sdata, const char* buf, int* sockd) {
    yyjson_mut_doc *    doc = yyjson_mut_doc_new(&ckyyalc), *res = NULL;
    char *              tmp, *username, *workername = NULL;
    yyjson_mut_val *    root, *client_arr;
    stratum_instance_t* client;
    user_instance_t*    user;
    yyjson_doc*         val = NULL;
    yyjson_read_err     err_val;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    if (!yyjson_obj_get_string(&workername, yyjson_doc_get_root(val), "worker")) {
        res = yyjson_errormsg("Failed to find worker key");
        goto out;
    }
    if (!strlen(workername)) {
        res = yyjson_errormsg("Zero length worker key");
        goto out;
    }
    tmp = strdupa(workername);
    username = strsep(&tmp, "._");
    user = get_user(sdata, username);
    client_arr = yyjson_mut_arr(doc);

    ck_rlock(&sdata->instance_lock);
    DL_FOREACH2(user->clients, client, user_next) {
        if (strcmp(client->workername, workername))
            continue;
        yyjson_mut_arr_add_int(doc, client_arr, client->id);
    }
    ck_runlock(&sdata->instance_lock);

    root = yyjson_mut_pack_val(doc, "{ss,so}", "worker", workername, "clients", client_arr);
    yyjson_mut_doc_set_root(doc, root);
    res = doc;
    doc = NULL;
out:
    if (doc)
        yyjson_mut_doc_free(doc);
    if (val)
        yyjson_doc_free(val);
    free(workername);
    send_api_yyresponse(res, *sockd);
}

static yyjson_mut_val* workerinfo(yyjson_mut_doc* doc, const user_instance_t* user, const worker_instance_t* worker) {
    return yyjson_mut_pack_val(
        doc, "{ss,ss,si,sf,sf,sf,sf,sI,sf,si,sb}", "user", user->username, "worker", worker->workername, "id", user->id,
        "dsps1", worker->dsps1, "dsps5", worker->dsps5, "dsps60", worker->dsps60, "dsps1440", worker->dsps1440,
        "lastshare", worker->last_share.tv_sec, "bestdiff", worker->best_diff, "mindiff", worker->mindiff, "idle",
        worker->idle);
}

static void getworker(sdata_t* sdata, const char* buf, int* sockd) {
    char *             tmp, *username, *workername = NULL;
    yyjson_mut_doc*    res = NULL;
    worker_instance_t* worker;
    user_instance_t*   user;
    yyjson_doc*        val = NULL;
    yyjson_read_err    err_val;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    if (!yyjson_obj_get_string(&workername, yyjson_doc_get_root(val), "worker")) {
        res = yyjson_errormsg("Failed to find worker key");
        goto out;
    }
    if (!strlen(workername)) {
        res = yyjson_errormsg("Zero length worker key");
        goto out;
    }
    tmp = strdupa(workername);
    username = strsep(&tmp, "._");
    user = get_user(sdata, username);
    worker = get_worker(sdata, user, workername);
    res = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_doc_set_root(res, workerinfo(res, user, worker));
out:
    if (val)
        yyjson_doc_free(val);
    free(workername);
    send_api_yyresponse(res, *sockd);
}

static void getworkers(sdata_t* sdata, int* sockd) {
    yyjson_mut_doc*    doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val *   root, *worker_arr;
    worker_instance_t* worker;
    user_instance_t*   user;

    worker_arr = yyjson_mut_arr(doc);

    ck_rlock(&sdata->instance_lock);
    for (user = sdata->user_instances; user; user = user->hh.next) {
        DL_FOREACH(user->worker_instances, worker) {
            yyjson_mut_arr_append(worker_arr, workerinfo(doc, user, worker));
        }
    }
    ck_runlock(&sdata->instance_lock);

    root = yyjson_mut_pack_val(doc, "{so}", "workers", worker_arr);
    yyjson_mut_doc_set_root(doc, root);
    send_api_yyresponse(doc, *sockd);
}

static void getusers(sdata_t* sdata, int* sockd) {
    yyjson_mut_doc*  doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val * root, *user_array;
    user_instance_t* user;

    user_array = yyjson_mut_arr(doc);

    ck_rlock(&sdata->instance_lock);
    for (user = sdata->user_instances; user; user = user->hh.next) {
        yyjson_mut_arr_append(user_array, userinfo(doc, user));
    }
    ck_runlock(&sdata->instance_lock);

    root = yyjson_mut_pack_val(doc, "{so}", "users", user_array);
    yyjson_mut_doc_set_root(doc, root);
    send_api_yyresponse(doc, *sockd);
}

static yyjson_mut_val* clientinfo(yyjson_mut_doc* doc, const stratum_instance_t* client) {
    yyjson_mut_val* val = yyjson_mut_obj(doc);

    /* Too many fields for a pack object, do each discretely to keep track */
    yyjson_mut_obj_add_int(doc, val, "id", client->id);
    yyjson_mut_obj_add_strcpy(doc, val, "enonce1", client->enonce1);
    yyjson_mut_obj_add_strcpy(doc, val, "enonce1var", client->enonce1var);
    yyjson_mut_obj_add_int(doc, val, "enonce1_64", client->enonce1_64);
    yyjson_mut_obj_add_real(doc, val, "diff", client->diff);
    yyjson_mut_obj_add_real(doc, val, "dsps1", client->dsps1);
    yyjson_mut_obj_add_real(doc, val, "dsps5", client->dsps5);
    yyjson_mut_obj_add_real(doc, val, "dsps60", client->dsps60);
    yyjson_mut_obj_add_real(doc, val, "dsps1440", client->dsps1440);
    yyjson_mut_obj_add_real(doc, val, "dsps10080", client->dsps10080);
    yyjson_mut_obj_add_int(doc, val, "lastshare", client->last_share.tv_sec);
    yyjson_mut_obj_add_int(doc, val, "starttime", client->start_time);
    yyjson_mut_obj_add_strcpy(doc, val, "address", client->address);
    yyjson_mut_obj_add_bool(doc, val, "subscribed", client->subscribed);
    yyjson_mut_obj_add_bool(doc, val, "authorised", client->authorised);
    yyjson_mut_obj_add_bool(doc, val, "idle", client->idle);
    yyjson_mut_obj_add_strcpy(doc, val, "useragent", client->useragent ? client->useragent : "");
    yyjson_mut_obj_add_strcpy(doc, val, "workername", client->workername ? client->workername : "");
    yyjson_mut_obj_add_int(doc, val, "userid", client->user_id);
    yyjson_mut_obj_add_int(doc, val, "server", client->server);
    yyjson_mut_obj_add_real(doc, val, "bestdiff", client->best_diff);
    yyjson_mut_obj_add_int(doc, val, "proxyid", client->proxyid);
    yyjson_mut_obj_add_int(doc, val, "subproxyid", client->subproxyid);

    return val;
}

static void getclient(sdata_t* sdata, const char* buf, int* sockd) {
    yyjson_mut_doc*     res = NULL;
    stratum_instance_t* client;
    yyjson_doc*         val = NULL;
    yyjson_read_err     err_val;
    int64_t             client_id;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    if (!yyjson_obj_get_int64(&client_id, yyjson_doc_get_root(val), "id")) {
        res = yyjson_errormsg("Failed to find id key");
        goto out;
    }
    client = ref_instance_by_id(sdata, client_id);
    if (!client) {
        res = yyjson_errormsg("Failed to find client %" PRId64, client_id);
        goto out;
    }
    res = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_doc_set_root(res, clientinfo(res, client));

    dec_instance_ref(sdata, client);
out:
    if (val)
        yyjson_doc_free(val);
    send_api_yyresponse(res, *sockd);
}

static void getclients(sdata_t* sdata, int* sockd) {
    yyjson_mut_doc*     doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val *    root, *client_arr;
    stratum_instance_t* client;

    client_arr = yyjson_mut_arr(doc);

    ck_rlock(&sdata->instance_lock);
    for (client = sdata->stratum_instances; client; client = client->hh.next) {
        yyjson_mut_arr_append(client_arr, clientinfo(doc, client));
    }
    ck_runlock(&sdata->instance_lock);

    root = yyjson_mut_pack_val(doc, "{so}", "clients", client_arr);
    yyjson_mut_doc_set_root(doc, root);
    send_api_yyresponse(doc, *sockd);
}

static void user_clientinfo(sdata_t* sdata, const char* buf, int* sockd) {
    yyjson_mut_doc *    doc = yyjson_mut_doc_new(&ckyyalc), *res = NULL;
    yyjson_mut_val *    root, *client_arr;
    stratum_instance_t* client;
    char*               username = NULL;
    user_instance_t*    user;
    yyjson_doc*         val = NULL;
    yyjson_read_err     err_val;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    if (!yyjson_obj_get_string(&username, yyjson_doc_get_root(val), "user")) {
        res = yyjson_errormsg("Failed to find user key");
        goto out;
    }
    if (!strlen(username)) {
        res = yyjson_errormsg("Zero length user key");
        goto out;
    }
    user = get_user(sdata, username);
    client_arr = yyjson_mut_arr(doc);

    ck_rlock(&sdata->instance_lock);
    DL_FOREACH2(user->clients, client, user_next) {
        yyjson_mut_arr_append(client_arr, clientinfo(doc, client));
    }
    ck_runlock(&sdata->instance_lock);

    root = yyjson_mut_pack_val(doc, "{ss,so}", "user", username, "clients", client_arr);
    yyjson_mut_doc_set_root(doc, root);
    res = doc;
    doc = NULL;
out:
    if (doc)
        yyjson_mut_doc_free(doc);
    if (val)
        yyjson_doc_free(val);
    free(username);
    send_api_yyresponse(res, *sockd);
}

static void worker_clientinfo(sdata_t* sdata, const char* buf, int* sockd) {
    yyjson_mut_doc *    doc = yyjson_mut_doc_new(&ckyyalc), *res = NULL;
    char *              tmp, *username, *workername = NULL;
    yyjson_mut_val *    root, *client_arr;
    stratum_instance_t* client;
    user_instance_t*    user;
    yyjson_doc*         val = NULL;
    yyjson_read_err     err_val;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    if (!yyjson_obj_get_string(&workername, yyjson_doc_get_root(val), "worker")) {
        res = yyjson_errormsg("Failed to find worker key");
        goto out;
    }
    if (!strlen(workername)) {
        res = yyjson_errormsg("Zero length worker key");
        goto out;
    }
    tmp = strdupa(workername);
    username = strsep(&tmp, "._");
    user = get_user(sdata, username);
    client_arr = yyjson_mut_arr(doc);

    ck_rlock(&sdata->instance_lock);
    DL_FOREACH2(user->clients, client, user_next) {
        if (strcmp(client->workername, workername))
            continue;
        yyjson_mut_arr_append(client_arr, clientinfo(doc, client));
    }
    ck_runlock(&sdata->instance_lock);

    root = yyjson_mut_pack_val(doc, "{ss,so}", "worker", workername, "clients", client_arr);
    yyjson_mut_doc_set_root(doc, root);
    res = doc;
    doc = NULL;
out:
    if (doc)
        yyjson_mut_doc_free(doc);
    if (val)
        yyjson_doc_free(val);
    free(workername);
    send_api_yyresponse(res, *sockd);
}

/* Return the user masked priority value of the proxy */
static int proxy_prio(const proxy_t* proxy) {
    int prio = proxy->priority & 0x00000000ffffffff;

    return prio;
}

static yyjson_mut_val* yyjson_proxyinfo(yyjson_mut_doc* doc, const proxy_t* proxy) {
    const proxy_t* parent = proxy->parent;

    return yyjson_mut_pack_val(
        doc, "{si,si,si,sf,ss,ss,ss,ss,ss,si,si,si,si,sb,sb,sI,sI,sI,sI,sI,si,sb,sb,si}", "id", proxy->id, "subid",
        proxy->subid, "priority", proxy_prio(parent), "diff", proxy->diff, "baseurl", proxy->baseurl, "url", proxy->url,
        "auth", proxy->auth, "pass", proxy->pass, "enonce1", proxy->enonce1, "enonce1constlen", proxy->enonce1constlen,
        "enonce1varlen", proxy->enonce1varlen, "nonce2len", proxy->nonce2len, "enonce2varlen", proxy->enonce2varlen,
        "subscribed", proxy->subscribed, "notified", proxy->notified, "clients", proxy->clients, "maxclients",
        proxy->max_clients, "bound_clients", proxy->bound_clients, "combined_clients", parent->combined_clients,
        "headroom", proxy->headroom, "subproxy_count", parent->subproxy_count, "dead", proxy->dead, "global",
        proxy->global, "userid", proxy->userid);
}

static void getproxy(sdata_t* sdata, const char* buf, int* sockd) {
    yyjson_mut_doc* res = NULL;
    yyjson_doc*     val = NULL;
    yyjson_read_err err_val;
    int             id, subid = 0;
    proxy_t*        proxy;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    if (!yyjson_obj_get_int(&id, yyjson_doc_get_root(val), "id")) {
        res = yyjson_errormsg("Failed to find id key");
        goto out;
    }
    yyjson_obj_get_int(&subid, yyjson_doc_get_root(val), "subid");
    if (!subid)
        proxy = existing_proxy(sdata, id);
    else
        proxy = existing_subproxy(sdata, id, subid);
    if (!proxy) {
        res = yyjson_errormsg("Failed to find proxy %d:%d", id, subid);
        goto out;
    }
    res = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_doc_set_root(res, yyjson_proxyinfo(res, proxy));
out:
    if (val)
        yyjson_doc_free(val);
    send_api_yyresponse(res, *sockd);
}

static void proxyinfo(sdata_t* sdata, const char* buf, int* sockd) {
    yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val *root, *arr_val = yyjson_mut_arr(doc);
    proxy_t *       proxy, *subproxy;
    yyjson_doc*     val = NULL;
    bool            all = true;
    int             userid = 0;

    if (buf) {
        /* See if there's a userid specified */
        val = yyjson_read(buf, strlen(buf), 0);
        if (yyjson_obj_get_int(&userid, yyjson_doc_get_root(val), "userid"))
            all = false;
    }

    mutex_lock(&sdata->proxy_lock);
    for (proxy = sdata->proxies; proxy; proxy = proxy->hh.next) {
        if (!all && proxy->userid != userid)
            continue;
        for (subproxy = proxy->subproxies; subproxy; subproxy = subproxy->sh.next)
            yyjson_mut_arr_append(arr_val, yyjson_proxyinfo(doc, subproxy));
    }
    mutex_unlock(&sdata->proxy_lock);

    if (val)
        yyjson_doc_free(val);
    root = yyjson_mut_pack_val(doc, "{so}", "proxies", arr_val);
    yyjson_mut_doc_set_root(doc, root);
    send_api_yyresponse(doc, *sockd);
}

static void setproxy(sdata_t* sdata, const char* buf, int* sockd) {
    yyjson_mut_doc* res = NULL;
    yyjson_doc*     val = NULL;
    yyjson_read_err err_val;
    int             id, priority;
    proxy_t*        proxy;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    if (!yyjson_obj_get_int(&id, yyjson_doc_get_root(val), "id")) {
        res = yyjson_errormsg("Failed to find id key");
        goto out;
    }
    if (!yyjson_obj_get_int(&priority, yyjson_doc_get_root(val), "priority")) {
        res = yyjson_errormsg("Failed to find priority key");
        goto out;
    }
    proxy = existing_proxy(sdata, id);
    if (!proxy) {
        res = yyjson_errormsg("Failed to find proxy %d", id);
        goto out;
    }
    if (priority != proxy_prio(proxy))
        set_proxy_prio(sdata, proxy, priority);
    res = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_doc_set_root(res, yyjson_proxyinfo(res, proxy));
out:
    if (val)
        yyjson_doc_free(val);
    send_api_yyresponse(res, *sockd);
}

static void get_poolstats(sdata_t* sdata, int* sockd) {
    pool_stats_t*   stats = &sdata->stats;
    yyjson_mut_doc* doc;

    mutex_lock(&sdata->stats_lock);
    doc = yyjson_mut_pack(
        "{sI,sI,si,si,si,sI,sf,sf,sf,sf,sI,sI,sf,sf,sf,sf,sf,sf,sf}", "start", stats->start_time.tv_sec, "update",
        stats->last_update.tv_sec, "workers", stats->workers + stats->remote_workers, "users",
        stats->users + stats->remote_users, "disconnected", stats->disconnected, "shares", stats->accounted_shares,
        "sps1", stats->sps1, "sps5", stats->sps5, "sps15", stats->sps15, "sps60", stats->sps60, "accepted",
        stats->accounted_diff_shares, "rejected", stats->accounted_rejects, "dsps1", stats->dsps1, "dsps5",
        stats->dsps5, "dsps15", stats->dsps15, "dsps60", stats->dsps60, "dsps360", stats->dsps360, "dsps1440",
        stats->dsps1440, "dsps10080", stats->dsps10080);
    mutex_unlock(&sdata->stats_lock);

    send_api_yyresponse(doc, *sockd);
}

static void get_uptime(sdata_t* sdata, int* sockd) {
    int             uptime = time(NULL) - sdata->stats.start_time.tv_sec;
    yyjson_mut_doc* doc;

    doc = yyjson_mut_pack("{si}", "uptime", uptime);
    send_api_yyresponse(doc, *sockd);
}

static void stratum_loop(proc_instance_t* pi) {
    sdata_t*    sdata = ckpool.sdata;
    unix_msg_t* umsg = NULL;
    int         ret = 0;
    char*       buf;

retry:
    if (umsg) {
        Close(umsg->sockd);
        free(umsg->buf);
        dealloc(umsg);
    }

    do {
        time_t end_t;

        end_t = time(NULL);
        if (end_t - sdata->update_time >= ckpool.update_interval) {
            sdata->update_time = end_t;
            if (!ckpool.proxy) {
                LOGDEBUG("%ds elapsed in strat_loop, updating gbt base", ckpool.update_interval);
                update_base(sdata, GEN_NORMAL);
            } else if (!ckpool.passthrough) {
                LOGDEBUG("%ds elapsed in strat_loop, pinging miners", ckpool.update_interval);
                broadcast_ping(sdata);
            }
        }

        umsg = get_unix_msg(pi);
    } while (!umsg);

    buf = umsg->buf;
    if (buf[0] == '{') {
        yyjson_doc* sdoc = yyjson_read(buf, strlen(buf), YYJSON_READ_STOP_WHEN_DONE);

        /* This is a message for a node */
        if (likely(sdoc)) {
            yyjson_mut_doc* doc = yyjson_doc_mut_copy(sdoc, &ckyyalc);
            smsg_t*         msg = ckzalloc(sizeof(smsg_t));

            yyjson_doc_free(sdoc);
            msg->doc = doc;
            ckmsgq_add(sdata->srecvs, msg);
        }
        goto retry;
    }
    if (cmdmatch(buf, "ping")) {
        LOGDEBUG("Stratifier received ping request");
        send_unix_msg(umsg->sockd, "pong");
        goto retry;
    }
    if (cmdmatch(buf, "stats")) {
        char* msg;

        LOGDEBUG("Stratifier received stats request");
        msg = stratifier_stats(sdata);
        send_unix_msg(umsg->sockd, msg);
        goto retry;
    }
    /* Parse API commands here to return a message to sockd */
    if (cmdmatch(buf, "clients")) {
        getclients(sdata, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "workers")) {
        getworkers(sdata, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "users")) {
        getusers(sdata, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "getclient")) {
        getclient(sdata, buf + 10, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "getuser")) {
        getuser(sdata, buf + 8, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "getworker")) {
        getworker(sdata, buf + 10, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "userclients")) {
        userclients(sdata, buf + 12, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "workerclients")) {
        workerclients(sdata, buf + 14, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "getproxy")) {
        getproxy(sdata, buf + 9, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "setproxy")) {
        setproxy(sdata, buf + 9, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "poolstats")) {
        get_poolstats(sdata, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "proxyinfo")) {
        proxyinfo(sdata, buf + 10, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "ucinfo")) {
        user_clientinfo(sdata, buf + 7, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "uptime")) {
        get_uptime(sdata, &umsg->sockd);
        goto retry;
    }
    if (cmdmatch(buf, "wcinfo")) {
        worker_clientinfo(sdata, buf + 7, &umsg->sockd);
        goto retry;
    }

    LOGDEBUG("Stratifier received request: %s", buf);
    if (cmdmatch(buf, "update")) {
        update_base(sdata, GEN_PRIORITY);
    } else if (cmdmatch(buf, "subscribe")) {
        /* Proxifier has a new subscription */
        update_subscribe(buf);
    } else if (cmdmatch(buf, "notify")) {
        /* Proxifier has a new notify ready */
        update_notify(buf);
    } else if (cmdmatch(buf, "diff")) {
        update_diff(buf);
    } else if (cmdmatch(buf, "dropclient")) {
        int64_t client_id;

        ret = sscanf(buf, "dropclient=%" PRId64, &client_id);
        if (ret < 0)
            LOGDEBUG("Stratifier failed to parse dropclient command: %s", buf);
        else
            drop_client(sdata, client_id);
    } else if (cmdmatch(buf, "reconnclient")) {
        int64_t client_id;

        ret = sscanf(buf, "reconnclient=%" PRId64, &client_id);
        if (ret < 0)
            LOGWARNING("Stratifier failed to parse reconnclient command: %s", buf);
        else
            reconnect_client_id(sdata, client_id);
    } else if (cmdmatch(buf, "dropall")) {
        drop_allclients();
    } else if (cmdmatch(buf, "reconnect")) {
        request_reconnect(sdata, buf);
    } else if (cmdmatch(buf, "deadproxy")) {
        dead_proxy(sdata, buf);
    } else if (cmdmatch(buf, "delproxy")) {
        del_proxy(sdata, buf);
    } else if (cmdmatch(buf, "loglevel")) {
        sscanf(buf, "loglevel=%d", &ckpool.loglevel);
    } else if (cmdmatch(buf, "resetshares")) {
        reset_bestshares(sdata);
    } else
        LOGWARNING("Unhandled stratifier message: %s", buf);
    goto retry;
}

static void* blockupdate(void __maybe_unused* arg) {
    sdata_t* sdata = ckpool.sdata;
    char     hash[68];

    pthread_detach(pthread_self());
    rename_proc("blockupdate");

    while (42) {
        int ret;

        ret = generator_getbest(hash);
        switch (ret) {
            case GETBEST_NOTIFY:
                cksleep_ms(5000);
                break;
            case GETBEST_SUCCESS:
                if (strcmp(hash, sdata->lastswaphash)) {
                    update_base(sdata, GEN_PRIORITY);
                    break;
                }
                fallthrough;
            case GETBEST_FAILED:
            default:
                cksleep_ms(ckpool.blockpoll);
        }
    }
    return NULL;
}

/* Enter holding workbase_lock and client a ref count. */
static void __fill_enonce1data(const workbase_t* wb, stratum_instance_t* client) {
    int constlen = wb->enonce1constlen, varlen = wb->enonce1varlen;

    /* Defence in depth against the fixed enonce1bin buffer overflowing
     * should the derived lengths ever be inconsistent. var also cannot
     * exceed the 8 bytes of enonce1_64 it is copied from. */
    if (unlikely(constlen > (int)sizeof(client->enonce1bin)))
        constlen = sizeof(client->enonce1bin);
    if (unlikely(varlen > 8))
        varlen = 8;
    if (unlikely(constlen + varlen > (int)sizeof(client->enonce1bin)))
        varlen = sizeof(client->enonce1bin) - constlen;

    if (constlen)
        memcpy(client->enonce1bin, wb->enonce1constbin, constlen);
    if (varlen) {
        memcpy(client->enonce1bin + constlen, &client->enonce1_64, varlen);
        __bin2hex(client->enonce1var, &client->enonce1_64, varlen);
    }
    __bin2hex(client->enonce1, client->enonce1bin, constlen + varlen);
}

/* Create a new enonce1 from the 64 bit enonce1_64 value, using only the number
 * of bytes we have to work with when we are proxying with a split nonce2.
 * When the proxy space is less than 32 bits to work with, we look for an
 * unused enonce1 value and reject clients instead if there is no space left.
 * Needs to be entered with client holding a ref count. */
static bool new_enonce1(sdata_t* ckp_sdata, sdata_t* sdata, stratum_instance_t* client) {
    proxy_t* proxy = NULL;
    uint64_t enonce1;

    if (ckpool.proxy) {
        if (!ckp_sdata->proxy)
            return false;

        mutex_lock(&ckp_sdata->proxy_lock);
        proxy = sdata->subproxy;
        client->proxyid = proxy->id;
        client->subproxyid = proxy->subid;
        mutex_unlock(&ckp_sdata->proxy_lock);

        if (proxy->clients >= proxy->max_clients) {
            LOGWARNING("Proxy reached max clients %" PRId64, proxy->max_clients);
            return false;
        }
    }

    /* Still initialising */
    if (unlikely(!sdata->current_workbase))
        return false;

    /* instance_lock protects enonce1_64. Incrementing a little endian 64bit
     * number ensures that no matter how many of the bits we take from the
     * left depending on nonce2 length, we'll always get a changing value
     * for every next client.*/
    ck_wlock(&ckp_sdata->instance_lock);
    enonce1 = le64toh(ckp_sdata->enonce1_64);
    enonce1++;
    client->enonce1_64 = ckp_sdata->enonce1_64 = htole64(enonce1);
    if (proxy) {
        client->proxy = proxy;
        proxy->clients++;
        proxy->bound_clients++;
        proxy->parent->combined_clients++;
    }
    ck_wunlock(&ckp_sdata->instance_lock);

    ck_rlock(&sdata->workbase_lock);
    __fill_enonce1data(sdata->current_workbase, client);
    ck_runlock(&sdata->workbase_lock);

    return true;
}

static void stratum_send_message(sdata_t* sdata, const stratum_instance_t* client, const char* msg);

/* Need to hold sdata->proxy_lock */
static proxy_t* __best_subproxy(proxy_t* proxy) {
    proxy_t *subproxy, *best = NULL, *tmp;
    int64_t  max_headroom;

    proxy->headroom = max_headroom = 0;
    HASH_ITER(sh, proxy->subproxies, subproxy, tmp) {
        int64_t subproxy_headroom;

        if (subproxy->dead)
            continue;
        if (!subproxy->sdata->current_workbase)
            continue;
        subproxy_headroom = proxy_free_slots(subproxy);

        add_headroom(&proxy->headroom, subproxy_headroom);
        if (subproxy_headroom > max_headroom) {
            best = subproxy;
            max_headroom = subproxy_headroom;
        }
        if (best)
            break;
    }
    return best;
}

/* Choose the stratifier data for a new client. Use the main ckp_sdata except
 * in proxy mode where we find a subproxy based on the current proxy with room
 * for more clients. Signal the generator to recruit more subproxies if we are
 * running out of room. */
static sdata_t* select_sdata(sdata_t* ckp_sdata, const int userid) {
    proxy_t *global, *proxy, *tmp, *best = NULL;

    if (!ckpool.proxy || ckpool.passthrough)
        return ckp_sdata;

    /* Proxies are ordered by priority so first available will be the best
     * priority */
    mutex_lock(&ckp_sdata->proxy_lock);
    best = global = ckp_sdata->proxy;

    HASH_ITER(hh, ckp_sdata->proxies, proxy, tmp) {
        if (proxy->userid < userid)
            continue;
        if (proxy->userid > userid)
            break;
        best = __best_subproxy(proxy);
        if (best)
            break;
    }
    mutex_unlock(&ckp_sdata->proxy_lock);

    if (!best) {
        if (!userid)
            LOGWARNING("Temporarily insufficient proxies to accept more clients");
        else
            LOGNOTICE("Temporarily insufficient proxies for userid %d to accept more clients", userid);
        return NULL;
    }
    if (!userid) {
        if (best->id != global->id || current_headroom(ckp_sdata, &proxy) < 2)
            generator_recruit(global->id, 1);
    } else {
        if (best_userproxy_headroom(ckp_sdata, userid) < 2)
            generator_recruit(best->id, 1);
    }
    return best->sdata;
}

static int int_from_sessionid(const char* sessionid) {
    int ret = 0, slen;

    if (!sessionid)
        goto out;
    slen = strlen(sessionid) / 2;
    if (slen < 1 || slen > 4)
        goto out;

    if (!validhex(sessionid))
        goto out;

    sscanf(sessionid, "%x", &ret);
out:
    return ret;
}

static int userid_from_sessionid(sdata_t* sdata, const int session_id) {
    session_t* session;
    int        ret = -1;

    ck_wlock(&sdata->instance_lock);
    HASH_FIND_INT(sdata->disconnected_sessions, &session_id, session);
    if (!session)
        goto out_unlock;
    HASH_DEL(sdata->disconnected_sessions, session);
    sdata->stats.disconnected--;
    ret = session->userid;
    dealloc(session);
out_unlock:
    ck_wunlock(&sdata->instance_lock);

    if (ret != -1)
        LOGINFO("Found old session id %d for userid %d", session_id, ret);
    return ret;
}

static int userid_from_sessionip(sdata_t* sdata, const char* address) {
    session_t *session, *tmp;
    int        ret = -1;

    ck_wlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->disconnected_sessions, session, tmp) {
        if (!strcmp(session->address, address)) {
            ret = session->userid;
            break;
        }
    }
    if (ret == -1)
        goto out_unlock;
    HASH_DEL(sdata->disconnected_sessions, session);
    sdata->stats.disconnected--;
    dealloc(session);
out_unlock:
    ck_wunlock(&sdata->instance_lock);

    if (ret != -1)
        LOGINFO("Found old session address %s for userid %d", address, ret);
    return ret;
}

/* Create a yyjson_mut_doc with just one string as the only entry */
static yyjson_mut_doc* yyjson_string(const char* msg) {
    yyjson_mut_doc* doc = yyjson_mut_pack("s", msg);
    return doc;
}

/* Extranonce1 must be set here. Needs to be entered with client holding a ref
 * count. */
static yyjson_mut_doc*
parse_subscribe(stratum_instance_t* client, const int64_t client_id, yyjson_mut_val* params_val) {
    sdata_t *       sdata, *ckp_sdata = ckpool.sdata;
    int             session_id = 0, userid = -1;
    bool            old_match = false;
    yyjson_mut_doc* doc;
    char            sessionid[12];
    int             arr_size;
    int             n2len;

    if (unlikely(!yyjson_mut_is_arr(params_val))) {
        stratum_send_message(ckp_sdata, client, "Invalid json: params not an array");
        return yyjson_string("params not an array");
    }

    sdata = select_sdata(ckp_sdata, 0);
    if (unlikely(!ckpool.node && (!sdata || !sdata->current_workbase))) {
        LOGWARNING("Failed to provide subscription due to no %s", sdata ? "current workbase" : "sdata");
        stratum_send_message(ckp_sdata, client, "Pool Initialising");
        return yyjson_string("Initialising");
    }

    arr_size = yyjson_mut_arr_size(params_val);
    /* NOTE useragent is NULL prior to this so should not be used in code
     * till after this point */
    if (arr_size > 0) {
        const char* buf;

        buf = yyjson_mut_get_str(yyjson_mut_arr_get(params_val, 0));
        if (buf && strlen(buf))
            client->useragent = strdup(buf);
        else
            client->useragent = ckzalloc(1);  // Set to ""
        if (arr_size > 1) {
            /* This would be the session id for reconnect, it will
             * not work for clients on a proxied connection. */
            buf = yyjson_mut_get_str(yyjson_mut_arr_get(params_val, 1));
            session_id = int_from_sessionid(buf);
            LOGDEBUG("Found old session id %d", session_id);
        }
        if (!ckpool.proxy && session_id && !subclient(client_id)) {
            if ((client->enonce1_64 = disconnected_sessionid_exists(sdata, session_id, client_id))) {
                sprintf(client->enonce1, "%016lx", client->enonce1_64);
                old_match = true;

                ck_rlock(&ckp_sdata->workbase_lock);
                __fill_enonce1data(sdata->current_workbase, client);
                ck_runlock(&ckp_sdata->workbase_lock);
            }
        }
    } else
        client->useragent = ckzalloc(1);

    /* Whitelist cgminer based clients to receive stratum messages */
    if (strcasestr(client->useragent, "gminer"))
        client->messages = true;

    /* We got what we needed */
    if (ckpool.node)
        return NULL;

    if (ckpool.proxy) {
        /* Use the session_id to tell us which user this was.
         * If it's not available, see if there's an IP address
         * which matches a recently disconnected session. */
        if (session_id)
            userid = userid_from_sessionid(ckp_sdata, session_id);
        if (userid == -1)
            userid = userid_from_sessionip(ckp_sdata, client->address);
        if (userid != -1) {
            sdata_t* user_sdata = select_sdata(ckp_sdata, userid);

            /* Only adopt the user's own proxy once it has work. The
             * check at the top of this function was made against the
             * sdata we started with, and a subproxy that has not yet
             * had its first workbase would subscribe the client
             * against a NULL one. Staying put costs the affinity, not
             * the subscription. */
            if (user_sdata && user_sdata->current_workbase)
                sdata = user_sdata;
        }
    }

    client->sdata = sdata;
    if (ckpool.proxy) {
        LOGINFO(
            "Current %d, selecting proxy %d:%d for client %s", ckp_sdata->proxy->id, sdata->subproxy->id,
            sdata->subproxy->subid, client->identity);
    }

    if (!old_match) {
        /* Create a new extranonce1 based on a uint64_t pointer */
        if (!new_enonce1(ckp_sdata, sdata, client)) {
            stratum_send_message(sdata, client, "Pool full of clients");
            client->reject = 3;
            yyjson_string("Proxy full");
        }
        LOGINFO(
            "Set new subscription %s to new enonce1 %lx string %s", client->identity, client->enonce1_64,
            client->enonce1);
    } else {
        LOGINFO(
            "Set new subscription %s to old matched enonce1 %lx string %s", client->identity, client->enonce1_64,
            client->enonce1);
    }

    /* Workbases will exist if sdata->current_workbase is not NULL, but that
     * was tested unlocked and possibly against a different sdata, so make the
     * dereference safe on its own terms rather than on that promise. */
    ck_rlock(&sdata->workbase_lock);
    if (unlikely(!sdata->workbases)) {
        ck_runlock(&sdata->workbase_lock);
        LOGWARNING("Failed to provide subscription due to no workbases");
        stratum_send_message(ckp_sdata, client, "Pool Initialising");
        return yyjson_string("Initialising");
    }
    n2len = sdata->workbases->enonce2varlen;
    sprintf(sessionid, "%08x", client->session_id);
    doc = yyjson_mut_pack("[[[s,s]],s,i]", "mining.notify", sessionid, client->enonce1, n2len);
    ck_runlock(&sdata->workbase_lock);

    client->subscribed = true;

    return doc;
}

static double dsps_from_key(yyjson_val* val, const char* key) {
    char * string, *endptr;
    double ret = 0;

    yyjson_obj_get_string(&string, val, key);
    if (!string)
        return ret;
    ret = strtod(string, &endptr) / nonces;
    if (endptr) {
        switch (endptr[0]) {
            case 'E':
                ret *= (double)1000;
                fallthrough;
            case 'P':
                ret *= (double)1000;
                fallthrough;
            case 'T':
                ret *= (double)1000;
                fallthrough;
            case 'G':
                ret *= (double)1000;
                fallthrough;
            case 'M':
                ret *= (double)1000;
                fallthrough;
            case 'K':
                ret *= (double)1000;
                fallthrough;
            default:
                break;
        }
    }
    free(string);
    return ret;
}

static void decay_client(stratum_instance_t* client, double diff, tv_t* now_t) {
    double tdiff = sane_tdiff(now_t, &client->last_decay);

    /* If we're calling the hashmeter too frequently we'll just end up
     * racing and having inappropriate values, so store up diff and update
     * at most 20 times per second. Use an integer for uadiff to make the
     * update atomic */
    if (tdiff < 0.05) {
        client->uadiff += diff;
        return;
    }
    copy_tv(&client->last_decay, now_t);
    diff += client->uadiff;
    client->uadiff = 0;
    decay_time(&client->dsps1, diff, tdiff, MIN1);
    decay_time(&client->dsps5, diff, tdiff, MIN5);
    decay_time(&client->dsps60, diff, tdiff, HOUR);
    decay_time(&client->dsps1440, diff, tdiff, DAY);
    decay_time(&client->dsps10080, diff, tdiff, WEEK);
}

static void decay_worker(worker_instance_t* worker, double diff, tv_t* now_t) {
    double tdiff = sane_tdiff(now_t, &worker->last_decay);

    if (tdiff < 0.05) {
        worker->uadiff += diff;
        return;
    }
    copy_tv(&worker->last_decay, now_t);
    diff += worker->uadiff;
    worker->uadiff = 0;
    decay_time(&worker->dsps1, diff, tdiff, MIN1);
    decay_time(&worker->dsps5, diff, tdiff, MIN5);
    decay_time(&worker->dsps60, diff, tdiff, HOUR);
    decay_time(&worker->dsps1440, diff, tdiff, DAY);
    decay_time(&worker->dsps10080, diff, tdiff, WEEK);
}

static void decay_user(user_instance_t* user, double diff, tv_t* now_t) {
    double tdiff = sane_tdiff(now_t, &user->last_decay);

    if (tdiff < 0.05) {
        user->uadiff += diff;
        return;
    }
    copy_tv(&user->last_decay, now_t);
    diff += user->uadiff;
    user->uadiff = 0;
    decay_time(&user->dsps1, diff, tdiff, MIN1);
    decay_time(&user->dsps5, diff, tdiff, MIN5);
    decay_time(&user->dsps60, diff, tdiff, HOUR);
    decay_time(&user->dsps1440, diff, tdiff, DAY);
    decay_time(&user->dsps10080, diff, tdiff, WEEK);
}

static user_instance_t* get_create_user(sdata_t* sdata, const char* username, bool* new_user);
static worker_instance_t*
get_create_worker(sdata_t* sdata, user_instance_t* user, const char* workername, bool* new_worker);

/* Load the statistics of and create all known users at startup */
static void read_userstats(sdata_t* sdata, int tvsec_diff) {
    char             dnam[256], s[4096], *username, *buf;
    int              ret, users = 0, workers = 0;
    user_instance_t* user;
    struct dirent*   dir;
    struct stat      fdbuf;
    yyjson_val*      val;
    yyjson_doc*      doc;
    bool             new_user;
    FILE*            fp;
    tv_t             now;
    DIR*             d;
    int              fd;

    snprintf(dnam, 255, "%susers", ckpool.logdir);
    d = opendir(dnam);
    if (!d) {
        LOGNOTICE("No user directory found");
        return;
    }

    tv_time(&now);

    while ((dir = readdir(d)) != NULL) {
        yyjson_val *    worker_array, *arr_val;
        yyjson_arr_iter iter;
        int64_t         authorised;
        int             lastshare;

        username = basename(dir->d_name);
        if (!strcmp(username, "/") || !strcmp(username, ".") || !strcmp(username, ".."))
            continue;

        new_user = false;
        user = get_create_user(sdata, username, &new_user);
        if (unlikely(!new_user)) {
            /* All users should be new at this stage */
            LOGWARNING("Duplicate user in read_userstats %s", username);
            continue;
        }
        users++;
        snprintf(s, 4095, "%s/%s", dnam, username);
        fp = fopen(s, "re");
        if (unlikely(!fp)) {
            /* Permission problems should be the only reason this happens */
            LOGWARNING("Failed to load user %s logfile to read", username);
            continue;
        }
        fd = fileno(fp);
        if (unlikely(fstat(fd, &fdbuf))) {
            LOGERR("Failed to fstat user %s logfile", username);
            fclose(fp);
            continue;
        }
        /* We don't know how big the logfile will be so allocate
         * according to file size */
        buf = ckzalloc(fdbuf.st_size + 1);
        ret = fread(buf, 1, fdbuf.st_size, fp);
        fclose(fp);
        if (ret < 1) {
            LOGNOTICE("Failed to read user %s logfile", username);
            dealloc(buf);
            continue;
        }
        doc = yyjson_read(buf, strlen(buf), 0);
        if (!doc) {
            LOGNOTICE("Failed to json decode user %s logfile: %s", username, buf);
            dealloc(buf);
            continue;
        }
        val = yyjson_doc_get_root(doc);
        dealloc(buf);

        copy_tv(&user->last_share, &now);
        copy_tv(&user->last_decay, &now);
        user->dsps1 = dsps_from_key(val, "hashrate1m");
        user->dsps5 = dsps_from_key(val, "hashrate5m");
        user->dsps60 = dsps_from_key(val, "hashrate1hr");
        user->dsps1440 = dsps_from_key(val, "hashrate1d");
        user->dsps10080 = dsps_from_key(val, "hashrate7d");
        yyjson_obj_get_int(&lastshare, val, "lastshare");
        user->last_share.tv_sec = lastshare;
        yyjson_obj_get_int64(&user->shares, val, "shares");
        yyjson_obj_get_double(&user->best_diff, val, "bestshare");
        yyjson_obj_get_int64(&user->best_ever, val, "bestever");
        yyjson_obj_get_int64(&authorised, val, "authorised");
        user->auth_time = authorised;
        if (user->best_diff > user->best_ever)
            user->best_ever = user->best_diff;
        LOGINFO(
            "Successfully read user %s stats %f %f %f %f %f %f %ld %ld", user->username, user->dsps1, user->dsps5,
            user->dsps60, user->dsps1440, user->dsps10080, user->best_diff, user->best_ever, user->auth_time);
        if (tvsec_diff > 60)
            decay_user(user, 0, &now);

        worker_array = yyjson_obj_get(val, "worker");
        yyjson_arr_iter_init(worker_array, &iter);
        while ((arr_val = yyjson_arr_iter_next(&iter)) != NULL) {
            const char*        workername = yyjson_get_str(yyjson_obj_get(arr_val, "workername"));
            worker_instance_t* worker;
            bool               new_worker = false;

            if (unlikely(!workername || !strlen(workername)) || !strstr(workername, username)) {
                LOGWARNING("Invalid workername in read_userstats %s", workername);
                continue;
            }
            worker = get_create_worker(sdata, user, workername, &new_worker);
            if (unlikely(!new_worker)) {
                LOGWARNING("Duplicate worker in read_userstats %s", workername);
                continue;
            }
            workers++;
            copy_tv(&worker->last_decay, &now);
            worker->dsps1 = dsps_from_key(arr_val, "hashrate1m");
            worker->dsps5 = dsps_from_key(arr_val, "hashrate5m");
            worker->dsps60 = dsps_from_key(arr_val, "hashrate1hr");
            worker->dsps1440 = dsps_from_key(arr_val, "hashrate1d");
            worker->dsps10080 = dsps_from_key(arr_val, "hashrate7d");
            yyjson_obj_get_int(&lastshare, arr_val, "lastshare");
            worker->last_share.tv_sec = lastshare;
            yyjson_obj_get_double(&worker->best_diff, arr_val, "bestshare");
            yyjson_obj_get_int64(&worker->best_ever, arr_val, "bestever");
            if (worker->best_diff > worker->best_ever)
                worker->best_ever = worker->best_diff;
            yyjson_obj_get_int64(&worker->shares, arr_val, "shares");
            LOGINFO(
                "Successfully read worker %s stats %f %f %f %f %f %ld", worker->workername, worker->dsps1,
                worker->dsps5, worker->dsps60, worker->dsps1440, worker->best_diff, worker->best_ever);
            if (tvsec_diff > 60)
                decay_worker(worker, 0, &now);
        }
        yyjson_doc_free(doc);
    }
    closedir(d);

    if (likely(users))
        LOGWARNING("Loaded %d users and %d workers", users, workers);
}

#define DEFAULT_AUTH_BACKOFF (3) /* Set initial backoff to 3 seconds */

static user_instance_t* __create_user(sdata_t* sdata, const char* username) {
    user_instance_t* user = ckzalloc(sizeof(user_instance_t));

    user->auth_backoff = DEFAULT_AUTH_BACKOFF;
    strcpy(user->username, username);
    user->id = ++sdata->user_instance_id;
    HASH_ADD_STR(sdata->user_instances, username, user);
    return user;
}

/* Find user by username or create one if it doesn't already exist. When
 * limited is set, refuse to create a new user once maxusers is reached and
 * return NULL instead. Every new user is a permanent heap record plus a file
 * under logs/users/, so paths driven by unauthenticated remote input must be
 * limited or a client rotating usernames can exhaust memory and disk. */
static user_instance_t* __get_create_user(sdata_t* sdata, const char* username, bool* new_user, const bool limited) {
    char             truncated[128];
    user_instance_t* user;
    bool             full = false;

    /* Usernames are stored in a fixed 128 byte array so truncate any that
     * are too long to fit, keeping lookup and creation consistent */
    if (unlikely(strlen(username) >= sizeof(truncated))) {
        strncpy(truncated, username, sizeof(truncated) - 1);
        truncated[sizeof(truncated) - 1] = '\0';
        username = truncated;
    }

    ck_wlock(&sdata->instance_lock);
    HASH_FIND_STR(sdata->user_instances, username, user);
    if (unlikely(!user)) {
        if (unlikely(limited && ckpool.maxusers && HASH_COUNT(sdata->user_instances) >= (unsigned int)ckpool.maxusers))
            full = true;
        else {
            user = __create_user(sdata, username);
            *new_user = true;
        }
    }
    ck_wunlock(&sdata->instance_lock);

    if (unlikely(full)) {
        LOGWARNING("Refusing to create user %s with maxusers %d reached", username, ckpool.maxusers);
    }
    return user;
}

static user_instance_t* get_create_user(sdata_t* sdata, const char* username, bool* new_user) {
    return __get_create_user(sdata, username, new_user, false);
}

static user_instance_t* get_user(sdata_t* sdata, const char* username) {
    bool dummy = false;

    return get_create_user(sdata, username, &dummy);
}

static worker_instance_t* __create_worker(user_instance_t* user, const char* workername) {
    worker_instance_t* worker = ckzalloc(sizeof(worker_instance_t));

    worker->workername = strdup(workername);
    worker->user_instance = user;
    DL_APPEND(user->worker_instances, worker);
    worker->start_time = time(NULL);
    return worker;
}

static worker_instance_t* __get_worker(user_instance_t* user, const char* workername) {
    worker_instance_t *worker = NULL, *tmp;

    DL_FOREACH(user->worker_instances, tmp) {
        if (!safecmp(workername, tmp->workername)) {
            worker = tmp;
            break;
        }
    }
    return worker;
}

/* Find worker amongst a user's workers by workername or create one if it
 * doesn't yet exist. */
static worker_instance_t*
get_create_worker(sdata_t* sdata, user_instance_t* user, const char* workername, bool* new_worker) {
    worker_instance_t* worker;

    ck_wlock(&sdata->instance_lock);
    worker = __get_worker(user, workername);
    if (!worker) {
        worker = __create_worker(user, workername);
        *new_worker = true;
    }
    ck_wunlock(&sdata->instance_lock);

    return worker;
}

static worker_instance_t* get_worker(sdata_t* sdata, user_instance_t* user, const char* workername) {
    bool dummy = false;

    return get_create_worker(sdata, user, workername, &dummy);
}

/* This simply strips off the first part of the workername and matches it to a
 * user or creates a new one. Needs to be entered with client holding a ref
 * count. */
static user_instance_t* generate_user(stratum_instance_t* client, const char* workername) {
    char *             base_username = strdupa(workername), *username;
    const char *       lightning_id = NULL, *lightning_domain = NULL;
    bool               new_user = false, new_worker = false, lnurl_changed = false;
    sdata_t*           sdata = ckpool.sdata;
    char *             combined_id = NULL, *lnurl = NULL;
    worker_instance_t* worker;
    user_instance_t*   user;
    int                len;

    /* A worker name may carry a lightning address as
     * btcaddress.worker.lightningid@domain. Every part is carved out of
     * this one stack copy, so there is nothing to free on the way out. */
    username = strsep(&base_username, ".");
    if (base_username) {
        char* rest = base_username;

        strsep(&rest, "."); /* worker suffix, not needed here */
        if (rest && *rest) {
            char* id = strsep(&rest, "@");

            if (rest && *rest) {
                size_t dlen = strlen(rest);

                /* Tolerate a trailing dot on the domain */
                if (dlen && rest[dlen - 1] == '.')
                    rest[dlen - 1] = '\0';
                if (*rest && id && *id) {
                    lightning_id = id;
                    lightning_domain = rest;
                }
            }
        }
    }
    if (!username || !strlen(username))
        username = strdupa(workername);
    len = strlen(username);
    if (unlikely(len > 127))
        username[127] = '\0';

    /* Limited: this is reached directly from mining.authorize. */
    user = __get_create_user(sdata, username, &new_user, true);
    if (unlikely(!user))
        return NULL;
    worker = get_create_worker(sdata, user, workername, &new_worker);

    if (lightning_id && lightning_domain)
        ASPRINTF(&combined_id, "%s@%s", lightning_id, lightning_domain);

    /* Create one worker instance for combined data from workers of the
     * same name */
    ck_wlock(&sdata->instance_lock);
    client->user_instance = user;
    client->worker_instance = worker;
    DL_APPEND2(user->clients, client, user_prev, user_next);
    __inc_worker(sdata, user, worker);
    /* Store the lightning address under the same lock its readers use, so
     * the value being replaced can be freed here. */
    if (combined_id && (!user->secondaryuserid || strcmp(user->secondaryuserid, combined_id))) {
        free(user->secondaryuserid);
        user->secondaryuserid = combined_id;
        combined_id = NULL;
        lnurl_changed = true;
        lnurl = strdup(user->secondaryuserid);
    }
    ck_wunlock(&sdata->instance_lock);
    /* Non-NULL only if the stored value was already identical */
    free(combined_id);

    if (!ckpool.proxy && (new_user || !user->btcaddress)) {
        /* Is this a btc address based username? */
        if (generator_checkaddr(username, &user->script, &user->segwit)) {
            user->btcaddress = true;
            user->txnlen = address_to_txn(user->txnbin, username, user->script, user->segwit);
        }
    }
    if (ckpool.logshares_db && user->btcaddress && (new_user || lnurl_changed))
        db_insert_user(sdata, user, lnurl, workername);
    if (new_user) {
        LOGNOTICE(
            "Added new user %s%s%s%s", username, user->btcaddress ? " as address based registration" : "",
            lnurl ? " with lightning address " : "", lnurl ?: "");
    }
    free(lnurl);

    return user;
}

static void check_global_user(user_instance_t* user, stratum_instance_t* client) {
    sdata_t* sdata = ckpool.sdata;
    proxy_t* proxy = best_proxy(sdata);
    int      proxyid = proxy->id;
    char     buf[256];

    sprintf(buf, "globaluser=%d:%d:%" PRId64 ":%s,%s", proxyid, user->id, client->id, user->username, client->password);
    send_proc(ckpool.generator, buf);
}

/* Manage the response to auth, client must hold ref */
static void client_auth(stratum_instance_t* client, user_instance_t* user, const bool ret) {
    if (ret) {
        client->authorised = ret;
        user->authorised = ret;
        if (ckpool.proxy) {
            LOGNOTICE(
                "Authorised client %s to proxy %d:%d, worker %s as user %s", client->identity, client->proxyid,
                client->subproxyid, client->workername, user->username);
            if (ckpool.userproxy)
                check_global_user(user, client);
        } else {
            LOGNOTICE(
                "Authorised client %s %s worker %s as user %s", client->identity, client->address, client->workername,
                user->username);
        }
        user->failed_authtime = 0;
        user->auth_backoff = DEFAULT_AUTH_BACKOFF; /* Reset auth backoff time */
        user->throttled = false;
        if (!user->auth_time)
            user->auth_time = time(NULL);
    } else {
        if (user->throttled) {
            LOGINFO(
                "Client %s %s worker %s failed to authorise as throttled user %s", client->identity, client->address,
                client->workername, user->username);
        } else {
            LOGNOTICE(
                "Client %s %s worker %s failed to authorise as user %s", client->identity, client->address,
                client->workername, user->username);
        }
        user->failed_authtime = time(NULL);
        user->auth_backoff <<= 1;
        /* Cap backoff time to 10 mins */
        if (user->auth_backoff > 600)
            user->auth_backoff = 600;
        client->reject = 3;
    }
    /* We can set this outside of lock safely */
    client->authorising = false;
}

static yyjson_mut_doc* __user_notify(const workbase_t* wb, const user_instance_t* user, const bool clean);

static void
update_solo_client(sdata_t* sdata, workbase_t* wb, const int64_t client_id, user_instance_t* user_instance) {
    yyjson_mut_doc* doc = __user_notify(wb, user_instance, true);

    stratum_add_yysend(sdata, doc, client_id, SM_UPDATE);
}

/* Needs to be entered with client holding a ref count. */
static bool parse_authorise(stratum_instance_t* client, yyjson_mut_val* params_val, yyjson_mut_doc** err_doc) {
    user_instance_t* user;
    const char *     buf, *pass;
    bool             ret = false;
    int              arr_size;
    ts_t             now;

    if (unlikely(!yyjson_mut_is_arr(params_val))) {
        *err_doc = yyjson_string("params not an array");
        goto out;
    }
    arr_size = yyjson_mut_arr_size(params_val);
    if (unlikely(arr_size < 1)) {
        *err_doc = yyjson_string("params missing array entries");
        goto out;
    }
    if (unlikely(!client->useragent)) {
        *err_doc = yyjson_string("Failed subscription");
        goto out;
    }
    buf = yyjson_mut_get_str(yyjson_mut_arr_get(params_val, 0));
    if (!buf) {
        *err_doc = yyjson_string("Invalid workername parameter");
        goto out;
    }
    if (!strlen(buf)) {
        *err_doc = yyjson_string("Empty workername parameter");
        goto out;
    }
    if (!memcmp(buf, ".", 1) || !memcmp(buf, "_", 1)) {
        *err_doc = yyjson_string("Empty username parameter");
        goto out;
    }
    if (strchr(buf, '/')) {
        *err_doc = yyjson_string("Invalid character in username");
        goto out;
    }
    pass = yyjson_mut_get_str(yyjson_mut_arr_get(params_val, 1));
    user = generate_user(client, buf);
    if (unlikely(!user)) {
        *err_doc = yyjson_string("Pool user limit reached");
        client->dropped = true;
        goto out;
    }
    client->user_id = user->id;
    ts_realtime(&now);
    client->start_time = now.tv_sec;
    /* NOTE workername is NULL prior to this so should not be used in code
     * till after this point */
    client->workername = strdup(buf);
    if (pass)
        client->password = strndup(pass, 64);
    else
        client->password = strdup("");
    if (user->failed_authtime) {
        time_t now_t = time(NULL);

        if (now_t < user->failed_authtime + user->auth_backoff) {
            if (!user->throttled) {
                user->throttled = true;
                LOGNOTICE(
                    "Client %s %s worker %s rate limited due to failed auth attempts", client->identity,
                    client->address, buf);
            } else {
                LOGINFO(
                    "Client %s %s worker %s rate limited due to failed auth attempts", client->identity,
                    client->address, buf);
            }
            client->dropped = true;
            goto out;
        }
    }
    if (!ckpool.btcsolo || client->user_instance->btcaddress)
        ret = true;

    /* We do the preauth etc. in remote mode, and leave final auth to
     * upstream pool to complete. */
    if (!ckpool.remote || ckpool.btcsolo)
        client_auth(client, user, ret);
out:
    if (ckpool.btcsolo && ret && !client->remote) {
        sdata_t*    sdata = ckpool.sdata;
        workbase_t* wb;

        /* To avoid grabbing recursive lock. current_workbase is NULL
         * until the first template arrives (bitcoind startup/IBD); the
         * node path reaches here without the spin-wait that protects
         * parse_instance_msg, so the NULL check is required. */
        ck_wlock(&sdata->workbase_lock);
        wb = sdata->current_workbase;
        if (likely(wb))
            wb->readcount++;
        ck_wunlock(&sdata->workbase_lock);

        if (wb) {
            ck_wlock(&sdata->instance_lock);
            __generate_userwb(sdata, wb, user);
            ck_wunlock(&sdata->instance_lock);

            update_solo_client(sdata, wb, client->id, user);

            ck_wlock(&sdata->workbase_lock);
            wb->readcount--;
            ck_wunlock(&sdata->workbase_lock);
        }

        /* Authorised either way; solo work arrives with the first
         * template if none exists yet. */
        stratum_send_diff(sdata, client);
    }
    return ret;
}

/* Needs to be entered with client holding a ref count. */
static void stratum_send_diff(sdata_t* sdata, const stratum_instance_t* client) {
    yyjson_mut_doc* doc;

#ifdef HAVE_SV2
    /* Virtual SV2 sessions have no connector socket — send SetTarget */
    if (client->sv2) {
        sv2_strat_set_instance_diff(client->id, (double)client->diff);
        return;
    }
#endif
    doc = yyjson_mut_pack("{s[I]snss}", "params", client->diff, "id", "method", "mining.set_difficulty");
    stratum_add_yysend(sdata, doc, client->id, SM_DIFF);
}

/* Needs to be entered with client holding a ref count. */
static void stratum_send_message(sdata_t* sdata, const stratum_instance_t* client, const char* msg) {
    yyjson_mut_doc* doc;

    /* Only send messages to whitelisted clients */
    if (!client->messages)
        return;
    doc = yyjson_mut_pack("{snsss[s]}", "id", "method", "client.show_message", "params", msg);
    stratum_add_yysend(sdata, doc, client->id, SM_MSG);
}

static double time_bias(const double tdiff, const double period) {
    double dexp = tdiff / period;

    /* Sanity check to prevent silly numbers for double accuracy **/
    if (unlikely(dexp > 36))
        dexp = 36;
    return 1.0 - 1.0 / exp(dexp);
}

/* Needs to be entered with client holding a ref count. */
static void add_submit(stratum_instance_t* client, const double diff, const bool valid, const bool submit) {
    sdata_t *          ckp_sdata = ckpool.sdata, *sdata = client->sdata;
    worker_instance_t* worker = client->worker_instance;
    double             tdiff, bdiff, dsps, drr, network_diff, bias;
    user_instance_t*   user = client->user_instance;
    int64_t            next_blockid, optimal, mindiff;
    tv_t               now_t;

    mutex_lock(&ckp_sdata->uastats_lock);
    if (valid) {
        ckp_sdata->stats.unaccounted_shares++;
        ckp_sdata->stats.unaccounted_diff_shares += diff;
    } else
        ckp_sdata->stats.unaccounted_rejects += diff;
    mutex_unlock(&ckp_sdata->uastats_lock);

    /* Count only accepted and stale rejects in diff calculation. */
    if (valid) {
        worker->shares += diff;
        user->shares += diff;
    } else if (!submit)
        return;

    tv_time(&now_t);

    ck_rlock(&sdata->workbase_lock);
    next_blockid = sdata->workbase_id;
    if (ckpool.proxy)
        network_diff = sdata->current_workbase->diff;
    else
        network_diff = sdata->current_workbase->network_diff;
    ck_runlock(&sdata->workbase_lock);

    if (unlikely(!client->first_share.tv_sec)) {
        copy_tv(&client->first_share, &now_t);
        copy_tv(&client->ldc, &now_t);
    }

    decay_client(client, diff, &now_t);
    copy_tv(&client->last_share, &now_t);

    decay_worker(worker, diff, &now_t);
    copy_tv(&worker->last_share, &now_t);
    worker->idle = false;

    decay_user(user, diff, &now_t);
    copy_tv(&user->last_share, &now_t);
    client->idle = false;

    /* Once we've updated user/client statistics in node mode, we can't
     * alter diff ourselves. */
    if (ckpool.node)
        return;

    client->ssdc++;
    bdiff = sane_tdiff(&now_t, &client->first_share);
    tdiff = sane_tdiff(&now_t, &client->ldc);

    /* Check the difficulty every 240 seconds or as many shares as we
     * should have had in that time, whichever comes first. */
    if (client->ssdc < 72 && tdiff < 240)
        return;

    if (diff != client->diff) {
        client->ssdc = 0;
        return;
    }

    /* Diff rate ratio.
     * If shares are coming in fast, calculate based on
     * the one minute rolling average for quick diff adjustment, otherwise
     * use the 5 minute rolling average */
    if (client->ssdc >= 72) {
        bias = time_bias(bdiff, 60);
        dsps = client->dsps1 / bias;
    } else {
        bias = time_bias(bdiff, 300);
        dsps = client->dsps5 / bias;
    }
    drr = dsps / (double)client->diff;

    /* Optimal rate product is 0.3, allow some hysteresis. */
    if (drr > 0.15 && drr < 0.4)
        return;

    /* Client suggest diff overrides worker mindiff */
    if (client->suggest_diff)
        mindiff = client->suggest_diff;
    else
        mindiff = worker->mindiff;
    /* Allow slightly lower diffs when users choose their own mindiff */
    if (mindiff) {
        if (drr < 0.5)
            return;
        optimal = lround(dsps * 2.4);
    } else
        optimal = lround(dsps * 3.33);

    /* Clamp to mindiff ~ network_diff */

    /* Set to higher of pool mindiff and optimal */
    optimal = MAX(optimal, ckpool.mindiff);

    /* Set to higher of optimal and user chosen diff */
    optimal = MAX(optimal, mindiff);

    /* Set to lower of optimal and pool maxdiff */
    if (ckpool.maxdiff)
        optimal = MIN(optimal, ckpool.maxdiff);

    /* Set to lower of optimal and network_diff */
    optimal = MIN(optimal, network_diff);

    if (unlikely(optimal < 1))
        return;

    if (client->diff == optimal)
        return;

    /* If this is the first share in a change, reset the last diff change
     * to make sure the client hasn't just fallen back after a leave of
     * absence */
    if (optimal < client->diff && client->ssdc == 1) {
        copy_tv(&client->ldc, &now_t);
        return;
    }

    client->ssdc = 0;

    LOGINFO(
        "Client %s biased dsps %.2f dsps %.2f drr %.2f adjust diff from %" PRId64 " to: %" PRId64 " ", client->identity,
        dsps, client->dsps5, drr, client->diff, optimal);

    copy_tv(&client->ldc, &now_t);
    client->diff_change_job_id = next_blockid;
    client->old_diff = client->diff;
    client->diff = optimal;
    stratum_send_diff(sdata, client);
}

static void
downstream_block(sdata_t* sdata, yyjson_mut_doc* val, const int cblen, const char* coinbase, const uchar* data) {
    yyjson_mut_doc* block_doc = yyjson_mut_doc_mut_copy(val, &ckyyalc);
    yyjson_mut_val* block_val = yyjson_mut_doc_get_root(block_doc);

    /* Strip unnecessary fields and add extra fields needed */
    yyjson_mut_obj_add_strcpy(block_doc, block_val, "method", stratum_msgs[SM_BLOCK]);
    add_remote_blockdata(block_doc, block_val, cblen, coinbase, data);
    downstream_yydoc(sdata, block_doc, 0, SSEND_PREPEND);
    yyjson_mut_doc_free(block_doc);
}

#ifdef HAVE_CAPNP
/* Submit a solved IPC-templated block via the mining interface. The coinbase
 * must be witness-serialised (marker+flag + coinbase witness reserved value);
 * the server reconstructs and validates the full block. Returns true if
 * accepted. */
static bool ipc_submit_block(
    const workbase_t* wb,
    const uchar*      data,
    const char*       coinbase,
    const int         cblen,
    const uint32_t    ntime32) {
    unsigned char* wcb;
    uint32_t       version, nonce;
    int            wl, accepted = 0;

    /* A real coinbase is well over 60 bytes; guard the tx version(4) +
     * body(cblen-8) + locktime(4) arithmetic below against a degenerate
     * length. */
    if (unlikely(!wb->tmpl || cblen < 12))
        return false;
    memcpy(&version, data, 4);
    memcpy(&nonce, data + 76, 4);

    /* BIP141 only permits the coinbase to carry a witness when the block
     * has a witness commitment; without one the coinbase must be serialised
     * legacy or the block is rejected. generate_coinbase only emits the
     * commitment when insert_witness is set, so mirror that here instead of
     * always segwit-serialising. Any block containing a single segwit
     * transaction forces a commitment, so this is only reached on a
     * witness-free block, in practice regtest. The coinbase is already in
     * legacy form (version || vin || vout || locktime) so submit it as is. */
    if (unlikely(!wb->insert_witness)) {
        if (mining_ipc_submit_solution(
                wb->tmpl, version, ntime32, nonce, (const unsigned char*)coinbase, (size_t)cblen, &accepted))
            accepted = 0;
        return accepted;
    }

    wcb = ckalloc(cblen + 40);
    memcpy(wcb, coinbase, 4); /* tx version */
    wl = 4;
    wcb[wl++] = 0x00;
    wcb[wl++] = 0x01;                          /* segwit marker + flag */
    memcpy(wcb + wl, coinbase + 4, cblen - 8); /* inputs + outputs */
    wl += cblen - 8;
    wcb[wl++] = 0x01; /* 1 witness stack item */
    wcb[wl++] = 0x20; /* 32 byte reserved value */
    memcpy(wcb + wl, wb->coinbase_witness, 32);
    wl += 32;
    memcpy(wcb + wl, coinbase + cblen - 4, 4); /* lock time */
    wl += 4;

    if (mining_ipc_submit_solution(wb->tmpl, version, ntime32, nonce, wcb, wl, &accepted))
        accepted = 0;
    free(wcb);
    return accepted;
}
#endif

/* We should already be holding a wb readcount. Needs to be entered with
 * client holding a ref count. */
static void test_blocksolve(
    const stratum_instance_t* client,
    const workbase_t*         wb,
    const uchar*              data,
    const uchar*              hash,
    const double              diff,
    const char*               coinbase,
    int                       cblen,
    const char*               nonce2,
    const char*               nonce,
    const uint32_t            ntime32,
    const uint32_t            version_mask,
    const bool                stale) {
    char            blockhash[68], cdfield[64], *gbt_block;
    sdata_t*        sdata = client->sdata;
    double          network_diff;
    yyjson_mut_doc* doc;
    yyjson_mut_val* val;
    uchar           flip32[32];
    ts_t            ts_now;
    bool            ret;

    /* Submit anything over 99.9% of the diff in case of rounding errors */
    network_diff = sdata->current_workbase->network_diff * 0.999;
    if (likely(diff < network_diff))
        return;

    /* Never submit a block whose miner supplied ntime is outside the window
     * bitcoind will accept - it would be rejected and the reward lost. The
     * callers range check ntime as well but only after this is reached, and
     * only to report the share level error, so gate it here where every
     * submission path converges. */
    if (unlikely(ntime32 < wb->ntime32 || ntime32 > wb->ntime32 + 7000)) {
        LOGWARNING("Not submitting block solve with out of range ntime %u vs workbase %u !", ntime32, wb->ntime32);
        return;
    }

    LOGWARNING("Possible %sblock solve diff %lf !", stale ? "stale share " : "", diff);
    /* Can't submit a block in proxy mode without the transactions */
    if (!ckpool.node && wb->proxy)
        return;

    ts_realtime(&ts_now);
    sprintf(cdfield, "%lu,%lu", ts_now.tv_sec, ts_now.tv_nsec);

#ifdef HAVE_CAPNP
    /* IPC blocks are submitted via the mining interface, not by assembling
     * the block hex, so just derive the block hash here. */
    if (wb->ipc) {
        flip_32(flip32, hash);
        __bin2hex(blockhash, flip32, 32);
        gbt_block = NULL;
    } else
#endif
        gbt_block = process_block(wb, coinbase, cblen, data, hash, flip32, blockhash);
    send_node_block(
        sdata, client->enonce1, nonce, nonce2, ntime32, version_mask, wb->id, diff, client->id, coinbase, cblen, data);

    doc = yyjson_mut_doc_new(&ckyyalc);
    val = yyjson_mut_obj(doc);
    yyjson_mut_doc_set_root(doc, val);
    yyjson_mut_obj_add_int(doc, val, "height", wb->height);
    yyjson_mut_obj_add_strcpy(doc, val, "blockhash", blockhash);
    yyjson_mut_obj_add_strcpy(doc, val, "confirmed", "n");
    yyjson_mut_obj_add_int(doc, val, "workinfoid", wb->id);
    yyjson_mut_obj_add_strcpy(doc, val, "username", client->user_instance->username);
    yyjson_mut_obj_add_strcpy(doc, val, "workername", client->workername);
    /* Which stratum the solving miner was speaking. Travels with the block
     * data so a trusted remote's solve is attributed the same way. */
    yyjson_mut_obj_add_strcpy(doc, val, "protocol", client->sv2 ? "SV2" : "SV1");
    if (ckpool.remote)
        yyjson_mut_obj_add_int(doc, val, "clientid", client->virtualid);
    else
        yyjson_mut_obj_add_int(doc, val, "clientid", client->id);
    yyjson_mut_obj_add_strcpy(doc, val, "enonce1", client->enonce1);
    yyjson_mut_obj_add_strcpy(doc, val, "nonce2", nonce2);
    yyjson_mut_obj_add_strcpy(doc, val, "nonce", nonce);
    yyjson_mut_obj_add_uint(doc, val, "ntime32", ntime32);
    yyjson_mut_obj_add_uint(doc, val, "version_mask", version_mask);
    yyjson_mut_obj_add_int(doc, val, "reward", wb->coinbasevalue);
    yyjson_mut_obj_add_real(doc, val, "diff", diff);
    yyjson_mut_obj_add_strcpy(doc, val, "createdate", cdfield);
    yyjson_mut_obj_add_strcpy(doc, val, "createby", "code");
    yyjson_mut_obj_add_strcpy(doc, val, "createcode", __func__);
    yyjson_mut_obj_add_strcpy(doc, val, "createinet", ckpool.serverurl[client->server]);

    if (ckpool.remote) {
        add_remote_blockdata(doc, val, cblen, coinbase, data);
        upstream_yydoc_msgtype(doc, SM_BLOCK);
    } else {
        downstream_block(sdata, doc, cblen, coinbase, data);
    }

    /* Submit block locally after sending it to remote locations avoiding
     * the delay of local verification */
#ifdef HAVE_CAPNP
    if (wb->ipc)
        ret = ipc_submit_block(wb, data, coinbase, cblen, ntime32);
    else
#endif
        ret = local_block_submit(gbt_block, flip32, wb->height);
    if (ret)
        block_solve(doc);
    else
        block_reject(doc);

    yyjson_mut_doc_free(doc);
}

/* Entered with instance_lock held */
static inline uchar* __user_coinb2(const stratum_instance_t* client, const workbase_t* wb, int* cb2len) {
    struct userwb* userwb;
    int64_t        id;

    if (!ckpool.btcsolo)
        goto out_nouserwb;

    id = wb->id;
    HASH_FIND_I64(client->user_instance->userwbs, &id, userwb);
    if (unlikely(!userwb))
        goto out_nouserwb;
    *cb2len = userwb->coinb2len;
    return userwb->coinb2bin;

out_nouserwb:
    *cb2len = wb->coinb2len;
    return wb->coinb2bin;
}

/* Needs to be entered with workbase readcount and client holding a ref count.
 * If full_version is true, version_field replaces the header nVersion entirely
 * (SV2 SubmitShares). Otherwise version_field is a BIP320 mask OR'd in (SV1). */
static double submission_diff(
    sdata_t*                  sdata,
    const stratum_instance_t* client,
    const workbase_t*         wb,
    const char*               nonce2,
    const uint32_t            ntime32,
    uint32_t                  version_field,
    const bool                full_version,
    const char*               nonce,
    uchar*                    hash,
    const bool                stale) {
    unsigned char merkle_root[32], merkle_sha[64];
    uint32_t *    data32, *swap32, benonce32;
    char *        coinbase, data[80];
    uchar         swap[80], hash1[32];
    int           cblen, i, cb2len;
    uchar*        coinb2bin;
    double        ret;

    /* The coinbase is coinb1 + extranonce (const + var) + extranonce2 +
     * coinb2, where coinb2 may be the per-user generation in btcsolo mode.
     * The user coinb2 is only valid while holding the instance lock so
     * assemble the whole coinbase into a buffer sized to its actual
     * content while the lock is held. */
    ck_rlock(&sdata->instance_lock);
    coinb2bin = __user_coinb2(client, wb, &cb2len);
    coinbase = alloca(wb->coinb1len + wb->enonce1constlen + wb->enonce1varlen + wb->enonce2varlen + cb2len);
    memcpy(coinbase, wb->coinb1bin, wb->coinb1len);
    cblen = wb->coinb1len;
    memcpy(coinbase + cblen, &client->enonce1bin, wb->enonce1constlen + wb->enonce1varlen);
    cblen += wb->enonce1constlen + wb->enonce1varlen;
    hex2bin(coinbase + cblen, nonce2, wb->enonce2varlen);
    cblen += wb->enonce2varlen;
    memcpy(coinbase + cblen, coinb2bin, cb2len);
    ck_runlock(&sdata->instance_lock);

    cblen += cb2len;

    gen_hash((uchar*)coinbase, merkle_root, cblen);
    memcpy(merkle_sha, merkle_root, 32);
    for (i = 0; i < wb->merkles; i++) {
        memcpy(merkle_sha + 32, &wb->merklebin[i], 32);
        gen_hash(merkle_sha, merkle_root, 64);
        memcpy(merkle_sha, merkle_root, 32);
    }
    data32 = (uint32_t*)merkle_sha;
    swap32 = (uint32_t*)merkle_root;
    flip_32(swap32, data32);

    /* Copy the cached header binary and insert the merkle root */
    memcpy(data, wb->headerbin, 80);
    memcpy(data + 36, merkle_root, 32);

    /* nVersion: full replace (SV2) or BIP320 mask OR (SV1) */
    if (full_version) {
        data32 = (uint32_t*)data;
        *data32 = htobe32(version_field);
    } else if (version_field) {
        uint32_t version_mask = htobe32(version_field);

        data32 = (uint32_t*)data;
        *data32 |= version_mask;
    }

    /* Insert the nonce value into the data */
    hex2bin(&benonce32, nonce, 4);
    data32 = (uint32_t*)(data + 64 + 12);
    *data32 = benonce32;

    /* Insert the ntime value into the data */
    data32 = (uint32_t*)(data + 68);
    *data32 = htobe32(ntime32);

    /* Hash the share */
    data32 = (uint32_t*)data;
    swap32 = (uint32_t*)swap;
    flip_80(swap32, data32);
    sha256(swap, 80, hash1);
    sha256(hash1, 32, hash);

    /* Calculate the diff of the share here */
    ret = diff_from_target(hash);

    /* Test we haven't solved a block regardless of share status */
    test_blocksolve(
        client, wb, swap, hash, ret, coinbase, cblen, nonce2, nonce, ntime32, full_version ? 0 : version_field, stale);

    return ret;
}

#ifdef HAVE_SV2
#include "sv2_work.h"
#include "sv2_strat.h"
#include "sv2_jd.h"
#include "connector.h"

bool stratifier_sv2_snapshot_work(struct sv2_work_snap* out, int64_t instance_id) {
    sdata_t*            sdata = ckpool.sdata;
    workbase_t*         wb = NULL;
    stratum_instance_t* client = NULL;
    bool                ok = false;
    int                 i, cb2len;
    uchar*              coinb2bin;

    if (!out || !sdata)
        return false;
    memset(out, 0, sizeof(*out));

    if (instance_id)
        client = ref_instance_by_id(sdata, instance_id);

    ck_wlock(&sdata->workbase_lock);
    wb = sdata->current_workbase;
    if (!wb || wb->coinb1len > (int)sizeof(out->coinb1)) {
        ck_wunlock(&sdata->workbase_lock);
        goto out_client;
    }
    wb->readcount++;
    out->wb_id = wb->id;
    out->version = wb->version;
    out->ntime = wb->ntime32;
    {
        uint32_t nbits_be = 0;

        if (strlen(wb->nbit) >= 8)
            hex2bin(&nbits_be, wb->nbit, 4);
        out->nbits = be32toh(nbits_be);
    }
    /*
     * wb->headerbin is ckpool "midstate" layout (same as used before
     * flip_80 in share hashing), not Bitcoin wire order.
     * SV2 U256 prev_hash must match rust-bitcoin BlockHash / consensus
     * header field bytes = flip_32 of the midstate prevhash region.
     * Sending midstate bytes unchanged makes clients mine a different
     * header than submission_diff → sdiff ~0 / difficulty-too-low.
     */
    flip_32(out->prevhash, wb->headerbin + 4);
    memcpy(out->coinb1, wb->coinb1bin, wb->coinb1len);
    out->coinb1len = wb->coinb1len;
    out->enonce1varlen = wb->enonce1varlen;
    out->enonce2varlen = wb->enonce2varlen;
    out->merkles = wb->merkles;
    if (out->merkles > 16)
        out->merkles = 16;
    for (i = 0; i < out->merkles; i++)
        memcpy(out->merklebin[i], wb->merklebin[i], 32);
    ck_wunlock(&sdata->workbase_lock);

    if (client) {
        ck_wlock(&sdata->instance_lock);
        /* Ensure solo per-user coinb2 exists for this workbase */
        if (ckpool.btcsolo && client->user_instance && client->user_instance->btcaddress)
            __generate_userwb(sdata, wb, client->user_instance);
        coinb2bin = __user_coinb2(client, wb, &cb2len);
        if (cb2len <= (int)sizeof(out->coinb2)) {
            memcpy(out->coinb2, coinb2bin, cb2len);
            out->coinb2len = cb2len;
            ok = true;
        }
        ck_wunlock(&sdata->instance_lock);
    } else {
        ck_rlock(&sdata->workbase_lock);
        if (wb->coinb2len <= (int)sizeof(out->coinb2)) {
            memcpy(out->coinb2, wb->coinb2bin, wb->coinb2len);
            out->coinb2len = wb->coinb2len;
            ok = true;
        }
        ck_runlock(&sdata->workbase_lock);
    }
    put_workbase(sdata, wb);
out_client:
    if (client)
        dec_instance_ref(sdata, client);
    return ok;
}

/* Allocate a unique extranonce prefix from the same global counter used by
 * SV1 new_enonce1, so SV1 clients and SV2 channels can never overlap on the
 * same workbase. Takes the first len bytes of the little-endian counter,
 * matching __fill_enonce1data. */
bool stratifier_sv2_alloc_enonce1(uint8_t* out, int len) {
    sdata_t* sdata = ckpool.sdata;
    uint64_t enonce1, enonce1_le;

    if (!out || !sdata || len < 2 || len > 8)
        return false;
    ck_wlock(&sdata->instance_lock);
    enonce1 = le64toh(sdata->enonce1_64);
    enonce1++;
    sdata->enonce1_64 = htole64(enonce1);
    enonce1_le = sdata->enonce1_64;
    ck_wunlock(&sdata->instance_lock);
    memcpy(out, &enonce1_le, len);
    return true;
}

bool stratifier_sv2_open_session(
    int64_t        connector_id,
    uint32_t       channel_id,
    const char*    user_identity,
    const char*    address,
    int            server,
    const uint8_t* enonce1,
    int            enonce1_len,
    double         diff,
    int64_t*       out_instance_id) {
    sdata_t*            sdata = ckpool.sdata;
    stratum_instance_t* client;
    user_instance_t*    user;
    int64_t             id;
    bool                ret = false;

    if (!sdata || !user_identity || !out_instance_id || enonce1_len < 1 || enonce1_len > 16)
        return false;

    id = connector_newclientid();
    *out_instance_id = id;

    ck_wlock(&sdata->instance_lock);
    client = __stratum_add_instance(id, address ? address : "sv2", server);
    /* __stratum_add_instance drops and re-grabs lock; holds write lock on return */
    __inc_instance_ref(client);
    ck_wunlock(&sdata->instance_lock);

    client->subscribed = true;
    client->sv2 = true;
    client->useragent = strdup("sv2");
    /* Round up so advertised target (from double diff) is never stricter
     * than the integer share threshold. */
    client->diff = client->old_diff = (int64_t)(diff + 0.999999);
    if (client->diff < 1)
        client->diff = client->old_diff = ckpool.startdiff;
    memcpy(client->enonce1bin, enonce1, enonce1_len);
    __bin2hex(client->enonce1, client->enonce1bin, enonce1_len);
    if (enonce1_len <= 8)
        memcpy(&client->enonce1_64, enonce1, enonce1_len);
    snprintf(client->identity, sizeof(client->identity), "sv2:%" PRId64 ":%u", connector_id, channel_id);

    user = generate_user(client, user_identity);
    if (unlikely(!user)) {
        LOGNOTICE("SV2 reject user %s with maxusers reached", user_identity);
        client->dropped = true;
        dec_instance_ref(sdata, client);
        return false;
    }
    client->user_id = user->id;
    client->workername = strdup(user_identity);
    client->password = strdup("x");
    client->start_time = time(NULL);

    if (ckpool.btcsolo && !user->btcaddress) {
        LOGNOTICE("SV2 solo reject invalid address user %s", user_identity);
        client->dropped = true;
        dec_instance_ref(sdata, client);
        return false;
    }

    client_auth(client, user, true);
    ret = client->authorised;

    if (ret && ckpool.btcsolo) {
        workbase_t* wb;

        ck_wlock(&sdata->workbase_lock);
        wb = sdata->current_workbase;
        if (wb)
            wb->readcount++;
        ck_wunlock(&sdata->workbase_lock);
        if (wb) {
            ck_wlock(&sdata->instance_lock);
            __generate_userwb(sdata, wb, user);
            ck_wunlock(&sdata->instance_lock);
            ck_wlock(&sdata->workbase_lock);
            wb->readcount--;
            ck_wunlock(&sdata->workbase_lock);
        }
    }

    LOGNOTICE(
        "SV2 session instance %" PRId64 " user %s authorised=%d diff=%" PRId64, id, user_identity, ret, client->diff);
    dec_instance_ref(sdata, client);
    return ret;
}

void stratifier_sv2_close_session(int64_t instance_id) {
    sdata_t*            sdata = ckpool.sdata;
    stratum_instance_t* client;

    if (!sdata || !instance_id)
        return;
    client = ref_instance_by_id(sdata, instance_id);
    if (!client)
        return;
    client->dropped = true;
    dec_instance_ref(sdata, client);
    sv2_strat_clear_instance(instance_id);
}

void stratifier_sv2_set_diff(int64_t instance_id, double diff) {
    sdata_t*            sdata = ckpool.sdata;
    stratum_instance_t* client;
    int64_t             d;

    if (!sdata || instance_id < 1 || !isfinite(diff) || diff < 1.0)
        return;
    d = (int64_t)(diff + 0.999999);
    if (d < 1)
        d = 1;
    client = ref_instance_by_id(sdata, instance_id);
    if (!client)
        return;
    /* Same grace as SV1 retarget: shares on pre-change workbases may still
     * use old_diff until the next tip/workbase. Without this, UpdateChannel
     * / auto-vardiff jumps cause mass difficulty-too-low on in-flight work. */
    client->old_diff = client->diff;
    client->diff = d;
    ck_rlock(&sdata->workbase_lock);
    if (sdata->current_workbase)
        client->diff_change_job_id = sdata->current_workbase->id + 1;
    else
        client->diff_change_job_id = sdata->workbase_id + 1;
    ck_runlock(&sdata->workbase_lock);
    dec_instance_ref(sdata, client);
}

bool stratifier_sv2_tip_for_jd(
    uint32_t* version_out,
    uint32_t* ntime_out,
    uint32_t* nbits_out,
    uint8_t   prevhash_header[32]) {
    sdata_t*    sdata = ckpool.sdata;
    workbase_t* wb;
    uint8_t     wire[80];
    uint32_t*   w32;

    if (!sdata || !version_out || !ntime_out || !nbits_out || !prevhash_header)
        return false;
    ck_rlock(&sdata->workbase_lock);
    wb = sdata->current_workbase;
    if (!wb) {
        ck_runlock(&sdata->workbase_lock);
        return false;
    }
    /*
     * wb->headerbin is ckpool midstate layout (pre flip_80). checkBlock
     * needs Bitcoin wire header fields. flip_80 the tip header once and
     * extract version / prevhash / ntime / nbits — same transform used
     * before share hashing. A naïve reverse of wb->prevhash left RPC
     * display order in the header → checkBlock inconclusive-not-best-prevblk.
     */
    flip_80(wire, wb->headerbin);
    w32 = (uint32_t*)wire;
    *version_out = le32toh(w32[0]);
    memcpy(prevhash_header, wire + 4, 32);
    *ntime_out = le32toh(w32[17]); /* offset 68 */
    *nbits_out = le32toh(w32[18]); /* offset 72 */
    ck_runlock(&sdata->workbase_lock);
    return true;
}

bool stratifier_sv2_merkle_root(
    int64_t   instance_id,
    uint8_t   merkle_root_le[32],
    int64_t*  wb_id_out,
    uint32_t* version_out,
    uint32_t* ntime_out,
    uint32_t* nbits_out,
    uint8_t   prevhash_out[32]) {
    struct sv2_work_snap snap;
    unsigned char        merkle_root[32], merkle_sha[64], *coinbase;
    stratum_instance_t*  client;
    sdata_t*             sdata = ckpool.sdata;
    int                  cblen, i, en1len;
    uint32_t *           data32, *swap32;

    if (!stratifier_sv2_snapshot_work(&snap, instance_id))
        return false;
    client = ref_instance_by_id(sdata, instance_id);
    if (!client)
        return false;
    en1len = snap.enonce1varlen;
    if (en1len < 1)
        en1len = 4;

    coinbase = alloca(snap.coinb1len + en1len + snap.enonce2varlen + snap.coinb2len);
    memcpy(coinbase, snap.coinb1, snap.coinb1len);
    cblen = snap.coinb1len;
    memcpy(coinbase + cblen, client->enonce1bin, en1len);
    cblen += en1len;
    memset(coinbase + cblen, 0, snap.enonce2varlen);
    cblen += snap.enonce2varlen;
    memcpy(coinbase + cblen, snap.coinb2, snap.coinb2len);
    cblen += snap.coinb2len;

    gen_hash(coinbase, merkle_root, cblen);
    memcpy(merkle_sha, merkle_root, 32);
    for (i = 0; i < snap.merkles; i++) {
        memcpy(merkle_sha + 32, snap.merklebin[i], 32);
        gen_hash(merkle_sha, merkle_root, 64);
        memcpy(merkle_sha, merkle_root, 32);
    }
    data32 = (uint32_t*)merkle_sha;
    swap32 = (uint32_t*)merkle_root_le;
    flip_32(swap32, data32);

    if (wb_id_out)
        *wb_id_out = snap.wb_id;
    if (version_out)
        *version_out = snap.version;
    if (ntime_out)
        *ntime_out = snap.ntime;
    if (nbits_out)
        *nbits_out = snap.nbits;
    if (prevhash_out)
        memcpy(prevhash_out, snap.prevhash, 32);

    dec_instance_ref(sdata, client);
    return true;
}

static bool new_share(sdata_t* sdata, const uchar* hash, const int64_t wb_id);
static void check_best_diff(
    sdata_t*            sdata,
    user_instance_t*    user,
    worker_instance_t*  worker,
    const double        sdiff,
    stratum_instance_t* client);

bool stratifier_sv2_submit_share(
    int64_t     instance_id,
    int64_t     workbase_id,
    uint32_t    ntime,
    uint32_t    nonce,
    uint32_t    version,
    const char* nonce2hex_in,
    char*       errbuf,
    size_t      errbufsz,
    double*     sdiff_out) {
    sdata_t*            sdata = ckpool.sdata;
    stratum_instance_t* client;
    workbase_t*         wb;
    char                nonce2hex[64], noncehex[16];
    uchar               hash[32];
    double              sdiff, diff;
    bool                stale = false, result = false, submit = false;
    int                 n2len, expect;

    if (errbuf && errbufsz)
        errbuf[0] = '\0';
    if (sdiff_out)
        *sdiff_out = 0;

    client = ref_instance_by_id(sdata, instance_id);
    if (!client || !client->authorised) {
        if (errbuf)
            snprintf(errbuf, errbufsz, "unauthorized-worker");
        /* null client usually means dropped after CloseChannel */
        LOGINFO(
            "SV2 reject instance %" PRId64
            " unauthorized-worker "
            "(client=%s authorised=%d)",
            instance_id, client ? "yes" : "null/dropped", client ? client->authorised : 0);
        if (client)
            dec_instance_ref(sdata, client);
        return false;
    }

    wb = get_workbase(sdata, workbase_id);
    if (!wb) {
        snprintf(errbuf, errbufsz, "invalid-job-id");
        dec_instance_ref(sdata, client);
        return false;
    }
    if (workbase_id < sdata->blockchange_id)
        stale = true;

    n2len = wb->enonce2varlen;
    if (n2len < 1)
        n2len = 1;
    if (n2len > 31)
        n2len = 31;
    expect = n2len * 2;
    if (nonce2hex_in && strlen(nonce2hex_in)) {
        if ((int)strlen(nonce2hex_in) != expect || !validhex(nonce2hex_in)) {
            snprintf(errbuf, errbufsz, "invalid-extranonce-size");
            put_workbase(sdata, wb);
            dec_instance_ref(sdata, client);
            return false;
        }
        memcpy(nonce2hex, nonce2hex_in, expect + 1);
    } else {
        memset(nonce2hex, '0', expect);
        nonce2hex[expect] = '\0';
    }
    sprintf(noncehex, "%08x", nonce);

    sdiff = submission_diff(sdata, client, wb, nonce2hex, ntime, version, true, noncehex, hash, stale);
    if (sdiff_out)
        *sdiff_out = sdiff;

    if (sdiff > client->best_diff) {
        user_instance_t*   user = client->user_instance;
        worker_instance_t* worker = client->worker_instance;

        client->best_diff = sdiff;
        LOGINFO(
            "SV2 user %s worker %s client %s new best diff %lf", user->username, worker->workername, client->identity,
            sdiff);
        check_best_diff(sdata, user, worker, sdiff, client);
    }

    if (stale) {
        snprintf(errbuf, errbufsz, "stale-share");
        put_workbase(sdata, wb);
        dec_instance_ref(sdata, client);
        return false;
    }
    if (ntime < wb->ntime32 || ntime > wb->ntime32 + 7000) {
        snprintf(errbuf, errbufsz, "invalid-ntime");
        put_workbase(sdata, wb);
        dec_instance_ref(sdata, client);
        return false;
    }

    /* Accept min(new,old) until workbase advances past retarget (SV1 parity). */
    diff = client->diff;
    if (workbase_id && workbase_id < client->diff_change_job_id && client->old_diff > 0)
        diff = MIN(diff, (double)client->old_diff);
    if (sdiff >= diff) {
        if (new_share(sdata, hash, workbase_id)) {
            result = true;
            LOGINFO("SV2 accepted instance %" PRId64 " sdiff %.1f/%.0f", instance_id, sdiff, diff);
        } else {
            snprintf(errbuf, errbufsz, "duplicate-share");
            result = false;
        }
    } else {
        snprintf(errbuf, errbufsz, "difficulty-too-low");
        LOGINFO("SV2 reject instance %" PRId64 " difficulty-too-low sdiff %.4f need %.0f", instance_id, sdiff, diff);
        result = false;
    }

    if (sdiff >= wb->diff)
        submit = true;
    ck_rlock(&sdata->workbase_lock);
    if (sdata->current_workbase && sdiff >= sdata->current_workbase->network_diff)
        submit = true;
    ck_runlock(&sdata->workbase_lock);

    add_submit(client, diff, result, submit);
    put_workbase(sdata, wb);
    dec_instance_ref(sdata, client);
    return result;
}

/* Height of the block currently being mined, 0 if we have no workbase yet. */
int stratifier_sv2_tip_height(void) {
    sdata_t* sdata = ckpool.sdata;
    int      height = 0;

    if (!sdata)
        return 0;
    ck_rlock(&sdata->workbase_lock);
    if (sdata->current_workbase)
        height = sdata->current_workbase->height;
    ck_runlock(&sdata->workbase_lock);
    return height;
}

double stratifier_sv2_network_diff(void) {
    sdata_t* sdata = ckpool.sdata;
    double   nd = 0;

    if (!sdata)
        return 0;
    ck_rlock(&sdata->workbase_lock);
    if (sdata->current_workbase)
        nd = sdata->current_workbase->network_diff;
    ck_runlock(&sdata->workbase_lock);
    return nd;
}

bool stratifier_sv2_account_share(
    int64_t             instance_id,
    int64_t             workbase_id,
    const unsigned char hash[32],
    double              sdiff,
    char*               errbuf,
    size_t              errbufsz,
    bool*               network_diff_met) {
    sdata_t*            sdata = ckpool.sdata;
    stratum_instance_t* client;
    double              diff, network_diff = 0;
    bool                result = false, submit = false;
    int64_t             wb_id = workbase_id;

    if (network_diff_met)
        *network_diff_met = false;
    if (errbuf && errbufsz)
        errbuf[0] = '\0';
    client = ref_instance_by_id(sdata, instance_id);
    if (!client || !client->authorised) {
        if (errbuf)
            snprintf(errbuf, errbufsz, "unauthorized-worker");
        if (client)
            dec_instance_ref(sdata, client);
        return false;
    }
    if (sdiff > client->best_diff) {
        user_instance_t*   user = client->user_instance;
        worker_instance_t* worker = client->worker_instance;

        client->best_diff = sdiff;
        LOGINFO(
            "SV2 user %s worker %s client %s new best diff %lf", user->username, worker->workername, client->identity,
            sdiff);
        check_best_diff(sdata, user, worker, sdiff, client);
    }
    if (!wb_id) {
        ck_rlock(&sdata->workbase_lock);
        if (sdata->current_workbase)
            wb_id = sdata->current_workbase->id;
        ck_runlock(&sdata->workbase_lock);
    }
    diff = client->diff;
    if (wb_id && wb_id < client->diff_change_job_id && client->old_diff > 0)
        diff = MIN(diff, (double)client->old_diff);
    if (sdiff >= diff) {
        if (new_share(sdata, hash, wb_id)) {
            result = true;
            LOGINFO("SV2 custom/accounted instance %" PRId64 " sdiff %.1f/%.0f", instance_id, sdiff, diff);
        } else {
            if (errbuf)
                snprintf(errbuf, errbufsz, "duplicate-share");
            result = false;
        }
    } else {
        if (errbuf)
            snprintf(errbuf, errbufsz, "difficulty-too-low");
        result = false;
    }
    ck_rlock(&sdata->workbase_lock);
    if (sdata->current_workbase) {
        network_diff = sdata->current_workbase->network_diff;
        if (sdiff >= sdata->current_workbase->diff)
            submit = true;
        if (sdiff >= network_diff)
            submit = true;
    }
    ck_runlock(&sdata->workbase_lock);
    if (network_diff_met && result && sdiff >= network_diff && network_diff > 0)
        *network_diff_met = true;
    add_submit(client, diff, result, submit);
    dec_instance_ref(sdata, client);
    return result;
}

bool stratifier_sv2_submit_block_bin(
    const unsigned char* block,
    size_t               block_len,
    int64_t              instance_id,
    const char*          workername_opt) {
    sdata_t*            sdata = ckpool.sdata;
    stratum_instance_t* client = NULL;
    char*               hex;
    bool                ret = false;
    uchar               hash1[32], hash[32], swap[32];
    char                blockhash[68] = {};
    char*               workername = NULL;
    int                 height = 0;
    double              network_diff = 0;

    if (!block || block_len < 80 || block_len > 4 * 1024 * 1024)
        return false;
    hex = ckalloc(block_len * 2 + 1);
    __bin2hex(hex, block, block_len);
    /* generator_submitblock expects hex block data via submit path */
    ret = generator_submitblock(hex);
    dealloc(hex);

    /* Block hash: dsha256(wire header), display = byte-reversed */
    {
        int i;

        sha256((uchar*)block, 80, hash1);
        sha256(hash1, 32, hash);
        for (i = 0; i < 32; i++)
            swap[i] = hash[31 - i];
        __bin2hex(blockhash, swap, 32);
    }

    ck_rlock(&sdata->workbase_lock);
    if (sdata->current_workbase) {
        height = sdata->current_workbase->height;
        network_diff = sdata->current_workbase->network_diff;
    }
    ck_runlock(&sdata->workbase_lock);

    if (workername_opt && workername_opt[0])
        workername = strdup(workername_opt);
    else if (instance_id > 0) {
        client = ref_instance_by_id(sdata, instance_id);
        if (client) {
            if (client->workername && client->workername[0])
                workername = strdup(client->workername);
            else if (client->user_instance)
                workername = strdup(client->user_instance->username);
            dec_instance_ref(sdata, client);
        }
    }

    if (ret) {
        block_share_summary(sdata);
        /* submitblock acceptance only — "confirmed" is block_solve()'s
         * word once the block update thread sees it on the chain. */
        if (workername)
            LOGWARNING("Block %d solved by %s accepted on SV2/JD", height, workername);
        else
            LOGWARNING("Block %d solved (SV2/JD) accepted", height);
        LOGWARNING("SV2/JD block hash %s network_diff %.1f", blockhash, network_diff);
        if (workername) {
            yyjson_mut_doc *   user_val, *worker_val;
            worker_instance_t* worker;
            user_instance_t*   user;
            char*              s;

            user = user_by_workername(sdata, workername);
            if (user) {
                worker = get_worker(sdata, user, workername);
                ck_rlock(&sdata->instance_lock);
                user_val = user_stats(user);
                worker_val = worker_stats(worker);
                ck_runlock(&sdata->instance_lock);
                s = yyjson_mut_write(user_val, 0, NULL);
                yyjson_mut_doc_free(user_val);
                LOGWARNING("User %s:%s", user->username, s);
                dealloc(s);
                s = yyjson_mut_write(worker_val, 0, NULL);
                yyjson_mut_doc_free(worker_val);
                LOGWARNING("Worker %s:%s", workername, s);
                dealloc(s);
            }
        }
    } else {
        LOGWARNING(
            "SV2/JD block submit rejected/error height %d hash %s by %s", height, blockhash,
            workername ? workername : "(unknown)");
    }
    free(workername);
    return ret;
}
#endif
/* Optimised for the common case where shares are new */
static bool new_share(sdata_t* sdata, const uchar* hash, const int64_t wb_id) {
    share_t *share = ckzalloc(sizeof(share_t)), *match = NULL;
    bool     ret = true;

    memcpy(share->hash, hash, 32);
    share->workbase_id = wb_id;

    mutex_lock(&sdata->share_lock);
    sdata->shares_generated++;
    HASH_FIND(hh, sdata->shares, hash, 32, match);
    if (likely(!match))
        HASH_ADD(hh, sdata->shares, hash, 32, share);
    mutex_unlock(&sdata->share_lock);

    if (unlikely(match)) {
        dealloc(share);
        ret = false;
    }
    return ret;
}

static void update_client(const stratum_instance_t* client, const int64_t client_id);

/* Submit a share in proxy mode to the parent pool. workbase_lock is held.
 * Needs to be entered with client holding a ref count. */
static void submit_share(
    stratum_instance_t* client,
    const int64_t       jobid,
    const char*         nonce2,
    const char*         ntime,
    const char*         nonce,
    const uint32_t      version_mask) {
    yyjson_mut_doc* doc;
    /* en1var (≤8 B) + en2 (≤8 B for SV1) → ≤32 hex chars; room for NUL. */
    char enonce2[65];

    snprintf(enonce2, sizeof(enonce2), "%s%s", client->enonce1var, nonce2 ? nonce2 : "");
    /* version_mask carries the client's BIP320 version-rolling bits so an SV2
     * upstream proxy can reconstruct the full nVersion for SubmitSharesExtended.
     * SV1 upstream proxysend ignores it. */
    doc = yyjson_mut_pack(
        "{sIsssssssIsisisi}", "jobid", jobid, "nonce2", enonce2, "ntime", ntime, "nonce", nonce, "client_id",
        client->id, "proxy", client->proxyid, "subproxy", client->subproxyid, "version_mask", version_mask);
    generator_add_send(doc);
}

static void check_best_diff(
    sdata_t*            sdata,
    user_instance_t*    user,
    worker_instance_t*  worker,
    const double        sdiff,
    stratum_instance_t* client) {
    char buf[512];
    bool best_ever = false, best_worker = false, best_user = false;

    if (sdiff > user->best_ever) {
        user->best_ever = sdiff;
        best_ever = true;
    }
    if (sdiff > worker->best_ever) {
        worker->best_ever = sdiff;
        best_ever = true;
    }
    if (sdiff > worker->best_diff) {
        worker->best_diff = sdiff;
        best_worker = true;
    }
    if (sdiff > user->best_diff) {
        user->best_diff = sdiff;
        best_user = true;
    }
    /* Check against pool's best diff unlocked first, then recheck once
     * the mutex is locked. Remote shares can arrive before we have a
     * current workbase so check it exists before dereferencing it. */
    if (sdiff > sdata->stats.best_diff && likely(sdata->current_workbase)) {
        /* Don't set pool best diff if it's a block since we will have
         * reset it to zero. */
        mutex_lock(&sdata->stats_lock);
        if (unlikely(sdiff > sdata->stats.best_diff && sdiff < sdata->current_workbase->network_diff))
            sdata->stats.best_diff = sdiff;
        mutex_unlock(&sdata->stats_lock);
    }
    if (likely((!best_user && !best_worker) || !client))
        return;
    snprintf(buf, 511, "New best %sshare for %s: %lf", best_ever ? "ever " : "", best_user ? "user" : "worker", sdiff);
    stratum_send_message(sdata, client, buf);
}

/* Needs to be entered with client holding a ref count. */
static bool parse_submit(stratum_instance_t* client, yyjson_mut_val* params_val, enum share_err* err_code) {
    bool             share = false, result = false, invalid = true, submit = false, stale = false;
    const char *     workername, *job_id, *ntime, *version_mask;
    double           diff = client->diff, wdiff = 0, sdiff = -1;
    char             hexhash[68] = {}, sharehash[32], cdfield[64];
    user_instance_t* user = client->user_instance;
    char *           fname = NULL, *nonce, *nonce2;
    uint32_t         ntime32, version_mask32 = 0;
    sdata_t*         sdata = client->sdata;
    sdata_t*         ckp_sdata = ckpool.sdata;
    enum share_err   err = SE_NONE;
    char             idstring[24] = {};
    workbase_t*      wb = NULL;
    yyjson_mut_doc*  doc;
    int64_t          id = 0;
    uchar            hash[32];
    int              nlen, len;
    int              wb_height = 0;
    time_t           now_t;
    ts_t             now;
    FILE*            fp;

    ts_realtime(&now);
    now_t = now.tv_sec;
    sprintf(cdfield, "%lu,%lu", now.tv_sec, now.tv_nsec);

    if (unlikely(!yyjson_mut_is_arr(params_val))) {
        err = SE_NOT_ARRAY;
        goto out;
    }
    if (unlikely(yyjson_mut_arr_size(params_val) < 5)) {
        err = SE_INVALID_SIZE;
        goto out;
    }
    workername = yyjson_mut_get_str(yyjson_mut_arr_get(params_val, 0));
    if (unlikely(!workername || !strlen(workername))) {
        err = SE_NO_USERNAME;
        goto out;
    }
    job_id = yyjson_mut_get_str(yyjson_mut_arr_get(params_val, 1));
    if (unlikely(!job_id || !strlen(job_id))) {
        err = SE_NO_JOBID;
        goto out;
    }
    nonce2 = (char*)yyjson_mut_get_str(yyjson_mut_arr_get(params_val, 2));
    if (unlikely(!nonce2 || !strlen(nonce2) || !validhex(nonce2))) {
        err = SE_NO_NONCE2;
        goto out;
    }
    ntime = yyjson_mut_get_str(yyjson_mut_arr_get(params_val, 3));
    if (unlikely(!ntime || !strlen(ntime) || !validhex(ntime))) {
        err = SE_NO_NTIME;
        goto out;
    }
    nonce = (char*)yyjson_mut_get_str(yyjson_mut_arr_get(params_val, 4));
    if (unlikely(!nonce || strlen(nonce) < 8 || !validhex(nonce))) {
        err = SE_NO_NONCE;
        goto out;
    }

    version_mask = yyjson_mut_get_str(yyjson_mut_arr_get(params_val, 5));
    if (version_mask && strlen(version_mask) && validhex(version_mask)) {
        sscanf(version_mask, "%x", &version_mask32);
        // check version mask
        if (version_mask32 && ((~ckpool.version_mask) & version_mask32) != 0) {
            // means client changed some bits which server doesn't allow to change
            err = SE_INVALID_VERSION_MASK;
            goto out;
        }
    }
    if (safecmp(workername, client->workername)) {
        err = SE_WORKER_MISMATCH;
        goto out;
    }
    sscanf(job_id, "%lx", &id);
    sscanf(ntime, "%x", &ntime32);

    share = true;

    if (unlikely(!sdata->current_workbase)) {
        err = SE_NO_WORKBASE;
        *err_code = err;
        return false;
    }

    wb = get_workbase(sdata, id);
    if (unlikely(!wb)) {
        id = sdata->current_workbase->id;
        err = SE_INVALID_JOBID;
        strncpy(idstring, job_id, 19);
        ASPRINTF(&fname, "%s.sharelog", sdata->current_workbase->logdir);
        goto out_nowb;
    }
    wdiff = wb->diff;
    wb_height = wb->height;
    strncpy(idstring, wb->idstring, 20);
    ASPRINTF(&fname, "%s.sharelog", wb->logdir);
    /* Fix broken clients sending too many/few chars. Nonce2 is part of the
     * read-only json so use a temporary buffer sized to the workbase. */
    len = wb->enonce2varlen * 2;
    nlen = strlen(nonce2);
    if (unlikely(nlen != len)) {
        if (nlen > len) {
            nonce2 = strdupa(nonce2);
            nonce2[len] = '\0';
        } else if (nlen < len) {
            char* tmp = nonce2;
            char* pad;

            /* Old code strdupa("000…") was only 16 hex (8 B) and
             * overflowed when enonce2varlen > 8. */
            pad = alloca((size_t)len + 1);
            memset(pad, '0', (size_t)len);
            pad[len] = '\0';
            memcpy(pad, tmp, (size_t)nlen);
            nonce2 = pad;
        }
    }
    /* Same with nonce, but we need at least 8 chars. We checked for this
     * earlier. */
    len = 8;
    nlen = strlen(nonce);
    if (unlikely(nlen > len)) {
        nonce = strdupa(nonce);
        nonce[len] = '\0';
    }
    if (id < sdata->blockchange_id)
        stale = true;
    sdiff = submission_diff(sdata, client, wb, nonce2, ntime32, version_mask32, false, nonce, hash, stale);
    if (sdiff > client->best_diff) {
        worker_instance_t* worker = client->worker_instance;

        client->best_diff = sdiff;
        LOGINFO(
            "User %s worker %s client %s new best diff %lf", user->username, worker->workername, client->identity,
            sdiff);
        check_best_diff(sdata, user, worker, sdiff, client);
    }
    bswap_256(sharehash, hash);
    __bin2hex(hexhash, sharehash, 32);

    if (stale) {
        /* Accept shares if they're received on remote nodes before the
         * workbase was retired. */
        if (client->latency) {
            int  latency;
            tv_t now_tv;

            ts_to_tv(&now_tv, &now);
            latency = ms_tvdiff(&now_tv, &wb->retired);
            if (latency < client->latency) {
                LOGDEBUG("Accepting %dms late share from client %s", latency, client->identity);
                goto no_stale;
            }
        }
        err = SE_STALE;
        goto out_submit;
    }
no_stale:
    /* Ntime cannot be less, but allow forward ntime rolling up to max */
    if (ntime32 < wb->ntime32 || ntime32 > wb->ntime32 + 7000) {
        err = SE_NTIME_INVALID;
        goto out_put;
    }
    invalid = false;
out_submit:
    if (sdiff >= wdiff)
        submit = true;
    if (unlikely(sdiff >= sdata->current_workbase->network_diff)) {
        /* Make sure we always submit any possible block solve */
        LOGWARNING("Submitting possible block solve share diff %lf !", sdiff);
        submit = true;
    }
out_put:
    put_workbase(sdata, wb);
out_nowb:

    /* Accept shares of the old diff until the next update. Strictly
     * speaking clients should not use the new diff until the next update
     * but very few clients do this properly, so accept whichever is the
     * minimum. */
    if (id < client->diff_change_job_id)
        diff = MIN(diff, client->old_diff);
    if (!invalid) {
        char wdiffsuffix[16];

        suffix_string(wdiff, wdiffsuffix, 16, 0);
        if (sdiff >= diff) {
            if (new_share(sdata, hash, id)) {
                LOGINFO(
                    "Accepted client %s share diff %.1f/%.0f/%s: %s", client->identity, sdiff, diff, wdiffsuffix,
                    hexhash);
                result = true;
            } else {
                err = SE_DUPE;
                LOGINFO(
                    "Rejected client %s dupe diff %.1f/%.0f/%s: %s", client->identity, sdiff, diff, wdiffsuffix,
                    hexhash);
                submit = false;
            }
        } else {
            err = SE_HIGH_DIFF;
            LOGINFO(
                "Rejected client %s high diff %.1f/%.0f/%s: %s", client->identity, sdiff, diff, wdiffsuffix, hexhash);
            submit = false;
        }
    } else
        LOGINFO("Rejected client %s invalid share %s", client->identity, SHARE_ERR(err));

    /* Submit share to upstream pool in proxy mode. We submit valid and
     * stale shares and filter out the rest. */
    if (wb && wb->proxy && submit) {
        LOGINFO("Submitting share upstream: %s", hexhash);
        submit_share(client, id, nonce2, ntime, nonce, version_mask32);
    }

    add_submit(client, diff, result, submit);

    doc = yyjson_mut_pack(
        "{sIsIsssssssssfsfsssbsssissssssssssssssss}", "workinfoid", id, "clientid",
        ckpool.remote ? client->virtualid : client->id, "enonce1", client->enonce1, "nonce2", nonce2, "nonce", nonce,
        "ntime", ntime, "diff", diff, "sdiff", sdiff, "hash", hexhash, "result", result, "error", SHARE_ERR(err),
        "errn", err, "createdate", cdfield, "createby", "code", "createcode", __func__, "createinet",
        ckpool.serverurl[client->server], "workername", client->workername, "username", user->username, "address",
        client->address, "agent", client->useragent);

    /* The lightning address, when the worker name carried one. Read under
     * the instance lock since generate_user() can replace it on reauth.
     * User instances live on the ckpool sdata, which is not client->sdata
     * in proxy mode, so lock the one generate_user() writes under. */
    ck_rlock(&ckp_sdata->instance_lock);
    if (user->secondaryuserid)
        yyjson_mut_obj_add_strcpy(doc, yyjson_mut_doc_get_root(doc), "lnurl", user->secondaryuserid);
    ck_runlock(&ckp_sdata->instance_lock);

    if (ckpool.logshares_db)
        db_log_share(sdata, doc, wb_height);

    if (ckpool.logshares_file) {
        fp = fopen(fname, "ae");
        if (likely(fp)) {
            yyjson_mut_write_file(fname, doc, YYJSON_WRITE_NEWLINE_AT_END, NULL, NULL);
            fclose(fp);
            if (unlikely(len < 0))
                LOGERR("Failed to fwrite to %s", fname);
        } else
            LOGERR("Failed to fopen %s", fname);
    }
    if (ckpool.remote)
        upstream_yydoc_msgtype(doc, SM_SHARE);
    yyjson_mut_doc_free(doc);
out:
    if (!sdata->wbincomplete && ((!result && !submit) || !share)) {
        /* Is this the first in a run of invalids? */
        if (client->first_invalid < client->last_share.tv_sec || !client->first_invalid)
            client->first_invalid = now_t;
        else if (client->first_invalid && client->first_invalid < now_t - 180 && client->reject < 3) {
            LOGNOTICE("Client %s rejecting for 180s, disconnecting", client->identity);
            if (ckpool.node)
                connector_drop_client(client->id);
            else
                stratum_send_message(sdata, client, "Disconnecting for continuous invalid shares");
            client->reject = 3;
        } else if (client->first_invalid && client->first_invalid < now_t - 120 && client->reject < 2) {
            LOGNOTICE("Client %s rejecting for 120s, reconnecting", client->identity);
            stratum_send_message(sdata, client, "Reconnecting for continuous invalid shares");
            reconnect_client(sdata, client);
            client->reject = 2;
        } else if (client->first_invalid && client->first_invalid < now_t - 60 && !client->reject) {
            LOGNOTICE("Client %s rejecting for 60s, sending update", client->identity);
            update_client(client, client->id);
            client->reject = 1;
        }
    } else if (client->reject < 3) {
        client->first_invalid = 0;
        client->reject = 0;
    }

    if (!share)
        LOGINFO("Invalid share from client %s: %s", client->identity, client->workername);
    free(fname);
    *err_code = err;
    return result;
}

/* Must enter with workbase_lock held */
static yyjson_mut_doc* __stratum_notify(const workbase_t* wb, const bool clean) {
    yyjson_mut_doc* doc;
    yyjson_mut_val* root;

    doc = yyjson_mut_doc_new(&ckyyalc);
    root = yyjson_mut_pack_val(
        doc, "{s:[ssssosssb],s:n,s:s}", "params", wb->idstring, wb->prevhash, wb->coinb1, wb->coinb2,
        yyjson_mut_doc_get_root(wb->yymerkle_doc), wb->bbversion, wb->nbit, wb->ntime, clean, "id", "method",
        "mining.notify");
    yyjson_mut_doc_set_root(doc, root);
    return doc;
}

static void stratum_broadcast_update(sdata_t* sdata, const workbase_t* wb, const bool clean) {
    yyjson_mut_doc* doc;

    ck_rlock(&sdata->workbase_lock);
    doc = __stratum_notify(wb, clean);
    ck_runlock(&sdata->workbase_lock);

    stratum_broadcast(sdata, doc, SM_UPDATE);
#ifdef HAVE_SV2
    sv2_strat_on_work_update(clean);
#endif
}

/* For sending a single stratum template update */
static void stratum_send_update(sdata_t* sdata, const int64_t client_id, const bool clean) {
    yyjson_mut_doc* doc;

    if (unlikely(!sdata->current_workbase)) {
        if (!ckpool.proxy)
            LOGWARNING("No current workbase to send stratum update");
        else
            LOGDEBUG("No current workbase to send stratum update for client %" PRId64, client_id);
        return;
    }

    ck_rlock(&sdata->workbase_lock);
    doc = __stratum_notify(sdata->current_workbase, clean);
    ck_runlock(&sdata->workbase_lock);

    stratum_add_yysend(sdata, doc, client_id, SM_UPDATE);
}

/* Hold instance and workbase lock */
static yyjson_mut_doc* __user_notify(const workbase_t* wb, const user_instance_t* user, const bool clean) {
    int64_t         id = wb->id;
    struct userwb*  userwb;
    yyjson_mut_doc* doc;
    yyjson_mut_val* root;

    HASH_FIND_I64(user->userwbs, &id, userwb);
    if (unlikely(!userwb)) {
        LOGINFO("Failed to find userwb in __user_notify!");
        return NULL;
    }

    doc = yyjson_mut_doc_new(&ckyyalc);
    root = yyjson_mut_pack_val(
        doc, "{s:[ssssosssb],s:n,s:s}", "params", wb->idstring, wb->prevhash, wb->coinb1, userwb->coinb2,
        yyjson_mut_doc_get_root(wb->yymerkle_doc), wb->bbversion, wb->nbit, wb->ntime, clean, "id", "method",
        "mining.notify");
    yyjson_mut_doc_set_root(doc, root);
    return doc;
}

/* Sends a stratum update with a unique coinb2 for every client. Avoid
 * recursive locking. */
static void stratum_broadcast_updates(sdata_t* sdata, bool clean) {
    stratum_instance_t *client, *tmp;
    yyjson_mut_doc*     doc;

    ck_wlock(&sdata->instance_lock);
    HASH_ITER(hh, sdata->stratum_instances, client, tmp) {
        if (!client->user_instance)
            continue;
        /* Virtual SV2 sessions have no connector client; SV1 notify
         * would fail send and drop them. Work is pushed by
         * sv2_strat_on_work_update after this loop. */
        if (client->sv2)
            continue;
        __inc_instance_ref(client);
        ck_wunlock(&sdata->instance_lock);

        ck_rlock(&sdata->workbase_lock);
        doc = __user_notify(sdata->current_workbase, client->user_instance, clean);
        ck_runlock(&sdata->workbase_lock);

        if (likely(doc))
            stratum_add_yysend(sdata, doc, client->id, SM_UPDATE);

        ck_wlock(&sdata->instance_lock);
        __dec_instance_ref(client);
    }
    ck_wunlock(&sdata->instance_lock);
#ifdef HAVE_SV2
    /* Solo path never went through stratum_broadcast_update before */
    sv2_strat_on_work_update(clean);
#endif
}

static void send_yyjson_err(sdata_t* sdata, const int64_t client_id, yyjson_mut_val* id_val, const char* err_msg) {
    yyjson_mut_doc* doc;

    /* Some clients have no id_val so pass back an empty string. */
    if (unlikely(!id_val))
        doc = yyjson_mut_pack("{ssss}", "id", "", "error", err_msg);
    else
        doc = yyjson_mut_pack("{soss}", "id", id_val, "error", err_msg);
    stratum_add_yysend(sdata, doc, client_id, SM_ERROR);
}

/* Needs to be entered with client holding a ref count. */
static void update_client(const stratum_instance_t* client, const int64_t client_id) {
    sdata_t* sdata = client->sdata;

    if (!ckpool.btcsolo)
        stratum_send_update(sdata, client_id, true);
    stratum_send_diff(sdata, client);
}

static json_params_t*
create_yyjson_params(const int64_t client_id, yyjson_mut_val* method, yyjson_mut_val* params, yyjson_mut_val* id_val) {
    json_params_t* jp = ckzalloc(sizeof(json_params_t));

    jp->doc = yyjson_mut_doc_new(&ckyyalc);
    jp->yymethod = yyjson_mut_val_mut_copy(jp->doc, method);
    jp->yyparams = yyjson_mut_val_mut_copy(jp->doc, params);
    jp->yyid_val = yyjson_mut_val_mut_copy(jp->doc, id_val);
    jp->client_id = client_id;

    return jp;
}

/* Implement support for the diff in the params as well as the originally
 * documented form of placing diff within the method. Needs to be entered with
 * client holding a ref count. */
static void suggest_diff(stratum_instance_t* client, const char* method, yyjson_mut_val* params_val) {
    yyjson_mut_val* arr_val = yyjson_mut_arr_get(params_val, 0);
    int64_t         sdiff;

    if (unlikely(!client_active(client))) {
        LOGNOTICE("Attempted to suggest diff on unauthorised client %s", client->identity);
        return;
    }
    if (arr_val && yyjson_mut_is_num(arr_val)) {
        double dsdiff = yyjson_mut_get_num(arr_val);

        /* Avoid undefined behaviour casting non finite or out of range
         * values, relying on the mindiff clamp below */
        if (unlikely(!isfinite(dsdiff) || dsdiff < 0 || dsdiff > 1e18))
            dsdiff = 0;
        sdiff = dsdiff;
    } else if (sscanf(method, "mining.suggest_difficulty(%" PRId64, &sdiff) != 1) {
        LOGINFO("Failed to parse suggest_difficulty for client %s", client->identity);
        return;
    }
    /* Clamp suggest diff to global pool mindiff */
    if (sdiff < ckpool.mindiff)
        sdiff = ckpool.mindiff;
    if (sdiff == client->suggest_diff)
        return;
    client->suggest_diff = sdiff;
    if (client->diff == sdiff)
        return;
    client->diff_change_job_id = client->sdata->workbase_id;
    client->old_diff = client->diff;
    client->diff = sdiff;
    stratum_send_diff(ckpool.sdata, client);
}

/* Send diff first when sending the first stratum template after subscribing */
static void init_client(const stratum_instance_t* client, const int64_t client_id) {
    sdata_t* sdata = client->sdata;

    stratum_send_diff(sdata, client);
    if (!ckpool.btcsolo)
        stratum_send_update(sdata, client_id, true);
}

/* When a node first connects it has no transactions so we have to send all
 * current ones to it. */
static void send_node_all_txns(sdata_t* sdata, const stratum_instance_t* client) {
    yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val *root = yyjson_mut_obj(doc), *txn_array, *txn_val;
    txntable_t *    txn, *tmp;
    smsg_t*         msg;

    yyjson_mut_doc_set_root(doc, root);
    txn_array = yyjson_mut_arr(doc);

    if (client->trusted)
        yyjson_mut_obj_add_strcpy(doc, root, "method", stratum_msgs[SM_TRANSACTIONS]);
    else
        yyjson_mut_obj_add_strcpy(doc, root, "node.method", stratum_msgs[SM_TRANSACTIONS]);

    ck_rlock(&sdata->txn_lock);
    HASH_ITER(hh, sdata->txns, txn, tmp) {
        txn_val = yyjson_mut_pack_val(doc, "{ss,ss}", "hash", txn->hash, "data", txn->data);
        yyjson_mut_arr_append(txn_array, txn_val);
    }
    ck_runlock(&sdata->txn_lock);

    yyjson_mut_obj_add_val(doc, root, "transaction", txn_array);
    msg = ckzalloc(sizeof(smsg_t));
    msg->doc = doc;
    msg->client_id = client->id;
    ckmsgq_add(sdata->ssends, msg);
    LOGNOTICE("Sending new node client %s all transactions", client->identity);
}

static void* setup_node(void* arg) {
    stratum_instance_t* client = (stratum_instance_t*)arg;

    pthread_detach(pthread_self());

    client->latency = round_trip(client->address) / 2;
    LOGNOTICE("Node client %s %s latency set to %dms", client->identity, client->address, client->latency);
    send_node_all_txns(client->sdata, client);
    dec_instance_ref(client->sdata, client);
    return NULL;
}

/* Create a thread to asynchronously set latency to the node to not
 * block. Increment the ref count to prevent the client pointer
 * dereferencing under us, allowing the thread to decrement it again when
 * finished. */
static void add_mining_node(sdata_t* sdata, stratum_instance_t* client) {
    pthread_t pth;

    ck_wlock(&sdata->instance_lock);
    client->node = true;
    DL_APPEND2(sdata->node_instances, client, node_prev, node_next);
    __inc_instance_ref(client);
    ck_wunlock(&sdata->instance_lock);

    LOGWARNING(
        "Added client %s %s as mining node on server %d:%s", client->identity, client->address, client->server,
        ckpool.serverurl[client->server]);

    create_pthread(&pth, setup_node, client);
}

static void add_remote_server(sdata_t* sdata, stratum_instance_t* client) {
    ck_wlock(&sdata->instance_lock);
    client->trusted = true;
    DL_APPEND2(sdata->remote_instances, client, remote_prev, remote_next);
    __inc_instance_ref(client);
    ck_wunlock(&sdata->instance_lock);

    send_node_all_txns(sdata, client);
    dec_instance_ref(sdata, client);
}

/* Enter with client holding ref count */
static void parse_method(
    sdata_t*            sdata,
    stratum_instance_t* client,
    const int64_t       client_id,
    yyjson_mut_val*     id_val,
    yyjson_mut_val*     method_val,
    yyjson_mut_val*     params_val) {
    const char* method;

    /* Random broken clients send something not an integer as the id so we
     * copy the json item for id_val as is for the response. By far the
     * most common messages will be shares so look for those first */
    method = yyjson_mut_get_str(method_val);
    if (likely(cmdmatch(method, "mining.submit") && client->authorised)) {
        json_params_t* jp = create_yyjson_params(client_id, method_val, params_val, id_val);

        if (!stratifier_queue_share_work(false, jp))
            discard_json_params(jp);
        return;
    }

    if (cmdmatch(method, "mining.term")) {
        LOGDEBUG("Mining terminate requested from %s %s", client->identity, client->address);
        drop_client(sdata, client_id);
        return;
    }

    if (cmdmatch(method, "mining.subscribe")) {
        yyjson_mut_doc *doc, *result_doc;
        yyjson_mut_val* root;

        if (unlikely(client->subscribed)) {
            LOGNOTICE("Client %s %s trying to subscribe twice", client->identity, client->address);
            return;
        }
        result_doc = parse_subscribe(client, client_id, params_val);
        /* Shouldn't happen, sanity check */
        if (unlikely(!result_doc)) {
            LOGWARNING("parse_subscribe returned NULL result_doc");
            return;
        }

        doc = yyjson_mut_doc_new(&ckyyalc);
        root =
            yyjson_mut_pack_val(doc, "{sososn}", "result", yyjson_mut_doc_get_root(result_doc), "id", id_val, "error");
        yyjson_mut_doc_free(result_doc);
        yyjson_mut_doc_set_root(doc, root);

        stratum_add_yysend(sdata, doc, client_id, SM_SUBSCRIBERESULT);
        if (likely(client->subscribed))
            init_client(client, client_id);
        return;
    }

    if (unlikely(cmdmatch(method, "mining.remote"))) {
        char buf[256];

        /* Add this client as a trusted remote node in the connector and
         * drop the client in the stratifier */
        if (!ckpool.trusted[client->server] || ckpool.proxy) {
            LOGNOTICE(
                "Dropping client %s %s trying to authorise as remote node on non trusted server %d", client->identity,
                client->address, client->server);
            connector_drop_client(client_id);
        } else {
            snprintf(buf, 255, "remote=%" PRId64, client_id);
            send_proc(ckpool.connector, buf);
            add_remote_server(sdata, client);
        }
        sprintf(client->identity, "remote:%" PRId64, client_id);
        return;
    }

    if (unlikely(cmdmatch(method, "mining.node"))) {
        char buf[256];

        /* Add this client as a passthrough in the connector and
         * add it to the list of mining nodes in the stratifier */
        if (!ckpool.nodeserver[client->server] || ckpool.proxy) {
            LOGNOTICE(
                "Dropping client %s %s trying to authorise as node on non node server %d", client->identity,
                client->address, client->server);
            connector_drop_client(client_id);
            drop_client(sdata, client_id);
        } else {
            snprintf(buf, 255, "passthrough=%" PRId64, client_id);
            send_proc(ckpool.connector, buf);
            add_mining_node(sdata, client);
            sprintf(client->identity, "node:%" PRId64, client_id);
        }
        return;
    }

    if (unlikely(cmdmatch(method, "mining.passthrough"))) {
        char buf[256];

        if (ckpool.proxy || ckpool.node) {
            LOGNOTICE(
                "Dropping client %s %s trying to connect as passthrough on unsupported server %d", client->identity,
                client->address, client->server);
            connector_drop_client(client_id);
            drop_client(sdata, client_id);
        } else {
            /*Flag this as a passthrough and manage its messages
             * accordingly. No data from this client id should ever
             * come directly back to this stratifier. */
            LOGNOTICE("Adding passthrough client %s %s", client->identity, client->address);
            client->passthrough = true;
            snprintf(buf, 255, "passthrough=%" PRId64, client_id);
            send_proc(ckpool.connector, buf);
            sprintf(client->identity, "passthrough:%" PRId64, client_id);
        }
        return;
    }

    /* We shouldn't really allow unsubscribed users to authorise first but
     * some broken stratum implementations do that and we can handle it. */
    if (cmdmatch(method, "mining.auth")) {
        json_params_t* jp;

        if (unlikely(client->authorised)) {
            LOGINFO("Client %s %s trying to authorise twice", client->identity, client->address);
            return;
        }
        jp = create_yyjson_params(client_id, method_val, params_val, id_val);
        ckmsgq_add(sdata->sauthq, jp);
        return;
    }

    if (cmdmatch(method, "mining.configure")) {
        yyjson_mut_doc* doc;
        yyjson_mut_val* root;

        char version_str[12];

        LOGINFO("Mining configure requested from %s %s", client->identity, client->address);
        sprintf(version_str, "%08x", ckpool.version_mask);

        doc = yyjson_mut_doc_new(&ckyyalc);
        root = yyjson_mut_pack_val(
            doc, "{s{sbss}sosn}", "result", "version-rolling", true, "version-rolling.mask", version_str, "id", id_val,
            "error");
        yyjson_mut_doc_set_root(doc, root);

        stratum_add_yysend(sdata, doc, client_id, SM_CONFIGURE);
        return;
    }

    /* We should only accept requests from subscribed and authed users here
     * on */
    if (!client->subscribed) {
        LOGINFO("Dropping %s from unsubscribed client %s %s", method, client->identity, client->address);
        connector_drop_client(client_id);
        return;
    }

    /* We should only accept authorised requests from here on */
    if (!client->authorised) {
        LOGINFO("Dropping %s from unauthorised client %s %s", method, client->identity, client->address);
        return;
    }

    if (cmdmatch(method, "mining.suggest")) {
        suggest_diff(client, method, params_val);
        return;
    }

    /* Covers both get_transactions and get_txnhashes */
    if (cmdmatch(method, "mining.get")) {
        json_params_t* jp = create_yyjson_params(client_id, method_val, params_val, id_val);

        ckmsgq_add(sdata->stxnq, jp);
        return;
    }

    /* Unhandled message here */
    LOGINFO("Unhandled client %s %s method %s", client->identity, client->address, method);
    return;
}

static void free_smsg(smsg_t* msg) {
    if (msg->doc)
        yyjson_mut_doc_free(msg->doc);
    free(msg);
}

/* Even though we check the results locally in node mode, check the upstream
 * results in case of runs of invalids. */
static void parse_share_result(stratum_instance_t* client, yyjson_mut_val* val) {
    time_t now_t;
    ts_t   now;

    if (likely(yyjson_mut_is_true(val))) {
        client->upstream_invalid = 0;
        return;
    }
    ts_realtime(&now);
    now_t = now.tv_sec;
    if (client->upstream_invalid < client->last_share.tv_sec || !client->upstream_invalid)
        client->upstream_invalid = now_t;
    else if (client->upstream_invalid && client->upstream_invalid < now_t - 150) {
        LOGNOTICE("Client %s upstream rejects for 150s, disconnecting", client->identity);
        connector_drop_client(client->id);
        client->reject = 3;
    }
}

static void parse_diff(stratum_instance_t* client, yyjson_mut_val* val) {
    double diff = yyjson_mut_get_num(yyjson_mut_arr_get(val, 0));

    /* Avoid undefined behaviour casting non finite or out of range
     * values to the int64_t client diff */
    if (unlikely(!isfinite(diff) || diff < 0 || diff > 1e18)) {
        LOGINFO("Discarding invalid diff %lf for client %s", diff, client->identity);
        return;
    }
    LOGINFO("Set client %s to diff %lf", client->identity, diff);
    client->diff = diff;
}

static void parse_subscribe_result(stratum_instance_t* client, yyjson_mut_val* val) {
    int len;

    strncpy(client->enonce1, yyjson_mut_get_str(yyjson_mut_arr_get(val, 1)) ?: "", 16);
    client->enonce1[16] = '\0';
    len = strlen(client->enonce1) / 2;
    hex2bin(client->enonce1bin, client->enonce1, len);
    memcpy(&client->enonce1_64, client->enonce1bin, 8);
    LOGINFO("Client %s got enonce1 %lx string %s", client->identity, client->enonce1_64, client->enonce1);
}

static void parse_authorise_result(sdata_t* sdata, stratum_instance_t* client, yyjson_mut_val* val) {
    if (!yyjson_mut_is_true(val)) {
        LOGNOTICE("Client %s was not authorised upstream, dropping", client->identity);
        client->authorised = false;
        connector_drop_client(client->id);
        drop_client(sdata, client->id);
    } else
        LOGINFO("Client %s was authorised upstream", client->identity);
}

static int node_msg_type(yyjson_mut_val* val) {
    const char* method;
    int         i, ret = -1;

    if (!val)
        goto out;
    method = yyjson_mut_get_str(yyjson_mut_obj_get(val, "node.method"));
    if (method) {
        for (i = 0; i < SM_NONE; i++) {
            if (!strcmp(method, stratum_msgs[i])) {
                ret = i;
                break;
            }
        }
        yyjson_mut_obj_remove_key(val, "node.method");
    } else
        method = yyjson_mut_get_str(yyjson_mut_obj_get(val, "method"));

    if (ret < 0 && method) {
        if (!safecmp(method, "mining.submit"))
            ret = SM_SHARE;
        else if (!safecmp(method, "mining.notify"))
            ret = SM_UPDATE;
        else if (!safecmp(method, "mining.subscribe"))
            ret = SM_SUBSCRIBE;
        else if (cmdmatch(method, "mining.auth"))
            ret = SM_AUTH;
        else if (cmdmatch(method, "mining.get"))
            ret = SM_TXNS;
        else if (cmdmatch(method, "mining.suggest_difficulty"))
            ret = SM_SUGGESTDIFF;
        else
            ret = SM_NONE;
    }
out:
    return ret;
}

static user_instance_t* generate_remote_user(const char* workername) {
    char *           base_username = strdupa(workername), *username;
    sdata_t*         sdata = ckpool.sdata;
    bool             new_user = false;
    user_instance_t* user;
    int              len;

    username = strsep(&base_username, "._");
    if (!username || !strlen(username))
        username = base_username;
    len = strlen(username);
    if (unlikely(len > 127))
        username[127] = '\0';

    user = get_create_user(sdata, username, &new_user);

    if (!ckpool.proxy && (new_user || !user->btcaddress)) {
        /* Is this a btc address based username? */
        if (generator_checkaddr(username, &user->script, &user->segwit)) {
            user->btcaddress = true;
            user->txnlen = address_to_txn(user->txnbin, username, user->script, user->segwit);
        }
    }
    if (new_user) {
        LOGNOTICE("Added new remote user %s%s", username, user->btcaddress ? " as address based registration" : "");
    }

    return user;
}

static void parse_remote_share(sdata_t* sdata, yyjson_mut_val* val, const char* buf) {
    worker_instance_t* worker;
    const char*        workername;
    double             diff, sdiff = 0;
    user_instance_t*   user;
    tv_t               now_t;

    workername = yyjson_mut_get_str(yyjson_mut_obj_get(val, "workername"));
    if (unlikely(!workername)) {
        LOGWARNING("Failed to get workername from remote message %s", buf);
        return;
    }
    if (unlikely(!yyjson_mut_obj_get_double(&diff, val, "diff") || diff < 1 || !isfinite(diff))) {
        LOGWARNING("Unable to parse valid diff from remote message %s", buf);
        return;
    }
    yyjson_mut_obj_get_double(&sdiff, val, "sdiff");
    /* A non finite sdiff would permanently poison best share values */
    if (unlikely(!isfinite(sdiff)))
        sdiff = 0;
    user = generate_remote_user(workername);
    user->authorised = true;
    worker = get_worker(sdata, user, workername);
    check_best_diff(sdata, user, worker, sdiff, NULL);

    mutex_lock(&sdata->uastats_lock);
    sdata->stats.unaccounted_shares++;
    sdata->stats.unaccounted_diff_shares += diff;
    mutex_unlock(&sdata->uastats_lock);

    worker->shares += diff;
    user->shares += diff;
    tv_time(&now_t);

    decay_worker(worker, diff, &now_t);
    copy_tv(&worker->last_share, &now_t);
    worker->idle = false;

    decay_user(user, diff, &now_t);
    copy_tv(&user->last_share, &now_t);

    LOGINFO("Added %.0lf remote shares to worker %s", diff, workername);
}

static void parse_remote_shareerr(yyjson_mut_val* val, const char* buf) {
    const char* workername;

    workername = yyjson_mut_get_str(yyjson_mut_obj_get(val, "workername"));
    if (unlikely(!workername)) {
        LOGWARNING("Failed to find workername in parse_remote_shareerr %s", buf);
        return;
    }
    /* Return value ignored */
    generate_remote_user(workername);
}

static void send_yyauth_response(
    sdata_t*        sdata,
    const int64_t   client_id,
    const bool      ret,
    yyjson_mut_val* id_val,
    yyjson_mut_val* err_val) {
    yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val* root;

    if (!err_val)
        root = yyjson_mut_pack_val(doc, "{sbsnso}", "result", ret, "error", "id", id_val);
    else
        root = yyjson_mut_pack_val(doc, "{sbsoso}", "result", ret, "error", err_val, "id", id_val);
    yyjson_mut_doc_set_root(doc, root);
    stratum_add_yysend(sdata, doc, client_id, SM_AUTHRESULT);
}

static void send_auth_success(sdata_t* sdata, stratum_instance_t* client) {
    char* buf;

    ASPRINTF(&buf, "Authorised, welcome to %s %s!", ckpool.name, client->user_instance->username);
    stratum_send_message(sdata, client, buf);
    free(buf);
}

static void send_auth_failure(sdata_t* sdata, stratum_instance_t* client) {
    stratum_send_message(sdata, client, "Failed authorisation :(");
}

/* For finding a client by its virtualid instead of client->id. This is an
 * inefficient lookup but only occurs once on parsing a remote auth from the
 * upstream pool on passthrough subclients. */
static stratum_instance_t* ref_instance_by_virtualid(sdata_t* sdata, int64_t* client_id) {
    stratum_instance_t *client, *ret = NULL;

    ck_wlock(&sdata->instance_lock);
    for (client = sdata->stratum_instances; client; client = client->hh.next) {
        if (likely(client->virtualid != *client_id))
            continue;
        if (likely(!client->dropped)) {
            ret = client;
            __inc_instance_ref(ret);
            /* Replace the client_id with the correct one, allowing
             * us to send the response to the correct client */
            *client_id = client->id;
        }
        break;
    }
    ck_wunlock(&sdata->instance_lock);

    return ret;
}

void parse_upstream_auth(yyjson_mut_val* val) {
    yyjson_mut_val *    id_val = NULL, *err_val = NULL;
    sdata_t*            sdata = ckpool.sdata;
    stratum_instance_t* client;
    bool                ret, warn = false;
    int64_t             client_id;

    id_val = yyjson_mut_obj_get(val, "id");
    if (unlikely(!id_val))
        goto out;
    if (unlikely(!yyjson_mut_obj_get_int64(&client_id, val, "client_id")))
        goto out;
    if (unlikely(!yyjson_mut_obj_get_bool(&ret, val, "result")))
        goto out;
    err_val = yyjson_mut_obj_get(val, "error");
    client = ref_instance_by_id(sdata, client_id);
    /* Is this client_id a virtualid from a passthrough subclient */
    if (!client)
        client = ref_instance_by_virtualid(sdata, &client_id);
    if (!client) {
        LOGINFO("Failed to find client id %" PRId64 " in parse_upstream_auth", client_id);
        goto out;
    }
    if (ret)
        send_auth_success(sdata, client);
    else
        send_auth_failure(sdata, client);
    send_yyauth_response(sdata, client_id, ret, id_val, err_val);
    client_auth(client, client->user_instance, ret);
    dec_instance_ref(sdata, client);
out:
    if (unlikely(warn)) {
        char* s = yyjson_mut_val_write(val, 0, NULL);

        LOGWARNING("Failed to get valid upstream result in parse_upstream_auth %s", s);
        free(s);
    }
}

void parse_upstream_workinfo(yyjson_mut_val* val) {
    add_node_base(val, true, 0);
}

#define parse_remote_workinfo(val, client_id) add_node_base(val, true, client_id)

static void
parse_remote_auth(sdata_t* sdata, yyjson_mut_val* val, stratum_instance_t* remote, const int64_t remote_id) {
    yyjson_mut_val *    params, *method, *id_val;
    stratum_instance_t* client;
    json_params_t*      jp;
    int64_t             client_id;

    if (ckpool.btcsolo) {
        LOGWARNING("Got remote auth request in btcsolo mode, ignoring!");
        return;
    }
    yyjson_mut_obj_get_int64(&client_id, val, "clientid");
    /* Encode remote server client_id into remote client's id */
    client_id = (remote_id << 32) | (client_id & 0xffffffffll);
    id_val = yyjson_mut_obj_get(val, "id");
    method = yyjson_mut_obj_get(val, "method");
    params = yyjson_mut_obj_get(val, "params");
    jp = create_yyjson_params(client_id, method, params, id_val);

    /* This is almost certainly the first time we'll see this client_id so
     * create a new stratum instance temporarily just for auth with a plan
     * to drop the client id locally once we finish with it */
    ck_wlock(&sdata->instance_lock);
    client = __instance_by_id(sdata, client_id);
    if (likely(!client))
        client = __stratum_add_instance(client_id, remote->address, remote->server);
    client->remote = true;
    yyjson_mut_obj_strdup(&client->useragent, val, "useragent");
    yyjson_mut_obj_strncpy(client->enonce1, val, "enonce1", sizeof(client->enonce1));
    yyjson_mut_obj_strncpy(client->address, val, "address", sizeof(client->address));
    ck_wunlock(&sdata->instance_lock);

    ckmsgq_add(sdata->sauthq, jp);
}

/* Get the remote worker count once per minute from all the remote servers */
static void parse_remote_workers(sdata_t* sdata, yyjson_mut_val* val, const char* buf) {
    user_instance_t* user;
    const char*      username;
    int              workers;

    username = yyjson_mut_get_str(yyjson_mut_obj_get(val, "username"));
    if (unlikely(!username)) {
        LOGWARNING("Failed to get username from remote message %s", buf);
        return;
    }
    user = get_user(sdata, username);
    if (unlikely(!yyjson_mut_obj_get_int(&workers, val, "workers"))) {
        LOGWARNING("Failed to get workers from remote message %s", buf);
        return;
    }
    user->remote_workers += workers;
    LOGDEBUG("Adding %d remote workers to user %s", workers, username);
}

/* Attempt to submit a remote block locally by recreating it from its workinfo */
static void
parse_remote_block(sdata_t* sdata, yyjson_mut_doc* doc, yyjson_mut_val* val, const char* buf, const int64_t client_id) {
    const char *    workername, *name, *coinbasehex, *swaphex, *cnfrm;
    yyjson_mut_doc* res;
    workbase_t*     wb = NULL;
    int64_t         reward = 0;
    double          diff = 0;
    int             height = 0;
    int64_t         id = 0;
    int             cblen = 0;
    char*           msg;

    name = yyjson_mut_get_str(yyjson_mut_obj_get(val, "name"));
    if (!name)
        goto out_add;

    /* If this is the confirm block message don't try to resubmit it */
    cnfrm = yyjson_mut_get_str(yyjson_mut_obj_get(val, "confirmed"));
    if (cnfrm && cnfrm[0] == '1') {
        if (ckpool.logshares_db) {
            const char* hash = yyjson_mut_get_str(yyjson_mut_obj_get(val, "blockhash"));

            if (hash)
                db_update_block_status(sdata, hash, true, false);
        }
        goto out_add;
    }

    yyjson_mut_obj_get_int64(&id, val, "workinfoid");
    coinbasehex = yyjson_mut_get_str(yyjson_mut_obj_get(val, "coinbasehex"));
    swaphex = yyjson_mut_get_str(yyjson_mut_obj_get(val, "swaphex"));
    yyjson_mut_obj_get_int(&cblen, val, "cblen");
    yyjson_mut_obj_get_double(&diff, val, "diff");
    yyjson_mut_obj_get_int(&height, val, "height");
    yyjson_mut_obj_get_int64(&reward, val, "reward");

    /* cblen is the full assembled coinbase which is bounded by its two
     * components, so reject anything larger as corrupt or malicious */
    if (likely(id && coinbasehex && swaphex && cblen > 0 && cblen <= 2 * MAX_COINBASE_LEN))
        wb = get_remote_workbase(sdata, id, client_id);

    if (unlikely(!wb))
        LOGWARNING("Inadequate data locally to attempt submit of remote block");
    else {
        uchar swap[80], hash[32], hash1[32], flip32[32];
        char *coinbase = alloca(cblen), *gbt_block;
        char  blockhash[68];

        LOGWARNING("Possible remote block solve diff %lf !", diff);
        /* Same as the node block path: refuse to assemble/submit on
         * corrupt hex so uninitialised alloca is never hashed in. */
        if (unlikely(!hex2bin(coinbase, coinbasehex, cblen) || !hex2bin(swap, swaphex, 80))) {
            LOGWARNING("Invalid hex in remote block for workinfoid %" PRId64, id);
            put_remote_workbase(sdata, wb);
            goto out_add;
        }
        sha256(swap, 80, hash1);
        sha256(hash1, 32, hash);
        gbt_block = process_block(wb, coinbase, cblen, swap, hash, flip32, blockhash);
        /* Note nodes use jobid of the mapped_id instead of workinfoid */
        yyjson_mut_obj_put(val, yyjson_mut_str(doc, "jobid"), yyjson_mut_sint(doc, wb->mapped_id));
        send_nodes_block(sdata, doc, client_id);

        if (ckpool.logshares_db) {
            const char* rworker = yyjson_mut_get_str(yyjson_mut_obj_get(val, "workername"));
            const char* ruser = yyjson_mut_get_str(yyjson_mut_obj_get(val, "username"));

            db_add_block(sdata, height, blockhash, false, rworker ?: "", ruser ?: "", diff, reward);
        }

        /* We rely on the remote server to give us the ID_BLOCK
         * responses, so only use this response to determine if we
         * should reset the best shares. */
        if (local_block_submit(gbt_block, flip32, wb->height)) {
            block_share_summary(sdata);
            reset_bestshares(sdata);
        }
        put_remote_workbase(sdata, wb);
    }

    workername = yyjson_mut_get_str(yyjson_mut_obj_get(val, "workername"));
    if (unlikely(!workername)) {
        LOGWARNING("Failed to get workername from remote message %s", buf);
        workername = "";
    }
    if (unlikely(!yyjson_mut_obj_get_int(&height, val, "height")))
        LOGWARNING("Failed to get height from remote message %s", buf);
    ASPRINTF(&msg, "Block %d solved by %s @ %s!", height, workername, name);
    LOGWARNING("%s", msg);
    stratum_broadcast_message(sdata, msg);
    free(msg);
out_add:
    /* Make a duplicate for use downstream */
    res = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_doc_set_root(res, yyjson_mut_val_mut_copy(res, val));
    remap_workinfo_id(sdata, res, yyjson_mut_doc_get_root(res), client_id);
    if (!ckpool.remote)
        downstream_yydoc(sdata, res, client_id, SSEND_PREPEND);

    yyjson_mut_doc_free(res);
}

void parse_upstream_block(yyjson_mut_doc* doc, yyjson_mut_val* val) {
    char*    buf;
    sdata_t* sdata = ckpool.sdata;

    buf = yyjson_mut_val_write(val, 0, NULL);
    parse_remote_block(sdata, doc, val, buf, 0);
    free(buf);
}

static void send_remote_pong(sdata_t* sdata, stratum_instance_t* client) {
    yyjson_mut_doc* doc;

    doc = yyjson_mut_pack("{ss}", "method", "pong");
    stratum_add_yysend(sdata, doc, client->id, SM_PONG);
}

static void add_node_txns(sdata_t* sdata, yyjson_mut_val* val) {
    yyjson_mut_val *    txn_array, *txn_val;
    yyjson_mut_arr_iter iter;
    txntable_t*         txns = NULL;
    int                 added = 0;

    txn_array = yyjson_mut_obj_get(val, "transaction");
    yyjson_mut_arr_iter_init(txn_array, &iter);

    while ((txn_val = yyjson_mut_arr_iter_next(&iter)) != NULL) {
        const char *hash, *data;

        data = yyjson_mut_get_str(yyjson_mut_obj_get(txn_val, "data"));
        hash = yyjson_mut_get_str(yyjson_mut_obj_get(txn_val, "hash"));
        if (unlikely(!data || !hash)) {
            LOGERR("Failed to get hash/data in add_node_txns");
            continue;
        }
        /* Txn hashes are fixed 64 hex char values copied into fixed
         * size arrays so reject anything else */
        if (unlikely(strlen(hash) != 64)) {
            LOGERR("Invalid hash length in add_node_txns");
            continue;
        }

        if (add_txn(sdata, &txns, hash, data, false))
            added++;
    }

    if (added)
        update_txns(sdata, txns, false);
}

void parse_remote_txns(yyjson_mut_val* val) {
    add_node_txns(ckpool.sdata, val);
}

static yyjson_mut_val* get_hash_transactions(sdata_t* sdata, yyjson_mut_doc* doc, yyjson_mut_val* hashes) {
    yyjson_mut_val *    txn_array = yyjson_mut_arr(doc), *arr_val, *hash_val;
    yyjson_mut_arr_iter iter;

    ck_rlock(&sdata->txn_lock);
    yyjson_mut_arr_iter_init(hashes, &iter);
    while ((hash_val = yyjson_mut_arr_iter_next(&iter)) != NULL) {
        const char* hash = yyjson_mut_get_str(hash_val);
        txntable_t* txn;

        if (unlikely(!hash))
            continue;
        HASH_FIND_STR(sdata->txns, hash, txn);
        if (!txn)
            continue;
        arr_val = yyjson_mut_pack_val(doc, "{ss,ss}", "hash", hash, "data", txn->data);
        yyjson_mut_arr_append(txn_array, arr_val);
    }
    ck_runlock(&sdata->txn_lock);

    return txn_array;
}

static yyjson_mut_doc* get_reqtxns(sdata_t* sdata, yyjson_mut_val* val, bool downstream) {
    yyjson_mut_val* hashes = yyjson_mut_obj_get(val, "hash");
    yyjson_mut_doc *doc, *ret = NULL;
    yyjson_mut_val *txns, *root;
    int             requested, found;

    if (unlikely(!hashes) || !yyjson_mut_is_arr(hashes))
        goto out;
    requested = yyjson_mut_arr_size(hashes);
    if (unlikely(!requested))
        goto out;

    doc = yyjson_mut_doc_new(&ckyyalc);
    txns = get_hash_transactions(sdata, doc, hashes);
    found = yyjson_mut_arr_size(txns);
    if (found) {
        root = yyjson_mut_obj(doc);
        yyjson_mut_obj_add_strcpy(doc, root, "method", stratum_msgs[SM_TRANSACTIONS]);
        yyjson_mut_obj_add_val(doc, root, "transaction", txns);
        yyjson_mut_doc_set_root(doc, root);
        ret = doc;
        LOGINFO("Sending %d found of %d requested txns %s", found, requested, downstream ? "downstream" : "upstream");
    } else
        yyjson_mut_doc_free(doc);
out:
    return ret;
}

static void parse_remote_reqtxns(sdata_t* sdata, yyjson_mut_val* val, const int64_t client_id) {
    yyjson_mut_doc* ret = get_reqtxns(sdata, val, true);

    if (!ret)
        return;
    stratum_add_yysend(sdata, ret, client_id, SM_TRANSACTIONS);
}

void parse_upstream_reqtxns(yyjson_mut_val* val) {
    yyjson_mut_doc* ret = get_reqtxns(ckpool.sdata, val, false);

    if (!ret)
        return;
    upstream_yyjson(ret);
    yyjson_mut_doc_free(ret);
}

static void parse_trusted_msg(sdata_t* sdata, yyjson_mut_doc* doc, yyjson_mut_val* val, stratum_instance_t* client) {
    char*       buf = yyjson_mut_val_write(val, 0, NULL);
    const char* method;

    LOGDEBUG("Got remote message %s", buf);
    method = yyjson_mut_get_str(yyjson_mut_obj_get(val, "method"));
    if (unlikely(!method)) {
        LOGWARNING("Failed to get method from remote message %s", buf);
        goto out;
    }

    if (likely(!safecmp(method, stratum_msgs[SM_SHARE])))
        parse_remote_share(sdata, val, buf);
    else if (!safecmp(method, stratum_msgs[SM_TRANSACTIONS]))
        add_node_txns(sdata, val);
    else if (!safecmp(method, stratum_msgs[SM_WORKINFO]))
        parse_remote_workinfo(val, client->id);
    else if (!safecmp(method, stratum_msgs[SM_AUTH]))
        parse_remote_auth(sdata, val, client, client->id);
    else if (!safecmp(method, stratum_msgs[SM_SHAREERR]))
        parse_remote_shareerr(val, buf);
    else if (!safecmp(method, stratum_msgs[SM_BLOCK]))
        parse_remote_block(sdata, doc, val, buf, client->id);
    else if (!safecmp(method, stratum_msgs[SM_REQTXNS]))
        parse_remote_reqtxns(sdata, val, client->id);
    else if (!safecmp(method, "workers"))
        parse_remote_workers(sdata, val, buf);
    else if (!safecmp(method, "ping"))
        send_remote_pong(sdata, client);
    else
        LOGWARNING("unrecognised trusted message %s", buf);
out:
    free(buf);
}

/* Entered with client holding ref count */
static void node_client_msg(yyjson_mut_val* val, stratum_instance_t* client) {
    yyjson_mut_val *params, *method, *res_val, *id_val;
    int             msg_type = node_msg_type(val);
    sdata_t*        sdata = ckpool.sdata;
    yyjson_mut_doc* tmpdoc;
    json_params_t*  jp;
    char*           buf = NULL;

    if (msg_type < 0) {
        buf = yyjson_mut_val_write(val, 0, NULL);
        LOGERR("Missing client %s node method from %s", client->identity, buf);
        goto out;
    }
    LOGDEBUG("Got client %s node method %d:%s", client->identity, msg_type, stratum_msgs[msg_type]);
    id_val = yyjson_mut_obj_get(val, "id");
    method = yyjson_mut_obj_get(val, "method");
    params = yyjson_mut_obj_get(val, "params");
    res_val = yyjson_mut_obj_get(val, "result");
    switch (msg_type) {
        yyjson_mut_doc* err_doc;
        case SM_SHARE:
            jp = create_yyjson_params(client->id, method, params, id_val);
            if (!stratifier_queue_share_work(false, jp))
                discard_json_params(jp);
            break;
        case SM_SHARERESULT:
            parse_share_result(client, res_val);
            break;
        case SM_DIFF:
            parse_diff(client, params);
            break;
        case SM_SUBSCRIBE:
            tmpdoc = parse_subscribe(client, client->id, params);
            if (tmpdoc)
                yyjson_mut_doc_free(tmpdoc);
            break;
        case SM_SUBSCRIBERESULT:
            parse_subscribe_result(client, res_val);
            break;
        case SM_AUTH:
            err_doc = NULL;
            parse_authorise(client, params, &err_doc);
            break;
        case SM_AUTHRESULT:
            parse_authorise_result(sdata, client, res_val);
            break;
        case SM_NONE:
            buf = yyjson_mut_val_write(val, 0, NULL);
            LOGNOTICE("Unrecognised method from client %s :%s", client->identity, buf);
            break;
        default:
            break;
    }
out:
    free(buf);
}

static void parse_node_msg(sdata_t* sdata, yyjson_mut_val* val) {
    int msg_type = node_msg_type(val);

    if (msg_type < 0) {
        char* buf = yyjson_mut_val_write(val, 0, NULL);

        LOGERR("Missing node method from %s", buf);
        free(buf);
        return;
    }
    LOGDEBUG("Got node method %d:%s", msg_type, stratum_msgs[msg_type]);
    switch (msg_type) {
        case SM_TRANSACTIONS:
            add_node_txns(sdata, val);
            break;
        case SM_WORKINFO:
            add_node_base(val, false, 0);
            break;
        case SM_BLOCK:
            submit_node_block(sdata, val);
            break;
        default:
            break;
    }
}

/* Entered with client holding ref count */
static void parse_instance_msg(sdata_t* sdata, smsg_t* msg, stratum_instance_t* client) {
    yyjson_mut_val *root, *id_val, *method, *params;
    int64_t         client_id = msg->client_id;
    yyjson_mut_doc* doc;
    int             delays = 0;

    if (client->reject == 3) {
        LOGINFO("Dropping client %s %s tagged for lazy invalidation", client->identity, client->address);
        connector_drop_client(client_id);
        return;
    }

    doc = msg->doc;
    root = yyjson_mut_doc_get_root(doc);

    /* Return back the same id_val even if it's null or not existent. */
    id_val = yyjson_mut_obj_get(root, "id");

    method = yyjson_mut_obj_get(root, "method");
    if (unlikely(!method)) {
        yyjson_mut_val* res_val = yyjson_mut_obj_get(root, "result");

        /* Is this a spurious result or ping response? */
        if (res_val) {
            const char* result = yyjson_mut_get_str(res_val);

            if (!safecmp(result, "pong"))
                LOGDEBUG("Received pong from client %s", client->identity);
            else
                LOGDEBUG("Received spurious response %s from client %s", result ? result : "", client->identity);
            return;
        }
        send_yyjson_err(sdata, client_id, id_val, "-3:method not found");
        return;
    }
    if (unlikely(!yyjson_mut_is_str(method))) {
        send_yyjson_err(sdata, client_id, id_val, "-1:method is not string");
        return;
    }
    params = yyjson_mut_obj_get(root, "params");
    if (unlikely(!params)) {
        send_yyjson_err(sdata, client_id, id_val, "-1:params not found");
        return;
    }

    /* At startup we block until there's a current workbase otherwise we
     * will reject miners with the initialising message. A slightly delayed
     * response to subscribe is better tolerated. */
    while (unlikely(!ckpool.proxy && !sdata->current_workbase)) {
        cksleep_ms(100);
        if (!(++delays % 50))
            LOGWARNING("%d Second delay waiting for bitcoind at startup", delays / 10);
    }

    parse_method(sdata, client, client_id, id_val, method, params);
}

static void srecv_process(smsg_t* msg) {
    char                address[INET6_ADDRSTRLEN], *buf = NULL;
    bool                noid = false, dropped = false;
    yyjson_mut_val *    root, *val;
    sdata_t*            sdata = ckpool.sdata;
    stratum_instance_t* client;
    yyjson_mut_doc*     doc;
    int                 server;

    if (unlikely(!msg)) {
        LOGWARNING("srecv_process received NULL msg!");
        return;
    }

    doc = msg->doc;
    if (unlikely(!doc)) {
        LOGWARNING("srecv_process received NULL doc!");
        goto out;
    }

    root = yyjson_mut_doc_get_root(doc);
    if (unlikely(!root)) {
        LOGWARNING("srecv_process received NULL root!");
        goto out;
    }

    val = yyjson_mut_obj_get(root, "client_id");
    if (unlikely(!val)) {
        if (ckpool.node)
            parse_node_msg(sdata, root);
        else {
            buf = yyjson_mut_write(doc, 0, NULL);
            LOGWARNING("Failed to extract client_id from connector json smsg %s", buf);
        }
        goto out;
    }

    /* These three keys are generated by the connector, which strips any
     * copy supplied by the remote end first. Validate them regardless:
     * passthrough and node peers also feed this path, and an unchecked
     * value here is a pre-auth crash. */
    if (unlikely(!yyjson_mut_is_int(val))) {
        buf = yyjson_mut_write(doc, 0, NULL);
        LOGWARNING("Non integer client_id in connector json smsg %s", buf);
        goto out;
    }
    msg->client_id = yyjson_mut_get_sint(val);
    if (unlikely(msg->client_id < 0)) {
        LOGWARNING("Negative client_id %" PRId64 " in connector json smsg", msg->client_id);
        goto out;
    }
    yyjson_mut_obj_clear(val);

    val = yyjson_mut_obj_get(root, "address");
    if (unlikely(!val)) {
        buf = yyjson_mut_write(doc, 0, NULL);
        LOGWARNING("Failed to extract address from connector json smsg %s", buf);
        goto out;
    }
    {
        const char* addr = yyjson_mut_get_str(val);

        if (unlikely(!addr || strlen(addr) >= sizeof(address))) {
            LOGWARNING("Invalid address for client %" PRId64 " in connector json smsg", msg->client_id);
            connector_drop_client(msg->client_id);
            goto out;
        }
        strcpy(address, addr);
    }
    yyjson_mut_obj_clear(val);

    val = yyjson_mut_obj_get(root, "server");
    if (unlikely(!val)) {
        buf = yyjson_mut_write(doc, 0, NULL);
        LOGWARNING("Failed to extract server from connector json smsg %s", buf);
        goto out;
    }
    if (unlikely(!yyjson_mut_is_int(val))) {
        LOGWARNING("Non integer server for client %" PRId64 " in connector json smsg", msg->client_id);
        connector_drop_client(msg->client_id);
        goto out;
    }
    server = yyjson_mut_get_sint(val);
    if (unlikely(server < 0 || server >= ckpool.serverurls)) {
        LOGWARNING("Out of range server %d for client %" PRId64 " in connector json smsg", server, msg->client_id);
        connector_drop_client(msg->client_id);
        goto out;
    }
    yyjson_mut_obj_clear(val);

    /* Parse the message here */
    ck_wlock(&sdata->instance_lock);
    client = __instance_by_id(sdata, msg->client_id);
    /* If client_id instance doesn't exist yet, create one */
    if (unlikely(!client)) {
        noid = true;
        client = __stratum_add_instance(msg->client_id, address, server);
    } else if (unlikely(client->dropped))
        dropped = true;
    if (likely(!dropped))
        __inc_instance_ref(client);
    ck_wunlock(&sdata->instance_lock);

    if (unlikely(dropped)) {
        /* Client may be NULL here */
        LOGNOTICE("Stratifier skipped dropped instance %" PRId64 " message from server %d", msg->client_id, server);
        connector_drop_client(msg->client_id);
        goto out;
    }
    if (unlikely(noid))
        LOGINFO("Stratifier added instance %s server %d", client->identity, server);

    if (client->trusted)
        parse_trusted_msg(sdata, doc, root, client);
    else if (ckpool.node)
        node_client_msg(root, client);
    else
        parse_instance_msg(sdata, msg, client);
    dec_instance_ref(sdata, client);
out:
    free_smsg(msg);
    free(buf);
}

void _stratifier_add_yyrecv(yyjson_mut_doc* doc, const char* file, const char* func, const int line) {
    sdata_t* sdata;
    smsg_t*  msg;

    if (unlikely(!doc)) {
        LOGWARNING("_stratifier_add_yyrecv received NULL doc from %s %s:%d", file, func, line);
        return;
    }
    sdata = ckpool.sdata;
    msg = ckzalloc(sizeof(smsg_t));
    msg->doc = doc;
    ckmsgq_add(sdata->srecvs, msg);
}

static void ssend_process(smsg_t* msg) {
    yyjson_mut_doc* doc = msg->doc;
    yyjson_mut_val* root;

    if (unlikely(!doc)) {
        LOGERR("Sent null json msg to stratum_sender");
        free(msg);
        return;
    }

    /* Add client_id to the json message and send it to the
     * connector process to be delivered */
    root = yyjson_mut_doc_get_root(doc);
    yyjson_mut_obj_add_sint(doc, root, "client_id", msg->client_id);
    connector_add_yymessage(doc);

    /* The connector will free msg->doc */
    free(msg);
}

static void discard_json_params(json_params_t* jp) {
    if (jp->doc)
        yyjson_mut_doc_free(jp->doc);
    free(jp);
}

/* Enqueue SV1 (json_params) or SV2 share job onto sshareq. */
bool stratifier_queue_share_work(bool is_sv2, void* payload) {
    sdata_t*            sdata = ckpool.sdata;
    struct shareq_item* si;

    if (!sdata || !sdata->sshareq || !payload)
        return false;
    si = ckzalloc(sizeof(*si));
    si->is_sv2 = is_sv2;
    si->payload = payload;
    if (!ckmsgq_add(sdata->sshareq, si)) {
        free(si);
        return false;
    }
    return true;
}

static void sshare_process_sv1(json_params_t* jp) {
    enum share_err      err_code = SE_NONE;
    stratum_instance_t* client;
    sdata_t*            sdata = ckpool.sdata;
    yyjson_mut_val*     root;
    yyjson_mut_doc*     doc;
    int64_t             client_id;
    bool                result;

    client_id = jp->client_id;

    client = ref_instance_by_id(sdata, client_id);
    if (unlikely(!client)) {
        LOGINFO("Share processor failed to find client id %" PRId64 " in hashtable!", client_id);
        goto out;
    }
    if (unlikely(!client->authorised)) {
        LOGDEBUG("Client %s no longer authorised to submit shares", client->identity);
        goto out_decref;
    }
    result = parse_submit(client, jp->yyparams, &err_code);
    doc = yyjson_mut_doc_new(&ckyyalc);
    if (err_code != SE_NONE) {
        root = yyjson_mut_pack_val(
            doc, "{sns[isn]so}", "result", "error", SHARE_ERR_CODE(err_code), SHARE_ERR(err_code), "id", jp->yyid_val);
    } else {
        root = yyjson_mut_pack_val(doc, "{sbsnso}", "result", result, "error", "id", jp->yyid_val);
    }
    yyjson_mut_doc_set_root(doc, root);
    stratum_add_yysend(sdata, doc, client_id, SM_SHARERESULT);
out_decref:
    dec_instance_ref(sdata, client);
out:
    discard_json_params(jp);
}

static void sshare_process(struct shareq_item* si) {
    if (!si)
        return;
#ifdef HAVE_SV2
    if (si->is_sv2) {
        sv2_strat_process_share_job(si->payload);
        free(si);
        return;
    }
#endif
    sshare_process_sv1(si->payload);
    free(si);
}

/* As ref_instance_by_id but only returns clients not authorising or authorised,
 * and sets the authorising flag */
static stratum_instance_t* preauth_ref_instance_by_id(sdata_t* sdata, const int64_t id) {
    stratum_instance_t* client;

    ck_wlock(&sdata->instance_lock);
    client = __instance_by_id(sdata, id);
    if (client) {
        if (client->dropped || client->authorising || client->authorised)
            client = NULL;
        else {
            __inc_instance_ref(client);
            client->authorising = true;
        }
    }
    ck_wunlock(&sdata->instance_lock);

    return client;
}

/* Send the auth upstream in trusted remote mode, allowing the connector to
 * asynchronously receive the response and return the auth response. */
static void upstream_auth(stratum_instance_t* client, json_params_t* jp) {
    yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val* val = yyjson_mut_obj(doc);

    yyjson_mut_doc_set_root(doc, val);

    yyjson_mut_obj_add_val(doc, val, "params", yyjson_mut_val_mut_copy(doc, jp->yyparams));
    yyjson_mut_obj_add_val(doc, val, "id", yyjson_mut_val_mut_copy(doc, jp->yyid_val));
    yyjson_mut_obj_add_strcpy(doc, val, "method", stratum_msgs[SM_AUTH]);

    yyjson_mut_obj_add_strcpy(doc, val, "useragent", client->useragent ?: "");
    yyjson_mut_obj_add_strcpy(doc, val, "enonce1", client->enonce1 ?: "");
    yyjson_mut_obj_add_strcpy(doc, val, "address", client->address);
    yyjson_mut_obj_add_int(doc, val, "clientid", client->virtualid);
    upstream_yyjson(doc);
    yyjson_mut_doc_free(doc);
}

static void sauth_process(json_params_t* jp) {
    yyjson_mut_doc*     err_doc = NULL;
    yyjson_mut_val*     err_val;
    sdata_t*            sdata = ckpool.sdata;
    stratum_instance_t* client;
    int64_t             mindiff, client_id;
    bool                ret;

    client_id = jp->client_id;

    client = preauth_ref_instance_by_id(sdata, client_id);
    if (unlikely(!client)) {
        LOGINFO("Authoriser failed to find client id %" PRId64 " in hashtable!", client_id);
        goto out_noclient;
    }

    ret = parse_authorise(client, jp->yyparams, &err_doc);
    if (ret) {
        /* So far okay in remote mode, remainder to be done by upstream
         * pool */
        if (ckpool.remote && !ckpool.btcsolo) {
            upstream_auth(client, jp);
            goto out;
        }
        send_auth_success(sdata, client);
    } else
        send_auth_failure(sdata, client);
    if (!err_doc) {
        err_doc = yyjson_mut_doc_new(&ckyyalc);
        err_val = yyjson_mut_null(err_doc);
    } else
        err_val = yyjson_mut_doc_get_root(err_doc);
    send_yyauth_response(sdata, client_id, ret, jp->yyid_val, err_val);
    yyjson_mut_doc_free(err_doc);
    if (!ret)
        goto out;

    if (client->remote) {
        /* We don't need to keep a record of clients on remote trusted
         * servers after auth'ing them. */
        client->dropped = true;
        goto out;
    }

    /* Update the client now if they have set a valid mindiff different
     * from the startdiff. suggest_diff overrides worker mindiff */
    if (client->suggest_diff)
        mindiff = client->suggest_diff;
    else
        mindiff = client->worker_instance->mindiff;
    if (mindiff) {
        mindiff = MAX(ckpool.mindiff, mindiff);
        if (mindiff != client->diff) {
            client->diff = mindiff;
            stratum_send_diff(sdata, client);
        }
    }

out:
    dec_instance_ref(sdata, client);
out_noclient:
    discard_json_params(jp);
}

static int transactions_by_jobid(sdata_t* sdata, const int64_t id) {
    workbase_t* wb;
    int         ret = -1;

    ck_rlock(&sdata->workbase_lock);
    HASH_FIND_I64(sdata->workbases, &id, wb);
    if (wb)
        ret = wb->txns;
    ck_runlock(&sdata->workbase_lock);

    return ret;
}

static char* txnhashes_by_jobid(sdata_t* sdata, const int64_t id) {
    char*       ret = NULL;
    workbase_t* wb;

    ck_rlock(&sdata->workbase_lock);
    HASH_FIND_I64(sdata->workbases, &id, wb);
    if (wb)
        ret = strdup(wb->txn_hashes);
    ck_runlock(&sdata->workbase_lock);

    return ret;
}

static void send_transactions(json_params_t* jp) {
    const char *msg = yyjson_mut_get_str(jp->yymethod),
               *params = yyjson_mut_get_str(yyjson_mut_arr_get(jp->yyparams, 0));
    stratum_instance_t* client = NULL;
    yyjson_mut_val*     root;
    yyjson_mut_doc*     doc;
    sdata_t*            sdata = ckpool.sdata;
    int64_t             job_id = 0;
    char*               hashes;
    time_t              now_t;

    if (unlikely(!msg || !strlen(msg))) {
        LOGWARNING("send_transactions received null method");
        goto out;
    }

    client = ref_instance_by_id(sdata, jp->client_id);
    if (unlikely(!client)) {
        LOGINFO("send_transactions failed to find client id %" PRId64 " in hashtable!", jp->client_id);
        goto out;
    }

    doc = yyjson_mut_doc_new(&ckyyalc);
    if (cmdmatch(msg, "mining.get_transactions")) {
        int txns;

        /* We don't actually send the transactions as that would use
         * up huge bandwidth, so we just return the number of
         * transactions :) . Support both forms of encoding the
         * request in method name and as a parameter. */
        if (params && strlen(params) > 0)
            sscanf(params, "%lx", &job_id);
        else
            sscanf(msg, "mining.get_transactions(%lx", &job_id);
        txns = transactions_by_jobid(sdata, job_id);
        if (txns != -1) {
            root = yyjson_mut_pack_val(doc, "{sisnso}", "result", txns, "error", "id", jp->yyid_val);
        } else {
            root = yyjson_mut_pack_val(doc, "{ssso}", "error", "Invalid job_id", "id", jp->yyid_val);
        }
        goto out_send;
    }
    if (!cmdmatch(msg, "mining.get_txnhashes")) {
        LOGDEBUG("Unhandled mining get request: %s", msg);
        root = yyjson_mut_pack_val(doc, "{ssso}", "error", "Unhandled", "id", jp->yyid_val);
        goto out_send;
    }

    now_t = time(NULL);
    if (now_t - client->last_txns < ckpool.update_interval) {
        LOGNOTICE("Rate limiting get_txnhashes on client %" PRId64 "!", jp->client_id);
        root = yyjson_mut_pack_val(doc, "{ssso}", "error", "Ratelimit", "id", jp->yyid_val);
        goto out_send;
    }
    client->last_txns = now_t;
    if (!params || !strlen(params)) {
        root = yyjson_mut_pack_val(doc, "{ssso}", "error", "Invalid params", "id", jp->yyid_val);
        goto out_send;
    }
    sscanf(params, "%lx", &job_id);

    /* Returns a copy of the hashes, needs to be released */
    hashes = txnhashes_by_jobid(sdata, job_id);
    if (hashes) {
        root = yyjson_mut_pack_val(doc, "{sssnso}", "result", hashes, "error", "id", jp->yyid_val);
        free(hashes);
    } else {
        root = yyjson_mut_pack_val(doc, "{ssso}", "error", "Invalid job_id", "id", jp->yyid_val);
    }
out_send:
    yyjson_mut_doc_set_root(doc, root);
    stratum_add_yysend(sdata, doc, jp->client_id, SM_TXNSRESULT);
out:
    if (client)
        dec_instance_ref(sdata, client);
    discard_json_params(jp);
}

static void add_log_entry(log_entry_t** entries, char** fname, char** buf) {
    log_entry_t* entry = ckalloc(sizeof(log_entry_t));

    entry->fname = *fname;
    *fname = NULL;
    entry->buf = *buf;
    *buf = NULL;
    DL_APPEND(*entries, entry);
}

static void dump_log_entries(log_entry_t** entries) {
    log_entry_t *entry, *tmpentry;
    FILE*        fp;

    DL_FOREACH_SAFE(*entries, entry, tmpentry) {
        DL_DELETE(*entries, entry);
        fp = fopen(entry->fname, "we");
        if (likely(fp)) {
            fprintf(fp, "%s", entry->buf);
            fclose(fp);
        } else
            LOGERR("Failed to fopen %s in dump_log_entries", entry->fname);
        free(entry->fname);
        free(entry->buf);
        free(entry);
    }
}

static void upstream_workers(user_instance_t* user) {
    char* msg;

    ASPRINTF(&msg, "{\"method\":\"workers\",\"username\":\"%s\",\"workers\":%d}\n", user->username, user->workers);
    connector_upstream_msg(msg);
}

/* To iterate over all users, if user is initially NULL, this will return the first entry,
 * otherwise it will return the entry after user, and NULL if there are no more entries.
 * Allows us to grab and drop the lock on each iteration. */
static user_instance_t* next_user(sdata_t* sdata, user_instance_t* user) {
    ck_rlock(&sdata->instance_lock);
    if (unlikely(!user))
        user = sdata->user_instances;
    else
        user = user->hh.next;
    ck_runlock(&sdata->instance_lock);

    return user;
}

/* Ditto for worker */
static worker_instance_t* next_worker(sdata_t* sdata, user_instance_t* user, worker_instance_t* worker) {
    ck_rlock(&sdata->instance_lock);
    if (!worker)
        worker = user->worker_instances;
    else
        worker = worker->next;
    ck_runlock(&sdata->instance_lock);

    return worker;
}

static void lazy_drop_client(stratum_instance_t* client) {
    /* Updated unlocked, a race will only delay the dropping which is
     * harmless. */
    client->dropped = true;
    connector_drop_client(client->id);
}

static void* statsupdate(void __maybe_unused* arg) {
    sdata_t*      sdata = ckpool.sdata;
    pool_stats_t* stats = &sdata->stats;

    pthread_detach(pthread_self());
    rename_proc("statsupdate");

    tv_time(&stats->start_time);
    cksleep_prepare_r(&stats->last_update);
    sleep(1);

    while (42) {
        double              ghs, ghs1, ghs5, ghs15, ghs60, ghs360, ghs1440, ghs10080, per_tdiff, percent;
        char                suffix1[16], suffix5[16], suffix15[16], suffix60[16];
        char                suffix360[16], suffix1440[16], suffix10080[16];
        int                 remote_users = 0, remote_workers = 0, idle_workers = 0;
        log_entry_t*        log_entries = NULL;
        char_entry_t*       char_list = NULL;
        stratum_instance_t* client;
        user_instance_t*    user;
        char *              fname, *s, *sp;
        yyjson_mut_doc*     doc;
        tv_t                now, diff;
        FILE*               fp;
        int                 i;

        tv_time(&now);
        timersub(&now, &stats->start_time, &diff);

        ck_wlock(&sdata->instance_lock);
        /* Grab the first entry */
        client = sdata->stratum_instances;
        if (likely(client))
            __inc_instance_ref(client);
        ck_wunlock(&sdata->instance_lock);

        while (client) {
            tv_time(&now);
            /* Look for clients that have been dropped which the
             * connector may not have been informed about and should
             * disconnect. */
            if (client->sv2) {
                /*
                 * Virtual SV2 sessions have no connector TCP client
                 * (no dropidle / test_client), but instance dsps are
                 * still exposed via getclient(s) — idle-decay like SV1.
                 */
                if (client->authorised) {
                    per_tdiff = tvdiff(&now, &client->last_share);
                    if (per_tdiff > 60) {
                        decay_client(client, 0, &now);
                        idle_workers++;
                        if (per_tdiff > 600)
                            client->idle = true;
                    }
                }
            } else if (client->dropped)
                connector_drop_client(client->id);
            else if (remote_server(client)) {
                /* Do nothing to these */
            } else if (!client->authorised) {
                /* Test for clients that haven't authed in over a minute
                 * and drop them lazily */
                if (now.tv_sec > client->start_time + 60)
                    lazy_drop_client(client);
            } else {
                per_tdiff = tvdiff(&now, &client->last_share);
                /* Decay times per connected instance */
                if (per_tdiff > 60) {
                    /* No shares for over a minute, decay to 0 */
                    decay_client(client, 0, &now);
                    idle_workers++;
                    if (ckpool.dropidle && per_tdiff > ckpool.dropidle) {
                        /* Drop clients idle for longer than
                         * ckpool.dropidle in seconds if set */
                        LOGINFO("Dropping client %" PRId64 " due to being idle", client->id);
                        lazy_drop_client(client);
                    } else if (per_tdiff > 600) {
                        client->idle = true;
                        /* Test idle clients are still connected */
                        connector_test_client(client->id);
                    }
                }
            }

            ck_wlock(&sdata->instance_lock);
            /* Drop the reference of the last entry we examined,
             * then grab the next client. */
            __dec_instance_ref(client);
            client = client->hh.next;
            /* Grab a reference to this client allowing us to examine
             * it without holding the lock */
            if (likely(client))
                __inc_instance_ref(client);
            ck_wunlock(&sdata->instance_lock);
        }

        user = NULL;

        while ((user = next_user(sdata, user)) != NULL) {
            yyjson_mut_val*    workers_arr;
            worker_instance_t* worker;
            yyjson_mut_val*    root;

            if (!user->authorised)
                continue;

            tv_time(&now);

            /* Decay times per user */
            per_tdiff = tvdiff(&now, &user->last_share);
            /* Drop storage of users with no shares */
            if (!user->last_share.tv_sec) {
                LOGDEBUG("Skipping inactive user %s", user->username);
                continue;
            }
            if (per_tdiff > 60)
                decay_user(user, 0, &now);

            ghs = user->dsps1440 * nonces;
            suffix_string(ghs, suffix1440, 16, 0);

            ghs = user->dsps1 * nonces;
            suffix_string(ghs, suffix1, 16, 0);

            ghs = user->dsps5 * nonces;
            suffix_string(ghs, suffix5, 16, 0);

            ghs = user->dsps60 * nonces;
            suffix_string(ghs, suffix60, 16, 0);

            ghs = user->dsps10080 * nonces;
            suffix_string(ghs, suffix10080, 16, 0);

            doc = yyjson_mut_pack(
                "{ss,ss,ss,ss,ss,sI,si,sI,sf,sI,sI}", "hashrate1m", suffix1, "hashrate5m", suffix5, "hashrate1hr",
                suffix60, "hashrate1d", suffix1440, "hashrate7d", suffix10080, "lastshare", user->last_share.tv_sec,
                "workers", user->workers + user->remote_workers, "shares", user->shares, "bestshare", user->best_diff,
                "bestever", user->best_ever, "authorised", user->auth_time);
            root = yyjson_mut_doc_get_root(doc);

            if (user->remote_workers) {
                remote_workers += user->remote_workers;
                /* Reset the remote_workers count once per minute */
                user->remote_workers = 0;
                /* We check this unlocked but transiently
                 * wrong is harmless */
                if (!user->workers)
                    remote_users++;
            }

            s = yyjson_mut_write(doc, 0, NULL);
            ASPRINTF(&sp, "User %s:%s", user->username, s);
            dealloc(s);
            add_msg_entry(&char_list, &sp);

            worker = NULL;

            workers_arr = yyjson_mut_arr(doc);
            yyjson_mut_obj_add(root, yyjson_mut_strcpy(doc, "worker"), workers_arr);

            /* Decay times per worker */
            while ((worker = next_worker(sdata, user, worker)) != NULL) {
                yyjson_mut_val* wval;

                per_tdiff = tvdiff(&now, &worker->last_share);
                if (per_tdiff > 60) {
                    decay_worker(worker, 0, &now);
                    worker->idle = true;
                    /* Drop storage of workers idle for 1 week */
                    if (per_tdiff > 600000) {
                        LOGDEBUG("Skipping inactive worker %s", worker->workername);
                        continue;
                    }
                }

                ghs = worker->dsps1440 * nonces;
                suffix_string(ghs, suffix1440, 16, 0);

                ghs = worker->dsps1 * nonces;
                suffix_string(ghs, suffix1, 16, 0);

                ghs = worker->dsps5 * nonces;
                suffix_string(ghs, suffix5, 16, 0);

                ghs = worker->dsps60 * nonces;
                suffix_string(ghs, suffix60, 16, 0);

                ghs = worker->dsps10080 * nonces;
                suffix_string(ghs, suffix10080, 16, 0);

                LOGDEBUG("Storing worker %s", worker->workername);

                wval = yyjson_mut_pack_val(
                    doc, "{ss,ss,ss,ss,ss,ss,sI,sI,sf,sI}", "workername", worker->workername, "hashrate1m", suffix1,
                    "hashrate5m", suffix5, "hashrate1hr", suffix60, "hashrate1d", suffix1440, "hashrate7d", suffix10080,
                    "lastshare", worker->last_share.tv_sec, "shares", worker->shares, "bestshare", worker->best_diff,
                    "bestever", worker->best_ever);
                yyjson_mut_arr_append(workers_arr, wval);
            }

            ASPRINTF(&fname, "%s/users/%s", ckpool.logdir, user->username);
            s = yyjson_mut_write(doc, YYJSON_WRITE_PRETTY | YYJSON_WRITE_NEWLINE_AT_END, NULL);
            add_log_entry(&log_entries, &fname, &s);
            yyjson_mut_doc_free(doc);
            if (ckpool.remote)
                upstream_workers(user);
        }

        if (remote_workers) {
            mutex_lock(&sdata->stats_lock);
            stats->remote_workers = remote_workers;
            stats->remote_users = remote_users;
            mutex_unlock(&sdata->stats_lock);
        }

        /* Dump log entries out of instance_lock */
        dump_log_entries(&log_entries);
        notice_msg_entries(&char_list);

        ghs1 = stats->dsps1 * nonces;
        suffix_string(ghs1, suffix1, 16, 0);

        ghs5 = stats->dsps5 * nonces;
        suffix_string(ghs5, suffix5, 16, 0);

        ghs15 = stats->dsps15 * nonces;
        suffix_string(ghs15, suffix15, 16, 0);

        ghs60 = stats->dsps60 * nonces;
        suffix_string(ghs60, suffix60, 16, 0);

        ghs360 = stats->dsps360 * nonces;
        suffix_string(ghs360, suffix360, 16, 0);

        ghs1440 = stats->dsps1440 * nonces;
        suffix_string(ghs1440, suffix1440, 16, 0);

        ghs10080 = stats->dsps10080 * nonces;
        suffix_string(ghs10080, suffix10080, 16, 0);

        ASPRINTF(&fname, "%s/pool/pool.status", ckpool.logdir);
        fp = fopen(fname, "we");
        if (unlikely(!fp)) {
            LOGERR("Failed to fopen %s", fname);
            dealloc(fname);
            goto out_status;
        }
        dealloc(fname);

        doc = yyjson_mut_pack(
            "{sI,sI,si,si,si,si}", "runtime", diff.tv_sec, "lastupdate", now.tv_sec, "Users",
            stats->users + stats->remote_users, "Workers", stats->workers + stats->remote_workers, "Idle", idle_workers,
            "Disconnected", stats->disconnected);
        s = yyjson_mut_write(doc, 0, NULL);
        yyjson_mut_doc_free(doc);
        LOGNOTICE("Pool:%s", s);
        fprintf(fp, "%s\n", s);
        dealloc(s);

        doc = yyjson_mut_pack(
            "{ss,ss,ss,ss,ss,ss,ss}", "hashrate1m", suffix1, "hashrate5m", suffix5, "hashrate15m", suffix15,
            "hashrate1hr", suffix60, "hashrate6hr", suffix360, "hashrate1d", suffix1440, "hashrate7d", suffix10080);
        s = yyjson_mut_write(doc, YYJSON_WRITE_FP_TO_FIXED(2), NULL);
        yyjson_mut_doc_free(doc);
        LOGNOTICE("Pool:%s", s);
        fprintf(fp, "%s\n", s);
        dealloc(s);

        /* Round to 4 significant digits */
        percent = round(stats->accounted_diff_shares * 10000 / stats->network_diff) / 100;
        doc = yyjson_mut_pack(
            "{sf,sI,sI,sI,sf,sf,sf,sf}", "diff", percent, "accepted", stats->accounted_diff_shares, "rejected",
            stats->accounted_rejects, "bestshare", stats->best_diff, "SPS1m", stats->sps1, "SPS5m", stats->sps5,
            "SPS15m", stats->sps15, "SPS1h", stats->sps60);
        s = yyjson_mut_write(doc, YYJSON_WRITE_FP_TO_FIXED(1), NULL);
        yyjson_mut_doc_free(doc);
        LOGNOTICE("Pool:%s", s);
        fprintf(fp, "%s\n", s);
        dealloc(s);

#ifdef HAVE_SV2
        /* Connected SV2 mining/JD client gauges (one JSON object line). */
        s = sv2_strat_stats_json();
        if (s) {
            LOGNOTICE("SV2:%s", s);
            fprintf(fp, "%s\n", s);
            dealloc(s);
        }
        /* Job Declaration operator metrics (one JSON object line). */
        if (sv2_jd_enabled()) {
            s = sv2_jd_stats_json();
            if (s) {
                LOGNOTICE("SV2JD:%s", s);
                fprintf(fp, "%s\n", s);
                dealloc(s);
            }
        }
#endif
        fclose(fp);

    out_status:
        if (ckpool.proxy && sdata->proxy) {
            proxy_t *proxy, *proxytmp, *subproxy, *subtmp;

            mutex_lock(&sdata->proxy_lock);
            doc = yyjson_mut_pack(
                "{si,si,si}", "current", sdata->proxy->id, "active", HASH_COUNT(sdata->proxies), "total",
                sdata->proxy_count);
            mutex_unlock(&sdata->proxy_lock);

            s = yyjson_mut_write(doc, 0, NULL);
            yyjson_mut_doc_free(doc);
            LOGNOTICE("Proxy:%s", s);
            dealloc(s);

            mutex_lock(&sdata->proxy_lock);
            HASH_ITER(hh, sdata->proxies, proxy, proxytmp) {
                doc = yyjson_mut_pack(
                    "{si,si,sI,sb}", "id", proxy->id, "subproxies", proxy->subproxy_count, "clients",
                    proxy->combined_clients, "alive", !proxy->dead);
                s = yyjson_mut_write(doc, 0, NULL);
                yyjson_mut_doc_free(doc);
                ASPRINTF(&sp, "Proxies:%s", s);
                dealloc(s);
                add_msg_entry(&char_list, &sp);
                HASH_ITER(sh, proxy->subproxies, subproxy, subtmp) {
                    doc = yyjson_mut_pack(
                        "{si,si,si,sI,sI,sf,sb}", "id", subproxy->id, "subid", subproxy->subid, "nonce2len",
                        subproxy->nonce2len, "clients", subproxy->bound_clients, "maxclients", subproxy->max_clients,
                        "diff", subproxy->diff, "alive", !subproxy->dead);
                    s = yyjson_mut_write(doc, 0, NULL);
                    yyjson_mut_doc_free(doc);
                    ASPRINTF(&sp, "Subproxies:%s", s);
                    dealloc(s);
                    add_msg_entry(&char_list, &sp);
                }
            }
            mutex_unlock(&sdata->proxy_lock);
            info_msg_entries(&char_list);
        }

        /* Update stats 32 times per minute to divide up userstats,
         * displaying status every minute. */
        for (i = 0; i < 32; i++) {
            int64_t unaccounted_shares, unaccounted_diff_shares, unaccounted_rejects;

            ts_to_tv(&diff, &stats->last_update);
            cksleep_ms_r(&stats->last_update, 1875);
            cksleep_prepare_r(&stats->last_update);
            ts_to_tv(&now, &stats->last_update);
            /* Calculate how long it's really been for accurate
             * stats update */
            per_tdiff = tvdiff(&now, &diff);

            mutex_lock(&sdata->uastats_lock);
            unaccounted_shares = stats->unaccounted_shares;
            unaccounted_diff_shares = stats->unaccounted_diff_shares;
            unaccounted_rejects = stats->unaccounted_rejects;
            stats->unaccounted_shares = stats->unaccounted_diff_shares = stats->unaccounted_rejects = 0;
            mutex_unlock(&sdata->uastats_lock);

            mutex_lock(&sdata->stats_lock);
            stats->accounted_shares += unaccounted_shares;
            stats->accounted_diff_shares += unaccounted_diff_shares;
            stats->accounted_rejects += unaccounted_rejects;

            decay_time(&stats->sps1, unaccounted_shares, per_tdiff, MIN1);
            decay_time(&stats->sps5, unaccounted_shares, per_tdiff, MIN5);
            decay_time(&stats->sps15, unaccounted_shares, per_tdiff, MIN15);
            decay_time(&stats->sps60, unaccounted_shares, per_tdiff, HOUR);

            decay_time(&stats->dsps1, unaccounted_diff_shares, per_tdiff, MIN1);
            decay_time(&stats->dsps5, unaccounted_diff_shares, per_tdiff, MIN5);
            decay_time(&stats->dsps15, unaccounted_diff_shares, per_tdiff, MIN15);
            decay_time(&stats->dsps60, unaccounted_diff_shares, per_tdiff, HOUR);
            decay_time(&stats->dsps360, unaccounted_diff_shares, per_tdiff, HOUR6);
            decay_time(&stats->dsps1440, unaccounted_diff_shares, per_tdiff, DAY);
            decay_time(&stats->dsps10080, unaccounted_diff_shares, per_tdiff, WEEK);
            mutex_unlock(&sdata->stats_lock);
        }

        /* Reset remote workers every minute since we measure it once
         * every minute only. */
        mutex_lock(&sdata->stats_lock);
        stats->remote_workers = stats->remote_users = 0;
        mutex_unlock(&sdata->stats_lock);
    }

    return NULL;
}

static void read_poolstats(int* tvsec_diff) {
    char *        s = alloca(4096), *pstats, *dsps, *sps;
    sdata_t*      sdata = ckpool.sdata;
    pool_stats_t* stats = &sdata->stats;
    yyjson_val*   val;
    yyjson_doc*   doc;
    tv_t          now, last;
    FILE*         fp;
    int           ret;

    snprintf(s, 4095, "%s/pool/pool.status", ckpool.logdir);
    fp = fopen(s, "re");
    if (!fp) {
        LOGINFO("Pool does not have a logfile to read");
        return;
    }
    memset(s, 0, 4096);
    ret = fread(s, 1, 4095, fp);
    fclose(fp);
    if (ret < 1 || !strlen(s)) {
        LOGDEBUG("No string to read in pool logfile");
        return;
    }
    /* Strip out end of line terminators */
    pstats = strsep(&s, "\n");
    dsps = strsep(&s, "\n");
    sps = strsep(&s, "\n");
    if (!s) {
        LOGINFO("Failed to find EOL in pool logfile");
        return;
    }
    doc = yyjson_read(pstats, strlen(pstats), 0);
    if (!doc) {
        LOGINFO("Failed to json decode pstats line from pool logfile: %s", pstats);
        return;
    }
    val = yyjson_doc_get_root(doc);
    tv_time(&now);
    last.tv_sec = 0;
    yyjson_obj_get_int64(&last.tv_sec, val, "lastupdate");
    yyjson_doc_free(doc);
    LOGINFO("Successfully read pool pstats: %s", pstats);

    doc = yyjson_read(dsps, strlen(dsps), 0);
    if (!doc) {
        LOGINFO("Failed to json decode dsps line from pool logfile: %s", sps);
        return;
    }
    val = yyjson_doc_get_root(doc);
    stats->dsps1 = dsps_from_key(val, "hashrate1m");
    stats->dsps5 = dsps_from_key(val, "hashrate5m");
    stats->dsps15 = dsps_from_key(val, "hashrate15m");
    stats->dsps60 = dsps_from_key(val, "hashrate1hr");
    stats->dsps360 = dsps_from_key(val, "hashrate6hr");
    stats->dsps1440 = dsps_from_key(val, "hashrate1d");
    stats->dsps10080 = dsps_from_key(val, "hashrate7d");
    yyjson_doc_free(doc);
    LOGINFO("Successfully read pool dsps: %s", dsps);

    doc = yyjson_read(sps, strlen(sps), 0);
    if (!doc) {
        LOGINFO("Failed to json decode sps line from pool logfile: %s", dsps);
        return;
    }
    val = yyjson_doc_get_root(doc);
    yyjson_obj_get_double(&stats->sps1, val, "SPS1m");
    yyjson_obj_get_double(&stats->sps5, val, "SPS5m");
    yyjson_obj_get_double(&stats->sps15, val, "SPS15m");
    yyjson_obj_get_double(&stats->sps60, val, "SPS1h");
    yyjson_obj_get_int64(&stats->accounted_diff_shares, val, "accepted");
    yyjson_obj_get_int64(&stats->accounted_rejects, val, "rejected");
    yyjson_obj_get_int64(&stats->best_diff, val, "bestshare");
    yyjson_doc_free(doc);

    LOGINFO("Successfully read pool sps: %s", sps);
    if (last.tv_sec)
        *tvsec_diff = now.tv_sec - last.tv_sec - 60;
    if (*tvsec_diff > 60) {
        LOGNOTICE("Old pool stats indicate pool down for %d seconds, decaying stats", *tvsec_diff);
        decay_time(&stats->sps1, 0, *tvsec_diff, MIN1);
        decay_time(&stats->sps5, 0, *tvsec_diff, MIN5);
        decay_time(&stats->sps15, 0, *tvsec_diff, MIN15);
        decay_time(&stats->sps60, 0, *tvsec_diff, HOUR);

        decay_time(&stats->dsps1, 0, *tvsec_diff, MIN1);
        decay_time(&stats->dsps5, 0, *tvsec_diff, MIN5);
        decay_time(&stats->dsps15, 0, *tvsec_diff, MIN15);
        decay_time(&stats->dsps60, 0, *tvsec_diff, HOUR);
        decay_time(&stats->dsps360, 0, *tvsec_diff, HOUR6);
        decay_time(&stats->dsps1440, 0, *tvsec_diff, DAY);
        decay_time(&stats->dsps10080, 0, *tvsec_diff, WEEK);
    }
}

static char* status_chars = "|/-\\";

void* throbber(void __maybe_unused* arg) {
    sdata_t* sdata = ckpool.sdata;
    int      counter = 0;

    if (ckpool.quiet)
        goto out;

    rename_proc("throbber");

    while (42) {
        double        sdiff;
        pool_stats_t* stats;
        char          stamp[128], hashrate[16], ch;

        sleep(1);
        if (ckpool.quiet)
            continue;
        sdiff = sdata->stats.accounted_diff_shares;
        stats = &sdata->stats;
        suffix_string(stats->dsps1 * nonces, hashrate, 16, 3);
        ch = status_chars[(counter++) & 0x3];
        get_timestamp(stamp);
        if (likely(sdata->current_workbase)) {
            double bdiff = sdiff / sdata->current_workbase->network_diff * 100;

            fprintf(
                stdout, "\33[2K\r%s %c %sH/s  %.1f SPS  %d users  %d workers  %.0f shares  %.1f%% diff", stamp, ch,
                hashrate, stats->sps1, stats->users + stats->remote_users, stats->workers + stats->remote_workers,
                sdiff, bdiff);
        } else {
            fprintf(
                stdout, "\33[2K\r%s %c %sH/s  %.1f SPS  %d users  %d workers  %.0f shares", stamp, ch, hashrate,
                stats->sps1, stats->users + stats->remote_users, stats->workers + stats->remote_workers, sdiff);
        }
        fflush(stdout);
    }
out:
    return NULL;
}

static void* zmqnotify(void __maybe_unused* arg) {
#ifdef HAVE_ZMQ_H
    sdata_t* sdata = ckpool.sdata;
    void *   context, *notify;
    int      rc;

    rename_proc("zmqnotify");

    context = zmq_ctx_new();
    notify = zmq_socket(context, ZMQ_SUB);
    if (!notify)
        quit(1, "zmq_socket failed with errno %d", errno);
    rc = zmq_setsockopt(notify, ZMQ_SUBSCRIBE, "hashblock", 0);
    if (rc < 0)
        quit(1, "zmq_setsockopt failed with errno %d", errno);
    rc = zmq_connect(notify, ckpool.zmqblock);
    if (rc < 0)
        quit(1, "zmq_connect failed with errno %d", errno);
    LOGNOTICE("ZMQ connected to %s", ckpool.zmqblock);

    while (42) {
        zmq_msg_t message;

        do {
            char hexhash[68] = {};
            int  size;

            zmq_msg_init(&message);
            rc = zmq_msg_recv(&message, notify, 0);
            if (unlikely(rc < 0)) {
                LOGWARNING("zmq_msg_recv failed with error %d", errno);
                sleep(5);
                zmq_msg_close(&message);
                continue;
            }

            size = zmq_msg_size(&message);
            switch (size) {
                case 9:
                    LOGDEBUG("ZMQ hashblock message");
                    break;
                case 4:
                    LOGDEBUG("ZMQ sequence number");
                    break;
                case 32:
                    update_base(sdata, GEN_PRIORITY);
                    __bin2hex(hexhash, zmq_msg_data(&message), 32);
                    LOGNOTICE("ZMQ block hash %s", hexhash);
                    break;
                default:
                    LOGWARNING("ZMQ message size error, size = %d!", size);
                    break;
            }
            zmq_msg_close(&message);
        } while (zmq_msg_more(&message));

        LOGDEBUG("ZMQ message complete");
    }

    zmq_close(notify);
    zmq_ctx_destroy(context);
#endif
    pthread_detach(pthread_self());

    return NULL;
}

#ifdef HAVE_CAPNP
/* Drive block updates from the bitcoind mining IPC interface, as an
 * alternative to zmqnotify. The Cap'n Proto client is thread affine so the
 * connection is established and used entirely on this thread. Waits for chain
 * tip changes and triggers a work update on each, reconnecting transparently
 * if bitcoind restarts. The blockupdate poll thread remains as a backstop. */
static void* ipcnotify(void __maybe_unused* arg) {
    sdata_t*        sdata = ckpool.sdata;
    mining_ipc_ctx* ctx;
    mining_tip      tip = {};
    bool            healthy = true;

    pthread_detach(pthread_self());
    rename_proc("ipcnotify");

    /* Connect on this thread, retrying until bitcoind is reachable or a
     * shutdown is requested. */
    while (!(ctx = mining_ipc_connect(ckpool.ipcmining))) {
        if (ckpool.shutdown)
            return NULL;
        LOGWARNING("Failed to connect to mining IPC %s, retrying in 5s", ckpool.ipcmining);
        sleep(5);
    }
    ckpool.btc_mining_ctx = ctx;
    LOGWARNING("Connected to bitcoind mining IPC %s", ckpool.ipcmining);

    /* Establish the initial tip, retrying while mining is not yet ready
     * (e.g. the node is still completing initial block download). */
    while (mining_ipc_get_tip(ctx, &tip)) {
        if (ckpool.shutdown)
            goto out;
        mining_ipc_try_obtain_mining(ctx);
        cksleep_ms(1000);
    }
    LOGNOTICE("IPC initial tip %s height %d", tip.hash, tip.height);

    while (!ckpool.shutdown) {
        mining_tip new_tip = {};
        /* Wait for a tip change. A new block returns immediately; the
         * timeout (in seconds) only bounds how often we wake to notice a
         * dropped connection or a shutdown request. A short window also
         * means we are rarely blocked mid-request, since closing the
         * socket while a long waitTipChanged executes wedges bitcoind's
         * IPC server. */
        int rc = mining_ipc_wait_tip_changed(ctx, tip.hash, 5.0, &new_tip);

        if (!rc) {
            if (memcmp(new_tip.hash, tip.hash, 64)) {
                update_base(sdata, GEN_PRIORITY);
                LOGNOTICE("IPC block hash %s height %d", new_tip.hash, new_tip.height);
                tip = new_tip;
            }
            continue;
        }

        /* Non-zero means the connection was lost (a benign timeout returns
         * the current tip, not an error). Re-obtain a Mining client,
         * reconnecting to a restarted bitcoind if necessary. */
        if (mining_ipc_try_obtain_mining(ctx)) {
            if (healthy) {
                LOGWARNING("Lost connection to mining IPC %s, reconnecting", ckpool.ipcmining);
                healthy = false;
            }
            cksleep_ms(1000);
            continue;
        }
        /* Usable again. On recovery from an outage, resync the tip so a
         * block found while we were disconnected triggers an update. */
        if (!healthy) {
            healthy = true;
            if (!mining_ipc_get_tip(ctx, &tip))
                update_base(sdata, GEN_PRIORITY);
            LOGWARNING("Reconnected to mining IPC %s tip %s height %d", ckpool.ipcmining, tip.hash, tip.height);
        }
    }
out:
    /* capnp clients are thread affine: disconnect here, on the thread that
     * owns the connection, so bitcoind sees an orderly RPC-level shutdown
     * (any in-flight request cancelled cleanly) rather than a socket
     * abruptly closed by process exit. Clearing btc_mining_ctx signals the
     * main thread that teardown is complete. */
    LOGNOTICE("Disconnecting mining IPC on shutdown");
    ckpool.btc_mining_ctx = NULL;
    mining_ipc_disconnect(ctx);
    return NULL;
}
#endif

/* Called from the main thread once a shutdown has been signalled (ckpool.shutdown
 * set), before the process exits. Waits, bounded, for the ipcnotify thread to
 * finish disconnecting from the mining IPC on its own thread-affine capnp
 * thread (it clears btc_mining_ctx when done). Safe to call unconditionally:
 * btc_mining_ctx is never set when the IPC interface is unused. */
void stratifier_shutdown_ipc(void) {
    int waited = 0;

    /* Bound the wait comfortably above the ipcnotify wait window (5s) so it
     * has time to notice the shutdown, break out and disconnect. */
    while (ckpool.btc_mining_ctx && waited++ < 200)
        cksleep_ms(50);
}

void* stratifier(void* arg) {
    pthread_t        pth_blockupdate, pth_statsupdate, pth_throbber, pth_zmqnotify;
    proc_instance_t* pi = (proc_instance_t*)arg;
    int              threads, tvsec_diff = 0;
    int64_t          randomiser;
    sdata_t*         sdata;

    rename_proc(pi->processname);
    LOGWARNING("%s stratifier starting", ckpool.name);
    sdata = ckzalloc(sizeof(sdata_t));
    ckpool.sdata = sdata;
    sdata->verbose = true;

    db_init(sdata);

    /* Wait for the generator to have something for us */
    while (!ckpool.proxy && !ckpool.generator_ready)
        cksleep_ms(10);
    while (ckpool.remote && !ckpool.connector_ready)
        cksleep_ms(10);

    if (!ckpool.proxy) {
        if (!generator_checkaddr(ckpool.btcaddress, &ckpool.script, &ckpool.segwit)) {
            LOGEMERG("Fatal: btcaddress invalid according to bitcoind");
            goto out;
        }

        /* Store this for use elsewhere */
        hex2bin(scriptsig_header_bin, scriptsig_header, 41);
        sdata->txnlen = address_to_txn(sdata->txnbin, ckpool.btcaddress, ckpool.script, ckpool.segwit);

        /* Probe the upstream donation addresses only to work out which
         * chain bitcoind is on, since regtest needs BIP34 height
         * serialisation in the coinbase. This fork does not donate:
         * the coinbase remainder is paid to our own btcaddress, set
         * below. */
        if (generator_checkaddr(ckpool.donaddress, &ckpool.donscript, &ckpool.donsegwit))
            LOGNOTICE("Mainnet detected");
        else if (generator_checkaddr(ckpool.tndonaddress, &ckpool.donscript, &ckpool.donsegwit))
            LOGNOTICE("Testnet/signet detected");
        else if (generator_checkaddr(ckpool.rtdonaddress, &ckpool.donscript, &ckpool.donsegwit)) {
            LOGNOTICE("Regtest detected");
            ckpool.regtest = true;
        } else
            LOGNOTICE("Could not determine chain from donation addresses");

        ckpool.donaddress = ckpool.btcaddress;
        ckpool.donscript = ckpool.script;
        ckpool.donsegwit = ckpool.segwit;
        ckpool.donvalid = true;
        sdata->dontxnlen = address_to_txn(sdata->dontxnbin, ckpool.donaddress, ckpool.donscript, ckpool.donsegwit);
    }

    randomiser = time(NULL);
    sdata->enonce1_64 = htole64(randomiser);
    sdata->session_id = randomiser;
    /* Set the initial id to time as high bits so as to not send the same
     * id on restarts */
    randomiser <<= 32;
    if (!ckpool.proxy)
        sdata->blockchange_id = sdata->workbase_id = randomiser;

    cklock_init(&sdata->instance_lock);
    cksem_init(&sdata->update_sem);
    cksem_post(&sdata->update_sem);

    /* Create half as many share processing and receiving threads as there
     * are CPUs */
    threads = sysconf(_SC_NPROCESSORS_ONLN) / 2 ?: 1;
    sdata->updateq = create_ckmsgq("updater", &block_update);
    sdata->sshareq = create_ckmsgqs("sprocessor", &sshare_process, threads);
    sdata->ssends = create_ckmsgqs("ssender", &ssend_process, threads);
    sdata->sauthq = create_ckmsgq("authoriser", &sauth_process);
    sdata->stxnq = create_ckmsgq("stxnq", &send_transactions);
    sdata->srecvs = create_ckmsgqs("sreceiver", &srecv_process, threads);
    create_pthread(&pth_throbber, throbber, NULL);
    read_poolstats(&tvsec_diff);
    read_userstats(sdata, tvsec_diff);

    /* Set diff impossibly large until we know the network diff */
    sdata->stats.network_diff = ~0ULL;

    cklock_init(&sdata->txn_lock);
    cklock_init(&sdata->workbase_lock);
    if (!ckpool.proxy)
        create_pthread(&pth_blockupdate, blockupdate, NULL);
    else {
        mutex_init(&sdata->proxy_lock);
    }

    mutex_init(&sdata->stats_lock);
    mutex_init(&sdata->uastats_lock);
    if (!ckpool.passthrough || ckpool.node)
        create_pthread(&pth_statsupdate, statsupdate, NULL);

    mutex_init(&sdata->share_lock);
    if (!ckpool.proxy) {
#ifdef HAVE_CAPNP
        /* Prefer the bitcoind mining IPC interface for block
         * notifications when a socket is configured and present,
         * falling back to ZMQ otherwise. */
        if (ckpool.ipcmining && !access(ckpool.ipcmining, F_OK)) {
            LOGNOTICE("Using bitcoind mining IPC %s for block notifications", ckpool.ipcmining);
            create_pthread(&pth_zmqnotify, ipcnotify, NULL);
        } else
#endif
            create_pthread(&pth_zmqnotify, zmqnotify, NULL);

#ifdef HAVE_CAPNP
        /* A valid ipcmining socket always implies IPC template
         * generation. The service connection runs its own thread; block
         * generation falls back to getblocktemplate when it is not
         * ready. */
        if (ckpool.ipcmining && !access(ckpool.ipcmining, F_OK)) {
            ckpool.btc_template_svc = mining_ipc_service_connect(ckpool.ipcmining);
            if (ckpool.btc_template_svc)
                LOGNOTICE("Started mining IPC block template service on %s", ckpool.ipcmining);
            else
                LOGWARNING("Failed to start mining IPC block template service");
#ifdef HAVE_SV2
            /* Phase 2: dedicated validation connection for checkBlock,
             * only needed when SV2 is configured (JD uses checkBlock
             * even if templates still use RPC). */
            if (ckpool.sv2urls || ckpool.sv2jdurls) {
                ckpool.btc_validation_svc = mining_ipc_service_connect(ckpool.ipcmining);
                if (ckpool.btc_validation_svc)
                    LOGNOTICE("Started mining IPC validation service (checkBlock) on %s", ckpool.ipcmining);
                else
                    LOGWARNING("Failed to start mining IPC validation service");
            }
#endif
        }
#endif
    }

    ckpool.stratifier_ready = true;
    LOGWARNING("%s stratifier ready", ckpool.name);

    stratum_loop(pi);
out:
    /* We should never get here unless there's a fatal error */
    LOGEMERG("Stratifier failure, shutting down");
    db_disconnect(sdata);
    exit(1);
    return NULL;
}
