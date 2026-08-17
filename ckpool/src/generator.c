/*
 * Copyright 2014-2017,2023,2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#include "config.h"

#include <sys/epoll.h>
#include <sys/socket.h>
#include <math.h>
#include <string.h>
#include <unistd.h>

#include "ckpool.h"
#include "libckpool.h"
#include "generator.h"
#include "stratifier.h"
#include "bitcoin.h"
#include "uthash.h"
#include "utlist.h"
#ifdef HAVE_SV2
#include "sv2_types.h"
#include "sv2_codec.h"
#include "sv2_jdc.h"
#include "sv2_noise.h"
#include "sv2_tx.h"
#endif

/* The largest coinbase payout scriptPubKey we will name in a log line; a longer
 * one is reported as unreadable rather than truncated. */
#define MAX_PAYOUT_SCRIPT 128

struct notify_instance {
    /* Hash table data */
    UT_hash_handle hh;
    int64_t        id64;

    char            prevhash[68];
    yyjson_mut_doc* jobid;
    char*           coinbase1;
    char*           coinbase2;
    int             coinb1len;
    int             merkles;
    char            merklehash[16][68];
    char            nbit[12];
    char            ntime[12];
    char            bbversion[12];
    bool            clean;

    time_t notify_time;

#ifdef HAVE_SV2
    /*
     * SV2 custom (job declaration) work: what the block solve paths need,
     * kept here rather than on the job ring because notify instances are
     * found under notify_lock and aged out ten minutes later, while the ring
     * is rotated by the receive thread under no lock at all. A share arrives
     * on a different thread, so it must not chase ring memory.
     */
    bool                     sv2_custom;
    struct sv2_jdc_template* sv2_tmpl; /* reference held */
    /* Declared-form coinbase either side of the extranonce hole, for a local
     * block submit (coinbase1/2 above are the legacy form miners hash). */
    uint8_t *sv2_dcb_prefix, *sv2_dcb_suffix;
    uint16_t sv2_dcb_prefix_len, sv2_dcb_suffix_len;
    /* The channel's extranonce prefix as it was when this job was sent, which
     * is the one baked into coinbase1 and declared to the JDS. Taken from here
     * and not from the live channel, so a SetExtranoncePrefix between the
     * notify and a share on it cannot rebuild a different coinbase. */
    uint8_t sv2_en_prefix[SV2_MAX_B0_32];
    uint8_t sv2_en_prefix_len;
#endif
};

typedef struct notify_instance notify_instance_t;

typedef struct proxy_instance proxy_instance_t;

struct share_msg {
    UT_hash_handle hh;
    int64_t        id64;  // Our own id for submitting upstream

    int64_t client_id;
    time_t  submit_time;
    double  diff;
};

typedef struct share_msg share_msg_t;

struct stratum_msg {
    struct stratum_msg* next;
    struct stratum_msg* prev;

    yyjson_mut_doc* doc;
    int64_t         client_id;
};

typedef struct stratum_msg stratum_msg_t;

struct pass_msg {
    proxy_instance_t* proxy;
    connsock_t*       cs;
    char*             msg;
};

typedef struct pass_msg pass_msg_t;
typedef struct cs_msg   cs_msg_t;

/* Statuses of various proxy states - connect, subscribe and auth */
enum proxy_stat { STATUS_INIT = 0, STATUS_SUCCESS, STATUS_FAIL };

static const char* proxy_status[] = {"Initial", "Success", "Failed"};

/* Per proxied pool instance data */
struct proxy_instance {
    UT_hash_handle    hh;   /* Proxy list */
    UT_hash_handle    sh;   /* Subproxy list */
    proxy_instance_t* next; /* For dead proxy list */
    proxy_instance_t* prev; /* For dead proxy list */

    connsock_t cs;
    bool       passthrough;
    bool       node;
    int        id;     /* Proxy server id*/
    int        subid;  /* Subproxy id */
    int        userid; /* User id if this proxy is bound to a user */

    char* baseurl;
    char* url;
    char* auth;
    char* pass;

    char* enonce1;
    char* enonce1bin;
    int   nonce1len;
    int   nonce2len;

    /* The address the last work we saw pays, only to notice it change. */
    char payaddr[MAX_PAYOUT_SCRIPT * 2 + 64];

    tv_t last_message;

    double diff;
    double diff_accepted;
    double diff_rejected;
    double total_accepted; /* Used only by parent proxy structures */
    double total_rejected; /* "" */
    tv_t   last_share;

    /* Diff shares per second for 1/5/60... minute rolling averages */
    double dsps1;
    double dsps5;
    double dsps60;
    double dsps360;
    double dsps1440;
    tv_t   last_decay;

    /* Total diff shares per second for all subproxies */
    double tdsps1;    /* Used only by parent proxy structures */
    double tdsps5;    /* "" */
    double tdsps60;   /* "" */
    double tdsps360;  /* "" */
    double tdsps1440; /* "" */
    tv_t   total_last_decay;

    bool no_params; /* Doesn't want any parameters on subscribe */

    bool    global;       /* Part of the global list of proxies */
    bool    disabled;     /* Subproxy no longer to be used */
    bool    reconnect;    /* We need to drop and reconnect */
    bool    reconnecting; /* Testing of parent in progress */
    int64_t recruit;      /* No of recruiting requests in progress */
    bool    alive;
    bool    authorised;

    /* Which of STATUS_* states are these in */
    enum proxy_stat connect_status;
    enum proxy_stat subscribe_status;
    enum proxy_stat auth_status;

    /* Back off from retrying if we fail one of the above */
    int backoff;

    /* Are we in the middle of a blocked write of this message? */
    cs_msg_t* sending;

    pthread_t pth_precv;

    ckmsgq_t* passsends;  // passthrough sends

    char_entry_t* recvd_lines; /* Linked list of unprocessed messages */

    int epfd; /* Epoll fd used by the parent proxy */

    mutex_t           proxy_lock;        /* Lock protecting hashlist of proxies */
    proxy_instance_t* parent;            /* Parent proxy of subproxies */
    proxy_instance_t* subproxies;        /* Hashlist of subproxies of this proxy */
    int64_t           clients_per_proxy; /* Max number of clients of this proxy */
    int               subproxy_count;    /* Number of subproxies */

#ifdef HAVE_SV2
    bool              sv2;                   /* Upstream speaks Stratum V2 (Noise binary) */
    bool              sv2_no_work_selection; /* Pool refused REQUIRES_WORK_SELECTION once */
    uint8_t           sv2_authority[32];     /* Pool authority x-only pubkey (from URL path) */
    struct sv2_proxy* sv2p;                  /* SV2 Noise/channel/job state (SV2 proxies only) */
#endif
};

/* Private data for the generator */
struct generator_data {
    mutex_t           lock;         /* Lock protecting linked lists */
    proxy_instance_t* proxies;      /* Hash list of all proxies */
    proxy_instance_t* dead_proxies; /* Disabled proxies */
    int               proxies_generated;
    int               subproxies_generated;

    int64_t   proxy_notify_id;  // Globally increasing notify id
    pthread_t pth_uprecv;       // User proxy receive thread
    pthread_t pth_psend;        // Combined proxy send thread

    mutex_t        psend_lock;  // Lock associated with conditional below
    pthread_cond_t psend_cond;

    stratum_msg_t* psends;
    int            psends_generated;

    mutex_t            notify_lock;
    notify_instance_t* notify_instances;

    mutex_t      share_lock;
    share_msg_t* shares;
    int64_t      share_id;

    server_instance_t* current_si;  // Current server instance

    proxy_instance_t* current_proxy;
};

typedef struct generator_data gdata_t;

/* Use a temporary fd when testing server_alive to avoid races on cs->fd */
static bool server_alive(server_instance_t* si, bool pinging) {
    char*       userpass = NULL;
    bool        ret = false;
    connsock_t* cs;
    gbtbase_t   gbt;
    int         fd;

    if (si->alive)
        return true;
    cs = &si->cs;
    if (!extract_sockaddr(si->url, &cs->url, &cs->port)) {
        LOGWARNING("Failed to extract address from %s", si->url);
        return ret;
    }
    userpass = strdup(si->auth);
    realloc_strcat(&userpass, ":");
    realloc_strcat(&userpass, si->pass);
    dealloc(cs->auth);
    cs->auth = http_base64(userpass);
    if (!cs->auth) {
        LOGWARNING("Failed to create base64 auth from %s", userpass);
        dealloc(userpass);
        return ret;
    }
    dealloc(userpass);

    fd = connect_socket(cs->url, cs->port);
    if (fd < 0) {
        if (!pinging)
            LOGWARNING("Failed to connect socket to %s:%s !", cs->url, cs->port);
        return ret;
    }

    /* Test we can connect, authorise and get a block template */
    if (!gen_gbtbase(cs, &gbt)) {
        if (!pinging) {
            LOGINFO("Failed to get test block template from %s:%s!", cs->url, cs->port);
        }
        goto out;
    }
    clear_gbtbase(&gbt);
    if (unlikely(ckpool.btcsolo && !ckpool.btcaddress)) {
        /* If no btcaddress is specified in solobtc mode, choose one of
         * the donation addresses from mainnet, testnet, or regtest for
         * coinbase validation later on, although it will not be used
         * for mining. */
        if (validate_address(cs, ckpool.donaddress, &ckpool.script, &ckpool.segwit))
            ckpool.btcaddress = ckpool.donaddress;
        else if (validate_address(cs, ckpool.tndonaddress, &ckpool.script, &ckpool.segwit))
            ckpool.btcaddress = ckpool.tndonaddress;
        else if (validate_address(cs, ckpool.rtdonaddress, &ckpool.script, &ckpool.segwit))
            ckpool.btcaddress = ckpool.rtdonaddress;
    }

    if (!ckpool.node && !validate_address(cs, ckpool.btcaddress, &ckpool.script, &ckpool.segwit)) {
        LOGWARNING("Invalid btcaddress: %s !", ckpool.btcaddress);
        goto out;
    }
    si->alive = cs->alive = ret = true;
    LOGWARNING("Server alive: %s:%s", cs->url, cs->port);
out:
    /* Close the file handle */
    close(fd);
    return ret;
}

/* Find the highest priority server alive and return it */
static server_instance_t* live_server(gdata_t* gdata) {
    server_instance_t* alive = NULL;
    connsock_t*        cs;
    int                i;

    LOGDEBUG("Attempting to connect to bitcoind");
retry:
    /* First find a server that is already flagged alive if possible
     * without blocking on server_alive() */
    for (i = 0; i < ckpool.btcds; i++) {
        server_instance_t* si = ckpool.servers[i];
        cs = &si->cs;

        if (si->alive) {
            alive = si;
            goto living;
        }
    }

    /* No servers flagged alive, try to connect to them blocking */
    for (i = 0; i < ckpool.btcds; i++) {
        server_instance_t* si = ckpool.servers[i];

        if (server_alive(si, false)) {
            alive = si;
            goto living;
        }
    }
    LOGWARNING("CRITICAL: No bitcoinds active!");
    sleep(5);
    goto retry;
living:
    gdata->current_si = alive;
    cs = &alive->cs;
    LOGINFO("Connected to live server %s:%s", cs->url, cs->port);
    send_proc(ckpool.connector, alive ? "accept" : "reject");
    return alive;
}

static void kill_server(server_instance_t* si) {
    connsock_t* cs;

    if (!si)  // This shouldn't happen
        return;

    LOGNOTICE("Killing server");
    cs = &si->cs;
    Close(cs->fd);
    empty_buffer(cs);
    dealloc(cs->url);
    dealloc(cs->port);
    dealloc(cs->auth);
}

static void clear_unix_msg(unix_msg_t** umsg) {
    if (*umsg) {
        Close((*umsg)->sockd);
        free((*umsg)->buf);
        free(*umsg);
        *umsg = NULL;
    }
}

bool generator_submitblock(const char* buf) {
    gdata_t*           gdata = ckpool.gdata;
    server_instance_t* si;
    bool               warn = false;
    connsock_t*        cs;

    while (unlikely(!(si = gdata->current_si))) {
        if (!warn)
            LOGWARNING("No live current server in generator_blocksubmit! Resubmitting indefinitely!");
        warn = true;
        cksleep_ms(10);
    }
    cs = &si->cs;
    LOGNOTICE("Submitting block data!");
    return submit_block(cs, buf);
}

void generator_preciousblock(const char* hash) {
    gdata_t*           gdata = ckpool.gdata;
    server_instance_t* si;
    connsock_t*        cs;

    if (unlikely(!(si = gdata->current_si))) {
        LOGWARNING("No live current server in generator_get_blockhash");
        return;
    }
    cs = &si->cs;
    precious_block(cs, hash);
}

bool generator_get_blockhash(int height, char* hash) {
    gdata_t*           gdata = ckpool.gdata;
    server_instance_t* si;
    connsock_t*        cs;

    if (unlikely(!(si = gdata->current_si))) {
        LOGWARNING("No live current server in generator_get_blockhash");
        return false;
    }
    cs = &si->cs;
    return get_blockhash(cs, height, hash);
}

static void gen_loop(proc_instance_t* pi) {
    server_instance_t *si = NULL, *old_si;
    unix_msg_t*        umsg = NULL;
    char*              buf = NULL;
    connsock_t*        cs;
    gbtbase_t          gbt;
    char               hash[68];

reconnect:
    clear_unix_msg(&umsg);
    old_si = si;
    si = live_server(ckpool.gdata);
    if (!si)
        goto out;
    if (unlikely(!ckpool.generator_ready)) {
        ckpool.generator_ready = true;
        LOGWARNING("%s generator ready", ckpool.name);
    }

    cs = &si->cs;
    if (!old_si)
        LOGWARNING("Connected to bitcoind: %s:%s", cs->url, cs->port);
    else if (si != old_si)
        LOGWARNING("Failed over to bitcoind: %s:%s", cs->url, cs->port);

retry:
    clear_unix_msg(&umsg);

    do {
        umsg = get_unix_msg(pi);
    } while (!umsg);

    if (unlikely(!si->alive)) {
        LOGWARNING("%s:%s Bitcoind socket invalidated, will attempt failover", cs->url, cs->port);
        goto reconnect;
    }

    buf = umsg->buf;
    LOGDEBUG("Generator received request: %s", buf);
    if (cmdmatch(buf, "getbase")) {
        if (!gen_gbtbase(cs, &gbt)) {
            LOGWARNING("Failed to get block template from %s:%s", cs->url, cs->port);
            si->alive = cs->alive = false;
            send_unix_msg(umsg->sockd, "Failed");
            goto reconnect;
        } else {
            char* s = yyjson_write(gbt.gbtdoc, 0, NULL);

            send_unix_msg(umsg->sockd, s);
            free(s);
            clear_gbtbase(&gbt);
        }
    } else if (cmdmatch(buf, "getbest")) {
        if (si->notify)
            send_unix_msg(umsg->sockd, "notify");
        else if (!get_bestblockhash(cs, hash)) {
            LOGINFO("No best block hash support from %s:%s", cs->url, cs->port);
            si->alive = cs->alive = false;
            send_unix_msg(umsg->sockd, "failed");
        } else {
            send_unix_msg(umsg->sockd, hash);
        }
    } else if (cmdmatch(buf, "getlast")) {
        int height;

        if (si->notify)
            send_unix_msg(umsg->sockd, "notify");
        else if ((height = get_blockcount(cs)) == -1) {
            si->alive = cs->alive = false;
            send_unix_msg(umsg->sockd, "failed");
            goto reconnect;
        } else {
            LOGDEBUG("Height: %d", height);
            if (!get_blockhash(cs, height, hash)) {
                si->alive = cs->alive = false;
                send_unix_msg(umsg->sockd, "failed");
                goto reconnect;
            } else {
                send_unix_msg(umsg->sockd, hash);
                LOGDEBUG("Hash: %s", hash);
            }
        }
    } else if (cmdmatch(buf, "submitblock:")) {
        char blockmsg[80];
        bool ret;

        /* cmdmatch only checks the prefix, so a short message would
         * over-read and write the memset below out of bounds. */
        if (unlikely(strlen(buf) < 12 + 64 + 1)) {
            LOGWARNING("Got too short submitblock message");
            goto retry;
        }
        LOGNOTICE("Submitting block data!");
        ret = submit_block(cs, buf + 12 + 64 + 1);
        memset(buf + 12 + 64, 0, 1);
        sprintf(blockmsg, "%sblock:%s", ret ? "" : "no", buf + 12);
        send_proc(ckpool.stratifier, blockmsg);
    } else if (cmdmatch(buf, "reconnect")) {
        goto reconnect;
    } else if (cmdmatch(buf, "loglevel")) {
        sscanf(buf, "loglevel=%d", &ckpool.loglevel);
    } else if (cmdmatch(buf, "ping")) {
        LOGDEBUG("Generator received ping request");
        send_unix_msg(umsg->sockd, "pong");
    }
    goto retry;

out:
    kill_server(si);
}

static bool connect_proxy(connsock_t* cs, proxy_instance_t* proxy) {
    if (cs->fd > 0) {
        epoll_ctl(proxy->epfd, EPOLL_CTL_DEL, cs->fd, NULL);
        Close(cs->fd);
    }
    cs->fd = connect_socket(cs->url, cs->port);
    if (cs->fd < 0) {
        LOGINFO("Failed to connect socket to %s:%s in connect_proxy", cs->url, cs->port);
        return false;
    }
    keep_sockalive(cs->fd);
    if (!ckpool.passthrough) {
        struct epoll_event event;

        event.events = EPOLLIN | EPOLLRDHUP;
        event.data.ptr = proxy;
        /* Add this connsock_t to the epoll list */
        if (unlikely(epoll_ctl(proxy->epfd, EPOLL_CTL_ADD, cs->fd, &event) == -1)) {
            LOGERR("Failed to add fd %d to epfd %d to epoll_ctl in proxy_alive", cs->fd, proxy->epfd);
            return false;
        }
    } else {
        /* We want large send/recv buffers on passthroughs */
        if (!ckpool.rmem_warn)
            cs->rcvbufsiz = set_recvbufsize(cs->fd, 1048576);
        if (!ckpool.wmem_warn)
            cs->sendbufsiz = set_sendbufsize(cs->fd, 1048576);
    }
    return true;
}

/* For some reason notify is buried at various different array depths so use
 * a reentrant function to try and find it. */
static yyjson_val* find_notify(yyjson_val* val) {
    yyjson_val* ret = NULL;
    int         arr_size, i;
    const char* entry;

    if (!yyjson_is_arr(val))
        return NULL;
    arr_size = yyjson_arr_size(val);
    entry = yyjson_get_str(yyjson_arr_get(val, 0));
    if (cmdmatch(entry, "mining.notify"))
        return val;
    for (i = 0; i < arr_size; i++) {
        yyjson_val* arr_val;

        arr_val = yyjson_arr_get(val, i);
        ret = find_notify(arr_val);
        if (ret)
            break;
    }
    return ret;
}

/* Get stored line in the proxy linked list of messages if any exist or NULL */
static char* cached_proxy_line(proxy_instance_t* proxi) {
    char* buf = NULL;

    if (proxi->recvd_lines) {
        char_entry_t* char_t = proxi->recvd_lines;

        DL_DELETE(proxi->recvd_lines, char_t);
        buf = char_t->buf;
        free(char_t);
    }
    return buf;
}

/* Get next line in the proxy linked list of messages or a new line from the
 * connsock if there are none. */
static char* next_proxy_line(connsock_t* cs, proxy_instance_t* proxi) {
    char* buf = cached_proxy_line(proxi);
    float timeout = 10;

    if (!buf && read_socket_line(cs, &timeout) > 0)
        buf = strdup(cs->buf);
    return buf;
}

/* For appending a line to the proxy recv list */
static void append_proxy_line(proxy_instance_t* proxi, const char* buf) {
    char_entry_t* char_t = ckalloc(sizeof(char_entry_t));
    char_t->buf = strdup(buf);
    DL_APPEND(proxi->recvd_lines, char_t);
}

/* Get a new line from the connsock and return a copy of it */
static char* new_proxy_line(connsock_t* cs) {
    float timeout = 10;
    char* buf = NULL;

    if (read_socket_line(cs, &timeout) < 1)
        goto out;
    buf = strdup(cs->buf);
out:
    return buf;
}

static inline bool parent_proxy(const proxy_instance_t* proxy) {
    return (proxy->parent == proxy);
}

static void recruit_subproxies(proxy_instance_t* proxi, const int recruits);

static bool parse_subscribe(connsock_t* cs, proxy_instance_t* proxi) {
    yyjson_val *res_val, *notify_val, *tmp;
    yyjson_doc* val = NULL;
    bool        parsed, ret = false;
    int         retries = 0, size;
    const char* string;
    char *      buf, *old;

retry:
    parsed = true;
    if (!(buf = new_proxy_line(cs))) {
        LOGNOTICE("Proxy %d:%d %s failed to receive line in parse_subscribe", proxi->id, proxi->subid, proxi->url);
        goto out;
    }
    LOGDEBUG("parse_subscribe received %s", buf);
    /* Ignore err_val here stored in &tmp */
    val = yyjson_msg_result(buf, &res_val, &tmp);
    if (!val || !res_val) {
        LOGINFO("Failed to get a json result in parse_subscribe, got: %s", buf);
        parsed = false;
    }
    if (!yyjson_is_arr(res_val)) {
        LOGINFO("Result in parse_subscribe not an array");
        parsed = false;
    }
    size = yyjson_arr_size(res_val);
    if (size < 3) {
        LOGINFO("Result in parse_subscribe array too small");
        parsed = false;
    }
    notify_val = find_notify(res_val);
    if (!notify_val) {
        LOGINFO("Failed to find notify in parse_subscribe");
        parsed = false;
    }
    if (!parsed) {
        if (++retries < 3) {
            /* We don't want this response so put it on the proxy
             * recvd list to be parsed later */
            append_proxy_line(proxi, buf);
            buf = NULL;
            if (val) {
                yyjson_doc_free(val);
                val = NULL;
            }
            goto retry;
        }
        LOGNOTICE(
            "Proxy %d:%d %s failed to parse subscribe response in parse_subscribe", proxi->id, proxi->subid,
            proxi->url);
        goto out;
    }

    tmp = yyjson_arr_get(res_val, 1);
    if (!tmp || !yyjson_is_str(tmp)) {
        LOGWARNING("Failed to parse enonce1 in parse_subscribe");
        goto out;
    }
    string = yyjson_get_str(tmp);
    old = proxi->enonce1;
    proxi->enonce1 = strdup(string);
    free(old);
    proxi->nonce1len = strlen(proxi->enonce1) / 2;
    if (proxi->nonce1len > 15) {
        LOGWARNING("Nonce1 too long at %d", proxi->nonce1len);
        goto out;
    }
    old = proxi->enonce1bin;
    proxi->enonce1bin = ckalloc(proxi->nonce1len);
    free(old);
    hex2bin(proxi->enonce1bin, proxi->enonce1, proxi->nonce1len);
    tmp = yyjson_arr_get(res_val, 2);
    if (!tmp || !yyjson_is_int(tmp)) {
        LOGWARNING("Failed to parse nonce2len in parse_subscribe");
        goto out;
    }
    size = yyjson_get_sint(tmp);
    if (size < 1 || size > 8) {
        LOGWARNING("Invalid nonce2len %d in parse_subscribe", size);
        goto out;
    }
    if (size < 3) {
        if (!proxi->subid) {
            LOGWARNING("Proxy %d %s Nonce2 length %d too small for fast miners", proxi->id, proxi->url, size);
        } else {
            LOGNOTICE("Proxy %d:%d Nonce2 length %d too small for fast miners", proxi->id, proxi->subid, size);
        }
    }
    proxi->nonce2len = size;
    proxi->clients_per_proxy = 1ll << ((size - 3) * 8);

    LOGNOTICE(
        "Found notify for new proxy %d:%d with enonce %s nonce2len %d", proxi->id, proxi->subid, proxi->enonce1,
        proxi->nonce2len);
    ret = true;

out:
    if (val)
        yyjson_doc_free(val);
    free(buf);
    return ret;
}

/* cs semaphore must be held */
static bool subscribe_stratum(connsock_t* cs, proxy_instance_t* proxi) {
    yyjson_mut_doc* req;
    bool            ret = false;

retry:
    /* Attempt to connect with the client description g*/
    if (!proxi->no_params) {
        req = yyjson_mut_pack("{s:i,s:s,s:[s]}", "id", 0, "method", "mining.subscribe", "params", PACKAGE "/" VERSION);
        /* Then try without any parameters */
    } else {
        req = yyjson_mut_pack("{s:i,s:s,s:[]}", "id", 0, "method", "mining.subscribe", "params");
    }
    ret = send_yyjson_msg(cs, req);
    yyjson_mut_doc_free(req);
    if (!ret) {
        LOGNOTICE("Proxy %d:%d %s failed to send message in subscribe_stratum", proxi->id, proxi->subid, proxi->url);
        goto out;
    }
    ret = parse_subscribe(cs, proxi);
    if (ret)
        goto out;

    if (proxi->no_params) {
        LOGNOTICE(
            "Proxy %d:%d %s failed all subscription options in subscribe_stratum", proxi->id, proxi->subid, proxi->url);
        goto out;
    }
    LOGINFO(
        "Proxy %d:%d %s failed connecting with parameters in subscribe_stratum, retrying without", proxi->id,
        proxi->subid, proxi->url);
    proxi->no_params = true;
    ret = connect_proxy(cs, proxi);
    if (!ret) {
        LOGNOTICE("Proxy %d:%d %s failed to reconnect in subscribe_stratum", proxi->id, proxi->subid, proxi->url);
        goto out;
    }
    goto retry;

out:
    if (!ret && cs->fd > 0) {
        epoll_ctl(proxi->epfd, EPOLL_CTL_DEL, cs->fd, NULL);
        Close(cs->fd);
    }
    return ret;
}

