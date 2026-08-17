/*
 * Copyright 2014-2018,2023,2025-2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#ifndef CKPOOL_H
#define CKPOOL_H

#include "config.h"

#include <sys/file.h>
#include <sys/socket.h>
#include <sys/types.h>

#include "libckpool.h"
#include "uthash.h"
#include "yyjson.h"
#include "yyjson_util.h"

#define RPC_TIMEOUT 60

struct ckpool_instance;
typedef struct ckpool_instance ckpool_t;

extern ckpool_t ckpool;

struct ckmsg {
    struct ckmsg* next;
    struct ckmsg* prev;
    void*         data;
};

typedef struct ckmsg ckmsg_t;

typedef struct unix_msg unix_msg_t;

struct unix_msg {
    unix_msg_t* next;
    unix_msg_t* prev;
    int         sockd;
    char*       buf;
};

typedef struct ckmsgq ckmsgq_t;

struct ckmsgq {
    char            name[16];
    pthread_t       pth;
    mutex_t*        lock;
    pthread_cond_t* cond;
    ckmsg_t*        msgs;
    void (*func)(void*);
    int64_t   messages;
    bool      active;
    ckmsgq_t* primary;
};

typedef struct proc_instance proc_instance_t;

struct proc_instance {
    unixsock_t us;
    char*      processname;
    char*      sockname;
    int        pid;
    int        oldpid;
    pthread_t  pth_process;

    /* Linked list of received messages, locking and conditional */
    unix_msg_t*    unix_msgs;
    mutex_t        rmsg_lock;
    pthread_cond_t rmsg_cond;
};

struct connsock {
    int   fd;
    char* url;
    char* port;
    char* auth;

    char* buf;
    int   bufofs;
    int   buflen;
    int   bufsize;
    int   rcvbufsiz;
    int   sendbufsiz;

    /* Semaphore used to serialise request/responses */
    sem_t sem;

    bool alive;
};

typedef struct connsock connsock_t;

typedef struct char_entry char_entry_t;

struct char_entry {
    char_entry_t* next;
    char_entry_t* prev;
    char*         buf;
};

typedef struct log_entry log_entry_t;

struct log_entry {
    log_entry_t* next;
    log_entry_t* prev;
    char*        fname;
    char*        buf;
};

struct server_instance {
    /* Hash table data */
    UT_hash_handle hh;
    int            id;

    char*      url;
    char*      auth;
    char*      pass;
    bool       notify;
    bool       alive;
    connsock_t cs;
};

typedef struct server_instance server_instance_t;

struct ckpool_instance {
    /* Start time */
    time_t starttime;
    /* Start pid */
    pid_t startpid;
    /* The initial command line arguments */
    char** initial_args;
    /* Number of arguments */
    int args;
    /* Filename of config file */
    char* config;
    /* Kill old instance with same name */
    bool killold;
    /* Whether to log shares at all; true if either sink below is set */
    bool logshares;
    /* Whether to log shares to the postgres database */
    bool logshares_db;
    /* Whether to log shares to per-workbase files */
    bool logshares_file;
    /* Whether to dump each workbase's txids to logdir/pool/pool.txns */
    bool logtxns;
    /* Logging level */
    int loglevel;
    /* Main process name */
    char* name;
    /* Directory where sockets are created */
    char* socket_dir;
    /* Group ID for unix sockets */
    char* grpnam;
    gid_t gr_gid;
    /* Directory where logs are written */
    char* logdir;
    /* Logfile */
    char*  logfilename;
    FILE*  logfp;
    int    logfd;
    time_t lastopen_t;
    /* Connector fds if we inherit them from a running process */
    int* oldconnfd;
    /* Should we inherit a running instance's socket and shut it down */
    bool handover;