/* cs semaphore must be held */
static bool passthrough_stratum(connsock_t* cs, proxy_instance_t* proxi) {
    yyjson_val *    res_val, *err_val;
    yyjson_doc*     val = NULL;
    bool            res, ret = false;
    yyjson_mut_doc* req;
    float           timeout = 10;

    req = yyjson_mut_pack("{ss,s[s]}", "method", "mining.passthrough", "params", PACKAGE "/" VERSION);
    res = send_yyjson_msg(cs, req);
    yyjson_mut_doc_free(req);
    if (!res) {
        LOGWARNING("Failed to send message in passthrough_stratum");
        goto out;
    }
    if (read_socket_line(cs, &timeout) < 1) {
        LOGWARNING("Failed to receive line in passthrough_stratum");
        goto out;
    }
    /* Ignore err_val here since we should always get a result from an
     * upstream passthrough server */
    val = yyjson_msg_result(cs->buf, &res_val, &err_val);
    if (!val || !res_val) {
        LOGWARNING("Failed to get a json result in passthrough_stratum, got: %s", cs->buf);
        goto out;
    }
    ret = yyjson_is_true(res_val);
    if (!ret) {
        LOGWARNING("Denied passthrough for stratum");
        goto out;
    }
    proxi->passthrough = true;
out:
    if (val)
        yyjson_doc_free(val);
    if (!ret)
        Close(cs->fd);
    return ret;
}

/* cs semaphore must be held */
static bool node_stratum(connsock_t* cs, proxy_instance_t* proxi) {
    yyjson_val *    res_val, *err_val;
    yyjson_doc*     val = NULL;
    bool            res, ret = false;
    yyjson_mut_doc* req;
    float           timeout = 10;

    req = yyjson_mut_pack("{ss,s[s]}", "method", "mining.node", "params", PACKAGE "/" VERSION);

    res = send_yyjson_msg(cs, req);
    yyjson_mut_doc_free(req);
    if (!res) {
        LOGWARNING("Failed to send message in node_stratum");
        goto out;
    }
    if (read_socket_line(cs, &timeout) < 1) {
        LOGWARNING("Failed to receive line in node_stratum");
        goto out;
    }
    /* Ignore err_val here since we should always get a result from an
     * upstream server */
    val = yyjson_msg_result(cs->buf, &res_val, &err_val);
    if (!val || !res_val) {
        LOGWARNING("Failed to get a json result in node_stratum, got: %s", cs->buf);
        goto out;
    }
    ret = yyjson_is_true(res_val);
    if (!ret) {
        LOGWARNING("Denied node setup for stratum");
        goto out;
    }
    proxi->node = true;
out:
    if (val)
        yyjson_doc_free(val);
    if (!ret)
        Close(cs->fd);
    return ret;
}

static void send_notify(proxy_instance_t* proxi, notify_instance_t* ni);

static void reconnect_generator(void) {
    send_proc(ckpool.generator, "reconnect");
}

struct genwork* generator_getbase(void) {
    gdata_t*           gdata = ckpool.gdata;
    gbtbase_t*         gbt = NULL;
    server_instance_t* si;
    connsock_t*        cs;

    /* Use temporary variables to prevent deref while accessing */
    si = gdata->current_si;
    if (unlikely(!si)) {
        LOGWARNING("No live current server in generator_genbase");
        goto out;
    }
    cs = &si->cs;
    gbt = ckzalloc(sizeof(gbtbase_t));
    if (unlikely(!gen_gbtbase(cs, gbt))) {
        LOGWARNING("Failed to get block template from %s:%s", cs->url, cs->port);
        si->alive = cs->alive = false;
        reconnect_generator();
        dealloc(gbt);
    }
out:
    return gbt;
}

int generator_getbest(char* hash) {
    gdata_t*           gdata = ckpool.gdata;
    int                ret = GETBEST_FAILED;
    server_instance_t* si;
    connsock_t*        cs;

    si = gdata->current_si;
    if (unlikely(!si)) {
        LOGWARNING("No live current server in generator_getbest");
        goto out;
    }
    if (si->notify) {
        ret = GETBEST_NOTIFY;
        goto out;
    }
    cs = &si->cs;
    if (unlikely(!get_bestblockhash(cs, hash))) {
        LOGWARNING("Failed to get best block hash from %s:%s", cs->url, cs->port);
        goto out;
    }
    ret = GETBEST_SUCCESS;
out:
    return ret;
}

/* Is there a bitcoind we can ask anything of right now? Used to tell an
 * address bitcoind rejected apart from one we could not put to it at all. */
bool generator_alive(void) {
    gdata_t* gdata = ckpool.gdata;

    return gdata && gdata->current_si;
}

bool generator_checkaddr(const char* addr, bool* script, bool* segwit) {
    gdata_t*           gdata = ckpool.gdata;
    server_instance_t* si;
    int                ret = false;
    connsock_t*        cs;

    si = gdata->current_si;
    if (unlikely(!si)) {
        LOGWARNING("No live current server in generator_checkaddr");
        goto out;
    }
    cs = &si->cs;
    ret = validate_address(cs, addr, script, segwit);
out:
    return ret;
}

char* generator_checktxn(const char* txn) {
    yyjson_doc*        doc;
    gdata_t*           gdata = ckpool.gdata;
    server_instance_t* si;
    char*              ret = NULL;
    connsock_t*        cs;

    si = gdata->current_si;
    if (unlikely(!si)) {
        LOGWARNING("No live current server in generator_checkaddr");
        goto out;
    }
    cs = &si->cs;
    doc = validate_txn(cs, txn);
    if (!doc) {
        LOGWARNING("Invalid response to generator_checkaddr");
        goto out;
    }
    ret = yyjson_write(doc, 0, NULL);
    yyjson_doc_free(doc);
out:
    return ret;
}

char* generator_get_txn(const char* hash) {
    gdata_t*           gdata = ckpool.gdata;
    server_instance_t* si;
    char*              ret = NULL;
    connsock_t*        cs;

    si = gdata->current_si;
    if (unlikely(!si)) {
        LOGWARNING("No live current server in generator_get_txn");
        goto out;
    }
    cs = &si->cs;
    ret = get_txn(cs, hash);
out:
    return ret;
}

static bool parse_notify(proxy_instance_t* proxi, yyjson_val* val) {
    const char *       prev_hash, *bbversion, *nbit, *ntime, *string;
    yyjson_mut_doc*    job_id = NULL;
    gdata_t*           gdata = ckpool.gdata;
    char *             coinbase1 = NULL, *coinbase2 = NULL;
    const char*        jobidbuf;
    bool               clean, ret = false;
    notify_instance_t* ni;
    yyjson_val *       arr, *jid;
    int                merkles, i;

    arr = yyjson_arr_get(val, 4);
    if (!arr || !yyjson_is_arr(arr))
        goto out;

    merkles = yyjson_arr_size(arr);
    /* merklehash is a fixed size array so reject rather than overflow it */
    if (unlikely(merkles > 16)) {
        LOGWARNING(
            "Proxy %d:%d received notify with %d merkles, exceeding max of 16", proxi->id, proxi->subid, merkles);
        goto out;
    }
    jid = yyjson_arr_get(val, 0);
    if (jid) {
        job_id = yyjson_mut_doc_new(&ckyyalc);
        yyjson_mut_doc_set_root(job_id, yyjson_val_mut_copy(job_id, jid));
    }
    prev_hash = yyjson_get_str(yyjson_arr_get(val, 1));
    string = yyjson_get_str(yyjson_arr_get(val, 2));
    if (string)
        coinbase1 = strdup(string);
    string = yyjson_get_str(yyjson_arr_get(val, 3));
    if (string)
        coinbase2 = strdup(string);
    bbversion = yyjson_get_str(yyjson_arr_get(val, 5));
    nbit = yyjson_get_str(yyjson_arr_get(val, 6));
    ntime = yyjson_get_str(yyjson_arr_get(val, 7));
    clean = yyjson_is_true(yyjson_arr_get(val, 8));
    if (!job_id || !prev_hash || !coinbase1 || !coinbase2 || !bbversion || !nbit || !ntime)
        goto out_free;
    /* These are all fixed length hex fields so reject anything else to
     * avoid overreading them or overflowing the fixed size arrays they
     * are copied to. */
    if (unlikely(strlen(prev_hash) != 64 || strlen(bbversion) != 8 || strlen(nbit) != 8 || strlen(ntime) != 8)) {
        LOGWARNING("Proxy %d:%d received notify with invalid header field lengths", proxi->id, proxi->subid);
        goto out_free;
    }
    /* Coinbase values are hex strings and must be of even length */
    if (unlikely(!strlen(coinbase1) || strlen(coinbase1) % 2 || strlen(coinbase2) % 2)) {
        LOGWARNING("Proxy %d:%d received notify with invalid coinbase lengths", proxi->id, proxi->subid);
        goto out_free;
    }
    for (i = 0; i < merkles; i++) {
        const char* merkle = yyjson_get_str(yyjson_arr_get(arr, i));

        /* Each merkle hash is a fixed 64 hex char (32 byte) value */
        if (unlikely(!merkle || strlen(merkle) != 64)) {
            LOGWARNING("Proxy %d:%d received notify with invalid merkle hash", proxi->id, proxi->subid);
            goto out_free;
        }
    }

    LOGDEBUG("Received new notify from proxy %d:%d", proxi->id, proxi->subid);
    ni = ckzalloc(sizeof(notify_instance_t));
    ni->jobid = job_id;
    jobidbuf = yyjson_get_str(jid);
    LOGDEBUG("JobID %s", jobidbuf);
    ni->coinbase1 = coinbase1;
    LOGDEBUG("Coinbase1 %s", coinbase1);
    ni->coinb1len = strlen(coinbase1) / 2;
    ni->coinbase2 = coinbase2;
    LOGDEBUG("Coinbase2 %s", coinbase2);
    memcpy(ni->prevhash, prev_hash, 65);
    LOGDEBUG("Prevhash %s", prev_hash);
    memcpy(ni->bbversion, bbversion, 9);
    LOGDEBUG("BBVersion %s", bbversion);
    memcpy(ni->nbit, nbit, 9);
    LOGDEBUG("Nbit %s", nbit);
    memcpy(ni->ntime, ntime, 9);
    LOGDEBUG("Ntime %s", ntime);
    ni->clean = clean;
    LOGDEBUG("Clean %s", clean ? "true" : "false");
    LOGDEBUG("Merkles %d", merkles);
    for (i = 0; i < merkles; i++) {
        const char* merkle = yyjson_get_str(yyjson_arr_get(arr, i));

        LOGDEBUG("Merkle %d %s", i, merkle);
        memcpy(&ni->merklehash[i][0], merkle, 65);
    }
    ni->merkles = merkles;
    ret = true;
    ni->notify_time = time(NULL);

    /* Add the notify instance to the parent proxy list, not the subproxy */
    mutex_lock(&gdata->notify_lock);
    ni->id64 = gdata->proxy_notify_id++;
    HASH_ADD_I64(gdata->notify_instances, id64, ni);
    mutex_unlock(&gdata->notify_lock);

    send_notify(proxi, ni);
    goto out;
out_free:
    if (job_id)
        yyjson_mut_doc_free(job_id);
    if (coinbase1)
        free(coinbase1);
    if (coinbase2)
        free(coinbase2);
out:
    return ret;
}

static bool parse_diff(proxy_instance_t* proxi, yyjson_val* val) {
    double diff = yyjson_get_num(yyjson_arr_get(val, 0));

    /* Ignore non finite or negative values that would wedge all the diff
     * calculations derived from this */
    if (!isfinite(diff) || diff <= 0 || diff == proxi->diff)
        return true;
    proxi->diff = diff;
    return true;
}

static bool send_version(proxy_instance_t* proxi, yyjson_val* val) {
    yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val* root;
    bool            ret;

    root = yyjson_mut_pack_val(
        doc, "{sosssn}", "id", yyjson_val_mut_copy(doc, yyjson_obj_get(val, "id")), "result", PACKAGE "/" VERSION,
        "error");
    yyjson_mut_doc_set_root(doc, root);
    ret = send_yyjson_msg(&proxi->cs, doc);
    yyjson_mut_doc_free(doc);
    return ret;
}

static bool show_message(yyjson_val* val) {
    const char* msg;

    if (!yyjson_is_arr(val))
        return false;
    msg = yyjson_get_str(yyjson_arr_get(val, 0));
    if (!msg)
        return false;
    LOGNOTICE("Pool message: %s", msg);
    return true;
}

static bool send_pong(proxy_instance_t* proxi, yyjson_val* val) {
    yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val* root;
    bool            ret;

    root = yyjson_mut_pack_val(
        doc, "{sosssn}", "id", yyjson_val_mut_copy(doc, yyjson_obj_get(val, "id")), "result", "pong", "error");
    yyjson_mut_doc_set_root(doc, root);
    ret = send_yyjson_msg(&proxi->cs, doc);
    yyjson_mut_doc_free(doc);
    return ret;
}

static void prepare_proxy(proxy_instance_t* proxi);

#ifdef HAVE_SV2
static void sv2_proxy_free(proxy_instance_t* proxi);
static void sv2_proxy_submit_share(proxy_instance_t* proxi, yyjson_mut_val* val, int64_t client_id);

/* An upstream URL is Stratum V2 when its path carries a valid base58check pool
 * authority pubkey (spec 04 §4.7) — the presence of that key is what marks it
 * SV2; the stratum2+tcp:// scheme prefix is accepted but not required. Any URL
 * scheme with a "host:port/KEY" tail (e.g. "host:port/9anr…" or
 * "stratum2+tcp://host:port/9anr…") is SV2. Sets proxi->sv2 and
 * proxi->sv2_authority. Returns false only when the URL is clearly meant to be
 * SV2 (explicit stratum2 scheme, or a path present) but the key is invalid, so
 * the caller can refuse it — we never connect an SV2 upstream unauthenticated.
 * A plain "host:port" (no path) is SV1 and returns true with sv2 left false. */
static bool proxy_parse_sv2_url(proxy_instance_t* proxi) {
    static const char b58set[] = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    const char *      url = proxi->url, *host, *path;
    bool              explicit_sv2;
    char              b58[128];
    uint8_t           authority[32];
    size_t            n;

    proxi->sv2 = false;
    if (!url)
        return true;
    explicit_sv2 = (strncasecmp(url, "stratum2", 8) == 0);
    /* Skip any scheme:// prefix to reach host:port, then find the path. */
    host = strstr(url, "//");
    host = host ? host + 2 : url;
    path = strchr(host, '/');
    if (!path || !path[1]) {
        /* No path component: plain host:port. SV1 unless the scheme
         * explicitly demanded SV2 (in which case the key is missing). */
        if (explicit_sv2) {
            LOGWARNING("SV2 proxy URL %s has no authority key in path", url);
            return false;
        }
        return true;
    }
    /* Take the leading base58 token of the path and see if it decodes as a
     * valid authority pubkey (correct version prefix + length + checksum —
     * a false positive on an SV1 path is astronomically unlikely). */
    path++;
    n = strspn(path, b58set);
    if (n >= 1 && n < sizeof(b58)) {
        memcpy(b58, path, n);
        b58[n] = '\0';
        if (sv2_noise_authority_b58_to_xonly(b58, authority))
            goto have_key;
    }
    /* Path present but not a valid authority key. */
    if (explicit_sv2) {
        LOGWARNING("SV2 proxy URL %s authority key invalid", url);
        return false;
    }
    /* Non-key path on a non-SV2 scheme: treat as a plain SV1 URL. */
    return true;

have_key:
    /* Valid authority key present → SV2 upstream. */
    proxi->sv2 = true;
    memcpy(proxi->sv2_authority, authority, 32);
    /* SV2 upstream is Mining-Protocol proxy mode only. Refuse it in modes
     * whose own upstream setup it would otherwise bypass in proxy_alive,
     * rather than silently taking over their connection handling. */
    if (ckpool.node || ckpool.passthrough || ckpool.redirector || ckpool.userproxy) {
        LOGWARNING("SV2 upstream %s is unsupported in node/passthrough/redirector/userproxy mode", url);
        return false;
    }
    LOGNOTICE("SV2 upstream configured for %s (authority key verified)", url);
    return true;
}
#endif

/* Creates a duplicate instance or proxi to be used as a subproxy, ignoring
 * fields we don't use in the subproxy. */
static proxy_instance_t*
create_subproxy(gdata_t* gdata, proxy_instance_t* proxi, const char* url, const char* baseurl) {
    proxy_instance_t* subproxy;

    mutex_lock(&gdata->lock);
    if (gdata->dead_proxies) {
        /* Recycle an old proxy instance if one exists */
        subproxy = gdata->dead_proxies;
        DL_DELETE(gdata->dead_proxies, subproxy);
    } else {
        gdata->subproxies_generated++;
        subproxy = ckzalloc(sizeof(proxy_instance_t));
    }
    mutex_unlock(&gdata->lock);

    mutex_lock(&proxi->proxy_lock);
    subproxy->subid = ++proxi->subproxy_count;
    mutex_unlock(&proxi->proxy_lock);

    subproxy->id = proxi->id;
    subproxy->userid = proxi->userid;
    subproxy->global = proxi->global;
    subproxy->url = strdup(url);
    subproxy->baseurl = strdup(baseurl);
    subproxy->auth = strdup(proxi->auth);
    subproxy->pass = strdup(proxi->pass);
    subproxy->parent = proxi;
    subproxy->epfd = proxi->epfd;
#ifdef HAVE_SV2
    /* Subproxies connect to the same upstream URL — inherit SV2 status and
     * the already-verified authority key from the parent. */
    subproxy->sv2 = proxi->sv2;
    memcpy(subproxy->sv2_authority, proxi->sv2_authority, 32);
#endif
    cksem_init(&subproxy->cs.sem);
    cksem_post(&subproxy->cs.sem);
    return subproxy;
}

static void add_subproxy(proxy_instance_t* proxi, proxy_instance_t* subproxy) {
    mutex_lock(&proxi->proxy_lock);
    HASH_ADD(sh, proxi->subproxies, subid, sizeof(int), subproxy);
    mutex_unlock(&proxi->proxy_lock);
}

static proxy_instance_t* __subproxy_by_id(proxy_instance_t* proxy, const int subid) {
    proxy_instance_t* subproxy;

    HASH_FIND(sh, proxy->subproxies, &subid, sizeof(int), subproxy);
    return subproxy;
}

/* Add to the dead list to be recycled if possible */
static void store_proxy(gdata_t* gdata, proxy_instance_t* proxy) {
    LOGINFO("Recycling data from proxy %d:%d", proxy->id, proxy->subid);

    mutex_lock(&gdata->lock);
    dealloc(proxy->enonce1);
    dealloc(proxy->url);
    dealloc(proxy->baseurl);
    dealloc(proxy->auth);
    dealloc(proxy->pass);
    dealloc(proxy->enonce1bin);
    /* connsock heap fields are not covered by the pointers above */
    dealloc(proxy->cs.buf);
    dealloc(proxy->cs.url);
    dealloc(proxy->cs.port);
#ifdef HAVE_SV2
    /* Free Noise/channel/job state before the instance is zeroed. */
    sv2_proxy_free(proxy);
#endif
    /* Drop any half-sent reference so send_json_msgq cannot UAF. */
    proxy->sending = NULL;
    memset(proxy, 0, sizeof(proxy_instance_t));
    DL_APPEND(gdata->dead_proxies, proxy);
    mutex_unlock(&gdata->lock);
}

/* The difference between a dead proxy and a deleted one is the parent proxy entry
 * is not removed from the stratifier as it assumes it is down whereas a deleted
 * proxy has had its entry removed from the generator. */
static void send_stratifier_deadproxy(const int id, const int subid) {
    char buf[256];

    if (ckpool.passthrough)
        return;
    sprintf(buf, "deadproxy=%d:%d", id, subid);
    send_proc(ckpool.stratifier, buf);
}

static void send_stratifier_delproxy(const int id, const int subid) {
    char buf[256];

    if (ckpool.passthrough)
        return;
    sprintf(buf, "delproxy=%d:%d", id, subid);
    send_proc(ckpool.stratifier, buf);
}

/* Close the subproxy socket if it's open and remove it from the epoll list */
static void close_proxy_socket(proxy_instance_t* proxy, proxy_instance_t* subproxy) {
    if (subproxy->cs.fd > 0) {
        epoll_ctl(proxy->epfd, EPOLL_CTL_DEL, subproxy->cs.fd, NULL);
        Close(subproxy->cs.fd);
    }
}

/* Remove the subproxy from the proxi list and put it on the dead list.
 * Further use of a non-parent subproxy pointer is invalid after return
 * (instance may already be zeroed and on the recycle list). Parent proxies
 * are only marked dead and closed — not recycled here. */
static void disable_subproxy(gdata_t* gdata, proxy_instance_t* proxi, proxy_instance_t* subproxy) {
    int subid;

    if (!subproxy)
        return;

    subproxy->alive = false;
    subid = subproxy->subid;
    send_stratifier_deadproxy(subproxy->id, subid);
    close_proxy_socket(proxi, subproxy);
    if (parent_proxy(subproxy))
        return;

    subproxy->disabled = true;
    subproxy->sending = NULL;

    mutex_lock(&proxi->proxy_lock);
    /* Re-resolve under lock so concurrent disable only recycles once. */
    subproxy = __subproxy_by_id(proxi, subid);
    if (likely(subproxy))
        HASH_DELETE(sh, proxi->subproxies, subproxy);
    mutex_unlock(&proxi->proxy_lock);

    if (subproxy) {
        send_stratifier_deadproxy(subproxy->id, subproxy->subid);
        store_proxy(gdata, subproxy);
    }
}

/* Copy the host part of a "host:port" or bare "host" url into buf, dropping the
 * port. Returns false if there is no host. */
static bool url_host(char* buf, const size_t bufsize, const char* url) {
    const char* colon;
    size_t      len;

    if (!url || !url[0])
        return false;
    /* Rightmost colon so IPv6 literals are not truncated at the first
     * group separator; a bracketed [::1]:port keeps the bracket which is
     * fine for an exact comparison. */
    colon = strrchr(url, ':');
    len = colon ? (size_t)(colon - url) : strlen(url);
    if (!len || len >= bufsize)
        return false;
    memcpy(buf, url, len);
    buf[len] = '\0';
    return true;
}

/* True if the reconnect target host is the same as, or a subdomain of, the
 * configured pool host. The configured host must have at least two labels so a
 * bare TLD cannot match every host under it, which was the original bug: a
 * ".com" suffix compare accepted any *.com host. Comparison is case
 * insensitive as DNS names are. Fails closed. */
static bool reconnect_host_allowed(const char* pool_url, const char* new_host) {
    char   pool_host[256];
    size_t plen, nlen;

    if (!url_host(pool_host, sizeof(pool_host), pool_url))
        return false;
    /* Require the configured host to contain a dot, i.e. at least two
     * labels, otherwise "example" or a bare TLD would match too broadly. */
    if (!strchr(pool_host, '.'))
        return false;
    /* Exact host match. */
    if (!strcasecmp(pool_host, new_host))
        return true;
    /* Subdomain match: new_host must end with ".<pool_host>". */
    plen = strlen(pool_host);
    nlen = strlen(new_host);
    if (nlen > plen + 1 && new_host[nlen - plen - 1] == '.' && !strcasecmp(new_host + nlen - plen, pool_host))
        return true;
    return false;
}

static bool parse_reconnect(proxy_instance_t* proxy, yyjson_val* val) {
    bool              sameurl = false, ret = false;
    gdata_t*          gdata = ckpool.gdata;
    proxy_instance_t* parent;
    const char*       new_url;
    int               new_port;
    char*             url;

    /* Operators can refuse all upstream redirects outright. */
    if (!ckpool.reconnect) {
        LOGWARNING("Denied stratum reconnect request from %s with reconnect disabled", proxy->url);
        goto out;
    }

    new_url = yyjson_get_str(yyjson_arr_get(val, 0));
    new_port = yyjson_get_sint(yyjson_arr_get(val, 1));
    /* See if we have an invalid entry listing port as a string instead of
     * integer and handle that. */
    if (!new_port) {
        const char* newport_string = yyjson_get_str(yyjson_arr_get(val, 1));

        if (newport_string)
            sscanf(newport_string, "%d", &new_port);
    }
    if (new_url && strlen(new_url) && new_port) {
        char new_host[256];

        if (!url_host(new_host, sizeof(new_host), new_url)) {
            LOGWARNING("Denied stratum reconnect request to url without host %s", new_url);
            goto out;
        }
        if (!reconnect_host_allowed(proxy->url, new_host)) {
            LOGWARNING("Denied stratum reconnect request from %s to non-matching host %s", proxy->url, new_url);
            goto out;
        }
        ASPRINTF(&url, "%s:%d", new_url, new_port);
    } else {
        url = strdup(proxy->url);
        sameurl = true;
    }
    LOGINFO("Processing reconnect request to %s", url);

    ret = true;
    parent = proxy->parent;
    if (parent != proxy) {
        /* Do not store_proxy here — the recv loop still holds this
         * pointer. Mark dead and close; hangup path will recycle. */
        proxy->alive = false;
        send_stratifier_deadproxy(proxy->id, proxy->subid);
        close_proxy_socket(parent, proxy);
        free(url);
        goto out;
    }

    disable_subproxy(gdata, parent, proxy);
    proxy->reconnect = true;
    LOGWARNING("Proxy %d:%s reconnect issue to %s, dropping existing connection", proxy->id, proxy->url, url);
    if (!sameurl) {
        char* oldurl = proxy->url;

        proxy->url = url;
        free(oldurl);
    } else
        free(url);
out:
    return ret;
}

static void send_diff(proxy_instance_t* proxi) {
    proxy_instance_t* proxy = proxi->parent;
    yyjson_mut_doc*   doc;
    char *            msg, *buf;

    /* Not set yet */
    if (!proxi->diff)
        return;

    doc = yyjson_mut_pack("{sisisf}", "proxy", proxy->id, "subproxy", proxi->subid, "diff", proxi->diff);
    msg = yyjson_mut_write(doc, 0, NULL);
    yyjson_mut_doc_free(doc);
    ASPRINTF(&buf, "diff=%s", msg);
    free(msg);
    send_proc(ckpool.stratifier, buf);
    free(buf);
}

/*
 * Latch what we last said about this proxy's payout, so each distinct answer
 * costs one line rather than one per notify. Returns false when we have already
 * said this.
 */
static bool payout_changed(proxy_instance_t* proxi, const char* pay) {
    if (!strcmp(proxi->payaddr, pay))
        return false;
    snprintf(proxi->payaddr, sizeof(proxi->payaddr), "%s", pay);
    return true;
}

/*
 * Say who the work we are about to mine pays, whatever protocol it arrived on:
 * the coinbase outputs are in the notify either way, so an SV1 upstream and an
 * SV2 one without job declaration are both checkable, and a solo pool paying
 * anything but our own address is visible without decoding a coinbase by hand.
 * Only logged when it changes, which is once per connection unless the upstream
 * really does move the payout. A script does not say which chain it is on, so
 * it is decoded against the account part of the username we authorise with,
 * which for a solo pool is the address itself.
 */
static void check_payout(proxy_instance_t* proxi, const notify_instance_t* ni) {
    uchar       script[MAX_PAYOUT_SCRIPT];
    char        addr[128], desc[sizeof(script) * 2 + 64], account[128] = {}, *cb1, *cb2;
    int         cb1len, cb2len, slen, n;
    const char* pay = addr;
    int64_t     value = 0;
    size_t      alen;

    /* Both halves are hex here, whichever protocol built them, so take their
     * lengths from the strings themselves rather than trusting coinb1len. */
    cb1len = strlen(ni->coinbase1) / 2;
    cb2len = strlen(ni->coinbase2) / 2;
    cb1 = ckalloc(cb1len + 1);
    cb2 = ckalloc(cb2len + 1);
    hex2bin(cb1, ni->coinbase1, cb1len);
    hex2bin(cb2, ni->coinbase2, cb2len);
    slen = sizeof(script);
    n = coinbase_payout_script(
        script, &slen, &value, (uchar*)cb1, cb1len, proxi->nonce1len + proxi->nonce2len, (uchar*)cb2, cb2len);
    dealloc(cb1);
    dealloc(cb2);
    if (n < 0) {
        /*
         * Latched like a payout, and said as loudly, because it means this
         * check is not running: with nothing else logged, silence would
         * otherwise read as a payout we had looked at and approved. The
         * parenthesised form cannot collide with an address or with the
         * non-standard script description below.
         */
        if (payout_changed(proxi, "(unreadable coinbase)")) {
            LOGNOTICE("Proxy %d:%d work has no coinbase payout we can read", proxi->id, proxi->subid);
        }
        return;
    }
    if (proxi->auth) {
        alen = strcspn(proxi->auth, "._");
        if (alen >= sizeof(account))
            alen = sizeof(account) - 1;
        memcpy(account, proxi->auth, alen);
        account[alen] = '\0';
    }
    if (!txn_to_address(addr, sizeof(addr), script, slen, account)) {
        char hex[sizeof(script) * 2 + 1];

        __bin2hex(hex, script, slen);
        snprintf(desc, sizeof(desc), "a non-standard %d byte script: %s", slen, hex);
        pay = desc;
    }
    if (!payout_changed(proxi, pay))
        return;
    LOGWARNING(
        "Proxy %d:%d work pays %s%s (%.8f BTC of %d output%s)", proxi->id, proxi->subid, pay,
        !strcasecmp(pay, account) ? ", the address we authorise as" : "", (double)value / 100000000, n,
        n > 1 ? "s" : "");
}

static void send_notify(proxy_instance_t* proxi, notify_instance_t* ni) {
    proxy_instance_t* proxy = proxi->parent;
    yyjson_mut_val *  root, *merkle_arr;
    yyjson_mut_doc*   doc;
    char *            msg, *buf;
    int               i;

    check_payout(proxi, ni);
    doc = yyjson_mut_doc_new(&ckyyalc);
    merkle_arr = yyjson_mut_arr(doc);

    for (i = 0; i < ni->merkles; i++)
        yyjson_mut_arr_add_strcpy(doc, merkle_arr, &ni->merklehash[i][0]);
    /* Use our own jobid instead of the server's one for easy lookup */
    root = yyjson_mut_pack_val(
        doc, "{sisisIsssisssssosssssssb}", "proxy", proxy->id, "subproxy", proxi->subid, "jobid", ni->id64, "prevhash",
        ni->prevhash, "coinb1len", ni->coinb1len, "coinbase1", ni->coinbase1, "coinbase2", ni->coinbase2, "merklehash",
        merkle_arr, "bbversion", ni->bbversion, "nbit", ni->nbit, "ntime", ni->ntime, "clean", ni->clean);
    yyjson_mut_doc_set_root(doc, root);

    msg = yyjson_mut_write(doc, 0, NULL);
    yyjson_mut_doc_free(doc);
    ASPRINTF(&buf, "notify=%s", msg);
    free(msg);
    send_proc(ckpool.stratifier, buf);
    free(buf);

    /* Send diff now as stratifier will not accept diff till it has a
     * valid workbase */
    send_diff(proxi);
}

static bool parse_method(proxy_instance_t* proxi, const char* msg) {
    yyjson_val *val, *method, *err_val, *params;
    yyjson_doc* doc = NULL;
    bool        ret = false;
    const char* buf;

    if (!msg)
        goto out;
    doc = yyjson_read(msg, strlen(msg), 0);
    if (!doc) {
        if (proxi->global) {
            LOGWARNING("JSON decode of proxy %d:%s msg %s failed", proxi->id, proxi->url, msg);
        } else {
            LOGNOTICE("JSON decode of proxy %d:%s msg %s failed", proxi->id, proxi->url, msg);
        }
        goto out;
    }
    val = yyjson_doc_get_root(doc);

    method = yyjson_obj_get(val, "method");
    if (!method) {
        /* Likely a share, look for harmless unhandled methods in
         * pool response */
        if (strstr(msg, "mining.suggest")) {
            LOGINFO("Unhandled suggest_diff from proxy %d:%s", proxi->id, proxi->url);
            ret = true;
        } else
            LOGDEBUG("Failed to find method in json for parse_method");
        goto out;
    }
    err_val = yyjson_obj_get(val, "error");
    params = yyjson_obj_get(val, "params");

    if (err_val && !yyjson_is_null(err_val)) {
        char* ss;

        if (err_val)
            ss = yyjson_val_write(err_val, 0, NULL);
        else
            ss = strdup("(unknown reason)");

        LOGINFO("JSON-RPC method decode failed: %s", ss);
        free(ss);
        goto out;
    }

    if (!yyjson_is_str(method)) {
        LOGINFO("Method is not string in parse_method");
        goto out;
    }
    buf = yyjson_get_str(method);
    if (!buf || strlen(buf) < 1) {
        LOGINFO("Invalid string for method in parse_method");
        goto out;
    }

    LOGDEBUG("Proxy %d:%d received method %s", proxi->id, proxi->subid, buf);
    if (cmdmatch(buf, "mining.notify")) {
        ret = parse_notify(proxi, params);
        goto out;
    }

    if (cmdmatch(buf, "mining.set_difficulty")) {
        ret = parse_diff(proxi, params);
        if (likely(ret))
            send_diff(proxi);
        goto out;
    }

    if (cmdmatch(buf, "client.reconnect")) {
        ret = parse_reconnect(proxi, params);
        goto out;
    }

    if (cmdmatch(buf, "client.get_version")) {
        ret = send_version(proxi, val);
        goto out;
    }

    if (cmdmatch(buf, "client.show_message")) {
        ret = show_message(params);
        goto out;
    }

    if (cmdmatch(buf, "mining.ping")) {
        ret = send_pong(proxi, val);
        goto out;
    }
out:
    if (doc)
        yyjson_doc_free(doc);
    return ret;
}

/* cs semaphore must be held */
static bool auth_stratum(connsock_t* cs, proxy_instance_t* proxi) {
    yyjson_val *    res_val, *err_val;
    yyjson_doc*     val = NULL;
    yyjson_mut_doc* req;
    char*           buf = NULL;
    bool            ret;

    req = yyjson_mut_pack(
        "{s:i,s:s,s:[s,s]}", "id", 42, "method", "mining.authorize", "params", proxi->auth, proxi->pass);
    ret = send_yyjson_msg(cs, req);
    yyjson_mut_doc_free(req);
    if (!ret) {
        LOGNOTICE("Proxy %d:%d %s failed to send message in auth_stratum", proxi->id, proxi->subid, proxi->url);
        if (cs->fd > 0) {
            epoll_ctl(proxi->epfd, EPOLL_CTL_DEL, cs->fd, NULL);
            Close(cs->fd);
        }
        goto out;
    }

    /* Read and parse any extra methods sent. Anything left in the buffer
     * should be the response to our auth request. */
    do {
        free(buf);
        buf = next_proxy_line(cs, proxi);
        if (!buf) {
            LOGNOTICE("Proxy %d:%d %s failed to receive line in auth_stratum", proxi->id, proxi->subid, proxi->url);
            ret = false;
            goto out;
        }
        ret = parse_method(proxi, buf);
    } while (ret);

    val = yyjson_msg_result(buf, &res_val, &err_val);
    if (!val) {
        if (proxi->global) {
            LOGWARNING(
                "Proxy %d:%d %s failed to get a json result in auth_stratum, got: %s", proxi->id, proxi->subid,
                proxi->url, buf);
        } else {
            LOGNOTICE(
                "Proxy %d:%d %s failed to get a json result in auth_stratum, got: %s", proxi->id, proxi->subid,
                proxi->url, buf);
        }
        goto out;
    }

    if (err_val && !yyjson_is_null(err_val)) {
        LOGWARNING(
            "Proxy %d:%d %s failed to authorise in auth_stratum due to err_val, got: %s", proxi->id, proxi->subid,
            proxi->url, buf);
        goto out;
    }
    if (res_val) {
        ret = yyjson_is_true(res_val);
        if (!ret) {
            if (proxi->global) {
                LOGWARNING(
                    "Proxy %d:%d %s failed to authorise in auth_stratum, got: %s", proxi->id, proxi->subid, proxi->url,
                    buf);
            } else {
                LOGNOTICE(
                    "Proxy %d:%d %s failed to authorise in auth_stratum, got: %s", proxi->id, proxi->subid, proxi->url,
                    buf);
            }
            goto out;
        }
    } else {
        /* No result and no error but successful val means auth success */
        ret = true;
    }
    LOGINFO("Proxy %d:%d %s auth success in auth_stratum", proxi->id, proxi->subid, proxi->url);
out:
    if (val)
        yyjson_doc_free(val);
    if (ret) {
        /* Now parse any cached responses so there are none in the
         * queue and they can be managed one at a time from now on. */
        while (42) {
            dealloc(buf);
            buf = cached_proxy_line(proxi);
            if (!buf)
                break;
            parse_method(proxi, buf);
        };
    }
    return ret;
}

static proxy_instance_t* proxy_by_id(gdata_t* gdata, const int id) {
    proxy_instance_t* proxi;

    mutex_lock(&gdata->lock);
    HASH_FIND_INT(gdata->proxies, &id, proxi);
    mutex_unlock(&gdata->lock);

    return proxi;
}

static void send_subscribe(proxy_instance_t* proxi) {
    yyjson_mut_doc* doc;
    char *          msg, *buf;

    doc = yyjson_mut_pack(
        "{ss,ss,ss,ss,si,si,ss,si,sb,si}", "baseurl", proxi->baseurl, "url", proxi->url, "auth", proxi->auth, "pass",
        proxi->pass, "proxy", proxi->id, "subproxy", proxi->subid, "enonce1", proxi->enonce1, "nonce2len",
        proxi->nonce2len, "global", proxi->global, "userid", proxi->userid);
    msg = yyjson_mut_write(doc, 0, NULL);
    yyjson_mut_doc_free(doc);
    ASPRINTF(&buf, "subscribe=%s", msg);
    free(msg);
    send_proc(ckpool.stratifier, buf);
    free(buf);
}

static proxy_instance_t* subproxy_by_id(proxy_instance_t* proxy, const int subid) {
    proxy_instance_t* subproxy;

    mutex_lock(&proxy->proxy_lock);
    subproxy = __subproxy_by_id(proxy, subid);
    mutex_unlock(&proxy->proxy_lock);

    return subproxy;
}

static void drop_proxy(gdata_t* gdata, const char* buf) {
    proxy_instance_t *proxy, *subproxy;
    int               id = -1, subid = -1;

    sscanf(buf, "dropproxy=%d:%d", &id, &subid);
    if (unlikely(!subid)) {
        LOGWARNING("Generator asked to drop parent proxy %d", id);
        return;
    }
    proxy = proxy_by_id(gdata, id);
    if (unlikely(!proxy)) {
        LOGINFO("Generator asked to drop subproxy from non-existent parent %d", id);
        return;
    }
    subproxy = subproxy_by_id(proxy, subid);
    if (!subproxy) {
        LOGINFO("Generator asked to drop non-existent subproxy %d:%d", id, subid);
        return;
    }
    LOGNOTICE("Generator asked to drop proxy %d:%d", id, subid);
    disable_subproxy(gdata, proxy, subproxy);
}

static void stratifier_reconnect_client(const int64_t id) {
    char buf[256];

    sprintf(buf, "reconnclient=%" PRId64, id);
    send_proc(ckpool.stratifier, buf);
}

/* Add a share to the gdata share hashlist. Returns the share id */
static int add_share(gdata_t* gdata, const int64_t client_id, const double diff) {
    share_msg_t *share = ckzalloc(sizeof(share_msg_t)), *tmpshare;
    time_t       now;
    int          ret;

    share->submit_time = now = time(NULL);
    share->client_id = client_id;
    share->diff = diff;

    /* Add new share entry to the share hashtable. Age old shares */
    mutex_lock(&gdata->share_lock);
    ret = share->id64 = gdata->share_id++;
    HASH_ADD_I64(gdata->shares, id64, share);
    HASH_ITER(hh, gdata->shares, share, tmpshare) {
        if (share->submit_time < now - 120) {
            HASH_DEL(gdata->shares, share);
            free(share);
        }
    }
    mutex_unlock(&gdata->share_lock);

    return ret;
}

static void submit_share(gdata_t* gdata, yyjson_mut_doc* doc) {
    yyjson_mut_val*   val = yyjson_mut_doc_get_root(doc);
    proxy_instance_t *proxy, *proxi;
    int               id, subid, share_id;
    bool              success = false;
    stratum_msg_t*    msg;
    int64_t           client_id;

    /* Get the client id so we can tell the stratifier to drop it if the
     * proxy it's bound to is not functional */
    if (unlikely(!yyjson_mut_obj_get_int64(&client_id, val, "client_id"))) {
        LOGWARNING("Got no client_id in share");
        goto out;
    }
    if (unlikely(!yyjson_mut_obj_get_int(&id, val, "proxy"))) {
        LOGWARNING("Got no proxy in share");
        goto out;
    }
    if (unlikely(!yyjson_mut_obj_get_int(&subid, val, "subproxy"))) {
        LOGWARNING("Got no subproxy in share");
        goto out;
    }
    proxy = proxy_by_id(gdata, id);
    if (unlikely(!proxy)) {
        LOGINFO("Client %" PRId64 " sending shares to non existent proxy %d, dropping", client_id, id);
        stratifier_reconnect_client(client_id);
        goto out;
    }
    proxi = subproxy_by_id(proxy, subid);
    if (unlikely(!proxi)) {
        LOGINFO("Client %" PRId64 " sending shares to non existent subproxy %d:%d, dropping", client_id, id, subid);
        stratifier_reconnect_client(client_id);
        goto out;
    }
    if (!proxi->alive) {
        LOGINFO("Client %" PRId64 " sending shares to dead subproxy %d:%d, dropping", client_id, id, subid);
        stratifier_reconnect_client(client_id);
        goto out;
    }
#ifdef HAVE_SV2
    if (proxi->sv2) {
        /* Encode SubmitSharesExtended and send on the Noise transport,
         * never SV1 JSON. Accounting happens on SubmitShares.Success/.Error. */
        sv2_proxy_submit_share(proxi, val, client_id);
        goto out;
    }
#endif

    success = true;
    msg = ckzalloc(sizeof(stratum_msg_t));
    msg->doc = doc;
    share_id = add_share(gdata, client_id, proxi->diff);
    yyjson_mut_obj_add_int(doc, val, "id", share_id);

    /* Add the new message to the psend list */
    mutex_lock(&gdata->psend_lock);
    gdata->psends_generated++;
    DL_APPEND(gdata->psends, msg);
    pthread_cond_signal(&gdata->psend_cond);
    mutex_unlock(&gdata->psend_lock);

out:
    if (!success)
        yyjson_mut_doc_free(doc);
}

static void clear_notify(notify_instance_t* ni) {
    if (ni->jobid)
        yyjson_mut_doc_free(ni->jobid);
    free(ni->coinbase1);
    free(ni->coinbase2);
#ifdef HAVE_SV2
    free(ni->sv2_dcb_prefix);
    free(ni->sv2_dcb_suffix);
    /* Releases the template's IPC handle once no job or solve needs it. */
    sv2_jdc_template_put(ni->sv2_tmpl);
#endif
    free(ni);
}

/* Entered with proxy_lock held */
static void __decay_proxy(proxy_instance_t* proxy, proxy_instance_t* parent, const double diff) {
    double tdiff;
    tv_t   now_t;

    tv_time(&now_t);
    tdiff = sane_tdiff(&now_t, &proxy->last_decay);
    decay_time(&proxy->dsps1, diff, tdiff, MIN1);
    decay_time(&proxy->dsps5, diff, tdiff, MIN5);
    decay_time(&proxy->dsps60, diff, tdiff, HOUR);
    decay_time(&proxy->dsps1440, diff, tdiff, DAY);
    copy_tv(&proxy->last_decay, &now_t);

    tdiff = sane_tdiff(&now_t, &parent->total_last_decay);
    decay_time(&parent->tdsps1, diff, tdiff, MIN1);
    decay_time(&parent->tdsps5, diff, tdiff, MIN5);
    decay_time(&parent->tdsps60, diff, tdiff, HOUR);
    decay_time(&parent->tdsps1440, diff, tdiff, DAY);
    copy_tv(&parent->total_last_decay, &now_t);
}

static void account_shares(proxy_instance_t* proxy, const double diff, const bool result) {
    proxy_instance_t* parent = proxy->parent;

    mutex_lock(&parent->proxy_lock);
    if (result) {
        proxy->diff_accepted += diff;
        parent->total_accepted += diff;
        __decay_proxy(proxy, parent, diff);
    } else {
        proxy->diff_rejected += diff;
        parent->total_rejected += diff;
        __decay_proxy(proxy, parent, 0);
    }
    mutex_unlock(&parent->proxy_lock);
}

/* Returns zero if it is not recognised as a share, 1 if it is a valid share
 * and -1 if it is recognised as a share but invalid. */
static int parse_share(gdata_t* gdata, proxy_instance_t* proxi, const char* buf) {
    yyjson_val * val, *idval;
    yyjson_doc*  doc = NULL;
    bool         result = false;
    share_msg_t* share;
    int          ret = 0;
    int64_t      id;

    doc = yyjson_read(buf, strlen(buf), 0);
    if (unlikely(!doc)) {
        LOGINFO("Failed to parse upstream json msg: %s", buf);
        goto out;
    }
    val = yyjson_doc_get_root(doc);
    idval = yyjson_obj_get(val, "id");
    if (unlikely(!idval)) {
        LOGINFO("Failed to find id in upstream json msg: %s", buf);
        goto out;
    }
    id = yyjson_get_sint(idval);
    {
        yyjson_val* res_val = yyjson_obj_get(val, "result");

        if (!yyjson_is_bool(res_val)) {
            yyjson_val* err_val = yyjson_obj_get(val, "error");

            if (unlikely(!(yyjson_is_null(res_val) && err_val && !yyjson_is_null(err_val)))) {
                LOGINFO("Failed to find result in upstream json msg: %s", buf);
                goto out;
            }
            result = false;
        } else
            result = yyjson_get_bool(res_val);
    }

    mutex_lock(&gdata->share_lock);
    HASH_FIND_I64(gdata->shares, &id, share);
    if (share)
        HASH_DEL(gdata->shares, share);
    mutex_unlock(&gdata->share_lock);

    if (!share) {
        LOGINFO("Proxy %d:%d failed to find matching share to result: %s", proxi->id, proxi->subid, buf);
        /* We don't know what diff these shares are so assume the
         * current proxy diff. */
        account_shares(proxi, proxi->diff, result);
        ret = -1;
        goto out;
    }
    ret = 1;
    account_shares(proxi, share->diff, result);
    LOGINFO("Proxy %d:%d share result %s from client %" PRId64, proxi->id, proxi->subid, buf, share->client_id);
    free(share);
out:
    if (doc)
        yyjson_doc_free(doc);
    return ret;
}

struct cs_msg {
    cs_msg_t*         next;
    cs_msg_t*         prev;
    proxy_instance_t* proxy;
    char*             buf;
    int               len;
    int               ofs;
};

/* Sends all messages in the queue ready to be dispatched, leaving those that
 * would block to be handled next pass */
static void send_json_msgq(gdata_t* gdata, cs_msg_t** csmsgq) {
    cs_msg_t *csmsg, *tmp;
    int       ret;

    DL_FOREACH_SAFE(*csmsgq, csmsg, tmp) {
        proxy_instance_t* proxy = csmsg->proxy;
        bool              drop = false;

        /* Only try to send one message at a time to each proxy
         * to avoid sending parts of different messages */
        if (proxy->sending && proxy->sending != csmsg)
            continue;
        while (csmsg->len > 0) {
            int fd;

            if (unlikely(!proxy->alive)) {
                LOGDEBUG("Dropping send message to dead proxy %d:%d in send_json_msgq", proxy->id, proxy->subid);
                csmsg->len = 0;
                break;
            }
            proxy->sending = csmsg;
            fd = proxy->cs.fd;
            ret = send(fd, csmsg->buf + csmsg->ofs, csmsg->len, MSG_DONTWAIT);
            if (ret < 1) {
                if (!ret)
                    break;
                ret = 0;
                if (errno == EAGAIN || errno == EWOULDBLOCK)
                    break;
                csmsg->len = 0;
                LOGNOTICE(
                    "Proxy %d:%d %s failed to send msg in send_json_msgq, dropping", proxy->id, proxy->subid,
                    proxy->url);
                drop = true;
                break;
            }
            csmsg->ofs += ret;
            csmsg->len -= ret;
        }
        if (csmsg->len < 1) {
            /* Clear sending before any recycle; after disable_subproxy
             * a non-parent subproxy pointer is invalid. */
            proxy->sending = NULL;
            if (drop)
                disable_subproxy(gdata, proxy->parent, proxy);
            DL_DELETE(*csmsgq, csmsg);
            free(csmsg->buf);
            free(csmsg);
        }
    }
}

static void add_yyjson_msgq(cs_msg_t** csmsgq, proxy_instance_t* proxy, yyjson_mut_doc* doc) {
    cs_msg_t* csmsg = ckzalloc(sizeof(cs_msg_t));
    size_t    len = 0;

    csmsg->buf = yyjson_mut_write(doc, YYJSON_WRITE_NEWLINE_AT_END, &len);
    yyjson_mut_doc_free(doc);
    if (unlikely(!csmsg->buf)) {
        LOGWARNING("Failed to create json dump in add_yyjson_msgq");
        free(csmsg);
        return;
    }
    csmsg->len = len;
    csmsg->proxy = proxy;
    DL_APPEND(*csmsgq, csmsg);
}

/* For processing and sending shares. proxy refers to parent proxy here */
static void* proxy_send(void __maybe_unused* arg) {
    gdata_t*       gdata = ckpool.gdata;
    stratum_msg_t* msg = NULL;
    cs_msg_t*      csmsgq = NULL;

    rename_proc("proxysend");

    pthread_detach(pthread_self());

    while (42) {
        int                proxyid = 0, subid = 0, share_id = 0;
        proxy_instance_t * proxy, *subproxy;
        yyjson_mut_val *   root, *sroot, *jobid;
        int64_t            client_id = 0, id;
        notify_instance_t* ni;
        yyjson_mut_doc*    doc;

        if (unlikely(msg)) {
            yyjson_mut_doc_free(msg->doc);
            free(msg);
        }

        mutex_lock(&gdata->psend_lock);
        if (!gdata->psends) {
            /* Poll every 10ms */
            const ts_t polltime = {0, 10000000};
            ts_t       timeout_ts;

            ts_realtime(&timeout_ts);
            timeraddspec(&timeout_ts, &polltime);
            cond_timedwait(&gdata->psend_cond, &gdata->psend_lock, &timeout_ts);
        }
        msg = gdata->psends;
        if (likely(msg))
            DL_DELETE(gdata->psends, msg);
        mutex_unlock(&gdata->psend_lock);

        if (!msg) {
            send_json_msgq(gdata, &csmsgq);
            continue;
        }

        sroot = yyjson_mut_doc_get_root(msg->doc);
        if (unlikely(!yyjson_mut_obj_get_int(&subid, sroot, "subproxy"))) {
            LOGWARNING("Failed to find subproxy in proxy_send msg");
            continue;
        }
        if (unlikely(!yyjson_mut_obj_get_int64(&id, sroot, "jobid"))) {
            LOGWARNING("Failed to find jobid in proxy_send msg");
            continue;
        }
        if (unlikely(!yyjson_mut_obj_get_int(&proxyid, sroot, "proxy"))) {
            LOGWARNING("Failed to find proxy in proxy_send msg");
            continue;
        }
        if (unlikely(!yyjson_mut_obj_get_int64(&client_id, sroot, "client_id"))) {
            LOGWARNING("Failed to find client_id in proxy_send msg");
            continue;
        }
        yyjson_mut_obj_get_int(&share_id, sroot, "id");
        proxy = proxy_by_id(gdata, proxyid);
        if (unlikely(!proxy)) {
            LOGWARNING("Proxysend for got message for non-existent proxy %d", proxyid);
            continue;
        }
        subproxy = subproxy_by_id(proxy, subid);
        if (unlikely(!subproxy)) {
            LOGWARNING("Proxysend for got message for non-existent subproxy %d:%d", proxyid, subid);
            continue;
        }

        doc = yyjson_mut_doc_new(&ckyyalc);
        jobid = NULL;
        mutex_lock(&gdata->notify_lock);
        HASH_FIND_I64(gdata->notify_instances, &id, ni);
        if (ni)
            jobid = yyjson_mut_val_mut_copy(doc, yyjson_mut_doc_get_root(ni->jobid));
        mutex_unlock(&gdata->notify_lock);

        if (unlikely(!jobid)) {
            stratifier_reconnect_client(client_id);
            LOGNOTICE("Proxy %d:%s failed to find matching jobid in proxysend", subproxy->id, subproxy->url);
            yyjson_mut_doc_free(doc);
            continue;
        }

        root = yyjson_mut_pack_val(
            doc, "{s[sosss]siss}", "params", subproxy->auth, jobid,
            yyjson_mut_get_str(yyjson_mut_obj_get(sroot, "nonce2")),
            yyjson_mut_get_str(yyjson_mut_obj_get(sroot, "ntime")),
            yyjson_mut_get_str(yyjson_mut_obj_get(sroot, "nonce")), "id", share_id, "method", "mining.submit");
        yyjson_mut_doc_set_root(doc, root);
        add_yyjson_msgq(&csmsgq, subproxy, doc);
        send_json_msgq(gdata, &csmsgq);
    }
    return NULL;
}