    /* Parse and validate the configuration, report it, and exit */
    bool testconfig;
    /* How many clients maximum to accept before rejecting further */
    int maxclients;
    /* Drop clients that have been idle for this many seconds, 0 to disable */
    int dropidle;
    /* Maximum bytes queued but unsent to one client before dropping it, 0
     * to use the per client derived default */
    int64_t maxsendqueue;
    /* Maximum distinct users to create from authorisations, 0 to disable */
    int maxusers;
    /* Honour client.reconnect requests from the upstream pool (proxy mode).
     * Defaults to true; set false to refuse all upstream redirects. */
    bool reconnect;

    /* API message queue */
    ckmsgq_t* ckpapi;

    /* Logger message queue */
    ckmsgq_t* logger;
    ckmsgq_t* console_logger;

    /* Process instance data of parent/child processes */
    proc_instance_t main;

    proc_instance_t generator;
    proc_instance_t stratifier;
    proc_instance_t connector;

    bool generator_ready;
    bool stratifier_ready;
    bool connector_ready;

    /* Set true once a shutdown has been initiated so long-running threads
     * (notably the mining IPC notifier) can stop and disconnect cleanly. */
    bool shutdown;

    /* Name of protocol used for ZMQ block notifications */
    char* zmqblock;

    /* Filesystem path to the bitcoind mining IPC unix socket. When set and
     * present, block notifications and template generation are driven from
     * the IPC interface in preference to ZMQ / getblocktemplate. */
    char* ipcmining;

    /* Threads of main process */
    pthread_t pth_listener;
    pthread_t pth_watchdog;

    /* Are we running in trusted remote node mode */
    bool remote;

    /* Are we running in node proxy mode */
    bool node;

    /* Are we running in passthrough mode */
    bool passthrough;

    /* Are we a redirecting passthrough */
    bool redirector;

    /* Are we running as a proxy */
    bool proxy;

    /* Are we running in btcsolo mode */
    bool btcsolo;

    /* Are we running in userproxy mode */
    bool userproxy;

    /* Should we daemonise the ckpool process */
    bool daemon;

    /* Should we disable the throbber */
    bool quiet;

    /* Have we given warnings about the inability to raise buf sizes */
    bool wmem_warn;
    bool rmem_warn;

    /* Bitcoind data */
    int    btcds;
    char** btcdurl;
    char** btcdauth;
    char** btcdpass;
    bool*  btcdnotify;
    int    blockpoll;     // How frequently in ms to poll bitcoind for block updates
    int    nonce1length;  // Extranonce1 length
    int    nonce2length;  // Extranonce2 length
    bool   signet;        // Request the signet rule in getblocktemplate

    /* Difficulty settings */
    int64_t mindiff;    // Default 1
    int64_t startdiff;  // Default 10000
    int64_t highdiff;   // Default 1000000
    int64_t maxdiff;    // No default

    /* Coinbase data */
    char* btcaddress;      // Address to mine to
    bool  script;          // Address is a script address
    bool  segwit;          // Address is a segwit address
    char* btcsig;          // Optional signature to add to coinbase
    bool  coinbase_valid;  // Coinbase transaction confirmed valid

    bool regtest;

    /* PostgreSQL logging. db_conn_str is NULL unless all four parts were
     * configured; logshares_db is refused without it. */
    char* database_host;
    char* database_name;
    char* database_user;
    char* database_pass;
    char* db_conn_str;

    /* Donation data */
    char*  donaddress;    // Donation address
    char*  tndonaddress;  // Testnet donation address
    char*  rtdonaddress;  // Regtest donation address
    bool   donscript;     // Donation is a script
    bool   donsegwit;     // Donation is segwit
    bool   donvalid;      // Donation address works on this network
    double donation;      // Percentage donation to development

    /* Stratum options */
    server_instance_t** servers;
    char**              serverurl;        // Array of URLs to bind our server/proxy to
    int                 serverurls;       // Number of server bindings
    bool*               server_highdiff;  // If this server is highdiff
    bool*               nodeserver;       // If this server URL serves node information
    int                 nodeservers;      // If this server has remote node servers
    bool*               trusted;          // If this server URL accepts trusted remote nodes
    char*               upstream;         // Upstream pool in trusted remote mode