static void passthrough_send(pass_msg_t* pm) {
    proxy_instance_t* proxy = pm->proxy;
    connsock_t*       cs = pm->cs;
    int               len, sent;

    if (unlikely(!proxy->alive || cs->fd < 0)) {
        LOGDEBUG("Dropping send to dead proxy of upstream json msg: %s", pm->msg);
        goto out;
    }
    LOGDEBUG("Sending upstream json msg: %s", pm->msg);
    len = strlen(pm->msg);
    sent = write_socket(cs->fd, pm->msg, len);
    if (unlikely(sent != len)) {
        LOGWARNING("Failed to passthrough %d bytes of message %s, attempting reconnect", len, pm->msg);
        Close(cs->fd);
        proxy->alive = false;
        reconnect_generator();
    }
out:
    free(pm->msg);
    free(pm);
}

static void passthrough_add_send(proxy_instance_t* proxy, char* msg) {
    pass_msg_t* pm = ckzalloc(sizeof(pass_msg_t));

    pm->proxy = proxy;
    pm->cs = &proxy->cs;
    pm->msg = msg;
    ckmsgq_add(proxy->passsends, pm);
}

void generator_add_send(yyjson_mut_doc* doc) {
    gdata_t* gdata = ckpool.gdata;
    char*    buf;

    if (!ckpool.passthrough) {
        submit_share(gdata, doc);
        return;
    }
    if (unlikely(!gdata->current_proxy)) {
        LOGWARNING("No current proxy to send passthrough data to");
        goto out;
    }
    buf = yyjson_mut_write(doc, YYJSON_WRITE_NEWLINE_AT_END, NULL);
    if (unlikely(!buf)) {
        LOGWARNING("Unable to decode json in generator_add_send");
        goto out;
    }
    passthrough_add_send(gdata->current_proxy, buf);
out:
    yyjson_mut_doc_free(doc);
}

static void suggest_diff(connsock_t* cs, proxy_instance_t* proxy) {
    yyjson_mut_doc* req;
    bool            ret;

    req = yyjson_mut_pack("{s:i,s:s, s:[I]}", "id", 41, "method", "mining.suggest", "params", ckpool.mindiff);
    ret = send_yyjson_msg(cs, req);
    yyjson_mut_doc_free(req);
    if (!ret) {
        LOGNOTICE("Proxy %d:%d %s failed to send message in suggest_diff", proxy->id, proxy->subid, proxy->url);
        if (cs->fd > 0) {
            epoll_ctl(proxy->epfd, EPOLL_CTL_DEL, cs->fd, NULL);
            Close(cs->fd);
        }
    }
    /* We don't care about the response here. It can get filtered out later
     * if it fails upstream. */
}

/* Upon failing connnect, subscribe, or auth, back off on the next attempt.
 * This function should be called on the parent proxy */
static void proxy_backoff(proxy_instance_t* proxy) {
    /* Add 5 seconds with each backoff, up to maximum of 1 minute */
    if (proxy->backoff < 60)
        proxy->backoff += 5;
}

#ifdef HAVE_SV2
/* ===================================================================
 * SV2 upstream (ckproxy speaks Stratum V2 Mining Protocol as client).
 * Reuses the existing notify_instance_t / send_notify path so the
 * stratifier is unchanged: SV2 jobs are translated into SV1-shaped
 * notify messages (see sv2_proxy_handle_frame / task 7).
 * =================================================================== */

/*
 * Job ring depth. Custom (job declaration) jobs share it with pool jobs and
 * both rotate every few seconds, so it is sized for two sources: a late share
 * still needs its job_id in the reverse map.
 */
#define SV2_PROXY_JOBS 32
/*
 * Upstream extranonce_size policy (rollable bytes on the channel):
 *  - refuse below MIN (need room for ≥1 client id byte + miner en2)
 *  - accept up to MAX (SV2 B0_32 wire max)
 * MIN is also the min_extranonce_size we request; the pool grants at least
 * this and keeps its own preferred (larger) size, so a small floor only
 * widens compatibility. 4 → en1var=1 (256 clients) + en2=3, matching the
 * classic small-grant SV1-upstream split; JD-mode pools that only offer 4
 * then connect instead of forcing solo fallback. Below 4, en2 shrinks to
 * 1-2 bytes and risks nonce2 exhaustion at high hashrate.
 * Downstream SV1 miners always get enonce2 ≤ 8 and enonce1var ≤ 8; any
 * upstream remainder becomes fixed zero pad folded into coinb1 (miner-
 * hashed) so the channel hole stays exactly extranonce_size.
 */
#define SV2_PROXY_MIN_EXTRANONCE 4
#define SV2_PROXY_MAX_EXTRANONCE 32
#define SV2_PROXY_SV1_EN2_MAX 8 /* never advertise more to SV1 */
#define SV2_PROXY_EN1VAR_MAX 8  /* enonce1_64 / stratifier limit */

/* One recent upstream job (extended). Kept so SetNewPrevHash can activate a
 * future job and so share submits can map our notify id back to job_id. */
struct sv2_proxy_job {
    bool     valid;
    bool     future;    /* min_ntime absent: awaiting SetNewPrevHash */
    uint32_t job_id;    /* upstream U32 job id */
    int64_t  notify_id; /* our notify_instance id64 once sent */
    uint32_t version;
    uint32_t min_ntime;
    uint8_t  merkle_count;
    uint8_t  merkle_path[SV2_MAX_MERKLE_PATH][32];
    /* coinb1 = coinbase_tx_prefix[0..cb_tx_prefix_len) ‖ extranonce_prefix */
    uint8_t* coinb1;
    int      coinb1len;
    int      cb_tx_prefix_len; /* for rebuild on SetExtranoncePrefix */
    uint8_t* coinb2;           /* coinbase_tx_suffix */
    int      coinb2len;
    /*
     * The tip this job was built on, rather than whatever the pool last
     * announced: a custom job must not inherit a prevhash it was not declared
     * against, and the arbiter can race a pool SetNewPrevHash against one.
     * Filled from sp-> for pool jobs when they are
     * sent, from the local template for custom ones.
     */
    uint8_t  prev_hash[32];
    uint32_t nbits;

    /* Job declaration (custom) job extras. */
    bool                     custom;
    struct sv2_jdc_template* tmpl;                    /* reference held; the solve path pins it */
    uint8_t *                dcb_prefix, *dcb_suffix; /* declared-form coinbase split */
    uint16_t                 dcb_prefix_len, dcb_suffix_len;
};

/* A submitted share awaiting SubmitShares.Success/.Error, keyed by sequence. */
struct sv2_pending_share {
    uint32_t       seq;
    double         diff;
    int64_t        client_id;
    UT_hash_handle hh;
};

struct sv2_proxy {
    sv2_noise_session_t*      noise;
    uint32_t                  channel_id;
    bool                      channel_open;
    uint32_t                  next_request_id;
    uint32_t                  next_seq;  /* SubmitSharesExtended sequence */
    mutex_t                   send_lock; /* serialises outbound Noise encrypt + pending map */
    struct sv2_pending_share* pending;   /* shares submitted, awaiting ack */

    uint8_t  extranonce_prefix[SV2_MAX_B0_32]; /* pool-assigned, into coinb1 */
    uint8_t  extranonce_prefix_len;
    uint16_t extranonce_size; /* upstream channel size (may be > usable) */
    /* Rollable bytes handed to the stratifier as nonce2len (en1var+en2 ≤ 16). */
    uint16_t usable_extranonce;
    /* Leading zero pad in the extranonce hole, folded into coinb1 (miner-
     * hashed); extranonce_size == pad_len + usable_extranonce. */
    uint16_t pad_len;

    /* Last SetNewPrevHash context (header-internal order) */
    bool     have_prevhash;
    uint8_t  prev_hash[32];
    uint32_t snph_min_ntime;
    uint32_t nbits;

    time_t last_update;    /* last UpdateChannel sent */
    bool   want_reconnect; /* Reconnect received: drop + reconnect */

    /*
     * Work-source multiplexer. Written only on this
     * connection's receive thread, which is where every event that moves it
     * arrives: pool tips, pool jobs and the pool's answer to a custom job.
     */
    enum sv2_work_src work_src;
    time_t            bridge_at; /* entered POOL_BRIDGE, for its timeout */
    uint64_t          bridges;   /* times the pool has led our node */
    /*
     * The tip downstream was last flushed for, whatever the source. One tip is
     * flushed once: pool work for a new tip, a fee bump on it, and the custom
     * job that supersedes them are all the same work change to a miner, and
     * only the first is worth a clean.
     */
    bool    have_flushed_prev;
    uint8_t flushed_prev[32];

    /*
     * SetCustomMiningJob awaiting Success / Error. Staged under send_lock by
     * the JD client's session thread and consumed by this connection's receive
     * thread, which only then puts it in the ring and fans it downstream.
     */
    bool                 custom_pending;
    uint32_t             custom_request_id;
    time_t               custom_sent;
    struct sv2_proxy_job custom_stage;
    /*
     * Tip of the custom work miners are on, while they are on it. The pool
     * discards custom jobs when its own tip moves, so this is cleared by any
     * SetNewPrevHash for a different tip; while it is set, a same-tip pool job
     * must not yank miners back onto pool work.
     */
    bool    have_custom_prev;
    uint8_t custom_prev[32];

    struct sv2_proxy_job jobs[SV2_PROXY_JOBS];
    int                  job_head;

    /* Raw transport byte reassembly */
    uint8_t* rx;
    size_t   rx_len, rx_cap;
};

/* Release everything a job slot owns, leaving it zeroed. */
static void sv2_proxy_job_clear(struct sv2_proxy_job* job) {
    dealloc(job->coinb1);
    dealloc(job->coinb2);
    dealloc(job->dcb_prefix);
    dealloc(job->dcb_suffix);
    if (job->tmpl) {
        sv2_jdc_template_put(job->tmpl);
        job->tmpl = NULL;
    }
    memset(job, 0, sizeof(*job));
}

static void sv2_proxy_free(proxy_instance_t* proxi) {
    struct sv2_proxy*         sp = proxi->sv2p;
    struct sv2_pending_share *ps, *tmp;
    int                       i;

    if (!sp)
        return;
    /*
     * What this connection announced says nothing about the pool's tip once the
     * connection is gone, and the JD client gates declares on it: a stale tip
     * across a reconnect would declare into a certain stale-prev-hash. Only the
     * parent entry's channel carries JD, and subproxies share its id, so
     * a recruited one going away must not speak for it.
     */
    if (!proxi->subid)
        sv2_jdc_pool_tip_clear(proxi->id);
    if (sp->noise)
        sv2_noise_session_free(sp->noise);
    for (i = 0; i < SV2_PROXY_JOBS; i++)
        sv2_proxy_job_clear(&sp->jobs[i]);
    /* A staged custom job dies with the channel it was built for. */
    sv2_proxy_job_clear(&sp->custom_stage);
    HASH_ITER(hh, sp->pending, ps, tmp) {
        HASH_DEL(sp->pending, ps);
        dealloc(ps);
    }
    mutex_destroy(&sp->send_lock);
    dealloc(sp->rx);
    dealloc(sp);
    proxi->sv2p = NULL;
}

/* Encrypt and send one plaintext SV2 message on the connection. The outbound
 * Noise CipherState is single-writer: send_lock serialises the submit path,
 * any UpdateChannel timer, and the connect sequence. */
static bool sv2_proxy_send(
    proxy_instance_t* proxi,
    connsock_t*       cs,
    uint8_t           msg_type,
    bool              channel_msg,
    const uint8_t*    pay,
    size_t            paylen) {
    struct sv2_proxy* sp = proxi->sv2p;
    uint8_t *         frame = NULL, *ct = NULL;
    size_t            flen = 0, ctlen = 0;
    uint16_t          ext = channel_msg ? SV2_CHANNEL_MSG_BIT : 0;
    bool              ret = false;

    if (!sp || !sp->noise)
        return false;
    if (!sv2_build_frame(ext, msg_type, pay, (uint32_t)paylen, &frame, &flen))
        goto out;
    mutex_lock(&sp->send_lock);
    if (sv2_noise_encrypt_frame(sp->noise, frame, flen, &ct, &ctlen))
        ret = write_socket(cs->fd, ct, ctlen) == (int)ctlen;
    mutex_unlock(&sp->send_lock);
out:
    dealloc(frame);
    dealloc(ct);
    return ret;
}

/* Append raw socket bytes to the reassembly buffer. */
static bool sv2_rx_append(struct sv2_proxy* sp, const uint8_t* data, size_t n) {
    if (sp->rx_len + n > sp->rx_cap) {
        size_t ncap = sp->rx_cap ? sp->rx_cap : 8192;

        while (ncap < sp->rx_len + n)
            ncap *= 2;
        /* Bound a single connection's rx at 2× the U24 payload ceiling. */
        if (ncap > (size_t)SV2_MAX_PAYLOAD * 2)
            return false;
        sp->rx = ckrealloc(sp->rx, ncap);
        sp->rx_cap = ncap;
    }
    memcpy(sp->rx + sp->rx_len, data, n);
    sp->rx_len += n;
    return true;
}

/* Extract the next complete plaintext frame from rx. 1 = got frame (caller
 * frees *plain), 0 = need more bytes, -1 = fatal AEAD/format error. */
static int sv2_rx_next(struct sv2_proxy* sp, uint8_t** plain, size_t* plainlen) {
    size_t consumed = 0;
    int    rc;

    *plain = NULL;
    *plainlen = 0;
    rc = sv2_noise_decrypt_frame(sp->noise, sp->rx, sp->rx_len, &consumed, plain, plainlen);
    if (rc == 0) {
        memmove(sp->rx, sp->rx + consumed, sp->rx_len - consumed);
        sp->rx_len -= consumed;
        return 1;
    }
    if (rc == -2)
        return -1;
    return 0; /* -1 from decrypt = need more */
}

/* Blocking read of one plaintext frame (handshake/setup phase). Returns 1 on
 * success, 0 on timeout, -1 on fatal error. */
static int
sv2_proxy_readframe(proxy_instance_t* proxi, connsock_t* cs, uint8_t** plain, size_t* plainlen, float timeout) {
    struct sv2_proxy* sp = proxi->sv2p;
    uint8_t           rbuf[8192];

    while (42) {
        int rc = sv2_rx_next(sp, plain, plainlen);
        int r;

        if (rc != 0)
            return rc;
        if (wait_read_select(cs->fd, timeout) < 1)
            return 0;
        r = read(cs->fd, rbuf, sizeof(rbuf));
        if (r < 1)
            return -1;
        if (!sv2_rx_append(sp, rbuf, (size_t)r))
            return -1;
    }
}

/* Noise NX initiator handshake against the upstream pool. cs->fd connected. */
static bool sv2_proxy_handshake(proxy_instance_t* proxi, connsock_t* cs) {
    struct sv2_proxy* sp = proxi->sv2p;
    uint8_t           act1[64], act2[234];

    sp->noise = sv2_noise_client_session_new(proxi->sv2_authority);
    if (!sp->noise) {
        LOGWARNING("SV2 proxy %d failed to create Noise session", proxi->id);
        return false;
    }
    if (!sv2_noise_client_act1(sp->noise, act1) || write_socket(cs->fd, act1, 64) != 64) {
        LOGNOTICE("SV2 proxy %d failed to send handshake act1", proxi->id);
        return false;
    }
    if (wait_read_select(cs->fd, 15) < 1 || read_length(cs->fd, act2, sizeof(act2)) != (int)sizeof(act2)) {
        LOGNOTICE("SV2 proxy %d failed to read handshake act2", proxi->id);
        return false;
    }
    if (!sv2_noise_client_act2(sp->noise, act2, sizeof(act2))) {
        LOGWARNING("SV2 proxy %d server certificate verification failed", proxi->id);
        return false;
    }
    LOGINFO("SV2 proxy %d Noise handshake complete, certificate verified", proxi->id);
    return true;
}

/* SetupConnection (Mining) and await Success. */
/*
 * True when this entry declares its own jobs, so its mining connection must ask
 * for work selection. Job declaration binds to the parent entry's first channel,
 * so recruited subproxies do not ask: an unused
 * work-selection flag would only narrow which pools accept them.
 */
static bool sv2_proxy_wants_work_selection(const proxy_instance_t* proxi) {
    if (proxi->subid || proxi->id < 0 || proxi->id >= ckpool.proxies)
        return false;
    return ckpool.proxyjds && ckpool.proxyjds[proxi->id];
}

static bool sv2_proxy_setup(proxy_instance_t* proxi, connsock_t* cs) {
    struct sv2_setup_connection sc;
    bool                        work_selection;
    uint8_t                     buf[512];
    size_t                      plen = 0;

    memset(&sc, 0, sizeof(sc));
    sc.protocol = SV2_PROTOCOL_MINING;
    sc.min_version = 2;
    sc.max_version = 2;
    /* Downstream SV1 miners need BIP320 version rolling. */
    sc.flags = SV2_FLAG_REQUIRES_VERSION_ROLLING;
    /*
     * SetCustomMiningJob is refused without this flag, and a pool with no job
     * declaration listener refuses the flag itself, so it is only asked for
     * when this entry has a jds and only until the pool says no once — after
     * that the entry runs mining-only rather than failing over.
     */
    work_selection = sv2_proxy_wants_work_selection(proxi) && !proxi->sv2_no_work_selection;
    if (work_selection)
        sc.flags |= SV2_FLAG_REQUIRES_WORK_SELECTION;
    snprintf(sc.endpoint_host, sizeof(sc.endpoint_host), "%s", cs->url ? cs->url : "");
    sc.endpoint_port = cs->port ? atoi(cs->port) : 0;
    snprintf(sc.vendor, sizeof(sc.vendor), "ckproxy");
    snprintf(sc.firmware, sizeof(sc.firmware), "%s", PACKAGE "/" VERSION);
    if (!sv2_encode_setup_connection(buf, sizeof(buf), &plen, &sc) ||
        !sv2_proxy_send(proxi, cs, SV2_MSG_SETUP_CONNECTION, false, buf, plen)) {
        LOGNOTICE("SV2 proxy %d failed to send SetupConnection", proxi->id);
        return false;
    }
    while (42) {
        uint8_t*         f = NULL;
        size_t           fl = 0;
        struct sv2_frame fr;
        const uint8_t*   pay;

        if (sv2_proxy_readframe(proxi, cs, &f, &fl, 15) < 1) {
            LOGNOTICE("SV2 proxy %d no SetupConnection response", proxi->id);
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
            /* Downstream SV1 miners need BIP320; fixed-version pools
             * cannot validate version-rolled shares. */
            if (good && (ok.flags & SV2_FLAG_REQUIRES_FIXED_VERSION)) {
                LOGWARNING("SV2 proxy %d SetupConnection.Success REQUIRES_FIXED_VERSION — unusable", proxi->id);
                return false;
            }
            if (good) {
                LOGNOTICE(
                    "SV2 proxy %d SetupConnection accepted%s", proxi->id, work_selection ? " with work selection" : "");
                return true;
            }
            LOGWARNING("SV2 proxy %d bad SetupConnection.Success", proxi->id);
            return false;
        }
        if (fr.msg_type == SV2_MSG_SETUP_CONNECTION_ERROR) {
            struct sv2_setup_connection_error err;

            memset(&err, 0, sizeof(err));
            sv2_decode_setup_connection_error(pay, fr.msg_length, &err);
            LOGWARNING("SV2 proxy %d SetupConnection.Error flags=0x%x: %s", proxi->id, err.flags, err.error_code);
            dealloc(f);
            /*
             * Work selection is the one flag we can drop and still be
             * useful: mining the pool's own templates beats not mining.
             * The reconnect that follows asks without it.
             */
            if (work_selection) {
                proxi->sv2_no_work_selection = true;
                LOGWARNING(
                    "SV2 proxy %d refused work selection — retrying "
                    "mining-only, no job declaration to this pool",
                    proxi->id);
            }
            return false;
        }
        /* Ignore any unexpected pre-setup frame. */
        dealloc(f);
    }
}

/* OpenExtendedMiningChannel and await Success, storing channel params. */
static bool sv2_proxy_open(proxy_instance_t* proxi, connsock_t* cs) {
    struct sv2_proxy*                sp = proxi->sv2p;
    struct sv2_open_extended_channel oc;
    uint8_t                          buf[512];
    size_t                           plen = 0;

    memset(&oc, 0, sizeof(oc));
    oc.request_id = ++sp->next_request_id;
    snprintf(oc.user_identity, sizeof(oc.user_identity), "%s", proxi->auth ? proxi->auth : "");
    oc.nominal_hash_rate = 0.0f;     /* no devices yet (spec 5.3.2) */
    memset(oc.max_target, 0xff, 32); /* impose no ceiling; pool policy governs */
    oc.min_extranonce_size = SV2_PROXY_MIN_EXTRANONCE;
    if (!sv2_encode_open_extended_channel(buf, sizeof(buf), &plen, &oc) ||
        !sv2_proxy_send(proxi, cs, SV2_MSG_OPEN_EXTENDED_MINING_CHANNEL, false, buf, plen)) {
        LOGNOTICE("SV2 proxy %d failed to send OpenExtendedMiningChannel", proxi->id);
        return false;
    }
    while (42) {
        uint8_t*         f = NULL;
        size_t           fl = 0;
        struct sv2_frame fr;
        const uint8_t*   pay;

        if (sv2_proxy_readframe(proxi, cs, &f, &fl, 15) < 1) {
            LOGNOTICE("SV2 proxy %d no OpenChannel response", proxi->id);
            return false;
        }
        if (!sv2_decode_header(f, fl, &fr)) {
            dealloc(f);
            return false;
        }
        pay = f + SV2_FRAME_HEADER_LEN;
        if (fr.msg_type == SV2_MSG_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS) {
            struct sv2_open_extended_channel_success ok;

            if (!sv2_decode_open_extended_channel_success(pay, fr.msg_length, &ok)) {
                dealloc(f);
                return false;
            }
            dealloc(f);
            /*
             * Accept U in [MIN, MAX]. The coinbase extranonce hole
             * MUST be exactly U bytes (miners hash that layout).
             * SV1 miners still only roll en2 ≤ 8; en1var ≤ 8 for
             * client ids. Any leftover is fixed zero pad folded into
             * coinb1 (see NewExtendedMiningJob) so the hole stays U
             * and the leading pad also leads the submitted extranonce
             * — never pad only on submit, which would desync the
             * merkle root vs the pool → difficulty-too-low.
             */
            if (ok.extranonce_size < SV2_PROXY_MIN_EXTRANONCE || ok.extranonce_size > SV2_PROXY_MAX_EXTRANONCE) {
                LOGWARNING(
                    "SV2 proxy %d unusable extranonce_size %u (need %d–%d)", proxi->id, ok.extranonce_size,
                    SV2_PROXY_MIN_EXTRANONCE, SV2_PROXY_MAX_EXTRANONCE);
                return false;
            }
            sp->channel_id = ok.channel_id;
            sp->extranonce_size = ok.extranonce_size;
            sp->extranonce_prefix_len = ok.extranonce_prefix_len;
            memcpy(sp->extranonce_prefix, ok.extranonce_prefix, ok.extranonce_prefix_len);
            sp->channel_open = true;
            /*
             * Extranonce hole layout (total U) — miners hash exactly
             * this and the pool reconstructs it on SubmitSharesExtended:
             *   pad zeros    = U - en1var - en2  (folded into coinb1)
             *   enonce1var   ≤ 8  (per-client id → max_clients)
             *   enonce2      ≤ 8  (miner-rolled, SV1-safe)
             * The pad lives in coinb1 (heap), never in the stratifier's
             * fixed enonce1 buffers, so any U up to 32 is safe.
             *
             * Auto-split (no conf nonce2length) mirrors classic SV1
             * proxy for U≤8 (e.g. U=8 → en1var=4, en2=4) so we do
             * not set max_clients=1. Larger U: en2=8, en1var≤8, pad.
             */
            {
                int U = (int)ok.extranonce_size;
                int en1, en2, pad;

                if (ckpool.nonce2length > 0) {
                    en2 = ckpool.nonce2length;
                    if (en2 > SV2_PROXY_SV1_EN2_MAX)
                        en2 = SV2_PROXY_SV1_EN2_MAX;
                    if (en2 > U)
                        en2 = U;
                    if (en2 < 1)
                        en2 = 1;
                    en1 = U - en2;
                    if (en1 > SV2_PROXY_EN1VAR_MAX)
                        en1 = SV2_PROXY_EN1VAR_MAX;
                } else if (U > 8) {
                    en2 = SV2_PROXY_SV1_EN2_MAX;
                    en1 = U - en2;
                    if (en1 > SV2_PROXY_EN1VAR_MAX)
                        en1 = SV2_PROXY_EN1VAR_MAX;
                } else {
                    /* Classic ckpool auto-split for small grants */
                    if (U > 7)
                        en1 = 4;
                    else if (U > 5)
                        en1 = 2;
                    else if (U > 3)
                        en1 = 1;
                    else
                        en1 = 0;
                    en2 = U - en1;
                }
                pad = U - en1 - en2;
                if (pad < 0)
                    pad = 0;
                sp->pad_len = (uint16_t)pad;
                sp->usable_extranonce = (uint16_t)(en1 + en2);

                /* enonce1const stays empty; the pad is folded into
                 * coinb1 (see NewExtendedMiningJob) so the stratifier
                 * only ever sees en1var (≤8) in its fixed buffers. */
                dealloc(proxi->enonce1);
                dealloc(proxi->enonce1bin);
                proxi->enonce1 = strdup("");
                proxi->enonce1bin = ckzalloc(1);
                proxi->nonce1len = 0;
                proxi->nonce2len = en1 + en2;
                if (en1 > 0 && en1 < 8)
                    proxi->clients_per_proxy = 1ll << (en1 * 8);
                else if (en1 >= 8)
                    proxi->clients_per_proxy = INT64_MAX / 2;
                else
                    proxi->clients_per_proxy = 1;
                if (proxi->clients_per_proxy < 1)
                    proxi->clients_per_proxy = 1;

                LOGNOTICE(
                    "SV2 proxy %d channel %u open: upstream_en=%u "
                    "const_pad=%d en1var=%d en2=%d max_clients=%" PRId64 " prefix_len=%u",
                    proxi->id, sp->channel_id, sp->extranonce_size, pad, en1, en2, proxi->clients_per_proxy,
                    sp->extranonce_prefix_len);
            }
            /* Initial channel target (SetTarget may refine it). */
            proxi->diff = diff_from_target(ok.target);
            if (proxi->diff < 1)
                proxi->diff = 1;
            LOGNOTICE("SV2 proxy %d channel %u diff=%.1f", proxi->id, sp->channel_id, proxi->diff);
            return true;
        }
        if (fr.msg_type == SV2_MSG_OPEN_MINING_CHANNEL_ERROR) {
            struct sv2_open_channel_error err;

            memset(&err, 0, sizeof(err));
            sv2_decode_open_channel_error(pay, fr.msg_length, &err);
            LOGWARNING("SV2 proxy %d OpenMiningChannel.Error: %s", proxi->id, err.error_code);
            dealloc(f);
            return false;
        }
        /* SetTarget / jobs may arrive before/around the success; buffer
         * handling of those is done by the recv loop, so ignore here. */
        dealloc(f);
    }
}