    int update_interval;  // Seconds between stratum updates

    uint32_t version_mask;  // Bits which set to true means allow miner to modify those bits

    /* Proxy options */
    int    proxies;
    char** proxyurl;
    char** proxyauth;
    char** proxypass;
    /* Per-entry SV2 Job Declaration server URL, NULL when the entry does no
     * job declaration. Presence enables the JD client for that upstream and
     * requires an SV2 proxyurl plus a mining IPC socket. */
    char** proxyjds;

    /* Passthrough redirect options */
    int    redirecturls;
    char** redirecturl;
    char** redirectport;

    /* Private data for each process */
    void* gdata;
    void* sdata;
    void* cdata;

    /* Opaque mining_ipc_ctx* for the bitcoind mining IPC connection, owned
     * by the ipcnotify thread. NULL when IPC is unavailable or unused. */
    void* btc_mining_ctx;

    /* Opaque mining_ipc_service* for IPC block template generation. NULL
     * when ipcmining is unset or the interface is unavailable. */
    void* btc_template_svc;

    /* Opaque mining_ipc_service* dedicated to Mining.checkBlock (SV2 JD
     * Phase 2). Separate EzRpcClient/service thread from btc_template_svc
     * so validation cannot HOL-block workgen/submit marshalling. */
    void* btc_validation_svc;

#ifdef HAVE_SV2
    /* Stratum V2 mining listen URLs (like serverurl). sv2urls==0 = disabled. */
    char** sv2url;
    int    sv2urls;
    /* Stratum V2 Job Declaration listen URLs. sv2jdurls==0 = off. */
    char** sv2jdurl;
    int    sv2jdurls;
    /* Optional paths to Noise authority / server static key material. */
    char* sv2_authority_key;
    char* sv2_static_key;
    /* Per-serverurl: true if this bind is SV2 (binary), not SV1 JSON. */
    bool* server_sv2;
    /* Per-serverurl: true if SV2 bind is Job Declaration (not Mining). */
    bool* server_sv2_jd;
#endif
};

enum stratum_msgtype {
    SM_RECONNECT = 0,
    SM_DIFF,
    SM_MSG,
    SM_UPDATE,
    SM_ERROR,
    SM_SUBSCRIBE,
    SM_SUBSCRIBERESULT,
    SM_SHARE,
    SM_SHARERESULT,
    SM_AUTH,
    SM_AUTHRESULT,
    SM_TXNS,
    SM_TXNSRESULT,
    SM_PING,
    SM_WORKINFO,
    SM_SUGGESTDIFF,
    SM_BLOCK,
    SM_PONG,
    SM_TRANSACTIONS,
    SM_SHAREERR,
    SM_WORKERSTATS,
    SM_REQTXNS,
    SM_CONFIGURE,
    SM_NONE
};

static const char __maybe_unused* stratum_msgs[] = {
    "reconnect",    "diff", "message",      "update",   "error",       "subscribe", "subscribe.result", "share",
    "share.result", "auth", "auth.result",  "txns",     "txns.result", "ping",      "workinfo",         "suggestdiff",
    "block",        "pong", "transactions", "shareerr", "workerstats", "reqtxns",   "mining.configure", ""};

#define SAFE_HASH_OVERHEAD(HASHLIST) (HASHLIST ? HASH_OVERHEAD(hh, HASHLIST) : 0)

void get_timestamp(char* stamp);

ckmsgq_t* create_ckmsgq(const char* name, const void* func);
ckmsgq_t* create_ckmsgqs(const char* name, const void* func, const int count);
bool      _ckmsgq_add(ckmsgq_t* ckmsgq, void* data, bool head, const char* file, const char* func, const int line);
#define ckmsgq_add(ckmsgq, data) _ckmsgq_add(ckmsgq, data, false, __FILE__, __func__, __LINE__)
#define ckmsgq_add_head(ckmsgq, data) _ckmsgq_add(ckmsgq, data, true, __FILE__, __func__, __LINE__)
bool        ckmsgq_empty(ckmsgq_t* ckmsgq);
unix_msg_t* get_unix_msg(proc_instance_t* pi);