/* Establish an SV2 upstream: allocate state, Noise handshake, SetupConnection,
 * OpenExtendedMiningChannel. cs->sem held by caller (proxy_alive). */
static bool sv2_proxy_connect(proxy_instance_t* proxi, connsock_t* cs) {
    sv2_proxy_free(proxi);
    proxi->sv2p = ckzalloc(sizeof(struct sv2_proxy));
    mutex_init(&proxi->sv2p->send_lock);
    if (!sv2_proxy_handshake(proxi, cs))
        goto fail;
    if (!sv2_proxy_setup(proxi, cs))
        goto fail;
    if (!sv2_proxy_open(proxi, cs))
        goto fail;
    return true;
fail:
    sv2_proxy_free(proxi);
    return false;
}

/* Slot for storing an upstream job, oldest evicted. */
#ifdef HAVE_SV2
/*
 * Extranonce layout of proxy entry proxy_id's SV2 mining channel, for the JD
 * client's declared coinbase. The parent entry's own
 * channel only: JD binds to one channel in Phase 2 v1, so recruited
 * subproxies — which may be granted a different extranonce_prefix — are not
 * consulted here.
 */
bool sv2_proxy_jd_channel(int proxy_id, struct sv2_jdc_channel* out) {
    gdata_t*          gdata = ckpool.gdata;
    proxy_instance_t* proxi;
    struct sv2_proxy* sp;

    memset(out, 0, sizeof(*out));
    if (!gdata)
        return false;
    proxi = proxy_by_id(gdata, proxy_id);
    if (!proxi || !proxi->sv2)
        return false;
    sp = proxi->sv2p;
    if (!sp || !sp->channel_open)
        return false;
    out->channel_id = sp->channel_id;
    out->extranonce_prefix_len = sp->extranonce_prefix_len;
    memcpy(out->extranonce_prefix, sp->extranonce_prefix, sp->extranonce_prefix_len);
    out->extranonce_size = sp->extranonce_size;
    out->pad_len = sp->pad_len;
    mutex_lock(&gdata->lock);
    out->current = (gdata->current_proxy == proxi);
    mutex_unlock(&gdata->lock);
    return true;
}

/*
 * Send a SetCustomMiningJob for an accepted declare and stage the material the
 * reply needs. Called on the JD client's session thread:
 * the mining connection's single-writer rule is send_lock, which also covers the
 * staged job the receive thread picks up on Success.
 *
 * Nothing goes downstream here. A notify for a job_id the pool has not
 * acknowledged only produces shares it will reject, so the ring slot and the
 * notify wait for SetCustomMiningJob.Success.
 */
bool sv2_proxy_set_custom_job(const struct sv2_jdc_custom_job* cj) {
    gdata_t*                         gdata = ckpool.gdata;
    struct sv2_set_custom_mining_job req;
    struct sv2_jdc_template*         t;
    struct sv2_proxy_job             stage;
    proxy_instance_t*                proxi;
    struct sv2_proxy*                sp;
    uint16_t                         eplen, hole;
    size_t                           need, plen = 0;
    uint32_t                         request_id;
    uint8_t*                         buf = NULL;
    bool                             current, ret = false;
    time_t                           now = time(NULL);
    int                              i;

    if (!cj || !cj->t || !gdata || !cj->token_len || !cj->outputs_len)
        return false;
    t = cj->t;
    proxi = proxy_by_id(gdata, cj->proxy_id);
    if (!proxi || !proxi->sv2)
        return false;
    sp = proxi->sv2p;
    if (!sp || !sp->channel_open)
        return false;
    /*
     * Phase 2 v1 binds job declaration to the parent entry's channel,
     * and this is that channel — but its extranonce layout can have changed
     * (SetExtranoncePrefix) since the declare was built, and the hole the JDS
     * accepted then no longer fits. Refuse rather than mine work whose coinbase
     * the pool reconstructs differently.
     */
    eplen = sp->extranonce_prefix_len;
    hole = eplen + sp->extranonce_size;
    if (cj->hole_len != hole) {
        LOGWARNING(
            "SV2 proxy %d custom job extranonce hole %u no longer matches "
            "the channel's %u — not sending",
            proxi->id, cj->hole_len, hole);
        return false;
    }
    mutex_lock(&gdata->lock);
    current = (gdata->current_proxy == proxi);
    mutex_unlock(&gdata->lock);
    if (!current) {
        LOGINFO("SV2 proxy %d is not the current upstream, no custom job", proxi->id);
        return false;
    }
    /* The scriptSig prefix needs no check: templates cap it at
     * SV2_JDC_MAX_SCRIPTSIG, well inside req.coinbase_prefix. */
    if (t->merkles > SV2_MAX_MERKLE_PATH)
        return false;

    memset(&req, 0, sizeof(req));
    req.channel_id = sp->channel_id;
    req.mining_job_token_len = cj->token_len;
    memcpy(req.mining_job_token, cj->token, cj->token_len);
    req.version = t->version;
    memcpy(req.prev_hash, t->prev_hash, 32);
    req.min_ntime = t->ntime;
    req.nbits = t->nbits;
    req.coinbase_tx_version = t->cb_version;
    /* Only the scriptSig prefix: the pool appends the channel's extranonce
     * prefix as en1 and the miner's extranonce itself. */
    req.coinbase_prefix_len = t->script_sig_prefix_len;
    memcpy(req.coinbase_prefix, t->script_sig_prefix, t->script_sig_prefix_len);
    req.coinbase_tx_input_nSequence = t->cb_sequence;
    /* Borrowed for the encode only — never freed through req. */
    req.coinbase_tx_outputs = (uint8_t*)cj->outputs;
    req.coinbase_tx_outputs_len = cj->outputs_len;
    req.coinbase_tx_locktime = t->cb_locktime;
    req.merkle_count = (uint8_t)t->merkles;
    for (i = 0; i < t->merkles; i++)
        memcpy(req.merkle_path[i], t->merkle_path[i], 32);

    /* The job as downstream will see it, from the legacy coinbase split. */
    memset(&stage, 0, sizeof(stage));
    stage.custom = true;
    stage.version = t->version;
    stage.min_ntime = t->ntime;
    stage.nbits = t->nbits;
    memcpy(stage.prev_hash, t->prev_hash, 32);
    stage.merkle_count = (uint8_t)t->merkles;
    for (i = 0; i < t->merkles; i++)
        memcpy(stage.merkle_path[i], t->merkle_path[i], 32);
    stage.cb_tx_prefix_len = cj->lcb_prefix_len;
    stage.coinb1len = cj->lcb_prefix_len + eplen + sp->pad_len;
    stage.coinb1 = ckalloc(stage.coinb1len);
    memcpy(stage.coinb1, cj->lcb_prefix, cj->lcb_prefix_len);
    memcpy(stage.coinb1 + cj->lcb_prefix_len, sp->extranonce_prefix, eplen);
    if (sp->pad_len)
        memset(stage.coinb1 + cj->lcb_prefix_len + eplen, 0, sp->pad_len);
    stage.coinb2len = cj->lcb_suffix_len;
    stage.coinb2 = ckalloc(stage.coinb2len ? stage.coinb2len : 1);
    memcpy(stage.coinb2, cj->lcb_suffix, cj->lcb_suffix_len);
    stage.dcb_prefix_len = cj->dcb_prefix_len;
    stage.dcb_prefix = ckalloc(cj->dcb_prefix_len);
    memcpy(stage.dcb_prefix, cj->dcb_prefix, cj->dcb_prefix_len);
    stage.dcb_suffix_len = cj->dcb_suffix_len;
    stage.dcb_suffix = ckalloc(cj->dcb_suffix_len);
    memcpy(stage.dcb_suffix, cj->dcb_suffix, cj->dcb_suffix_len);
    /* Taken before send_lock: the template store's lock is only ever acquired
     * after this connection's, never the other way about. */
    stage.tmpl = sv2_jdc_template_ref(t);

    mutex_lock(&sp->send_lock);
    if (sp->custom_pending) {
        LOGNOTICE(
            "SV2 proxy %d replacing a custom job req=%u unanswered for %ds", proxi->id, sp->custom_request_id,
            (int)(now - sp->custom_sent));
    }
    sv2_proxy_job_clear(&sp->custom_stage);
    sp->custom_stage = stage;
    sp->custom_pending = true;
    sp->custom_request_id = request_id = ++sp->next_request_id;
    sp->custom_sent = now;
    mutex_unlock(&sp->send_lock);
    memset(&stage, 0, sizeof(stage)); /* the stage owns it now */

    req.request_id = request_id;
    need = sv2_set_custom_mining_job_encoded_size(&req);
    buf = ckalloc(need);
    if (!sv2_encode_set_custom_mining_job(buf, need, &plen, &req) ||
        !sv2_proxy_send(proxi, &proxi->cs, SV2_MSG_SET_CUSTOM_MINING_JOB, true, buf, plen)) {
        LOGNOTICE("SV2 proxy %d failed to encode or send SetCustomMiningJob req=%u", proxi->id, request_id);
        mutex_lock(&sp->send_lock);
        if (sp->custom_pending && sp->custom_request_id == request_id) {
            sp->custom_pending = false;
            stage = sp->custom_stage;
            memset(&sp->custom_stage, 0, sizeof(sp->custom_stage));
        }
        mutex_unlock(&sp->send_lock);
        goto out;
    }
    ret = true;
    LOGNOTICE(
        "SV2 proxy %d SetCustomMiningJob req=%u template %" PRIu64
        " height %d "
        "ntime %08x nbits %08x, %d merkles, coinbase %u+%u+%u, frame %zu bytes",
        proxi->id, request_id, t->id, t->height, t->ntime, t->nbits, t->merkles, cj->lcb_prefix_len, hole,
        cj->lcb_suffix_len, plen);
out:
    dealloc(buf);
    /* Zeroed once staged, so this only frees material that never got there. */
    sv2_proxy_job_clear(&stage);
    return ret;
}
#endif

static struct sv2_proxy_job* sv2_proxy_job_slot(struct sv2_proxy* sp, uint32_t job_id) {
    struct sv2_proxy_job* job = &sp->jobs[sp->job_head];

    sp->job_head = (sp->job_head + 1) % SV2_PROXY_JOBS;
    sv2_proxy_job_clear(job);
    job->job_id = job_id;
    job->valid = true;
    return job;
}

static struct sv2_proxy_job* sv2_proxy_find_job(struct sv2_proxy* sp, uint32_t job_id) {
    int i;

    for (i = 0; i < SV2_PROXY_JOBS; i++) {
        if (sp->jobs[i].valid && sp->jobs[i].job_id == job_id)
            return &sp->jobs[i];
    }
    return NULL;
}

/*
 * Whether work for tip prev is a real work change downstream and so worth a
 * clean (flushing) notify, or another job for a tip miners are already on.
 * One tip is flushed once, whichever source it came
 * from: the cutover from pool work to our own custom work for the same tip is
 * not a tip change, and flushing it would throw away in-flight shares.
 */
static bool sv2_proxy_want_clean(const struct sv2_proxy* sp, const uint8_t prev[32]) {
    return !sp->have_flushed_prev || memcmp(sp->flushed_prev, prev, 32) != 0;
}

/*
 * Move the work-source multiplexer. Every change is
 * logged: which of the three states a proxy is in is the single fact that
 * explains what miners are being given and why.
 */
static void sv2_proxy_work_src(proxy_instance_t* proxi, enum sv2_work_src src, const char* why) {
    struct sv2_proxy* sp = proxi->sv2p;
    time_t            now = time(NULL);

    if (sp->work_src == src)
        return;
    if (sp->work_src == SV2_WS_BRIDGE) {
        LOGNOTICE(
            "SV2 proxy %d spent %ds on pool work while our node caught up", proxi->id, (int)(now - sp->bridge_at));
    }
    LOGNOTICE(
        "SV2 proxy %d work source %s -> %s: %s", proxi->id, sv2_work_src_str(sp->work_src), sv2_work_src_str(src), why);
    sp->work_src = src;
    if (src == SV2_WS_BRIDGE) {
        sp->bridge_at = now;
        sp->bridges++;
    }
}

/* Translate a stored SV2 job + current prevhash context into an SV1-shaped
 * notify_instance and hand it to the stratifier via send_notify. clean=true on
 * a real tip change (SetNewPrevHash), false for a same-tip immediate job. */
static void sv2_proxy_send_job(proxy_instance_t* proxi, struct sv2_proxy_job* job, bool clean) {
    struct sv2_proxy*  sp = proxi->sv2p;
    gdata_t*           gdata = ckpool.gdata;
    notify_instance_t* ni;
    uint8_t            flip[32];
    int                i;

    if (!job || !job->coinb1)
        return;
    /*
     * Pool jobs take the tip the pool last announced — they have no other —
     * and need one before miners can work. A custom job carries the tip it was
     * declared against, which the pool has already confirmed is its own.
     */
    if (!job->custom) {
        if (!sp->have_prevhash)
            return;
        memcpy(job->prev_hash, sp->prev_hash, 32);
        job->nbits = sp->nbits;
    }
    /* Defense in depth: same fixed-array limit as parse_notify / SV1. */
    if (unlikely(job->merkle_count > 16)) {
        LOGWARNING(
            "SV2 proxy %d job %u has %u merkles, exceeding max of 16 — not notifying", proxi->id, job->job_id,
            job->merkle_count);
        return;
    }

    ni = ckzalloc(sizeof(notify_instance_t));
    /* Retain the upstream U32 job id (for share submission mapping, Phase 3). */
    ni->jobid = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_doc_set_root(ni->jobid, yyjson_mut_uint(ni->jobid, job->job_id));

    ni->coinb1len = job->coinb1len;
    ni->coinbase1 = ckalloc(job->coinb1len * 2 + 1);
    __bin2hex(ni->coinbase1, job->coinb1, job->coinb1len);
    ni->coinbase2 = ckalloc(job->coinb2len * 2 + 1);
    __bin2hex(ni->coinbase2, job->coinb2, job->coinb2len);

    /* prevhash: SV1 notify byte order = flip_32(SV2 header-internal prev_hash).
     * The stratifier rebuilds headerbin+4 = hex2bin(prevhash), and flip_80'ing
     * it for PoW yields back the SV2 (wire) prev_hash. */
    flip_32(flip, job->prev_hash);
    __bin2hex(ni->prevhash, flip, 32);

    sprintf(ni->bbversion, "%08x", job->version);
    sprintf(ni->nbit, "%08x", job->nbits);
    /* Immediate jobs carry their own min_ntime; a future job activated by
     * SetNewPrevHash uses the prevhash's min_ntime. */
    sprintf(ni->ntime, "%08x", job->future ? sp->snph_min_ntime : job->min_ntime);

    ni->merkles = job->merkle_count;
    for (i = 0; i < job->merkle_count; i++)
        __bin2hex(&ni->merklehash[i][0], job->merkle_path[i], 32);

    ni->clean = clean;
    ni->notify_time = time(NULL);

    /*
     * A custom job's solve paths need its declared coinbase and the template
     * it came from, and a share can arrive on another thread long after the
     * ring slot has rotated, so copy them onto the notify instance.
     */
    if (job->custom && job->tmpl) {
        ni->sv2_custom = true;
        ni->sv2_tmpl = sv2_jdc_template_ref(job->tmpl);
        ni->sv2_dcb_prefix_len = job->dcb_prefix_len;
        ni->sv2_dcb_prefix = ckalloc(job->dcb_prefix_len);
        memcpy(ni->sv2_dcb_prefix, job->dcb_prefix, job->dcb_prefix_len);
        ni->sv2_dcb_suffix_len = job->dcb_suffix_len;
        ni->sv2_dcb_suffix = ckalloc(job->dcb_suffix_len);
        memcpy(ni->sv2_dcb_suffix, job->dcb_suffix, job->dcb_suffix_len);
        ni->sv2_en_prefix_len = sp->extranonce_prefix_len;
        memcpy(ni->sv2_en_prefix, sp->extranonce_prefix, sp->extranonce_prefix_len);
    }

    mutex_lock(&gdata->notify_lock);
    ni->id64 = gdata->proxy_notify_id++;
    HASH_ADD_I64(gdata->notify_instances, id64, ni);
    mutex_unlock(&gdata->notify_lock);

    job->notify_id = ni->id64;
    /* One flush per tip, tracked here so every path that emits work shares the
     * same record of what downstream has already been flushed for. */
    if (clean) {
        memcpy(sp->flushed_prev, job->prev_hash, 32);
        sp->have_flushed_prev = true;
    }
    LOGINFO(
        "SV2 proxy %d sending notify id %" PRId64 " for %s job %u clean=%d", proxi->id, ni->id64,
        job->custom ? "custom" : "pool", job->job_id, clean);
    send_notify(proxi, ni);
}

/*
 * A share on a custom (job declaration) job may be a block. Rebuild exactly what
 * the miner hashed and, if it meets the network target, take the two solve paths
 * ckproxy owns: PushSolution upstream and a submit to our own node.
 * The pool has already had the share itself by now.
 *
 * The material lives on the notify instance, which is found under notify_lock
 * and aged out ten minutes later, so it is safe to read here on the share thread
 * — unlike the job ring, which the receive thread rotates.
 */
static void sv2_proxy_custom_solve(
    proxy_instance_t* proxi,
    int64_t           notify_id,
    const uint8_t*    full_en,
    uint8_t           full_len,
    const uint8_t*    en2,
    int               en2len,
    uint32_t          version,
    uint32_t          ntime,
    uint32_t          nonce) {
    uint8_t                 coinbase[1024], merkle[64], root[32], header[80], hash[32];
    uint8_t                 prevbin[32], flip[32];
    struct sv2_jdc_solution sol;
    gdata_t*                gdata = ckpool.gdata;
    double                  sdiff = 0, netdiff = 0;
    notify_instance_t*      ni;
    size_t                  cblen, dcblen;
    uint8_t*                dcb = NULL;
    uint32_t                nbits = 0;
    char                    hex[68];
    uint32_t                le;
    int                     c1, c2, i;

    memset(&sol, 0, sizeof(sol));
    mutex_lock(&gdata->notify_lock);
    HASH_FIND_I64(gdata->notify_instances, &notify_id, ni);
    if (!ni || !ni->sv2_custom || !ni->sv2_tmpl)
        goto unlock;
    c1 = ni->coinb1len;
    c2 = strlen(ni->coinbase2) / 2;
    cblen = (size_t)c1 + en2len + c2;
    if (cblen > sizeof(coinbase) || c1 < 1 || en2len < 1) {
        LOGWARNING("SV2 proxy %d custom coinbase %zu bytes unrebuildable", proxi->id, cblen);
        goto unlock;
    }
    /* coinb1 already carries the channel prefix and pad, so the miner's
     * enonce1var ‖ enonce2 completes the hole. */
    hex2bin(coinbase, ni->coinbase1, c1);
    memcpy(coinbase + c1, en2, en2len);
    hex2bin(coinbase + c1 + en2len, ni->coinbase2, c2);

    /* Coinbase txid, then fold the branch: natural SHA256d order throughout. */
    gen_hash(coinbase, root, (int)cblen);
    memcpy(merkle, root, 32);
    for (i = 0; i < ni->merkles; i++) {
        hex2bin(merkle + 32, &ni->merklehash[i][0], 32);
        gen_hash(merkle, root, 64);
        memcpy(merkle, root, 32);
    }

    /* The Bitcoin wire header, as the pool's own custom share validation
     * builds it. ni->prevhash is SV1 notify order, so flip it back. */
    memset(header, 0, sizeof(header));
    le = htole32(version);
    memcpy(header, &le, 4);
    hex2bin(prevbin, ni->prevhash, 32);
    flip_32(header + 4, prevbin);
    memcpy(header + 36, root, 32);
    le = htole32(ntime);
    memcpy(header + 68, &le, 4);
    sscanf(ni->nbit, "%x", &nbits);
    le = htole32(nbits);
    memcpy(header + 72, &le, 4);
    le = htole32(nonce);
    memcpy(header + 76, &le, 4);
    gen_hash(header, hash, 80);
    sdiff = diff_from_target(hash);
    /* From the U32, not from header + 72: those are the little-endian wire
     * bytes and diff_from_nbits() reads the first byte as the exponent. */
    netdiff = sv2_diff_from_nbits(nbits);
    /* Floored at 1 as the stratifier floors network_diff, so a regtest chain
     * (network diff far below 1) still resolves solves, and submitted on the
     * same 99.9% tolerance it uses for rounding. */
    if (netdiff < 1)
        netdiff = 1;
    netdiff *= 0.999;
    if (sdiff < netdiff)
        goto unlock;

    /*
     * A block. The submitted coinbase is the *declared* serialisation — with
     * the witness marker/flag and reserved value when the template has a
     * commitment — not the legacy shape hashed above.
     */
    dcblen = (size_t)ni->sv2_dcb_prefix_len + full_len + ni->sv2_dcb_suffix_len;
    dcb = ckalloc(dcblen);
    memcpy(dcb, ni->sv2_dcb_prefix, ni->sv2_dcb_prefix_len);
    memcpy(dcb + ni->sv2_dcb_prefix_len, full_en, full_len);
    memcpy(dcb + ni->sv2_dcb_prefix_len + full_len, ni->sv2_dcb_suffix, ni->sv2_dcb_suffix_len);
    sol.t = sv2_jdc_template_ref(ni->sv2_tmpl);
    sol.coinbase = dcb;
    sol.coinbase_len = dcblen;
    sol.extranonce = full_en;
    sol.extranonce_len = full_len;
    memcpy(sol.prev_hash, header + 4, 32);
    sol.version = version;
    sol.ntime = ntime;
    sol.nonce = nonce;
    sol.nbits = nbits;
unlock:
    mutex_unlock(&gdata->notify_lock);
    if (!sol.t) {
        dealloc(dcb);
        return;
    }
    flip_32(flip, hash);
    __bin2hex(hex, flip, 32);
    LOGWARNING("SV2 proxy %d custom block solve! diff %.1f of %.1f hash %s", proxi->id, sdiff, netdiff, hex);
    sv2_jdc_solved(&sol);
    sv2_jdc_template_put(sol.t);
    dealloc(dcb);
}

/* Encode and send an upstream SubmitSharesExtended for a downstream share that
 * met the upstream target. val is the stratifier's share doc (jobid=our notify
 * id64, nonce2=full extranonce, ntime, nonce, version_mask). */
static void sv2_proxy_submit_share(proxy_instance_t* proxi, yyjson_mut_val* val, int64_t client_id) {
    struct sv2_proxy*                 sp = proxi->sv2p;
    gdata_t*                          gdata = ckpool.gdata;
    notify_instance_t*                ni;
    struct sv2_submit_shares_extended sub;
    struct sv2_pending_share*         ps;
    const char *                      nonce2, *ntime_s, *nonce_s;
    uint32_t                          version_mask = 0, base_version = 0, upstream_job = 0, seq;
    uint8_t                           full_en[SV2_MAX_B0_32];
    uint8_t                           nonce2bin[32];
    uint8_t                           full_len = 0, ep[SV2_MAX_B0_32], eplen = 0;
    bool                              custom = false;
    int64_t                           jobid = 0;
    uint8_t                           pbuf[128];
    size_t                            plen = 0;
    int                               en_len;

    if (!sp || !sp->channel_open)
        return;
    if (!yyjson_mut_obj_get_int64(&jobid, val, "jobid"))
        return;
    nonce2 = yyjson_mut_get_str(yyjson_mut_obj_get(val, "nonce2"));
    ntime_s = yyjson_mut_get_str(yyjson_mut_obj_get(val, "ntime"));
    nonce_s = yyjson_mut_get_str(yyjson_mut_obj_get(val, "nonce"));
    yyjson_mut_obj_get_uint32(&version_mask, val, "version_mask");
    if (!nonce2 || !ntime_s || !nonce_s)
        return;

    /* Map our notify id64 → upstream U32 job_id + base nVersion. The hash is
     * keyed with HASH_ADD_I64 (8-byte key), so it must be found with
     * HASH_FIND_I64 — HASH_FIND_INT's 4-byte key never matches. */
    mutex_lock(&gdata->notify_lock);
    HASH_FIND_I64(gdata->notify_instances, &jobid, ni);
    if (ni) {
        upstream_job = (uint32_t)yyjson_mut_get_uint(yyjson_mut_doc_get_root(ni->jobid));
        sscanf(ni->bbversion, "%x", &base_version);
        custom = ni->sv2_custom;
        eplen = ni->sv2_en_prefix_len;
        memcpy(ep, ni->sv2_en_prefix, eplen);
    }
    mutex_unlock(&gdata->notify_lock);
    if (!ni) {
        LOGNOTICE("SV2 proxy %d submit: no notify for jobid %" PRId64, proxi->id, jobid);
        return;
    }

    /*
     * Stratifier packs en1var‖en2 as "nonce2". The channel hole is
     * pad_zeros ‖ en1var ‖ en2 (= channel size U); the leading pad is
     * folded into coinb1 (miner-hashed) so it must also lead the
     * submitted extranonce. sub is zeroed, so we place nonce2 after it.
     */
    en_len = strlen(nonce2) / 2;
    {
        int padlen = sp->pad_len;
        int total = padlen + en_len;

        if (en_len < 1 || en_len > (int)sizeof(nonce2bin) || total != (int)sp->extranonce_size ||
            total > (int)sizeof(sub.extranonce) || padlen < 0) {
            LOGNOTICE(
                "SV2 proxy %d submit: extranonce pad=%d var+en2=%d channel=%u", proxi->id, padlen, en_len,
                sp->extranonce_size);
            return;
        }
        memset(&sub, 0, sizeof(sub));
        hex2bin(nonce2bin, nonce2, en_len);
        memcpy(sub.extranonce + padlen, nonce2bin, en_len);
        sub.extranonce_len = (uint8_t)total;
        /*
         * The whole coinbase hole as the pool and our own node see it:
         * the prefix this job was built with, then what was submitted.
         * Only the solve path needs it, but it is only assembled here.
         */
        if (custom && (size_t)eplen + total <= sizeof(full_en)) {
            memcpy(full_en, ep, eplen);
            memcpy(full_en + eplen, sub.extranonce, total);
            full_len = (uint8_t)(eplen + total);
        } else
            custom = false;
    }
    sub.base.channel_id = sp->channel_id;
    mutex_lock(&sp->send_lock);
    seq = sub.base.sequence_number = sp->next_seq++;
    mutex_unlock(&sp->send_lock);
    sub.base.job_id = upstream_job;
    sub.base.nonce = strtoul(nonce_s, NULL, 16);
    sub.base.ntime = strtoul(ntime_s, NULL, 16);
    /* Full nVersion = job base version with the miner's BIP320 bits OR'd in,
     * matching how the stratifier built the header it validated. */
    sub.base.version = base_version | version_mask;

    if (!sv2_encode_submit_shares_extended(pbuf, sizeof(pbuf), &plen, &sub))
        return;
    if (!sv2_proxy_send(proxi, &proxi->cs, SV2_MSG_SUBMIT_SHARES_EXTENDED, true, pbuf, plen)) {
        LOGNOTICE("SV2 proxy %d failed to send share seq %u", proxi->id, seq);
        return;
    }
    /* Track for accounting when the batched Success / Error arrives. */
    ps = ckzalloc(sizeof(*ps));
    ps->seq = seq;
    ps->diff = proxi->diff;
    ps->client_id = client_id;
    mutex_lock(&sp->send_lock);
    HASH_ADD(hh, sp->pending, seq, sizeof(uint32_t), ps);
    mutex_unlock(&sp->send_lock);
    LOGINFO("SV2 proxy %d submitted share seq %u job %u ver %08x", proxi->id, seq, upstream_job, sub.base.version);

    /* Upstream has its copy; now see whether it was a block. */
    if (custom) {
        sv2_proxy_custom_solve(
            proxi, jobid, full_en, full_len, nonce2bin, en_len, sub.base.version, sub.base.ntime, sub.base.nonce);
    }
}

/* Dispatch one decrypted server→client frame, translating the SV2 mining flow
 * into the stratifier's notify/diff interface. */
static void sv2_proxy_handle_frame(proxy_instance_t* proxi, const uint8_t* frame, size_t flen) {
    struct sv2_proxy* sp = proxi->sv2p;
    struct sv2_frame  fr;
    const uint8_t*    pay;
    uint32_t          pl;

    if (!sv2_decode_header(frame, flen, &fr))
        return;
    /* Discard unknown extension_type frames (spec 3.4). */
    if (fr.extension_type & SV2_EXTENSION_MASK)
        return;
    pay = frame + SV2_FRAME_HEADER_LEN;
    pl = fr.msg_length;
    switch (fr.msg_type) {
        case SV2_MSG_NEW_EXTENDED_MINING_JOB: {
            struct sv2_new_extended_mining_job j;
            struct sv2_proxy_job*              job;
            int                                i;

            if (!sv2_decode_new_extended_mining_job(pay, pl, &j))
                break;
            /*
             * notify_instance_t.merklehash is fixed at 16 entries (SV1
             * parity). Codec allows up to SV2_MAX_MERKLE_PATH (32); reject
             * rather than overflow when translating to send_notify.
             */
            if (unlikely(j.merkle_count > 16)) {
                LOGWARNING(
                    "SV2 proxy %d job %u has %u merkles, exceeding max of 16 — dropped", proxi->id, j.job_id,
                    j.merkle_count);
                sv2_new_extended_mining_job_free(&j);
                break;
            }
            job = sv2_proxy_job_slot(sp, j.job_id);
            job->future = !j.min_ntime_present;
            job->min_ntime = j.min_ntime;
            job->version = j.version;
            job->merkle_count = j.merkle_count;
            for (i = 0; i < j.merkle_count; i++)
                memcpy(job->merkle_path[i], j.merkle_path[i], 32);
            /* coinb1 = coinbase_tx_prefix ‖ extranonce_prefix ‖ pad zeros.
             * The pad fills the channel extranonce hole up to extranonce_size
             * so miners hash the exact U-byte layout the pool reconstructs. */
            job->cb_tx_prefix_len = j.coinbase_tx_prefix_len;
            job->coinb1len = j.coinbase_tx_prefix_len + sp->extranonce_prefix_len + sp->pad_len;
            job->coinb1 = ckalloc(job->coinb1len ? job->coinb1len : 1);
            memcpy(job->coinb1, j.coinbase_tx_prefix, j.coinbase_tx_prefix_len);
            memcpy(job->coinb1 + j.coinbase_tx_prefix_len, sp->extranonce_prefix, sp->extranonce_prefix_len);
            if (sp->pad_len)
                memset(job->coinb1 + j.coinbase_tx_prefix_len + sp->extranonce_prefix_len, 0, sp->pad_len);
            job->coinb2len = j.coinbase_tx_suffix_len;
            job->coinb2 = ckalloc(job->coinb2len ? job->coinb2len : 1);
            memcpy(job->coinb2, j.coinbase_tx_suffix, j.coinbase_tx_suffix_len);
            sv2_new_extended_mining_job_free(&j);
            LOGINFO(
                "SV2 proxy %d stored job %u future=%d merkles=%u", proxi->id, job->job_id, job->future,
                job->merkle_count);
            if (job->future)
                break;
            /*
             * A pool fee bump on the tip our declared work is on must not
             * replace it. ckpool stops sending its own
             * jobs to a channel that has custom work, but nothing in the
             * protocol obliges a pool to, and the job is still stored so a
             * share arriving against it maps back to its job_id.
             */
            if (sp->work_src == SV2_WS_LOCAL_JD && sp->have_custom_prev && sp->have_prevhash &&
                !memcmp(sp->custom_prev, sp->prev_hash, 32)) {
                LOGINFO(
                    "SV2 proxy %d not notifying pool job %u: custom work is "
                    "live on this tip",
                    proxi->id, job->job_id);
                break;
            }
            /* Immediate job: mine now on the current prevhash (no flush). */
            sv2_proxy_send_job(proxi, job, false);
            break;
        }
        case SV2_MSG_SET_NEW_PREV_HASH: {
            struct sv2_set_new_prev_hash p;
            struct sv2_proxy_job*        job;
            const char*                  implausible = NULL;
            enum sv2_tip_rel             rel;
            bool                         custom_tip;

            if (!sv2_decode_set_new_prev_hash(pay, pl, &p))
                break;
            /* Spec: unknown job_id is a protocol error — fail closed without
             * mutating tip (avoids silent non-clean work on a new tip). */
            job = sv2_proxy_find_job(sp, p.job_id);
            if (!job) {
                LOGWARNING("SV2 proxy %d SetNewPrevHash job %u not found — reconnecting", proxi->id, p.job_id);
                sp->want_reconnect = true;
                break;
            }
            memcpy(sp->prev_hash, p.prev_hash, 32);
            sp->nbits = p.nbits;
            sp->snph_min_ntime = p.min_ntime;
            sp->have_prevhash = true;
            /*
             * ckpool's JDS drops a channel's custom jobs as soon as its own tip
             * moves (channel_clear_custom_locked), so custom work only outlives
             * a SetNewPrevHash that repeats the tip it was declared on.
             */
            custom_tip = sp->have_custom_prev && !memcmp(sp->custom_prev, p.prev_hash, 32);
            sp->have_custom_prev = custom_tip;
            /*
             * Where the pool's tip sits relative to our own chain is what the
             * whole arbiter turns on, and only the JD client knows: it owns the
             * local template provider. Job declaration
             * binds to the parent entry's channel in v1 and subproxies
             * share its id, so only the parent speaks for the pool's tip.
             */
            rel = proxi->subid ? SV2_TIP_NO_LOCAL
                               : sv2_jdc_pool_tip(proxi->id, p.prev_hash, p.nbits, p.min_ntime, &implausible);
            LOGNOTICE(
                "SV2 proxy %d new prevhash for job %u nbits=0x%08x (%s)", proxi->id, p.job_id, p.nbits,
                sv2_tip_rel_str(rel));
            if (custom_tip) {
                /*
                 * The pool has repeated the tip our custom work is on. It is
                 * not a work change, and handing miners pool work for it
                 * would cost them the declared job for nothing.
                 */
                LOGINFO(
                    "SV2 proxy %d not notifying pool job %u: custom work is "
                    "live on this tip",
                    proxi->id, p.job_id);
                break;
            }
            switch (rel) {
                case SV2_TIP_AHEAD:
                    /*
                     * The pool leads our node. Its work is what it
                     * credits, so miners follow it either way; the
                     * bridge only records that we expect to be back on
                     * declared work within seconds.
                     */
                    if (implausible) {
                        LOGWARNING(
                            "SV2 proxy %d pool tip has %s — mining its "
                            "work, but not as a tip our node is about "
                            "to reach",
                            proxi->id, implausible);
                        sv2_proxy_work_src(proxi, SV2_WS_POOL, implausible);
                    } else {
                        sv2_proxy_work_src(proxi, SV2_WS_BRIDGE, "the pool is ahead of our node");
                    }
                    break;
                case SV2_TIP_BEHIND:
                    /*
                     * Our node is ahead — the common case for a well
                     * connected one. A declare for our higher tip
                     * cannot be accepted, so sess_declare holds it back
                     * until this pool announces the tip; nothing local
                     * goes downstream in the meantime.
                     */
                    sv2_proxy_work_src(proxi, SV2_WS_POOL, "our node is ahead of the pool");
                    break;
                case SV2_TIP_SAME:
                    sv2_proxy_work_src(proxi, SV2_WS_POOL, "tips agree; declaring this tip");
                    break;
                case SV2_TIP_NO_LOCAL:
                    sv2_proxy_work_src(
                        proxi, SV2_WS_POOL, proxi->subid ? "subproxy: pool work only" : "no local template to declare");
                    break;
            }
            /* Activate the referenced job, flushing only a tip miners are not
             * already working on. */
            sv2_proxy_send_job(proxi, job, sv2_proxy_want_clean(sp, p.prev_hash));
            break;
        }
        case SV2_MSG_SET_CUSTOM_MINING_JOB_SUCCESS: {
            struct sv2_set_custom_mining_job_success ok;
            struct sv2_proxy_job                     stage, *job;
            bool                                     ours, clean;

            if (!sv2_decode_set_custom_mining_job_success(pay, pl, &ok))
                break;
            memset(&stage, 0, sizeof(stage));
            mutex_lock(&sp->send_lock);
            ours = sp->custom_pending && ok.request_id == sp->custom_request_id;
            if (ours) {
                sp->custom_pending = false;
                stage = sp->custom_stage;
                memset(&sp->custom_stage, 0, sizeof(sp->custom_stage));
            }
            mutex_unlock(&sp->send_lock);
            if (!ours) {
                LOGNOTICE("SV2 proxy %d SetCustomMiningJob.Success req=%u is not ours", proxi->id, ok.request_id);
                break;
            }
            /*
             * Flush only if downstream is not already on this tip: the pool
             * work we bridged with, or an earlier custom job for it, has
             * already been flushed, and the cutover between them is not a work
             * change to a miner.
             */
            clean = sv2_proxy_want_clean(sp, stage.prev_hash);
            memcpy(sp->custom_prev, stage.prev_hash, 32);
            sp->have_custom_prev = true;
            /* Custom and pool job ids come from one server-side counter, so
             * they share the ring without colliding. */
            job = sv2_proxy_job_slot(sp, ok.job_id);
            stage.job_id = ok.job_id;
            stage.valid = true;
            *job = stage;
            LOGNOTICE(
                "SV2 proxy %d custom job %u accepted (req=%u) clean=%d", proxi->id, ok.job_id, ok.request_id, clean);
            sv2_proxy_send_job(proxi, job, clean);
            sv2_proxy_work_src(proxi, SV2_WS_LOCAL_JD, "declared work is live");
            sv2_jdc_custom_result(true, ok.job_id, NULL);
            break;
        }
        case SV2_MSG_SET_CUSTOM_MINING_JOB_ERROR: {
            struct sv2_set_custom_mining_job_error err;
            struct sv2_proxy_job                   stage;
            bool                                   ours;

            memset(&err, 0, sizeof(err));
            if (!sv2_decode_set_custom_mining_job_error(pay, pl, &err))
                break;
            memset(&stage, 0, sizeof(stage));
            mutex_lock(&sp->send_lock);
            ours = sp->custom_pending && err.request_id == sp->custom_request_id;
            if (ours) {
                sp->custom_pending = false;
                stage = sp->custom_stage;
                memset(&sp->custom_stage, 0, sizeof(sp->custom_stage));
            }
            mutex_unlock(&sp->send_lock);
            if (!ours)
                break;
            /* Keep whatever work is live: never emit a job the pool has not
             * acknowledged. The JD client decides whether to re-declare. */
            sv2_proxy_job_clear(&stage);
            LOGWARNING("SV2 proxy %d SetCustomMiningJob.Error req=%u: %s", proxi->id, err.request_id, err.error_code);
            sv2_jdc_custom_result(false, 0, err.error_code);
            break;
        }
        case SV2_MSG_SET_TARGET: {
            struct sv2_set_target t;
            double                diff;

            if (!sv2_decode_set_target(pay, pl, &t))
                break;
            diff = diff_from_target(t.maximum_target);
            if (diff < 1)
                diff = 1;
            proxi->diff = diff;
            LOGNOTICE("SV2 proxy %d SetTarget diff %.1f", proxi->id, diff);
            send_diff(proxi);
            break;
        }
        case SV2_MSG_SET_EXTRANONCE_PREFIX: {
            struct sv2_set_extranonce_prefix e;
            struct sv2_proxy_job             dropped = {};
            bool                             had_custom = false;
            int                              ji;

            if (!sv2_decode_set_extranonce_prefix(pay, pl, &e))
                break;
            memcpy(sp->extranonce_prefix, e.extranonce_prefix, e.extranonce_prefix_len);
            sp->extranonce_prefix_len = e.extranonce_prefix_len;
            LOGNOTICE(
                "SV2 proxy %d SetExtranoncePrefix len=%u — rebuild + clean notify", proxi->id, e.extranonce_prefix_len);
            /* All stored jobs embed the old prefix in coinb1; rebuild. */
            for (ji = 0; ji < SV2_PROXY_JOBS; ji++) {
                struct sv2_proxy_job* job = &sp->jobs[ji];
                uint8_t*              ncoinb1;
                int                   nlen;

                /*
                 * A custom job cannot be rebuilt this way: its coinbase hole
                 * is what the JDS accepted and what both the pool's block
                 * rebuild and our PushSolution assume, so a new prefix — even
                 * one of the same length — makes it unmineable. Drop it and
                 * have the template declared again for the new layout.
                 */
                if (job->custom) {
                    if (job->valid)
                        had_custom = true;
                    sv2_proxy_job_clear(job);
                    continue;
                }
                if (!job->valid || !job->coinb1 || job->cb_tx_prefix_len < 0 || job->cb_tx_prefix_len > job->coinb1len)
                    continue;
                nlen = job->cb_tx_prefix_len + sp->extranonce_prefix_len + sp->pad_len;
                ncoinb1 = ckalloc(nlen ? nlen : 1);
                memcpy(ncoinb1, job->coinb1, job->cb_tx_prefix_len);
                memcpy(ncoinb1 + job->cb_tx_prefix_len, sp->extranonce_prefix, sp->extranonce_prefix_len);
                if (sp->pad_len)
                    memset(ncoinb1 + job->cb_tx_prefix_len + sp->extranonce_prefix_len, 0, sp->pad_len);
                dealloc(job->coinb1);
                job->coinb1 = ncoinb1;
                job->coinb1len = nlen;
            }
            /* Flush newest minable job so downstream adopts the prefix. */
            if (sp->have_prevhash) {
                for (ji = 0; ji < SV2_PROXY_JOBS; ji++) {
                    int                   idx = (sp->job_head - 1 - ji + SV2_PROXY_JOBS) % SV2_PROXY_JOBS;
                    struct sv2_proxy_job* job = &sp->jobs[idx];

                    if (job->valid && !job->future && job->coinb1) {
                        sv2_proxy_send_job(proxi, job, true);
                        break;
                    }
                }
            }
            /* A SetCustomMiningJob still in flight was built on the old hole, so
             * its Success would install work nobody can mine. */
            mutex_lock(&sp->send_lock);
            if (sp->custom_pending) {
                sp->custom_pending = false;
                dropped = sp->custom_stage;
                memset(&sp->custom_stage, 0, sizeof(sp->custom_stage));
                had_custom = true;
            }
            mutex_unlock(&sp->send_lock);
            sv2_proxy_job_clear(&dropped);
            if (had_custom) {
                sp->have_custom_prev = false;
                sv2_proxy_work_src(proxi, SV2_WS_POOL, "custom work dropped for a new extranonce prefix");
                sv2_jdc_custom_dropped("SetExtranoncePrefix");
            }
            break;
        }
        case SV2_MSG_SUBMIT_SHARES_SUCCESS: {
            struct sv2_submit_shares_success ok;
            struct sv2_pending_share *       ps, *tmp;
            double*                          credit = NULL;
            int                              credited = 0, i;

            if (!sv2_decode_submit_shares_success(pay, pl, &ok))
                break;
            /* Unlink every pending share up to last_sequence_number (batched
             * ack; signed diff handles sequence wraparound), then account for
             * them *after* dropping send_lock: account_shares takes proxy_lock
             * and proxystats takes proxy_lock before send_lock, so crediting
             * under send_lock would invert the lock order and deadlock. */
            mutex_lock(&sp->send_lock);
            HASH_ITER(hh, sp->pending, ps, tmp) {
                if ((int32_t)(ok.last_sequence_number - ps->seq) >= 0) {
                    credit = ckrealloc(credit, (credited + 1) * sizeof(double));
                    credit[credited++] = ps->diff;
                    HASH_DEL(sp->pending, ps);
                    dealloc(ps);
                }
            }
            mutex_unlock(&sp->send_lock);
            for (i = 0; i < credited; i++)
                account_shares(proxi, credit[i], true);
            dealloc(credit);
            LOGINFO(
                "SV2 proxy %d SubmitShares.Success last_seq=%u accepted=%u credited=%d", proxi->id,
                ok.last_sequence_number, ok.new_submits_accepted_count, credited);
            break;
        }
        case SV2_MSG_SUBMIT_SHARES_ERROR: {
            struct sv2_submit_shares_error err;
            struct sv2_pending_share*      ps;
            double                         diff = 0;
            bool                           found = false;
            uint32_t                       seq;

            if (!sv2_decode_submit_shares_error(pay, pl, &err))
                break;
            seq = err.sequence_number;
            /* As above: unlink under send_lock, account after releasing it. */
            mutex_lock(&sp->send_lock);
            HASH_FIND(hh, sp->pending, &seq, sizeof(uint32_t), ps);
            if (ps) {
                diff = ps->diff;
                found = true;
                HASH_DEL(sp->pending, ps);
                dealloc(ps);
            }
            mutex_unlock(&sp->send_lock);
            if (found)
                account_shares(proxi, diff, false);
            LOGNOTICE("SV2 proxy %d SubmitShares.Error seq=%u: %s", proxi->id, err.sequence_number, err.error_code);
            break;
        }
        case SV2_MSG_RECONNECT: {
            struct sv2_reconnect rc;

            memset(&rc, 0, sizeof(rc));
            sv2_decode_reconnect(pay, pl, &rc);
            /* Drop and reconnect. The handshake re-verifies the server cert
             * against the same authority key (spec 3.6.5), so we never follow
             * a redirect to a different pool. new_host redirect to a different
             * host is not followed — we reconnect to the configured URL. */
            if (rc.new_host[0])
                LOGNOTICE(
                    "SV2 proxy %d Reconnect to %s:%u — reconnecting to configured host", proxi->id, rc.new_host,
                    rc.new_port);
            else
                LOGNOTICE("SV2 proxy %d Reconnect received", proxi->id);
            sp->want_reconnect = true;
            break;
        }
        default:
            LOGDEBUG("SV2 proxy %d unhandled msg 0x%02x", proxi->id, fr.msg_type);
            break;
    }
}

/* Read available socket bytes and dispatch every complete frame. Returns false
 * on fatal error (caller drops + reconnects). cs->sem held by caller. */
static bool sv2_proxy_service(proxy_instance_t* proxi, connsock_t* cs) {
    struct sv2_proxy* sp = proxi->sv2p;
    uint8_t           rbuf[8192];
    int               r;

    if (!sp)
        return false;
    /* Blocking socket, called after EPOLLIN: one read returns what is
     * available; partial frames persist in rx across wakeups. */
    r = read(cs->fd, rbuf, sizeof(rbuf));
    if (r < 1)
        return false;
    if (!sv2_rx_append(sp, rbuf, (size_t)r))
        return false;
    while (42) {
        uint8_t* plain = NULL;
        size_t   plainlen = 0;
        int      rc = sv2_rx_next(sp, &plain, &plainlen);

        if (rc == 1) {
            sv2_proxy_handle_frame(proxi, plain, plainlen);
            dealloc(plain);
            continue;
        }
        if (rc == -1)
            return false;
        break;
    }

    /* A Reconnect frame asks us to drop and re-handshake. */
    if (sp->want_reconnect)
        return false;

    /*
     * Bridging the pool's lead is a race we expect to win in seconds, so a
     * bridge this old means our node is stuck rather than catching up — in IBD,
     * partitioned, or the pool is on a chain we will not follow. Nothing
     * downstream changes (pool work is always what the pool credits) and no JD
     * resync is needed: the declare fires off whichever of the two tips moves
     * next. The operator, however, should hear that local templates have
     * stopped contributing.
     */
    if (sp->work_src == SV2_WS_BRIDGE && time(NULL) - sp->bridge_at >= SV2_BRIDGE_TIMEOUT_SECS) {
        LOGWARNING(
            "SV2 proxy %d has mined the pool's tip for %ds without our node "
            "reaching it; no declared work until it does",
            proxi->id, SV2_BRIDGE_TIMEOUT_SECS);
        sv2_proxy_work_src(proxi, SV2_WS_POOL, "our node did not catch up");
    }

    /*
     * Abandon a SetCustomMiningJob the pool never answered, so its material
     * (and the template it pins) is not held forever. Checked on inbound
     * traffic, which for a live channel means at least every job.
     */
    if (sp->custom_pending) {
        struct sv2_proxy_job stage;
        time_t               now = time(NULL);
        bool                 timedout = false;

        memset(&stage, 0, sizeof(stage));
        mutex_lock(&sp->send_lock);
        if (sp->custom_pending && now - sp->custom_sent >= SV2_JDC_CUSTOM_TIMEOUT_SECS) {
            sp->custom_pending = false;
            stage = sp->custom_stage;
            memset(&sp->custom_stage, 0, sizeof(sp->custom_stage));
            timedout = true;
        }
        mutex_unlock(&sp->send_lock);
        if (timedout) {
            LOGNOTICE(
                "SV2 proxy %d custom job req=%u unanswered after %ds, "
                "dropped",
                proxi->id, sp->custom_request_id, (int)(now - sp->custom_sent));
            sv2_proxy_job_clear(&stage);
            sv2_jdc_custom_result(false, 0, "timeout");
        }
    }

    /* Periodically inform the pool of our aggregate hashrate (spec 5.3.7).
     * Informational only — the pool's own target policy still governs; our
     * hysteresis-side fix means well-behaved servers won't thrash on it. */
    if (sp->channel_open) {
        time_t now = time(NULL);

        if (now - sp->last_update >= 60) {
            struct sv2_update_channel uc;
            double                    hr = proxi->dsps5 * 4294967296.0; /* diff/s → h/s */
            uint8_t                   pbuf[64];
            size_t                    plen = 0;

            sp->last_update = now;
            memset(&uc, 0, sizeof(uc));
            uc.channel_id = sp->channel_id;
            uc.nominal_hash_rate = (float)(hr > 0 ? hr : 0);
            memset(uc.maximum_target, 0xff, 32); /* impose no ceiling */
            if (sv2_encode_update_channel(pbuf, sizeof(pbuf), &plen, &uc))
                sv2_proxy_send(proxi, cs, SV2_MSG_UPDATE_CHANNEL, true, pbuf, plen);
        }
    }
    return true;
}
#endif /* HAVE_SV2 */