bool ping_main(void);
void empty_buffer(connsock_t* cs);
int  set_sendbufsize(const int fd, const int len);
int  set_recvbufsize(const int fd, const int len);
int  read_socket_line(connsock_t* cs, float* timeout);
void _queue_proc(proc_instance_t* pi, const char* msg, const char* file, const char* func, const int line);
#define send_proc(pi, msg) _queue_proc(&(pi), msg, __FILE__, __func__, __LINE__)
char* _send_recv_proc(
    const proc_instance_t* pi,
    const char*            msg,
    int                    writetimeout,
    int                    readtimedout,
    const char*            file,
    const char*            func,
    const int              line);
#define send_recv_proc(pi, msg) \
    _send_recv_proc(&(pi), msg, UNIX_WRITE_TIMEOUT, UNIX_READ_TIMEOUT, __FILE__, __func__, __LINE__)

yyjson_doc* yyjson_rpc_call(connsock_t* cs, const char* rpc_req);
yyjson_doc* yyjson_rpc_response(connsock_t* cs, const char* rpc_req);
void        yyjson_rpc_msg(connsock_t* cs, const char* rpc_req);

bool _send_yyjson_msg(connsock_t* cs, yyjson_mut_doc* doc, const char* file, const char* func, const int line);
#define send_yyjson_msg(CS, DOC) _send_yyjson_msg(CS, DOC, __FILE__, __func__, __LINE__)
yyjson_doc* yyjson_msg_result(const char* msg, yyjson_val** res_val, yyjson_val** err_val);

bool yyjson_obj_get_string(char** store, yyjson_val* val, const char* res);
bool yyjson_obj_get_int64(int64_t* store, yyjson_val* val, const char* res);
bool yyjson_obj_get_int(int* store, yyjson_val* val, const char* res);
bool yyjson_obj_get_double(double* store, yyjson_val* val, const char* res);
bool yyjson_obj_get_uint32(uint32_t* store, yyjson_val* val, const char* res);
bool yyjson_obj_get_bool(bool* store, yyjson_val* val, const char* res);
bool yyjson_mut_obj_get_string(char** store, yyjson_mut_val* val, const char* res);
bool yyjson_mut_obj_get_int64(int64_t* store, yyjson_mut_val* val, const char* res);
bool yyjson_mut_obj_get_int(int* store, yyjson_mut_val* val, const char* res);
bool yyjson_mut_obj_get_double(double* store, yyjson_mut_val* val, const char* res);
bool yyjson_mut_obj_get_uint32(uint32_t* store, yyjson_mut_val* val, const char* res);
bool yyjson_mut_obj_get_bool(bool* store, yyjson_mut_val* val, const char* res);
bool yyjson_mut_obj_getdel_int(int* store, yyjson_mut_val* val, const char* res);
bool yyjson_mut_obj_getdel_int64(int64_t* store, yyjson_mut_val* val, const char* res);

/* API Placeholders for future API implementation */
typedef struct apimsg apimsg_t;

struct apimsg {
    char* buf;
    int   sockd;
};

// static inline void ckpool_api(apimsg_t __maybe_unused *apimsg) {};
static inline yyjson_mut_doc* yyjson_encode_errormsg(yyjson_read_err __maybe_unused* err_val) {
    return NULL;
};
static inline yyjson_mut_doc* yyjson_errormsg(const char __maybe_unused* fmt, ...) {
    return NULL;
};
static inline void send_api_yyresponse(yyjson_mut_doc* doc, const int __maybe_unused sockd) {
    if (doc)
        yyjson_mut_doc_free(doc);
};

/* Subclients have client_ids in the high bits. Returns the value of the parent
 * client if one exists. */
static inline int64_t subclient(const int64_t client_id) {
    return (client_id >> 32);
}

#endif /* CKPOOL_H */