static bool proxy_alive(proxy_instance_t* proxi, connsock_t* cs, bool pinging) {
    proxy_instance_t* parent = proxi->parent;
    bool              ret = false;

    /* Has this proxy already been reconnected? */
    if (proxi->alive)
        return true;
    if (proxi->disabled)
        return false;

    /* Serialise all send/recvs here with the cs semaphore */
    cksem_wait(&cs->sem);
    /* Check again after grabbing semaphore */
    if (unlikely(proxi->alive)) {
        ret = true;
        goto out;
    }
    if (!extract_sockaddr(proxi->url, &cs->url, &cs->port)) {
        LOGWARNING("Failed to extract address from %s", proxi->url);
        goto out;
    }
    if (!connect_proxy(cs, proxi)) {
        if (!pinging) {
            LOGINFO("Failed to connect to %s:%s in proxy_mode!", cs->url, cs->port);
        }
        parent->connect_status = STATUS_FAIL;
        proxy_backoff(parent);
        goto out;
    }
    parent->connect_status = STATUS_SUCCESS;

#ifdef HAVE_SV2
    if (proxi->sv2) {
        /* SV2 upstream: Noise handshake + SetupConnection + open channel
         * in place of subscribe/authorise. */
        if (!sv2_proxy_connect(proxi, cs)) {
            if (!pinging) {
                LOGWARNING("Failed SV2 setup to %s:%s !", cs->url, cs->port);
            }
            parent->subscribe_status = STATUS_FAIL;
            proxy_backoff(parent);
            goto out;
        }
        parent->subscribe_status = STATUS_SUCCESS;
        parent->auth_status = STATUS_SUCCESS;
        /* Register the channel's enonce interface with the stratifier so
         * it creates workbases from our translated notifies. */
        send_subscribe(proxi);
        proxi->authorised = ret = true;
        parent->backoff = 0;
        goto out;
    }
#endif

    if (ckpool.node) {
        if (!node_stratum(cs, proxi)) {
            LOGWARNING("Failed initial node setup to %s:%s !", cs->url, cs->port);
            goto out;
        }
        ret = true;
        goto out;
    }
    if (ckpool.passthrough) {
        if (!passthrough_stratum(cs, proxi)) {
            LOGWARNING("Failed initial passthrough to %s:%s !", cs->url, cs->port);
            goto out;
        }
        ret = true;
        goto out;
    }
    /* Test we can connect, authorise and get stratum information */
    if (!subscribe_stratum(cs, proxi)) {
        if (!pinging) {
            LOGWARNING("Failed initial subscribe to %s:%s !", cs->url, cs->port);
        }
        parent->subscribe_status = STATUS_FAIL;
        proxy_backoff(parent);
        goto out;
    }
    parent->subscribe_status = STATUS_SUCCESS;

    if (!ckpool.passthrough)
        send_subscribe(proxi);
    if (!auth_stratum(cs, proxi)) {
        if (!pinging) {
            LOGWARNING("Failed initial authorise to %s:%s with %s:%s !", cs->url, cs->port, proxi->auth, proxi->pass);
        }
        parent->auth_status = STATUS_FAIL;
        proxy_backoff(parent);
        goto out;
    }
    parent->auth_status = STATUS_SUCCESS;
    proxi->authorised = ret = true;
    parent->backoff = 0;
    if (ckpool.mindiff > 1)
        suggest_diff(cs, proxi);
out:
    if (!ret) {
        send_stratifier_deadproxy(proxi->id, proxi->subid);
        /* Close and invalidate the file handle */
        Close(cs->fd);
    }
    proxi->alive = ret;
    cksem_post(&cs->sem);

    /* Decrease the parent's recruit count after sending the stratifier the
     * new subscribe so it can get an accurate headroom count before
     * requesting more proxies. */
    if (ret) {
        proxy_instance_t* parent = proxi->parent;

        if (parent) {
            mutex_lock(&parent->proxy_lock);
            parent->recruit -= proxi->clients_per_proxy;
            if (parent->recruit < 0)
                parent->recruit = 0;
            mutex_unlock(&parent->proxy_lock);
        }
    }

    return ret;
}

static void* proxy_recruit(void* arg) {
    proxy_instance_t *proxy, *parent = (proxy_instance_t*)arg;
    gdata_t*          gdata = ckpool.gdata;
    bool              recruit, alive;

    pthread_detach(pthread_self());

    /* We do this in a separate thread so it's okay to sleep here */
    if (parent->backoff)
        sleep(parent->backoff);

retry:
    recruit = false;
    proxy = create_subproxy(gdata, parent, parent->url, parent->baseurl);
    alive = proxy_alive(proxy, &proxy->cs, false);
    if (!alive) {
        LOGNOTICE("Subproxy failed proxy_alive testing");
        store_proxy(gdata, proxy);
    } else
        add_subproxy(parent, proxy);

    mutex_lock(&parent->proxy_lock);
    if (alive && parent->recruit > 0)
        recruit = true;
    else /* Reset so the next request will try again */
        parent->recruit = 0;
    mutex_unlock(&parent->proxy_lock);

    if (recruit)
        goto retry;

    return NULL;
}

static void recruit_subproxies(proxy_instance_t* proxi, const int recruits) {
    bool      recruit = false;
    pthread_t pth;

    mutex_lock(&proxi->proxy_lock);
    if (!proxi->recruit)
        recruit = true;
    if (proxi->recruit < recruits)
        proxi->recruit = recruits;
    mutex_unlock(&proxi->proxy_lock);

    if (recruit)
        create_pthread(&pth, proxy_recruit, proxi);
}

/* Queue up to the requested amount */
static void recruit_subproxy(gdata_t* gdata, const char* buf) {
    int               recruits = 1, id = 0;
    proxy_instance_t* proxy;

    sscanf(buf, "recruit=%d:%d", &id, &recruits);
    proxy = proxy_by_id(gdata, id);
    if (unlikely(!proxy)) {
        LOGNOTICE("Generator failed to find proxy id %d to recruit subproxies", id);
        return;
    }
    recruit_subproxies(proxy, recruits);
}

static void* proxy_reconnect(void* arg) {
    proxy_instance_t* proxy = (proxy_instance_t*)arg;
    connsock_t*       cs = &proxy->cs;

    pthread_detach(pthread_self());
    if (proxy->parent->backoff)
        sleep(proxy->parent->backoff);
    proxy_alive(proxy, cs, true);
    proxy->reconnecting = false;
    return NULL;
}

/* For reconnecting the parent proxy instance async */
static void reconnect_proxy(proxy_instance_t* proxi) {
    pthread_t pth;

    if (proxi->reconnecting)
        return;
    proxi->reconnecting = true;
    create_pthread(&pth, proxy_reconnect, proxi);
}

/* For receiving messages from an upstream pool to pass downstream. Responsible
 * for setting up the connection and testing pool is live. */
static void* passthrough_recv(void* arg) {
    proxy_instance_t* proxi = (proxy_instance_t*)arg;
    connsock_t*       cs = &proxi->cs;
    bool              alive;

    rename_proc("passrecv");

    proxi->parent = proxi;
    if (proxy_alive(proxi, cs, false))
        LOGWARNING("Passthrough proxy %d:%s connection established", proxi->id, proxi->url);
    alive = proxi->alive;

    while (42) {
        float timeout = 5;
        int   ret;

        while (!proxy_alive(proxi, cs, true)) {
            alive = false;
            sleep(5);
        }
        if (!alive) {
            reconnect_generator();
            LOGWARNING("Passthrough %d:%s recovered", proxi->id, proxi->url);
            alive = true;
        }

        cksem_wait(&cs->sem);
        ret = read_socket_line(cs, &timeout);
        /* Simply forward the message on, as is, to the connector to
         * process. Possibly parse parameters sent by upstream pool
         * here */
        if (likely(ret > 0)) {
            LOGDEBUG("Passthrough recv received upstream msg: %s", cs->buf);
            send_proc(ckpool.connector, cs->buf);
        } else if (ret < 0) {
            /* Read failure */
            LOGWARNING(
                "Passthrough %d:%s failed to read_socket_line in passthrough_recv, attempting reconnect", proxi->id,
                proxi->url);
            alive = proxi->alive = false;
            Close(cs->fd);
            reconnect_generator();
        } else /* No messages during timeout */
            LOGDEBUG("Passthrough %d:%s no messages received", proxi->id, proxi->url);
        cksem_post(&cs->sem);
    }
    return NULL;
}

static bool subproxies_alive(proxy_instance_t* proxy) {
    proxy_instance_t *subproxy, *tmp;
    bool              ret = false;

    mutex_lock(&proxy->proxy_lock);
    HASH_ITER(sh, proxy->subproxies, subproxy, tmp) {
        if (subproxy->alive) {
            ret = true;
            break;
        }
    }
    mutex_unlock(&proxy->proxy_lock);

    return ret;
}

/* For receiving messages from the upstream proxy, also responsible for setting
 * up the connection and testing it's alive. */
static void* proxy_recv(void* arg) {
    proxy_instance_t*  proxi = (proxy_instance_t*)arg;
    connsock_t*        cs = &proxi->cs;
    proxy_instance_t*  subproxy;
    gdata_t*           gdata = ckpool.gdata;
    struct epoll_event event;
    bool               alive;
    int                epfd;

    rename_proc("proxyrecv");
    pthread_detach(pthread_self());

    proxi->epfd = epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        LOGEMERG("FATAL: Failed to create epoll in proxyrecv");
        return NULL;
    }

    if (proxy_alive(proxi, cs, false)) {
        LOGWARNING("Proxy %d:%s connection established", proxi->id, proxi->url);
        /*
         * The generator chose the current proxy as soon as any entry was
         * alive, so a slower one that should be preferred — an SV2 upstream
         * spends a Noise handshake and a channel open where SV1 needs one
         * connect — would otherwise never take over. Make it re-examine.
         */
        reconnect_generator();
    }

    alive = proxi->alive;

    while (42) {
        bool               message = false, hup = false;
        share_msg_t *      share, *tmpshare;
        notify_instance_t *ni, *tmp;
        /* Initialised: the timeout/error branch below reaches the drain
         * loop's read_socket_line without otherwise setting it, and a
         * HUP arriving without EPOLLIN skips the assignment at EPOLLIN
         * too. */
        float  timeout = 0;
        time_t now;
        int    ret;

        subproxy = proxi;
        if (!proxi->alive) {
            reconnect_proxy(proxi);
            while (!subproxies_alive(proxi)) {
                reconnect_proxy(proxi);
                if (alive) {
                    reconnect_generator();
                    LOGWARNING("Proxy %d:%s failed, attempting reconnect", proxi->id, proxi->url);
                    alive = false;
                }
                sleep(5);
            }
        }
        if (!alive) {
            reconnect_generator();
            LOGWARNING("Proxy %d:%s recovered", proxi->id, proxi->url);
            alive = true;
        }

        now = time(NULL);

        /* Age old notifications older than 10 mins old */
        mutex_lock(&gdata->notify_lock);
        HASH_ITER(hh, gdata->notify_instances, ni, tmp) {
            if (HASH_COUNT(gdata->notify_instances) < 3)
                break;
            if (ni->notify_time < now - 600) {
                HASH_DEL(gdata->notify_instances, ni);
                clear_notify(ni);
            }
        }
        mutex_unlock(&gdata->notify_lock);

        /* Similary with shares older than 2 mins without response */
        mutex_lock(&gdata->share_lock);
        HASH_ITER(hh, gdata->shares, share, tmpshare) {
            if (share->submit_time < now - 120) {
                HASH_DEL(gdata->shares, share);
                free(share);
            }
        }
        mutex_unlock(&gdata->share_lock);

        cs = NULL;
        /* If we don't get an update within 10 minutes the upstream pool
         * has likely stopped responding. SV2 upstreams send jobs at least
         * every ~30s, so a 90s silence means a dead connection — a frame
         * resets the wait, so this only fires when genuinely stalled. */
        {
            int etimeout = 600000;
#ifdef HAVE_SV2
            if (proxi->sv2)
                etimeout = 90000;
#endif
            ret = epoll_wait(epfd, &event, 1, etimeout);
        }
        if (likely(ret > 0)) {
            subproxy = event.data.ptr;
            cs = &subproxy->cs;
            if (!subproxy->alive) {
                cs = NULL;
                continue;
            }

            /* Serialise messages from here once we have a cs by
             * holding the semaphore. */
            cksem_wait(&cs->sem);
            /* Process any messages before checking for errors in
             * case a message is sent and then the socket
             * immediately closed.
             */
            if (event.events & EPOLLIN) {
#ifdef HAVE_SV2
                if (subproxy->sv2) {
                    /* Binary Noise transport: read + dispatch
                     * frames rather than newline JSON. */
                    if (!sv2_proxy_service(subproxy, cs)) {
                        LOGNOTICE(
                            "SV2 proxy %d:%d %s recv failed in proxy_recv", proxi->id, subproxy->subid, subproxy->url);
                        hup = true;
                    }
                } else
#endif
                {
                    timeout = 30;
                    ret = read_socket_line(cs, &timeout);
                    /* If we are unable to read anything within 30
                     * seconds at this point after EPOLLIN is set
                     * then the socket is dead. */
                    if (ret < 1) {
                        LOGNOTICE(
                            "Proxy %d:%d %s failed to read_socket_line in proxy_recv", proxi->id, subproxy->subid,
                            subproxy->url);
                        hup = true;
                    } else {
                        message = true;
                        timeout = 0;
                    }
                }
            }
            if (event.events & (EPOLLHUP | EPOLLERR | EPOLLRDHUP)) {
                LOGNOTICE("Proxy %d:%d %s epoll hangup in proxy_recv", proxi->id, subproxy->subid, subproxy->url);
                hup = true;
            }
        } else if (ret < 0 && errno == EINTR) {
            /* Interrupted by a signal, not a stalled upstream, so
             * just go around again. */
            continue;
        } else {
            /* Timeout (ret == 0) or epoll error. cs is still NULL
             * here, so the drain loop and the hangup handler below
             * would both be skipped and the parent would never
             * reconnect. Point cs at the parent and take its
             * semaphore so the hangup path stays balanced with the
             * cksem_post at the end of the loop. */
            LOGNOTICE("Proxy %d:%s epoll timeout/error in proxy_recv, forcing reconnect", proxi->id, proxi->url);
            subproxy = proxi;
            cs = &proxi->cs;
            cksem_wait(&cs->sem);
            hup = true;
        }

        /* Parse any other messages already fully buffered with a zero
         * timeout. SV2 proxies are serviced above (binary transport).
         * cs is NULL on the timeout path only when we did not take the
         * semaphore, which cannot happen now, but guard it regardless. */
#ifdef HAVE_SV2
        if (!subproxy->sv2 && cs)
#else
        if (cs)
#endif
            while (message || read_socket_line(cs, &timeout) > 0) {
                message = false;
                timeout = 0;
                /* client.reconnect marks non-parent subproxies !alive
                 * without recycling; treat as hangup and stop parsing. */
                if (parse_method(subproxy, cs->buf)) {
                    if (!subproxy->alive) {
                        hup = true;
                        break;
                    }
                    continue;
                }
                if (!subproxy->alive) {
                    hup = true;
                    break;
                }
                /* If it's not a method it should be a share result */
                if (!parse_share(gdata, subproxy, cs->buf)) {
                    LOGNOTICE("Proxy %d:%d unhandled stratum message: %s", subproxy->id, subproxy->subid, cs->buf);
                }
            }

        /* Process hangup only after parsing messages. Non-parent
         * subproxies are recycled in disable_subproxy — do not touch
         * their connsock (or post its sem) after that. */
        if (hup && cs) {
            bool is_parent = parent_proxy(subproxy);

            disable_subproxy(gdata, proxi, subproxy);
            if (!is_parent)
                cs = NULL;
        }
        if (cs)
            cksem_post(&cs->sem);
    }

    return NULL;
}

/* Thread that handles all received messages from user proxies */
static void* userproxy_recv(void __maybe_unused* arg) {
    gdata_t*           gdata = ckpool.gdata;
    struct epoll_event event;
    int                epfd;

    rename_proc("uproxyrecv");
    pthread_detach(pthread_self());

    epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        LOGEMERG("FATAL: Failed to create epoll in userproxy_recv");
        return NULL;
    }

    while (42) {
        proxy_instance_t * proxy, *tmpproxy;
        bool               message = false, hup = false;
        share_msg_t *      share, *tmpshare;
        notify_instance_t *ni, *tmp;
        connsock_t*        cs;
        float              timeout;
        time_t             now;
        int                ret;

        mutex_lock(&gdata->lock);
        HASH_ITER(hh, gdata->proxies, proxy, tmpproxy) {
            if (!proxy->global && !proxy->alive) {
                proxy->epfd = epfd;
                reconnect_proxy(proxy);
            }
        }
        mutex_unlock(&gdata->lock);

        ret = epoll_wait(epfd, &event, 1, 1000);
        if (ret < 1) {
            if (likely(!ret))
                continue;
            LOGEMERG("Failed to epoll_wait in userproxy_recv");
            break;
        }
        proxy = event.data.ptr;
        /* Make sure we haven't popped this off before we've finished
         * subscribe/auth */
        if (unlikely(!proxy->authorised))
            continue;

        now = time(NULL);

        mutex_lock(&gdata->notify_lock);
        HASH_ITER(hh, gdata->notify_instances, ni, tmp) {
            if (HASH_COUNT(gdata->notify_instances) < 3)
                break;
            if (ni->notify_time < now - 600) {
                HASH_DEL(gdata->notify_instances, ni);
                clear_notify(ni);
            }
        }
        mutex_unlock(&gdata->notify_lock);

        /* Similary with shares older than 2 mins without response */
        mutex_lock(&gdata->share_lock);
        HASH_ITER(hh, gdata->shares, share, tmpshare) {
            if (share->submit_time < now - 120) {
                HASH_DEL(gdata->shares, share);
                free(share);
            }
        }
        mutex_unlock(&gdata->share_lock);

        cs = &proxy->cs;

#if 0
		/* Is this needed at all? */
		if (!proxy->alive)
			continue;
#endif

        if ((event.events & (EPOLLHUP | EPOLLERR | EPOLLRDHUP))) {
            LOGNOTICE("Proxy %d:%d %s hangup in userproxy_recv", proxy->id, proxy->subid, proxy->url);
            hup = true;
        }

        if (likely(event.events & EPOLLIN)) {
            timeout = 30;

            cksem_wait(&cs->sem);
            ret = read_socket_line(cs, &timeout);
            /* If we are unable to read anything within 30
             * seconds at this point after EPOLLIN is set
             * then the socket is dead. */
            if (ret < 1) {
                LOGNOTICE(
                    "Proxy %d:%d %s failed to read_socket_line in userproxy_recv", proxy->id, proxy->subid, proxy->url);
                hup = true;
            } else {
                message = true;
                timeout = 0;
            }
            while (message || (ret = read_socket_line(cs, &timeout)) > 0) {
                message = false;
                timeout = 0;
                if (parse_method(proxy, cs->buf)) {
                    if (!proxy->alive) {
                        hup = true;
                        break;
                    }
                    continue;
                }
                if (!proxy->alive) {
                    hup = true;
                    break;
                }
                /* If it's not a method it should be a share result */
                if (!parse_share(gdata, proxy, cs->buf)) {
                    LOGNOTICE("Proxy %d:%d unhandled stratum message: %s", proxy->id, proxy->subid, cs->buf);
                }
            }
            cksem_post(&cs->sem);
        }

        if (hup) {
            disable_subproxy(gdata, proxy->parent, proxy);
            continue;
        }
    }
    return NULL;
}

static void prepare_proxy(proxy_instance_t* proxi) {
    proxi->parent = proxi;
    mutex_init(&proxi->proxy_lock);
    add_subproxy(proxi, proxi);
    if (proxi->global)
        create_pthread(&proxi->pth_precv, proxy_recv, proxi);
}

static proxy_instance_t* wait_best_proxy(gdata_t* gdata) {
    proxy_instance_t *ret = NULL, *proxi, *tmp;
    int               retries = 0;

    while (42) {
        mutex_lock(&gdata->lock);
        HASH_ITER(hh, gdata->proxies, proxi, tmp) {
            if (proxi->disabled || !proxi->global)
                continue;
            if (proxi->alive || subproxies_alive(proxi)) {
                if (!ret || proxi->id < ret->id)
                    ret = proxi;
            }
        }
        mutex_unlock(&gdata->lock);

        if (ret)
            break;
        /* Send reject message if we are unable to find an active
         * proxy for more than 5 seconds */
        if (!((retries++) % 5))
            send_proc(ckpool.connector, "reject");
        sleep(1);
    }
    send_proc(ckpool.connector, ret ? "accept" : "reject");
    return ret;
}

static void send_list(gdata_t* gdata, const int sockd) {
    yyjson_mut_doc*   doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val *  val, *array_val, *root;
    proxy_instance_t *proxy, *tmp;

    array_val = yyjson_mut_arr(doc);

    mutex_lock(&gdata->lock);
    HASH_ITER(hh, gdata->proxies, proxy, tmp) {
        val = yyjson_mut_pack_val(
            doc, "{si,sb,si,ss,ss,sf,sb,sb,si}", "id", proxy->id, "global", proxy->global, "userid", proxy->userid,
            "auth", proxy->auth, "pass", proxy->pass, "diff", proxy->diff, "disabled", proxy->disabled, "alive",
            proxy->alive, "subproxies", proxy->subproxy_count);
        if (proxy->enonce1) {
            yyjson_mut_obj_add_strcpy(doc, val, "enonce1", proxy->enonce1);
            yyjson_mut_obj_add_int(doc, val, "nonce1len", proxy->nonce1len);
            yyjson_mut_obj_add_int(doc, val, "nonce2len", proxy->nonce2len);
        }
        yyjson_mut_arr_append(array_val, val);
    }
    mutex_unlock(&gdata->lock);

    root = yyjson_mut_pack_val(doc, "{so}", "proxies", array_val);
    yyjson_mut_doc_set_root(doc, root);
    send_api_yyresponse(doc, sockd);
}

static void send_sublist(gdata_t* gdata, const int sockd, const char* buf) {
    yyjson_mut_doc *  doc = yyjson_mut_doc_new(&ckyyalc), *res = NULL;
    yyjson_mut_val *  sval, *array_val, *root;
    proxy_instance_t *proxy, *subproxy, *tmp;
    yyjson_doc*       val = NULL;
    yyjson_read_err   err_val;
    int64_t           id;

    array_val = yyjson_mut_arr(doc);

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    if (unlikely(!yyjson_obj_get_int64(&id, yyjson_doc_get_root(val), "id"))) {
        res = yyjson_errormsg("Failed to get ID in send_sublist JSON: %s", buf);
        goto out;
    }
    proxy = proxy_by_id(gdata, id);
    if (unlikely(!proxy)) {
        res = yyjson_errormsg("Failed to find proxy %" PRId64 " in send_sublist", id);
        goto out;
    }

    mutex_lock(&gdata->lock);
    HASH_ITER(sh, proxy->subproxies, subproxy, tmp) {
        sval = yyjson_mut_pack_val(
            doc, "{si,ss,ss,sf,sb,sb}", "subid", subproxy->id, "auth", subproxy->auth, "pass", subproxy->pass, "diff",
            subproxy->diff, "disabled", subproxy->disabled, "alive", subproxy->alive);
        if (subproxy->enonce1) {
            yyjson_mut_obj_add_strcpy(doc, sval, "enonce1", subproxy->enonce1);
            yyjson_mut_obj_add_int(doc, sval, "nonce1len", subproxy->nonce1len);
            yyjson_mut_obj_add_int(doc, sval, "nonce2len", subproxy->nonce2len);
        }
        yyjson_mut_arr_append(array_val, sval);
    }
    mutex_unlock(&gdata->lock);

    root = yyjson_mut_pack_val(doc, "{so}", "subproxies", array_val);
    yyjson_mut_doc_set_root(doc, root);
    res = doc;
    doc = NULL;
out:
    if (doc)
        yyjson_mut_doc_free(doc);
    if (val)
        yyjson_doc_free(val);
    send_api_yyresponse(res, sockd);
}

static proxy_instance_t* __add_proxy(gdata_t* gdata, const int num);

static proxy_instance_t*
__add_userproxy(gdata_t* gdata, const int id, const int userid, char* url, char* auth, char* pass) {
    proxy_instance_t* proxy;

    gdata->proxies_generated++;
    proxy = ckzalloc(sizeof(proxy_instance_t));
    proxy->id = id;
    proxy->userid = userid;
    proxy->url = url;
    proxy->baseurl = strdup(url);
    proxy->auth = auth;
    proxy->pass = pass;
    cksem_init(&proxy->cs.sem);
    cksem_post(&proxy->cs.sem);
    HASH_ADD_INT(gdata->proxies, id, proxy);
    return proxy;
}

static void add_userproxy(gdata_t* gdata, const int userid, const char* url, const char* auth, const char* pass) {
    proxy_instance_t* proxy;
    char*             newurl = strdup(url);
    char*             newauth = strdup(auth);
    char*             newpass = strdup(pass ? pass : "");
    int               id;

    mutex_lock(&gdata->lock);
    id = ckpool.proxies++;
    proxy = __add_userproxy(gdata, id, userid, newurl, newauth, newpass);
    mutex_unlock(&gdata->lock);

    LOGWARNING("Adding non global user %s, %d proxy %d:%s", auth, userid, id, url);
    prepare_proxy(proxy);
}

static void parse_addproxy(gdata_t* gdata, const int sockd, const char* buf) {
    char *            url = NULL, *auth = NULL, *pass = NULL;
    yyjson_mut_doc*   res = NULL;
    proxy_instance_t* proxy;
    yyjson_val*       root = NULL;
    yyjson_doc*       val = NULL;
    yyjson_read_err   err_val;
    int               id, userid;
    bool              global;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    root = yyjson_doc_get_root(val);
    yyjson_obj_get_string(&url, root, "url");
    yyjson_obj_get_string(&auth, root, "auth");
    yyjson_obj_get_string(&pass, root, "pass");
    if (yyjson_obj_get_int(&userid, root, "userid"))
        global = false;
    else
        global = true;
    if (unlikely(!url || !auth || !pass)) {
        res = yyjson_errormsg("Failed to decode url/auth/pass in addproxy %s", buf);
        goto out;
    }

    mutex_lock(&gdata->lock);
    id = ckpool.proxies++;
    if (global) {
        ckpool.proxyurl = realloc(ckpool.proxyurl, sizeof(char**) * ckpool.proxies);
        ckpool.proxyauth = realloc(ckpool.proxyauth, sizeof(char**) * ckpool.proxies);
        ckpool.proxypass = realloc(ckpool.proxypass, sizeof(char**) * ckpool.proxies);
        ckpool.proxyurl[id] = url;
        ckpool.proxyauth[id] = auth;
        ckpool.proxypass[id] = pass;
        proxy = __add_proxy(gdata, id);
    } else
        proxy = __add_userproxy(gdata, id, userid, url, auth, pass);
    mutex_unlock(&gdata->lock);

    if (global)
        LOGNOTICE("Adding global proxy %d:%s", id, proxy->url);
    else
        LOGNOTICE("Adding user %d proxy %d:%s", userid, id, proxy->url);
    prepare_proxy(proxy);
    if (global) {
        res = yyjson_mut_pack("{si,ss,ss,ss}", "id", proxy->id, "url", url, "auth", auth, "pass", pass);
    } else {
        res = yyjson_mut_pack(
            "{si,ss,ss,ss,si}", "id", proxy->id, "url", url, "auth", auth, "pass", pass, "userid", proxy->userid);
    }
out:
    if (val)
        yyjson_doc_free(val);
    send_api_yyresponse(res, sockd);
}

static void delete_proxy(gdata_t* gdata, proxy_instance_t* proxy) {
    proxy_instance_t* subproxy;

    /* Remove the proxy from the master list first */
    mutex_lock(&gdata->lock);
    HASH_DEL(gdata->proxies, proxy);
    /* Disable all its threads */
    pthread_cancel(proxy->pth_precv);
    close_proxy_socket(proxy, proxy);
    mutex_unlock(&gdata->lock);

    /* Recycle all its subproxies */
    do {
        mutex_lock(&proxy->proxy_lock);
        subproxy = proxy->subproxies;
        if (subproxy)
            HASH_DELETE(sh, proxy->subproxies, subproxy);
        mutex_unlock(&proxy->proxy_lock);

        if (subproxy) {
            close_proxy_socket(proxy, subproxy);
            send_stratifier_delproxy(subproxy->id, subproxy->subid);
            if (proxy != subproxy)
                store_proxy(gdata, subproxy);
        }
    } while (subproxy);

    /* Recycle the proxy itself */
    store_proxy(gdata, proxy);
}

static void parse_delproxy(gdata_t* gdata, const int sockd, const char* buf) {
    yyjson_mut_doc*   res = NULL;
    proxy_instance_t* proxy;
    yyjson_doc*       val = NULL;
    yyjson_read_err   err_val;
    int               id = -1;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    yyjson_obj_get_int(&id, yyjson_doc_get_root(val), "id");
    proxy = proxy_by_id(gdata, id);
    if (!proxy) {
        res = yyjson_errormsg("Proxy id %d not found", id);
        goto out;
    }
    res = yyjson_mut_pack(
        "{si,ss,ss,ss,ss}", "id", proxy->id, "url", proxy->url, "baseurl", proxy->baseurl, "auth", proxy->auth, "pass",
        proxy->pass);

    LOGNOTICE("Deleting proxy %d:%s", proxy->id, proxy->url);
    delete_proxy(gdata, proxy);
out:
    if (val)
        yyjson_doc_free(val);
    send_api_yyresponse(res, sockd);
}

static void parse_ableproxy(gdata_t* gdata, const int sockd, const char* buf, bool disable) {
    yyjson_mut_doc*   res = NULL;
    proxy_instance_t* proxy;
    yyjson_doc*       val = NULL;
    yyjson_read_err   err_val;
    int               id = -1;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = yyjson_encode_errormsg(&err_val);
        goto out;
    }
    yyjson_obj_get_int(&id, yyjson_doc_get_root(val), "id");
    proxy = proxy_by_id(gdata, id);
    if (!proxy) {
        res = yyjson_errormsg("Proxy id %d not found", id);
        goto out;
    }
    res = yyjson_mut_pack(
        "{si,ss, ss,ss,ss}", "id", proxy->id, "url", proxy->url, "baseurl", proxy->baseurl, "auth", proxy->auth, "pass",
        proxy->pass);
    if (proxy->disabled != disable) {
        proxy->disabled = disable;
        LOGNOTICE("%sabling proxy %d:%s", disable ? "Dis" : "En", id, proxy->url);
    }
    if (disable) {
        /* Set disabled bool here in case this is a parent proxy */
        proxy->disabled = true;
        disable_subproxy(gdata, proxy, proxy);
    } else
        reconnect_proxy(proxy);
out:
    if (val)
        yyjson_doc_free(val);
    send_api_yyresponse(res, sockd);
}

static void send_stats(gdata_t* gdata, const int sockd) {
    yyjson_mut_doc*   doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val *  root = yyjson_mut_obj(doc), *subval;
    int               total_objects, objects;
    int64_t           generated, memsize;
    proxy_instance_t* proxy;
    stratum_msg_t*    msg;

    yyjson_mut_doc_set_root(doc, root);

    mutex_lock(&gdata->lock);
    objects = HASH_COUNT(gdata->proxies);
    memsize = SAFE_HASH_OVERHEAD(gdata->proxies) + sizeof(proxy_instance_t) * objects;
    generated = gdata->proxies_generated;
    subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "proxies", subval);

    DL_COUNT(gdata->dead_proxies, proxy, objects);
    memsize = sizeof(proxy_instance_t) * objects;
    subval = yyjson_mut_pack_val(doc, "{si,sI}", "count", objects, "memory", memsize);
    yyjson_mut_obj_add_val(doc, root, "dead_proxies", subval);

    total_objects = memsize = 0;
    for (proxy = gdata->proxies; proxy; proxy = proxy->hh.next) {
        mutex_lock(&proxy->proxy_lock);
        total_objects += objects = HASH_COUNT(proxy->subproxies);
        memsize += SAFE_HASH_OVERHEAD(proxy->subproxies) + sizeof(proxy_instance_t) * objects;
        mutex_unlock(&proxy->proxy_lock);
    }
    generated = gdata->subproxies_generated;
    mutex_unlock(&gdata->lock);

    subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", total_objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "subproxies", subval);

    mutex_lock(&gdata->notify_lock);
    objects = HASH_COUNT(gdata->notify_instances);
    memsize = SAFE_HASH_OVERHEAD(gdata->notify_instances) + sizeof(notify_instance_t) * objects;
    generated = gdata->proxy_notify_id;
    mutex_unlock(&gdata->notify_lock);

    subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "notifies", subval);

    mutex_lock(&gdata->share_lock);
    objects = HASH_COUNT(gdata->shares);
    memsize = SAFE_HASH_OVERHEAD(gdata->shares) + sizeof(share_msg_t) * objects;
    generated = gdata->share_id;
    mutex_unlock(&gdata->share_lock);

    subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "shares", subval);

    mutex_lock(&gdata->psend_lock);
    DL_COUNT(gdata->psends, msg, objects);
    generated = gdata->psends_generated;
    mutex_unlock(&gdata->psend_lock);

    memsize = sizeof(stratum_msg_t) * objects;
    subval = yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "psends", subval);

    send_api_yyresponse(doc, sockd);
}

/* Entered with parent proxy locked */
static yyjson_mut_val*
__proxystats(yyjson_mut_doc* doc, proxy_instance_t* proxy, proxy_instance_t* parent, bool discrete) {
    yyjson_mut_val* val = yyjson_mut_obj(doc);

    /* Opportunity to update hashrate just before we report it without
     * needing to check on idle proxies regularly */
    __decay_proxy(proxy, parent, 0);

    yyjson_mut_obj_add_int(doc, val, "id", proxy->id);
    yyjson_mut_obj_add_int(doc, val, "userid", proxy->userid);
    yyjson_mut_obj_add_strcpy(doc, val, "baseurl", proxy->baseurl);
    yyjson_mut_obj_add_strcpy(doc, val, "url", proxy->url);
    yyjson_mut_obj_add_strcpy(doc, val, "auth", proxy->auth);
    yyjson_mut_obj_add_strcpy(doc, val, "pass", proxy->pass);
    yyjson_mut_obj_add_strcpy(doc, val, "enonce1", proxy->enonce1 ? proxy->enonce1 : "");
    yyjson_mut_obj_add_int(doc, val, "nonce1len", proxy->nonce1len);
    yyjson_mut_obj_add_int(doc, val, "nonce2len", proxy->nonce2len);
    yyjson_mut_obj_add_real(doc, val, "diff", proxy->diff);
#ifdef HAVE_SV2
    yyjson_mut_obj_add_strcpy(doc, val, "protocol", proxy->sv2 ? "sv2" : "sv1");
    if (proxy->sv2 && proxy->sv2p) {
        struct sv2_proxy* sp = proxy->sv2p;
        int               pending;

        mutex_lock(&sp->send_lock);
        pending = HASH_COUNT(sp->pending);
        mutex_unlock(&sp->send_lock);
        yyjson_mut_obj_add_int(doc, val, "sv2_channel", sp->channel_id);
        yyjson_mut_obj_add_int(doc, val, "sv2_extranonce_size", sp->extranonce_size);
        yyjson_mut_obj_add_int(doc, val, "sv2_usable_extranonce", sp->usable_extranonce);
        yyjson_mut_obj_add_int(doc, val, "sv2_submitted", sp->next_seq);
        yyjson_mut_obj_add_int(doc, val, "sv2_pending", pending);
    }
#endif
    if (parent_proxy(proxy)) {
        yyjson_mut_obj_add_real(doc, val, "total_accepted", proxy->total_accepted);
        yyjson_mut_obj_add_real(doc, val, "total_rejected", proxy->total_rejected);
        yyjson_mut_obj_add_int(doc, val, "subproxies", proxy->subproxy_count);
        yyjson_mut_obj_add_real(doc, val, "tdsps1", proxy->tdsps1);
        yyjson_mut_obj_add_real(doc, val, "tdsps5", proxy->tdsps5);
        yyjson_mut_obj_add_real(doc, val, "tdsps60", proxy->tdsps60);
        yyjson_mut_obj_add_real(doc, val, "tdsps1440", proxy->tdsps1440);
    }
    if (discrete) {
        yyjson_mut_obj_add_real(doc, val, "dsps1", proxy->dsps1);
        yyjson_mut_obj_add_real(doc, val, "dsps5", proxy->dsps5);
        yyjson_mut_obj_add_real(doc, val, "dsps60", proxy->dsps60);
        yyjson_mut_obj_add_real(doc, val, "dsps1440", proxy->dsps1440);
        yyjson_mut_obj_add_real(doc, val, "accepted", proxy->diff_accepted);
        yyjson_mut_obj_add_real(doc, val, "rejected", proxy->diff_rejected);
    }
    yyjson_mut_obj_add_strcpy(doc, val, "connect", proxy_status[parent->connect_status]);
    yyjson_mut_obj_add_strcpy(doc, val, "subscribe", proxy_status[parent->subscribe_status]);
    yyjson_mut_obj_add_strcpy(doc, val, "authorise", proxy_status[parent->auth_status]);
    yyjson_mut_obj_add_int(doc, val, "backoff", parent->backoff);
    yyjson_mut_obj_add_int(doc, val, "lastshare", proxy->last_share.tv_sec);
    yyjson_mut_obj_add_bool(doc, val, "global", proxy->global);
    yyjson_mut_obj_add_bool(doc, val, "disabled", proxy->disabled);
    yyjson_mut_obj_add_bool(doc, val, "alive", proxy->alive);
    yyjson_mut_obj_add_int(doc, val, "maxclients", proxy->clients_per_proxy);

    return val;
}

static yyjson_mut_val* proxystats(yyjson_mut_doc* doc, proxy_instance_t* proxy, bool discrete) {
    proxy_instance_t* parent = proxy->parent;
    yyjson_mut_val*   val;

    mutex_lock(&parent->proxy_lock);
    val = __proxystats(doc, proxy, parent, discrete);
    mutex_unlock(&parent->proxy_lock);

    return val;
}

static yyjson_mut_doc* all_proxystats(gdata_t* gdata) {
    yyjson_mut_doc*   doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val *  root, *arr_val = yyjson_mut_arr(doc);
    proxy_instance_t *proxy, *tmp;

    mutex_lock(&gdata->lock);
    HASH_ITER(hh, gdata->proxies, proxy, tmp) {
        mutex_unlock(&gdata->lock);
        yyjson_mut_arr_append(arr_val, proxystats(doc, proxy, false));
        mutex_lock(&gdata->lock);
    }
    mutex_unlock(&gdata->lock);

    root = yyjson_mut_pack_val(doc, "{so}", "proxy", arr_val);
    yyjson_mut_doc_set_root(doc, root);
    return doc;
}

static void parse_proxystats(gdata_t* gdata, const int sockd, const char* buf) {
    yyjson_mut_doc*   res = NULL;
    proxy_instance_t* proxy;
    yyjson_val*       root = NULL;
    yyjson_doc*       val = NULL;
    yyjson_read_err   err_val;
    bool              totals = false;
    int               id, subid = 0;

    val = yyjson_read_opts((char*)buf, strlen(buf), 0, NULL, &err_val);
    if (unlikely(!val)) {
        res = all_proxystats(gdata);
        goto out_noval;
    }
    root = yyjson_doc_get_root(val);
    if (!yyjson_obj_get_int(&id, root, "id")) {
        res = all_proxystats(gdata);
        goto out;
    }
    if (!yyjson_obj_get_int(&subid, root, "subid"))
        totals = true;
    proxy = proxy_by_id(gdata, id);
    if (!proxy) {
        res = yyjson_errormsg("Proxy id %d not found", id);
        goto out;
    }
    if (!totals)
        proxy = subproxy_by_id(proxy, subid);
    if (!proxy) {
        res = yyjson_errormsg("Proxy id %d:%d not found", id, subid);
        goto out;
    }
    res = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_doc_set_root(res, proxystats(res, proxy, true));
out:
    yyjson_doc_free(val);
out_noval:
    send_api_yyresponse(res, sockd);
}

static void send_subproxystats(gdata_t* gdata, const int sockd) {
    yyjson_mut_doc*   doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val *  root, *arr_val = yyjson_mut_arr(doc);
    proxy_instance_t *parent, *tmp;

    mutex_lock(&gdata->lock);
    HASH_ITER(hh, gdata->proxies, parent, tmp) {
        yyjson_mut_val *  val, *subarr_val = yyjson_mut_arr(doc);
        proxy_instance_t *subproxy, *subtmp;

        mutex_unlock(&gdata->lock);

        mutex_lock(&parent->proxy_lock);
        HASH_ITER(sh, parent->subproxies, subproxy, subtmp) {
            val = __proxystats(doc, subproxy, parent, true);
            yyjson_mut_obj_add_int(doc, val, "subid", subproxy->subid);
            yyjson_mut_arr_append(subarr_val, val);
        }
        mutex_unlock(&parent->proxy_lock);

        val = yyjson_mut_pack_val(doc, "{si,so}", "id", parent->id, "subproxy", subarr_val);
        yyjson_mut_arr_append(arr_val, val);
        mutex_lock(&gdata->lock);
    }
    mutex_unlock(&gdata->lock);

    root = yyjson_mut_pack_val(doc, "{so}", "proxy", arr_val);
    yyjson_mut_doc_set_root(doc, root);
    send_api_yyresponse(doc, sockd);
}

static void parse_globaluser(gdata_t* gdata, const char* buf) {
    char *            url, *username, *pass = strdupa(buf);
    int               userid = -1, proxyid = -1;
    proxy_instance_t *proxy, *tmp;
    int64_t           clientid = -1;
    bool              found = false;

    sscanf(buf, "%d:%d:%" PRId64 ":%s", &proxyid, &userid, &clientid, pass);
    if (unlikely(clientid < 0 || userid < 0 || proxyid < 0)) {
        LOGWARNING("Failed to parse_globaluser ids from command %s", buf);
        return;
    }
    username = strsep(&pass, ",");
    if (unlikely(!username)) {
        LOGWARNING("Failed to parse_globaluser username from command %s", buf);
        return;
    }

    LOGDEBUG(
        "Checking userproxy proxy %d user %d:%" PRId64 " worker %s pass %s", proxyid, userid, clientid, username, pass);

    if (unlikely(proxyid >= ckpool.proxies)) {
        LOGWARNING("Trying to find non-existent proxy id %d in parse_globaluser", proxyid);
        return;
    }

    mutex_lock(&gdata->lock);
    url = ckpool.proxyurl[proxyid];
    HASH_ITER(hh, gdata->proxies, proxy, tmp) {
        if (!strcmp(proxy->auth, username)) {
            found = true;
            break;
        }
    }
    mutex_unlock(&gdata->lock);

    if (found)
        return;
    add_userproxy(gdata, userid, url, username, pass);
}

static void proxy_loop(proc_instance_t* pi) {
    proxy_instance_t * proxi = NULL, *cproxy;
    server_instance_t *si = NULL, *old_si;
    gdata_t*           gdata = ckpool.gdata;
    unix_msg_t*        umsg = NULL;
    connsock_t*        cs = NULL;
    char*              buf = NULL;

reconnect:
    clear_unix_msg(&umsg);

    if (ckpool.node) {
        old_si = si;
        si = live_server(gdata);
        if (!si)
            goto out;
        cs = &si->cs;
        if (!old_si)
            LOGWARNING("Connected to bitcoind: %s:%s", cs->url, cs->port);
        else if (si != old_si)
            LOGWARNING("Failed over to bitcoind: %s:%s", cs->url, cs->port);
    }

    /* This does not necessarily mean we reconnect, but a change has
     * occurred and we need to reexamine the proxies. */
    cproxy = wait_best_proxy(gdata);
    if (!cproxy)
        goto out;
    if (proxi != cproxy) {
        gdata->current_proxy = proxi = cproxy;
        LOGWARNING(
            "Successfully connected to pool %d %s as proxy%s", proxi->id, proxi->url,
            ckpool.passthrough ? " in passthrough mode" : "");
    }

    if (unlikely(!ckpool.generator_ready)) {
        ckpool.generator_ready = true;
        LOGWARNING("%s generator ready", ckpool.name);
    }
retry:
    clear_unix_msg(&umsg);
    do {
        umsg = get_unix_msg(pi);
    } while (!umsg);

    buf = umsg->buf;
    LOGDEBUG("Proxy received request: %s", buf);
    if (cmdmatch(buf, "stats")) {
        send_stats(gdata, umsg->sockd);
    } else if (cmdmatch(buf, "list")) {
        send_list(gdata, umsg->sockd);
    } else if (cmdmatch(buf, "sublist")) {
        send_sublist(gdata, umsg->sockd, buf + 8);
    } else if (cmdmatch(buf, "addproxy")) {
        parse_addproxy(gdata, umsg->sockd, buf + 9);
    } else if (cmdmatch(buf, "delproxy")) {
        parse_delproxy(gdata, umsg->sockd, buf + 9);
    } else if (cmdmatch(buf, "enableproxy")) {
        parse_ableproxy(gdata, umsg->sockd, buf + 12, false);
    } else if (cmdmatch(buf, "disableproxy")) {
        parse_ableproxy(gdata, umsg->sockd, buf + 13, true);
    } else if (cmdmatch(buf, "proxystats")) {
        parse_proxystats(gdata, umsg->sockd, buf + 11);
    } else if (cmdmatch(buf, "subproxystats")) {
        send_subproxystats(gdata, umsg->sockd);
    } else if (cmdmatch(buf, "globaluser")) {
        parse_globaluser(gdata, buf + 11);
    } else if (cmdmatch(buf, "reconnect")) {
        goto reconnect;
    } else if (cmdmatch(buf, "submitblock:")) {
        char blockmsg[80];
        bool ret;

        /* cmdmatch only checks the prefix, so a short message would
         * over-read and write the memset below out of bounds. */
        if (unlikely(strlen(buf) < 12 + 64 + 1)) {
            LOGWARNING("Got too short submitblock message");
            goto retry;
        }
        LOGNOTICE("Submitting likely block solve share from upstream pool");
        ret = submit_block(cs, buf + 12 + 64 + 1);
        memset(buf + 12 + 64, 0, 1);
        sprintf(blockmsg, "%sblock:%s", ret ? "" : "no", buf + 12);
        send_proc(ckpool.stratifier, blockmsg);
    } else if (cmdmatch(buf, "submittxn:")) {
        if (unlikely(strlen(buf) < 11)) {
            LOGWARNING("Got zero length submittxn");
            goto retry;
        }
        submit_txn(cs, buf + 10);
    } else if (cmdmatch(buf, "loglevel")) {
        sscanf(buf, "loglevel=%d", &ckpool.loglevel);
    } else if (cmdmatch(buf, "ping")) {
        LOGDEBUG("Proxy received ping request");
        send_unix_msg(umsg->sockd, "pong");
    } else if (cmdmatch(buf, "recruit")) {
        recruit_subproxy(gdata, buf);
    } else if (cmdmatch(buf, "dropproxy")) {
        drop_proxy(gdata, buf);
    } else {
        LOGWARNING("Generator received unrecognised message: %s", buf);
    }
    goto retry;
out:
    return;
}

/* Check which servers are alive, maintaining a connection with them and
 * reconnect if a higher priority one is available. */
static void* server_watchdog(void __maybe_unused* arg) {
    gdata_t* gdata = ckpool.gdata;

    rename_proc("swatchdog");

    pthread_detach(pthread_self());

    while (42) {
        server_instance_t* best = NULL;
        ts_t               timer_t;
        int                i;

        cksleep_prepare_r(&timer_t);
        for (i = 0; i < ckpool.btcds; i++) {
            server_instance_t* si = ckpool.servers[i];

            /* Have we reached the current server? */
            if (server_alive(si, true) && !best)
                best = si;
        }
        if (best && best != gdata->current_si)
            send_proc(ckpool.generator, "reconnect");
        cksleep_ms_r(&timer_t, 5000);
    }
    return NULL;
}

static void setup_servers(void) {
    pthread_t pth_watchdog;
    int       i;

    ckpool.servers = ckalloc(sizeof(server_instance_t*) * ckpool.btcds);
    for (i = 0; i < ckpool.btcds; i++) {
        server_instance_t* si;
        connsock_t*        cs;

        ckpool.servers[i] = ckzalloc(sizeof(server_instance_t));
        si = ckpool.servers[i];
        si->url = ckpool.btcdurl[i];
        si->auth = ckpool.btcdauth[i];
        si->pass = ckpool.btcdpass[i];
        si->notify = ckpool.btcdnotify[i];
        si->id = i;
        cs = &si->cs;
        cksem_init(&cs->sem);
        cksem_post(&cs->sem);
    }

    create_pthread(&pth_watchdog, server_watchdog, NULL);
}

static void server_mode(proc_instance_t* pi) {
    int i;

    setup_servers();

    gen_loop(pi);

    for (i = 0; i < ckpool.btcds; i++) {
        server_instance_t* si = ckpool.servers[i];

        kill_server(si);
        dealloc(si);
    }
    dealloc(ckpool.servers);
}

static proxy_instance_t* __add_proxy(gdata_t* gdata, const int id) {
    proxy_instance_t* proxy;

    gdata->proxies_generated++;
    proxy = ckzalloc(sizeof(proxy_instance_t));
    proxy->id = id;
    proxy->url = strdup(ckpool.proxyurl[id]);
    proxy->baseurl = strdup(proxy->url);
    proxy->auth = strdup(ckpool.proxyauth[id]);
    if (ckpool.proxypass[id])
        proxy->pass = strdup(ckpool.proxypass[id]);
    else
        proxy->pass = strdup("");
#ifdef HAVE_SV2
    /* Refuse an SV2 upstream whose authority key is absent/invalid rather
     * than connecting unauthenticated. */
    if (!proxy_parse_sv2_url(proxy)) {
        LOGWARNING("Disabling proxy %d: invalid SV2 upstream URL %s", id, proxy->url);
        proxy->disabled = true;
    } else if (!proxy->sv2 && ckpool.proxyjds && ckpool.proxyjds[id]) {
        /*
         * Whether a url is SV2 is settled by the authority key in its path,
         * which validate_jd_config() cannot check: at config time a keyless
         * "host:port" is indistinguishable from a deliberately SV1 entry.
         * Job declaration is negotiated on the mining connection, so this
         * entry will connect and mine and silently never declare anything.
         * Say so where it is finally known, and loudly enough to be heard.
         */
        LOGWARNING(
            "Proxy %d has a jds entry but its url %s carries no SV2 authority "
            "key — mining only, no job declaration to this pool",
            id, proxy->url);
    }
#endif
    HASH_ADD_INT(gdata->proxies, id, proxy);
    proxy->global = true;
    cksem_init(&proxy->cs.sem);
    cksem_post(&proxy->cs.sem);
    return proxy;
}

static void proxy_mode(proc_instance_t* pi) {
    gdata_t*          gdata = ckpool.gdata;
    proxy_instance_t* proxy;
    int               i;

    mutex_init(&gdata->lock);
    mutex_init(&gdata->notify_lock);
    mutex_init(&gdata->share_lock);

    if (ckpool.node)
        setup_servers();

#ifdef HAVE_SV2
    /* Local template provider for SV2 job declaration. Started here, not in
     * the stratifier's pool-only IPC block, because the generator owns
     * upstream state and the work-source arbiter. */
    sv2_jdc_start();
#endif

    /* Create all our proxy structures and pointers */
    for (i = 0; i < ckpool.proxies; i++) {
        proxy = __add_proxy(gdata, i);
        if (ckpool.passthrough) {
            create_pthread(&proxy->pth_precv, passthrough_recv, proxy);
            proxy->passsends = create_ckmsgq("passsend", &passthrough_send);
        } else {
            mutex_init(&gdata->psend_lock);
            cond_init(&gdata->psend_cond);
            prepare_proxy(proxy);
            create_pthread(&gdata->pth_uprecv, userproxy_recv, NULL);
            create_pthread(&gdata->pth_psend, proxy_send, NULL);
        }
    }

    proxy_loop(pi);
}

void* generator(void* arg) {
    proc_instance_t* pi = (proc_instance_t*)arg;
    gdata_t*         gdata;

    rename_proc(pi->processname);
    LOGWARNING("%s generator starting", ckpool.name);
    gdata = ckzalloc(sizeof(gdata_t));
    ckpool.gdata = gdata;

    if (ckpool.proxy) {
        /* Wait for the stratifier to be ready for us */
        while (!ckpool.stratifier_ready)
            cksleep_ms(10);
        proxy_mode(pi);
    } else
        server_mode(pi);
    /* We should never get here unless there's a fatal error */
    LOGEMERG("Generator failure, shutting down");
    exit(1);
    return NULL;
}
