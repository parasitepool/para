/*
 * Copyright 2014-2017,2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#include "config.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <string.h>
#include <unistd.h>

#include "ckpool.h"
#include "libckpool.h"
#include "uthash.h"
#include "utlist.h"
#include "stratifier.h"
#include "generator.h"
#ifdef HAVE_SV2
#include "sv2_conn.h"
#include "sv2_strat.h"
#include "sv2_jd.h"
#endif

#define MAX_MSGSIZE 1024
/* Trusted remote / node peers may send larger JSON than a miner, but must
 * still have a finite ceiling so a compromised peer cannot grow the recv
 * buffer without bound. */
#define MAX_REMOTE_MSGSIZE (16 * 1024 * 1024)

typedef struct client_instance client_instance_t;
typedef struct sender_send     sender_send_t;
typedef struct share           share_t;
typedef struct redirect        redirect_t;

struct client_instance {
    /* For clients hashtable */
    UT_hash_handle hh;
    int64_t        id;

    /* fd cannot be changed while a ref is held */
    int fd;

    /* Reference count for when this instance is used outside of the
     * connector_data lock */
    int ref;

    /* Have we disabled this client to be removed when there are no refs? */
    bool invalid;

    /* For dead_clients list */
    client_instance_t* dead_next;
    client_instance_t* dead_prev;

    client_instance_t* recycled_next;
    client_instance_t* recycled_prev;

    struct sockaddr_storage address_storage;
    struct sockaddr*        address;
    char                    address_name[INET6_ADDRSTRLEN];

    /* Which serverurl is this instance connected to */
    int server;

    char*         buf;
    unsigned long bufofs;

    /* Are we currently sending a blocked message from this client */
    sender_send_t* sending;

    /* Is this a trusted remote server */
    bool remote;

    /* Is this the parent passthrough client */
    bool passthrough;

    /* Linked list of shares in redirector mode.*/
    share_t* shares;

    /* Has this client already been told to redirect */
    bool redirected;
    /* Has this client been authorised in redirector mode */
    bool authorised;

    /* Time this client started blocking, 0 when not blocked */
    time_t blocked_time;

    /* Time this client was accepted, and whether it has ever delivered a
     * complete message. A stratum_instance_t is only created once a whole
     * message reaches the stratifier, so a socket that connects and stays
     * silent is invisible to the stratifier's unauthorised client reaper
     * while still occupying an fd and a maxclients slot. Only the connector
     * can see these, so they are reaped here. */
    time_t accept_time;
    bool   got_msg;

    /* The size of the socket send buffer */
    int sendbufsize;

    /* Bytes currently queued but not yet written to this client. Only
     * modified under the owning csender's lock. */
    int64_t queued_bytes;

#ifdef HAVE_SV2
    /* True if accepted on an SV2 listen socket (binary Noise stream). */
    bool sv2;
    /* True if this SV2 socket is Job Declaration (not Mining). */
    bool sv2_jd;
    /* Per-connection Noise + reassembly (connector-owned). */
    struct sv2_conn* sv2c;
#endif
};

struct sender_send {
    struct sender_send* next;
    struct sender_send* prev;

    client_instance_t* client;
    char*              buf;
    int                len;
    int                ofs;
    /* Length accounted against the client's queued_bytes at queue time.
     * len is consumed by partial writes and rewritten by SV2 encryption so
     * it cannot be used to reverse the accounting. */
    int qlen;
#ifdef HAVE_SV2
    /* If true, buf is a plaintext SV2 frame; encrypt once on the owning
     * sender-shard thread before the first write. */
    bool sv2_encrypt;
#endif
};

struct share {
    share_t* next;
    share_t* prev;

    time_t  submitted;
    int64_t id;
};

struct redirect {
    UT_hash_handle hh;
    char           address_name[INET6_ADDRSTRLEN];
    int            id;
    int            redirect_no;
};

typedef struct csender csender_t;

/* Private data for the connector */
struct connector_data {
    cklock_t         lock;
    proc_instance_t* pi;

    time_t start_time;

    /* Array of server fds */
    int* serverfd;
    /* All time count of clients connected */
    int nfds;
    /* The epoll fd */
    int epfd;

    bool      accept;
    pthread_t pth_receiver;

    /* For the hashtable of all clients */
    client_instance_t* clients;
    /* Linked list of dead clients no longer in use but may still have references */
    client_instance_t* dead_clients;
    /* Linked list of client structures we can reuse */
    client_instance_t* recycled_clients;

    int clients_generated;
    int dead_generated;

    int64_t client_ids;

    /* client yyjson message process queue */
    ckmsgq_t* cympq;

    /* client message event process queue */
    ckmsgq_t* cevents;

    /* Array of nsenders independent sender shards. Each shard is its own
     * thread with its own pending-send queue, handling the disjoint set of
     * clients whose id modulo nsenders equals the shard index, so per-client
     * send state needs no locking. */
    int        nsenders;
    csender_t* csenders;

    /* Hash list of all redirected IP address in redirector mode */
    redirect_t* redirects;
    /* What redirect we're currently up to */
    int redirect;

    /* Pending sends to the upstream server */
    ckmsgq_t*  upstream_sends;
    connsock_t upstream_cs;

    /* Have we given the warning about inability to raise sendbuf size */
    bool wmem_warn;
};

typedef struct connector_data cdata_t;

/* One shard of the sender: an independent thread with its own pending-send
 * queue and lock/cond, handling clients whose id modulo cdata->nsenders equals
 * this shard's index. */
struct csender {
    cdata_t* cdata;
    int      id;

    /* Linked list of pending sends for this shard */
    sender_send_t* sends;

    int64_t sends_generated;
    int64_t sends_delayed;
    int64_t sends_queued;
    int64_t sends_size;

    mutex_t        lock;
    pthread_cond_t cond;
    pthread_t      pth;
};

void connector_upstream_msg(char* msg) {
    cdata_t* cdata = ckpool.cdata;

    LOGDEBUG("Upstreaming %s", msg);
    ckmsgq_add(cdata->upstream_sends, msg);
}

/* Increase the reference count of instance */
static void __inc_instance_ref(client_instance_t* client) {
    client->ref++;
}

static void inc_instance_ref(cdata_t* cdata, client_instance_t* client) {
    ck_wlock(&cdata->lock);
    __inc_instance_ref(client);
    ck_wunlock(&cdata->lock);
}

/* Increase the reference count of instance */
static void __dec_instance_ref(client_instance_t* client) {
    client->ref--;
}

static void dec_instance_ref(cdata_t* cdata, client_instance_t* client) {
    ck_wlock(&cdata->lock);
    __dec_instance_ref(client);
    ck_wunlock(&cdata->lock);
}

/* Recruit a client structure from a recycled one if available, creating a
 * new structure only if we have none to reuse. */
static client_instance_t* recruit_client(cdata_t* cdata) {
    client_instance_t* client = NULL;

    ck_wlock(&cdata->lock);
    if (cdata->recycled_clients) {
        client = cdata->recycled_clients;
        DL_DELETE2(cdata->recycled_clients, client, recycled_prev, recycled_next);
    } else
        cdata->clients_generated++;
    ck_wunlock(&cdata->lock);

    if (!client) {
        LOGDEBUG("Connector created new client instance");
        client = ckzalloc(sizeof(client_instance_t));
    } else
        LOGDEBUG("Connector recycled client instance");

    client->buf = ckzalloc(PAGESIZE);

    return client;
}

static void __recycle_client(cdata_t* cdata, client_instance_t* client) {
    share_t *share, *tmp;

    dealloc(client->buf);
    DL_FOREACH_SAFE(client->shares, share, tmp)
    dealloc(share);
#ifdef HAVE_SV2
    if (client->sv2c) {
        sv2_conn_free(client->sv2c);
        client->sv2c = NULL;
    }
#endif

    memset(client, 0, sizeof(client_instance_t));
    client->id = -1;
    DL_APPEND2(cdata->recycled_clients, client, recycled_prev, recycled_next);
}

static void recycle_client(cdata_t* cdata, client_instance_t* client) {
    ck_wlock(&cdata->lock);
    __recycle_client(cdata, client);
    ck_wunlock(&cdata->lock);
}

/* Allows the stratifier to get a unique local virtualid for subclients */
int64_t connector_newclientid(void) {
    int64_t ret;

    cdata_t* cdata = ckpool.cdata;

    ck_wlock(&cdata->lock);
    ret = cdata->client_ids++;
    ck_wunlock(&cdata->lock);

    return ret;
}

/* Accepts incoming connections on the server socket and generates client
 * instances */
static int accept_client(cdata_t* cdata, const int epfd, const uint64_t server) {
    int                fd, port, no_clients, sockd;
    client_instance_t* client;
    struct epoll_event event;
    socklen_t          address_len;
    socklen_t          optlen;

    ck_rlock(&cdata->lock);
    no_clients = HASH_COUNT(cdata->clients);
    ck_runlock(&cdata->lock);

    sockd = cdata->serverfd[server];

    if (unlikely(ckpool.maxclients && no_clients >= ckpool.maxclients)) {
        static time_t last_full_warn = 0;
        static int    full_suppressed = 0;
        time_t        now_t = time(NULL);

        /* The listen socket is level triggered so leaving the pending
         * connection unaccepted makes epoll_wait return immediately
         * forever, spinning a core and flooding the log. Accept and
         * close it instead so the backlog drains, and rate limit the
         * warning. */
        fd = accept(sockd, NULL, NULL);
        if (likely(fd >= 0))
            Close(fd);
        if (now_t - last_full_warn >= 60) {
            if (full_suppressed) {
                LOGWARNING("Server full with %d clients (%d further connections dropped)", no_clients, full_suppressed);
            } else
                LOGWARNING("Server full with %d clients", no_clients);
            last_full_warn = now_t;
            full_suppressed = 0;
        } else
            full_suppressed++;
        return 0;
    }

    client = recruit_client(cdata);
    client->server = server;
    client->address = (struct sockaddr*)&client->address_storage;
    address_len = sizeof(client->address_storage);
    fd = accept(sockd, client->address, &address_len);
    if (unlikely(fd < 0)) {
        /* Handle these errors gracefully should we ever share this
         * socket */
        if (errno == EAGAIN || errno == EWOULDBLOCK || errno == ECONNABORTED) {
            LOGNOTICE("Recoverable error on accept in accept_client");
            recycle_client(cdata, client);
            return 0;
        }
        LOGERR("Failed to accept on socket %d in acceptor", sockd);
        recycle_client(cdata, client);
        return -1;
    }

    switch (client->address->sa_family) {
        const struct sockaddr_in*  inet4_in;
        const struct sockaddr_in6* inet6_in;

        case AF_INET:
            inet4_in = (struct sockaddr_in*)client->address;
            inet_ntop(AF_INET, &inet4_in->sin_addr, client->address_name, INET6_ADDRSTRLEN);
            port = htons(inet4_in->sin_port);
            break;
        case AF_INET6:
            inet6_in = (struct sockaddr_in6*)client->address;
            inet_ntop(AF_INET6, &inet6_in->sin6_addr, client->address_name, INET6_ADDRSTRLEN);
            port = htons(inet6_in->sin6_port);
            break;
        default:
            LOGWARNING("Unknown INET type for client %d on socket %d", cdata->nfds, fd);
            Close(fd);
            recycle_client(cdata, client);
            return 0;
    }

    keep_sockalive(fd);
    noblock_socket(fd);

    LOGINFO(
        "Connected new client %d on socket %d to %d active clients from %s:%d", cdata->nfds, fd, no_clients,
        client->address_name, port);

#ifdef HAVE_SV2
    if (ckpool.server_sv2 && server < (uint64_t)ckpool.serverurls && ckpool.server_sv2[server]) {
        /*
         * Reserve HS slot before keys/ECDH so rate limits cannot be
         * TOCTOUed past and expensive work is not free under flood.
         * Release on any failure path before hs_inflight is set.
         */
        if (!sv2_handshake_try_reserve(client->address_name)) {
            LOGNOTICE("SV2 handshake rate-limited from %s", client->address_name);
            Close(fd);
            recycle_client(cdata, client);
            return 0;
        }
        if (!sv2_get_server_keys()) {
            LOGERR("SV2 client rejected: server keys not initialised");
            sv2_handshake_release();
            Close(fd);
            recycle_client(cdata, client);
            return 0;
        }
        client->sv2 = true;
        client->sv2_jd = ckpool.server_sv2_jd && server < (uint64_t)ckpool.serverurls && ckpool.server_sv2_jd[server];
        client->sv2c = sv2_conn_new(sv2_get_server_keys());
        if (!client->sv2c) {
            LOGERR("SV2 conn setup failed for %s", client->address_name);
            sv2_handshake_release();
            Close(fd);
            recycle_client(cdata, client);
            return 0;
        }
        /*
         * JD: raise reassembly to JD policy payload + AEAD/header headroom.
         * Mining keeps SV2_MAX_MINING_PAYLOAD-scale default from sv2_conn_new
         * (not full U24) so a post-Noise peer cannot pin tens of MiB.
         */
        if (client->sv2_jd)
            sv2_conn_set_rx_max(client->sv2c, sv2_rx_max_for_payload(SV2_MAX_JD_PAYLOAD));
        /* Slot owned by this conn until handshake complete or free */
        client->sv2c->hs_inflight = true;
        LOGNOTICE("SV2 %s client accepted from %s", client->sv2_jd ? "JD" : "mining", client->address_name);
    }
#endif

    ck_wlock(&cdata->lock);
    client->id = cdata->client_ids++;
    HASH_ADD_I64(cdata->clients, id, client);
    cdata->nfds++;
    ck_wunlock(&cdata->lock);

#ifdef HAVE_SV2
    if (client->sv2) {
        if (client->sv2_jd)
            sv2_jd_note_client(client->id, client->address_name, (int)server);
        else
            sv2_strat_note_client(client->id, client->address_name, (int)server);
    }
#endif

    /* We increase the ref count on this client as epoll creates a pointer
     * to it. We drop that reference when the socket is closed which
     * removes it automatically from the epoll list. */
    __inc_instance_ref(client);
    client->fd = fd;
    client->accept_time = time(NULL);
    optlen = sizeof(client->sendbufsize);
    getsockopt(fd, SOL_SOCKET, SO_RCVBUF, &client->sendbufsize, &optlen);
    LOGDEBUG("Client sendbufsize detected as %d", client->sendbufsize);

    event.data.u64 = client->id;
    event.events = EPOLLIN | EPOLLRDHUP | EPOLLONESHOT;
    if (unlikely(epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &event) < 0)) {
        LOGERR("Failed to epoll_ctl add in accept_client");
        dec_instance_ref(cdata, client);
        return 0;
    }

    return 1;
}

static int __drop_client(cdata_t* cdata, client_instance_t* client) {
    int ret = -1;

    if (client->invalid)
        goto out;
    client->invalid = true;
    ret = client->fd;
    /* Closing the fd will automatically remove it from the epoll list */
    Close(client->fd);
    HASH_DEL(cdata->clients, client);
    DL_APPEND2(cdata->dead_clients, client, dead_prev, dead_next);
    /* This is the reference to this client's presence in the
     * epoll list. */
    __dec_instance_ref(client);
    cdata->dead_generated++;
out:
    return ret;
}

static void stratifier_drop_id(const int64_t id) {
    char buf[256];

    sprintf(buf, "dropclient=%" PRId64, id);
    send_proc(ckpool.stratifier, buf);
}

/* Client must hold a reference count */
static int drop_client(cdata_t* cdata, client_instance_t* client) {
    bool    passthrough = client->passthrough, remote = client->remote;
    char    address_name[INET6_ADDRSTRLEN];
    int64_t client_id = client->id;
    int     fd = -1;

    strcpy(address_name, client->address_name);
    ck_wlock(&cdata->lock);
    fd = __drop_client(cdata, client);
    ck_wunlock(&cdata->lock);

    if (fd > -1) {
        if (passthrough) {
            LOGNOTICE("Connector dropped passthrough %" PRId64 " %s", client_id, address_name);
        } else if (remote) {
            LOGWARNING("Remote trusted server client %" PRId64 " %s disconnected", client_id, address_name);
        }
        LOGDEBUG("Connector dropped fd %d", fd);
        stratifier_drop_id(client_id);
#ifdef HAVE_SV2
        sv2_strat_drop_client(client_id);
        sv2_jd_drop_client(client_id);
#endif
    }

    return fd;
}

/* For sending the drop command to the upstream pool in passthrough mode */
static void generator_drop_client(const client_instance_t* client) {
    yyjson_mut_doc* doc;

    doc = yyjson_mut_pack(
        "{si,sI:ss:si:ss:s[]}", "id", 42, "client_id", client->id, "address", client->address_name, "server",
        client->server, "method", "mining.term", "params");
    generator_add_send(doc);
}

static void stratifier_drop_client(const client_instance_t* client) {
    stratifier_drop_id(client->id);
}

/* Invalidate this instance. Remove them from the hashtables we look up
 * regularly but keep the instances in a linked list until their ref count
 * drops to zero when we can remove them lazily. Client must hold a reference
 * count. */
static int invalidate_client(cdata_t* cdata, client_instance_t* client) {
    client_instance_t* tmp;
    int                ret;

    ret = drop_client(cdata, client);
    if ((!ckpool.passthrough || ckpool.node) && !client->passthrough)
        stratifier_drop_client(client);
    if (ckpool.passthrough)
        generator_drop_client(client);

    /* Cull old unused clients lazily when there are no more reference
     * counts for them. */
    ck_wlock(&cdata->lock);
    DL_FOREACH_SAFE2(cdata->dead_clients, client, tmp, dead_next) {
        if (!client->ref) {
            DL_DELETE2(cdata->dead_clients, client, dead_prev, dead_next);
            LOGINFO("Connector recycling client %" PRId64, client->id);
            /* We only close the client fd once we're sure there
             * are no references to it left to prevent fds being
             * reused on new and old clients. */
            nolinger_socket(client->fd);
            Close(client->fd);
            __recycle_client(cdata, client);
        }
    }
    ck_wunlock(&cdata->lock);

    return ret;
}

static void drop_all_clients(cdata_t* cdata) {
    client_instance_t *client, *tmp;
    int64_t*           ids = NULL;
    int                n = 0, i;

    /*
     * Match drop_client() side effects: after unhashing under cdata->lock,
     * notify stratifier and tear down SV2 mining/JD state so tokens,
     * channels, and client refs cannot leak across a bulk drop (e.g. reject).
     */
    ck_wlock(&cdata->lock);
    HASH_ITER(hh, cdata->clients, client, tmp) {
        int64_t id = client->id;

        if (__drop_client(cdata, client) > -1) {
            ids = ckrealloc(ids, (n + 1) * sizeof(*ids));
            ids[n++] = id;
        }
    }
    ck_wunlock(&cdata->lock);

    for (i = 0; i < n; i++) {
        stratifier_drop_id(ids[i]);
#ifdef HAVE_SV2
        sv2_strat_drop_client(ids[i]);
        sv2_jd_drop_client(ids[i]);
#endif
    }
    dealloc(ids);
}

static void send_client(cdata_t* cdata, int64_t id, char* buf);

/* Look for shares being submitted via a redirector and add them to a linked
 * list for looking up the responses. */
static void parse_redirector_share(cdata_t* cdata, client_instance_t* client, yyjson_mut_val* val) {
    share_t *share, *tmp;
    time_t   now;
    int64_t  id;

    if (!yyjson_mut_obj_get_int64(&id, val, "id")) {
        LOGNOTICE("Failed to find redirector share id");
        return;
    }
    share = ckzalloc(sizeof(share_t));
    now = time(NULL);
    share->submitted = now;
    share->id = id;

    LOGINFO("Redirector adding client %" PRId64 " share id: %" PRId64, client->id, id);

    /* We use the cdata lock instead of a separate lock since this function
     * is called infrequently. */
    ck_wlock(&cdata->lock);
    DL_APPEND(client->shares, share);

    /* Age old shares. */
    DL_FOREACH_SAFE(client->shares, share, tmp) {
        if (now > share->submitted + 120) {
            DL_DELETE(client->shares, share);
            dealloc(share);
        }
    }
    ck_wunlock(&cdata->lock);
}

/* The stratifier trusts client_id, address and server as connector generated.
 * yyjson permits duplicate keys and returns the first match, so a key supplied
 * by the remote end would shadow the one we append. Remove any existing copies
 * before adding ours; yyjson_mut_obj_remove_key strips all duplicates of a
 * name, so one call each is enough. Passthrough messages legitimately carry
 * the subclient's address from the downstream connector so we only strip that
 * on the direct client path where we generate it ourselves; srecv_process
 * validates it in either case. */
static void strip_reserved_keys(yyjson_mut_val* root, const bool strip_address) {
    yyjson_mut_obj_remove_key(root, "client_id");
    yyjson_mut_obj_remove_key(root, "server");
    if (strip_address)
        yyjson_mut_obj_remove_key(root, "address");
}

#ifdef HAVE_SV2
/* Forward decls — defined later in this file. */
static void               queue_sender_send(cdata_t* cdata, client_instance_t* client, sender_send_t* sender_send);
static void               send_client_bin(cdata_t* cdata, client_instance_t* client, uint8_t* buf, int len);
static client_instance_t* ref_client_by_id(cdata_t* cdata, int64_t id);

/* SV2 binary path: Noise reassembly, decrypt, stratifier handle, encrypt reply.
 * Returns false to drop. */
static bool parse_client_msg_sv2(cdata_t* cdata, client_instance_t* client) {
    uint8_t   rdbuf[8192];
    int       ret;
    uint8_t*  reply = NULL;
    size_t    reply_len = 0;
    uint8_t** frames = NULL;
    size_t*   frame_lens = NULL;
    size_t    nframes = 0, i;

    ret = read(client->fd, rdbuf, sizeof(rdbuf));
    if (ret < 1) {
        if (likely(errno == EAGAIN || errno == EWOULDBLOCK || !ret))
            return true;
        LOGINFO(
            "SV2 client id %" PRId64 " fd %d disconnected - recv fail ret %d errno %d", client->id, client->fd, ret,
            errno);
        return false;
    }
    if (!sv2_conn_feed(client->sv2c, rdbuf, (size_t)ret, &reply, &reply_len, &frames, &frame_lens, &nframes)) {
        LOGNOTICE("SV2 client %" PRId64 " protocol/crypto error, dropping", client->id);
        dealloc(reply);
        for (i = 0; i < nframes; i++)
            dealloc(frames[i]);
        dealloc(frames);
        dealloc(frame_lens);
        return false;
    }
    /* A cleared inflight flag means the Noise handshake completed, so this
     * peer has proven it speaks SV2 and is no longer reapable as idle. A
     * peer that connects and never completes the handshake keeps its slot
     * in the global inflight limit, so it must be reaped. */
    if (likely(!client->sv2c->hs_inflight))
        client->got_msg = true;
    /* Handshake ciphertext — already Noise-framed; do not re-encrypt. */
    if (reply && reply_len) {
        sender_send_t* ss = ckzalloc(sizeof(sender_send_t));

        ss->client = client;
        ss->buf = (char*)reply;
        ss->len = (int)reply_len;
        ss->sv2_encrypt = false;
        inc_instance_ref(cdata, client);
        queue_sender_send(cdata, client, ss);
        reply = NULL;
    }
    dealloc(reply);

    for (i = 0; i < nframes; i++) {
        size_t   rlen = 0;
        uint8_t* rplain;

        /*
         * Policy plaintext caps (rx_append already bounds ciphertext).
         * JD: SV2_MAX_JD_PAYLOAD; mining: SV2_MAX_MINING_PAYLOAD.
         */
        {
            size_t pcap = client->sv2_jd ? (size_t)SV2_MAX_JD_PAYLOAD : (size_t)SV2_MAX_MINING_PAYLOAD;

            if (frame_lens[i] > (size_t)SV2_FRAME_HEADER_LEN + pcap) {
                LOGNOTICE(
                    "SV2 %s client %" PRId64
                    " plaintext frame %zu > "
                    "cap %zu — dropping frame",
                    client->sv2_jd ? "JD" : "mining", client->id, frame_lens[i], pcap);
                dealloc(frames[i]);
                continue;
            }
        }
        if (client->sv2_jd)
            rplain = sv2_jd_handle_frame(client->id, frames[i], frame_lens[i], &rlen);
        else
            rplain = sv2_strat_handle_frame(client->id, frames[i], frame_lens[i], &rlen);

        dealloc(frames[i]);
        if (rplain && rlen)
            send_client_bin(cdata, client, rplain, (int)rlen);
        else
            dealloc(rplain);
    }
    dealloc(frames);
    dealloc(frame_lens);
    return true;
}

/* Queue a plaintext SV2 frame for the owning sender shard. Encryption of the
 * outbound Noise CipherState happens only on that shard thread. */
static void send_client_bin(cdata_t* cdata, client_instance_t* client, uint8_t* plain, int plainlen) {
    sender_send_t* ss;

    if (!client->sv2 || !client->sv2c || plainlen < 1) {
        dealloc(plain);
        return;
    }
    ss = ckzalloc(sizeof(sender_send_t));
    ss->client = client;
    ss->buf = (char*)plain;
    ss->len = plainlen;
    ss->sv2_encrypt = true;
    inc_instance_ref(cdata, client);
    queue_sender_send(cdata, client, ss);
}

void connector_sv2_send_plain(int64_t client_id, uint8_t* plain, size_t plainlen) {
    cdata_t*           cdata = ckpool.cdata;
    client_instance_t* client;

    if (!cdata || !plain) {
        dealloc(plain);
        return;
    }
    client = ref_client_by_id(cdata, client_id);
    if (!client || !client->sv2 || !client->sv2c) {
        if (client)
            dec_instance_ref(cdata, client);
        dealloc(plain);
        return;
    }
    /* Queue only; shard thread encrypts (single-writer CipherState). */
    send_client_bin(cdata, client, plain, (int)plainlen);
    dec_instance_ref(cdata, client);
}
#endif /* HAVE_SV2 */

/* Client is holding a reference count from being on the epoll list. Returns
 * true if we will still be receiving messages from this client. */
static bool parse_client_msg(cdata_t* cdata, client_instance_t* client) {
    yyjson_doc* sdoc;
    int         buflen, ret;
    char*       eol;

#ifdef HAVE_SV2
    if (client->sv2)
        return parse_client_msg_sv2(cdata, client);
#endif

retry:
    if (unlikely(client->bufofs > MAX_MSGSIZE)) {
        /* Remote clients are allowed a larger but finite ceiling. */
        if (unlikely(!client->remote || client->bufofs > MAX_REMOTE_MSGSIZE)) {
            LOGNOTICE(
                "Client id %" PRId64 " fd %d overloaded buffer without EOL, disconnecting", client->id, client->fd);
            return false;
        }
        client->buf = realloc(client->buf, round_up_page(client->bufofs + MAX_MSGSIZE + 1));
        if (unlikely(!client->buf)) {
            LOGERR("Client id %" PRId64 " failed to grow remote recv buffer", client->id);
            return false;
        }
    }
    /* This read call is non-blocking since the socket is set to O_NOBLOCK */
    ret = read(client->fd, client->buf + client->bufofs, MAX_MSGSIZE);
    if (ret < 1) {
        if (likely(errno == EAGAIN || errno == EWOULDBLOCK || !ret))
            return true;
        LOGINFO(
            "Client id %" PRId64 " fd %d disconnected - recv fail with bufofs %lu ret %d errno %d %s", client->id,
            client->fd, client->bufofs, ret, errno, ret && errno ? strerror(errno) : "");
        return false;
    }
    client->bufofs += ret;
    /* Always keep the buffer null terminated as we use string functions
     * on it below. There is always room for this as non-remote clients
     * are bounded to MAX_MSGSIZE and the remote realloc above allows for
     * bufofs + MAX_MSGSIZE + 1. */
    client->buf[client->bufofs] = '\0';
reparse:
    eol = memchr(client->buf, '\n', client->bufofs);
    if (!eol)
        goto retry;

    /* Do something useful with this message now */
    buflen = eol - client->buf + 1;
    if (unlikely(buflen > MAX_MSGSIZE && (!client->remote || buflen > MAX_REMOTE_MSGSIZE))) {
        LOGNOTICE("Client id %" PRId64 " fd %d message oversize, disconnecting", client->id, client->fd);
        return false;
    }

    /* Filter out non- or incomplete json */
    if (unlikely(
            client->buf[0] != '{' ||
            !(sdoc = yyjson_read(client->buf, strlen(client->buf), YYJSON_READ_STOP_WHEN_DONE)))) {
        char* buf = strdup("Invalid JSON, disconnecting\n");

        LOGINFO("Client id %" PRId64 " sent invalid json message %s", client->id, client->buf);
        send_client(cdata, client->id, buf);
        return false;
    } else {
        yyjson_mut_doc* doc = yyjson_doc_mut_copy(sdoc, &ckyyalc);
        yyjson_mut_val* root = yyjson_mut_doc_get_root(doc);

        yyjson_doc_free(sdoc);

        if (client->passthrough) {
            double  subclient_id = yyjson_mut_get_num(yyjson_mut_obj_get(root, "client_id"));
            int64_t passthrough_id;

            /* Sanitise the remotely supplied subclient id which may
             * only occupy the lower 32 bits, avoiding undefined
             * behaviour casting out of range values and preventing
             * bits being set in another client's id space. NaN
             * fails both comparisons here. */
            if (unlikely(!(subclient_id >= 0 && subclient_id < 4294967296.0)))
                subclient_id = 0;
            passthrough_id = (client->id << 32) | (int64_t)subclient_id;
            /* Strip only after reading the remotely supplied subclient
             * id above, which is a legitimate input on this path. */
            strip_reserved_keys(root, false);
            yyjson_mut_obj_add_sint(doc, root, "client_id", passthrough_id);
        } else {
            if (ckpool.redirector && !client->redirected && strstr(client->buf, "mining.submit"))
                parse_redirector_share(cdata, client, root);
            strip_reserved_keys(root, true);
            yyjson_mut_obj_add_sint(doc, root, "client_id", client->id);
            yyjson_mut_obj_add_str(doc, root, "address", client->address_name);
        }
        yyjson_mut_obj_add_sint(doc, root, "server", client->server);

        /* This peer has delivered a complete, parseable message so it
         * is speaking the protocol and is no longer reapable as idle. */
        client->got_msg = true;

        /* Do not send messages of clients we've already dropped. We
         * do this unlocked as the occasional false negative can be
         * filtered by the stratifier. */
        if (likely(!client->invalid)) {
            if (!ckpool.passthrough)
                stratifier_add_yyrecv(doc);
            else if (ckpool.node)
                stratifier_add_yyrecv(doc);
            else if (ckpool.passthrough)
                generator_add_send(doc);
        } else
            yyjson_mut_doc_free(doc);
    }
    client->bufofs -= buflen;
    if (client->bufofs)
        memmove(client->buf, client->buf + buflen, client->bufofs);
    client->buf[client->bufofs] = '\0';

    if (client->bufofs)
        goto reparse;
    goto retry;
}

static client_instance_t* ref_client_by_id(cdata_t* cdata, int64_t id) {
    client_instance_t* client;

    ck_wlock(&cdata->lock);
    HASH_FIND_I64(cdata->clients, &id, client);
    if (client) {
        if (!client->invalid)
            __inc_instance_ref(client);
        else
            client = NULL;
    }
    ck_wunlock(&cdata->lock);

    return client;
}

/* Safely drop a client from a context that does not already hold a reference
 * to it, taking one for the duration. */
static void drop_client_by_id(cdata_t* cdata, const int64_t id) {
    client_instance_t* client = ref_client_by_id(cdata, id);

    if (unlikely(!client))
        return;
    invalidate_client(cdata, client);
    dec_instance_ref(cdata, client);
}

static void add_remote_client(cdata_t* cdata, int64_t id) {
    client_instance_t* client;
    yyjson_mut_doc*    doc;
    bool               found = false;
    char*              buf;

    ck_wlock(&cdata->lock);
    HASH_FIND_I64(cdata->clients, &id, client);
    if (likely(client)) {
        found = true;
        client->remote = true;
    }
    ck_wunlock(&cdata->lock);

    if (likely(found))
        LOGWARNING("Added trusted remote client %ld", id);
    else
        LOGWARNING("Unable to find trusted remote client %ld", id);
    doc = yyjson_mut_pack("{sb}", "result", found);
    buf = yyjson_mut_write(doc, YYJSON_WRITE_NEWLINE_AT_END, NULL);
    yyjson_mut_doc_free(doc);
    send_client(cdata, id, buf);
}

static void redirect_client(client_instance_t* client);

static bool redirect_matches(cdata_t* cdata, client_instance_t* client) {
    redirect_t* redirect;

    ck_rlock(&cdata->lock);
    HASH_FIND_STR(cdata->redirects, client->address_name, redirect);
    ck_runlock(&cdata->lock);

    return redirect;
}

static void client_event_processor(struct epoll_event* event) {
    const uint32_t     events = event->events;
    const uint64_t     id = event->data.u64;
    cdata_t*           cdata = ckpool.cdata;
    client_instance_t* client;

    client = ref_client_by_id(cdata, id);
    if (unlikely(!client)) {
        LOGNOTICE("Failed to find client by id %" PRId64 " in receiver!", id);
        goto outnoclient;
    }
    /* We can have both messages and read hang ups so process the
     * message first. */
    if (likely(events & EPOLLIN)) {
        /* Rearm the client for epoll events if we have successfully
         * parsed a message from it */
        if (unlikely(!parse_client_msg(cdata, client))) {
            invalidate_client(cdata, client);
            goto out;
        }
    }
    if (unlikely(events & EPOLLERR)) {
        socklen_t errlen = sizeof(int);
        int       error = 0;

        /* See what type of error this is and raise the log
         * level of the message if it's unexpected. */
        getsockopt(client->fd, SOL_SOCKET, SO_ERROR, (void*)&error, &errlen);
        if (error != 104) {
            LOGNOTICE(
                "Client id %" PRId64 " fd %d epollerr HUP in epoll with errno %d: %s", client->id, client->fd, error,
                strerror(error));
        } else {
            LOGINFO(
                "Client id %" PRId64 " fd %d epollerr HUP in epoll with errno %d: %s", client->id, client->fd, error,
                strerror(error));
        }
        invalidate_client(cdata, client);
    } else if (unlikely(events & EPOLLHUP)) {
        /* Client connection reset by peer */
        LOGINFO("Client id %" PRId64 " fd %d HUP in epoll", client->id, client->fd);
        invalidate_client(cdata, client);
    } else if (unlikely(events & EPOLLRDHUP)) {
        /* Client disconnected by peer */
        LOGINFO("Client id %" PRId64 " fd %d RDHUP in epoll", client->id, client->fd);
        invalidate_client(cdata, client);
    }
out:
    if (likely(!client->invalid)) {
        /* Rearm the fd in the epoll list if it's still active */
        event->data.u64 = id;
        event->events = EPOLLIN | EPOLLRDHUP | EPOLLONESHOT;
        epoll_ctl(cdata->epfd, EPOLL_CTL_MOD, client->fd, event);
    }
    dec_instance_ref(cdata, client);
outnoclient:
    free(event);
}

/* Seconds a connected socket may go without delivering a single complete
 * protocol message before it is reaped, matching the stratifier's existing
 * policy for clients that connect but never authorise. */
#define IDLE_ACCEPT_TIMEOUT 60
/* Maximum clients reaped per sweep. Any remainder is caught on the next pass,
 * which keeps the instance lock held briefly and avoids allocating under it. */
#define IDLE_REAP_BATCH 128

/* Reap sockets that connected but have never delivered a complete message.
 * A stratum_instance_t is only created once a whole message reaches the
 * stratifier, so these are invisible to the stratifier's unauthorised client
 * reaper while still holding an fd and a maxclients slot - and for SV2, a slot
 * in the global handshake inflight limit, which dropping the client releases
 * via sv2_conn_free. */
static void reap_idle_clients(cdata_t* cdata) {
    int64_t            ids[IDLE_REAP_BATCH];
    client_instance_t *client, *tmp;
    time_t             now_t = time(NULL);
    int                nids = 0, i;

    ck_rlock(&cdata->lock);
    HASH_ITER(hh, cdata->clients, client, tmp) {
        if (likely(client->got_msg) || client->invalid)
            continue;
        if (likely(now_t - client->accept_time <= IDLE_ACCEPT_TIMEOUT))
            continue;
        ids[nids++] = client->id;
        if (unlikely(nids >= IDLE_REAP_BATCH))
            break;
    }
    ck_runlock(&cdata->lock);

    for (i = 0; i < nids; i++) {
        client = ref_client_by_id(cdata, ids[i]);
        if (unlikely(!client))
            continue;
        LOGNOTICE(
            "Reaping client id %" PRId64 " %s idle without a message for %d seconds", client->id, client->address_name,
            IDLE_ACCEPT_TIMEOUT);
        invalidate_client(cdata, client);
        dec_instance_ref(cdata, client);
    }
}

/* Waits on fds ready to read on from the list stored in conn_instance and
 * handles the incoming messages */
static void* receiver(void* arg) {
    time_t              last_reap = time(NULL);
    cdata_t*            cdata = (cdata_t*)arg;
    struct epoll_event* event = ckzalloc(sizeof(struct epoll_event));
    uint64_t            serverfds, i;
    int                 ret, epfd;

    rename_proc("creceiver");

    epfd = cdata->epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        LOGEMERG("FATAL: Failed to create epoll in receiver");
        goto out;
    }
    serverfds = ckpool.serverurls;
    /* Add all the serverfds to the epoll */
    for (i = 0; i < serverfds; i++) {
        /* The small values will be less than the first client ids */
        event->data.u64 = i;
        event->events = EPOLLIN | EPOLLRDHUP;
        ret = epoll_ctl(epfd, EPOLL_CTL_ADD, cdata->serverfd[i], event);
        if (ret < 0) {
            LOGEMERG("FATAL: Failed to add epfd %d to epoll_ctl", epfd);
            goto out;
        }
    }

    /* Wait for the stratifier to be ready for us */
    while (!ckpool.stratifier_ready)
        cksleep_ms(10);

    while (42) {
        uint64_t edu64;
        time_t   now_t;

        while (unlikely(!cdata->accept))
            cksleep_ms(10);
        ret = epoll_wait(epfd, event, 1, 1000);
        /* The epoll timeout gives us a roughly once per second tick to
         * sweep idle sockets on without a dedicated thread. Rate limit
         * it since a busy pool returns from epoll_wait immediately. */
        now_t = time(NULL);
        if (now_t != last_reap) {
            last_reap = now_t;
            reap_idle_clients(cdata);
        }
        if (unlikely(ret < 1)) {
            if (unlikely(ret == -1)) {
                LOGEMERG("FATAL: Failed to epoll_wait in receiver");
                break;
            }
            /* Nothing to service, still very unlikely */
            continue;
        }
        edu64 = event->data.u64;
        if (edu64 < serverfds) {
            ret = accept_client(cdata, epfd, edu64);
            if (unlikely(ret < 0)) {
                LOGEMERG("FATAL: Failed to accept_client in receiver");
                break;
            }
            continue;
        }
        /* Event structure is handed off to client_event_processor
         * here to be freed so we need to allocate a new one */
        ckmsgq_add(cdata->cevents, event);
        event = ckzalloc(sizeof(struct epoll_event));
    }
out:
    /* We shouldn't get here unless there's an error */
    return NULL;
}

/* Send a sender_send message and return true if we've finished sending it or
 * are unable to send any more. */
static bool send_sender_send(cdata_t* cdata, sender_send_t* sender_send) {
    client_instance_t* client = sender_send->client;
    time_t             now_t;

    if (unlikely(client->invalid))
        goto out_true;

    /* Make sure we only send one message at a time to each client */
    if (unlikely(client->sending && client->sending != sender_send))
        return false;

    client->sending = sender_send;
    now_t = time(NULL);

#ifdef HAVE_SV2
    /* Noise outbound encrypt runs only here — the csender shard is the
     * single writer for this client's send CipherState. Encrypt once on
     * first attempt (before any partial write). */
    if (sender_send->sv2_encrypt) {
        uint8_t* ct = NULL;
        size_t   ctlen = 0;

        if (!client->sv2c ||
            !sv2_conn_encrypt(client->sv2c, (const uint8_t*)sender_send->buf, (size_t)sender_send->len, &ct, &ctlen)) {
            LOGINFO("SV2 encrypt failed client %" PRId64 " on sender shard, dropping", client->id);
            sender_send->sv2_encrypt = false;
            invalidate_client(cdata, client);
            goto out_true;
        }
        dealloc(sender_send->buf);
        sender_send->buf = (char*)ct;
        sender_send->len = (int)ctlen;
        sender_send->ofs = 0;
        sender_send->sv2_encrypt = false;
    }
#endif

    /* Increase sendbufsize to match large messages sent to clients - this
     * usually only applies to clients as mining nodes. */
    if (unlikely(!ckpool.wmem_warn && sender_send->len > client->sendbufsize))
        client->sendbufsize = set_sendbufsize(client->fd, sender_send->len);

    while (sender_send->len) {
        int ret = write(client->fd, sender_send->buf + sender_send->ofs, sender_send->len);

        if (ret < 1) {
            /* Invalidate clients that block for more than 60 seconds */
            if (unlikely(client->blocked_time && now_t - client->blocked_time >= 60)) {
                LOGNOTICE("Client id %" PRId64 " fd %d blocked for >60 seconds, disconnecting", client->id, client->fd);
                invalidate_client(cdata, client);
                goto out_true;
            }
            if (errno == EAGAIN || errno == EWOULDBLOCK || !ret) {
                if (!client->blocked_time)
                    client->blocked_time = now_t;
                return false;
            }
            LOGINFO(
                "Client id %" PRId64 " fd %d disconnected with write errno %d:%s", client->id, client->fd, errno,
                strerror(errno));
            invalidate_client(cdata, client);
            goto out_true;
        }
        sender_send->ofs += ret;
        sender_send->len -= ret;
        client->blocked_time = 0;
    }
out_true:
    client->sending = NULL;
    return true;
}

static void clear_sender_send(sender_send_t* sender_send, cdata_t* cdata) {
    client_instance_t* client = sender_send->client;
    csender_t*         cs = &cdata->csenders[(uint64_t)client->id % cdata->nsenders];

    /* Reverse the accounting taken in queue_sender_send under the same
     * lock that applied it. */
    mutex_lock(&cs->lock);
    client->queued_bytes -= sender_send->qlen;
    if (unlikely(client->queued_bytes < 0))
        client->queued_bytes = 0;
    mutex_unlock(&cs->lock);

    dec_instance_ref(cdata, client);
    free(sender_send->buf);
    free(sender_send);
}

/* Use a thread to send queued messages, appending them to the sends list and
 * iterating over all of them, attempting to send them all non-blocking to
 * only send to those clients ready to receive data. */
static void* sender(void* arg) {
    csender_t*     cs = (csender_t*)arg;
    cdata_t*       cdata = cs->cdata;
    sender_send_t* sends = NULL;
    char           procname[16];

    snprintf(procname, sizeof(procname), "csender%d", cs->id);
    rename_proc(procname);

    while (42) {
        int64_t        sends_queued = 0, sends_size = 0;
        sender_send_t *sending, *tmp;

        /* Check all sends to see if they can be written out */
        DL_FOREACH_SAFE(sends, sending, tmp) {
            if (send_sender_send(cdata, sending)) {
                DL_DELETE(sends, sending);
                clear_sender_send(sending, cdata);
            } else {
                sends_queued++;
                sends_size += sizeof(sender_send_t) + sending->len + 1;
            }
        }

        mutex_lock(&cs->lock);
        cs->sends_delayed += sends_queued;
        cs->sends_queued = sends_queued;
        cs->sends_size = sends_size;
        /* Poll every 10ms if there are no new sends. */
        if (!cs->sends) {
            const ts_t polltime = {0, 10000000};
            ts_t       timeout_ts;

            ts_realtime(&timeout_ts);
            timeraddspec(&timeout_ts, &polltime);
            cond_timedwait(&cs->cond, &cs->lock, &timeout_ts);
        }
        if (cs->sends) {
            DL_CONCAT(sends, cs->sends);
            cs->sends = NULL;
        }
        mutex_unlock(&cs->lock);
    }
    /* We shouldn't get here unless there's an error */
    return NULL;
}

/* Append a pending send to the shard owning this client (client id modulo
 * nsenders) and wake that sender thread. A given client always maps to the
 * same shard, keeping its send state single-writer. */
/* Bytes queued but unsent to one client before it is considered unable to keep
 * up and dropped. Node and trusted remote peers legitimately receive far larger
 * messages than a miner so they get a higher ceiling. Both are overridden by
 * the maxsendqueue config option. */
#define DEFAULT_SENDQUEUE_LIMIT (1024 * 1024)
#define REMOTE_SENDQUEUE_LIMIT (64 * 1024 * 1024)

static int64_t client_sendqueue_limit(const client_instance_t* client) {
    int64_t limit;

    if (ckpool.maxsendqueue > 0)
        return ckpool.maxsendqueue;
    if (client->remote || client->passthrough)
        return REMOTE_SENDQUEUE_LIMIT;
    /* Scale with the socket send buffer so clients on fat links are not
     * penalised, with a floor for the common case. */
    limit = (int64_t)client->sendbufsize * 4;
    if (limit < DEFAULT_SENDQUEUE_LIMIT)
        limit = DEFAULT_SENDQUEUE_LIMIT;
    return limit;
}

static void queue_sender_send(cdata_t* cdata, client_instance_t* client, sender_send_t* sender_send) {
    csender_t* cs = &cdata->csenders[(uint64_t)client->id % cdata->nsenders];
    int64_t    queued;

    sender_send->qlen = sender_send->len;

    mutex_lock(&cs->lock);
    cs->sends_generated++;
    client->queued_bytes += sender_send->qlen;
    queued = client->queued_bytes;
    DL_APPEND(cs->sends, sender_send);
    pthread_cond_signal(&cs->cond);
    mutex_unlock(&cs->lock);

    /* A client that reads just enough to keep resetting the blocked
     * timeout can otherwise accumulate queued messages without bound, so
     * cap the queue independently of blocked_time. Drop outside the send
     * lock as invalidate_client takes the instance lock. */
    if (unlikely(queued > client_sendqueue_limit(client) && !client->invalid)) {
        LOGNOTICE(
            "Client id %" PRId64 " fd %d %s exceeded send queue limit with %" PRId64 " bytes queued, disconnecting",
            client->id, client->fd, client->address_name, queued);
        drop_client_by_id(cdata, client->id);
    }
}

static int add_redirect(cdata_t* cdata, client_instance_t* client) {
    redirect_t* redirect;
    bool        found;

    ck_wlock(&cdata->lock);
    HASH_FIND_STR(cdata->redirects, client->address_name, redirect);
    if (!redirect) {
        redirect = ckzalloc(sizeof(redirect_t));
        strcpy(redirect->address_name, client->address_name);
        redirect->redirect_no = cdata->redirect++;
        if (cdata->redirect >= ckpool.redirecturls)
            cdata->redirect = 0;
        HASH_ADD_STR(cdata->redirects, address_name, redirect);
        found = false;
    } else
        found = true;
    ck_wunlock(&cdata->lock);

    LOGNOTICE(
        "Redirecting client %" PRId64 " from %s IP %s to redirecturl %d", client->id, found ? "matching" : "new",
        client->address_name, redirect->redirect_no);
    return redirect->redirect_no;
}

static void redirect_client(client_instance_t* client) {
    sender_send_t*  sender_send;
    cdata_t*        cdata = ckpool.cdata;
    yyjson_mut_doc* doc;
    size_t          len;
    char*           buf;
    int             num;

    /* Set the redirected boool to only try redirecting them once */
    client->redirected = true;

    num = add_redirect(cdata, client);
    doc = yyjson_mut_pack(
        "{snsss[ssi]}", "id", "method", "client.reconnect", "params", ckpool.redirecturl[num], ckpool.redirectport[num],
        0);
    buf = yyjson_mut_write(doc, YYJSON_WRITE_NEWLINE_AT_END, &len);
    yyjson_mut_doc_free(doc);

    sender_send = ckzalloc(sizeof(sender_send_t));
    sender_send->client = client;
    sender_send->buf = buf;
    sender_send->len = len;
    inc_instance_ref(cdata, client);

    queue_sender_send(cdata, client, sender_send);
}

/* Look for accepted shares in redirector mode to know we can redirect this
 * client to a protected server. */
static bool test_redirector_shares(cdata_t* cdata, client_instance_t* client, const char* buf) {
    yyjson_doc* doc = yyjson_read(buf, strlen(buf), 0);
    share_t *   share, *found = NULL;
    yyjson_val* val;
    bool        ret = false;
    int64_t     id;

    if (!doc) {
        /* Can happen when responding to invalid json from client */
        LOGINFO("Invalid json response to client %" PRId64 "%s", client->id, buf);
        return ret;
    }
    val = yyjson_doc_get_root(doc);
    if (!yyjson_obj_get_int64(&id, val, "id")) {
        LOGINFO("Failed to find response id");
        goto out;
    }

    ck_rlock(&cdata->lock);
    DL_FOREACH(client->shares, share) {
        if (share->id == id) {
            LOGDEBUG("Found matching share %" PRId64 " in trs for client %" PRId64, id, client->id);
            found = share;
            break;
        }
    }
    ck_runlock(&cdata->lock);

    if (found) {
        bool result = false;

        {
            yyjson_val* res_val = yyjson_obj_get(val, "result");

            if (!yyjson_is_bool(res_val)) {
                yyjson_val* err_val = yyjson_obj_get(val, "error");

                if (unlikely(!(yyjson_is_null(res_val) && err_val && !yyjson_is_null(err_val)))) {
                    LOGINFO("Failed to find result in trs share");
                    goto out;
                }
                result = false;
            } else
                result = yyjson_get_bool(res_val);
        }
        if (!yyjson_is_null(yyjson_obj_get(val, "error"))) {
            LOGINFO("Got error for trs share");
            goto out;
        }
        if (!result) {
            LOGDEBUG("Rejected trs share");
            goto out;
        }
        LOGNOTICE("Found accepted share for client %" PRId64 " - redirecting", client->id);
        ret = true;

        /* Clear the list now since we don't need it any more */
        ck_wlock(&cdata->lock);
        DL_FOREACH_SAFE(client->shares, share, found) {
            DL_DELETE(client->shares, share);
            dealloc(share);
        }
        ck_wunlock(&cdata->lock);
    }
out:
    yyjson_doc_free(doc);
    return ret;
}

/* Send a client by id a heap allocated buffer, allowing this function to
 * free the ram. */
static void send_client(cdata_t* cdata, const int64_t id, char* buf) {
    sender_send_t*     sender_send;
    client_instance_t* client;
    bool               redirect = false;
    int64_t            pass_id;
    int                len;

    if (unlikely(!buf)) {
        LOGWARNING("Connector send_client sent a null buffer");
        return;
    }
    len = strlen(buf);
    if (unlikely(!len)) {
        LOGWARNING("Connector send_client sent a zero length buffer");
        free(buf);
        return;
    }

    if (unlikely(ckpool.node && !id)) {
        LOGDEBUG("Message for node: %s", buf);
        send_proc(ckpool.stratifier, buf);
        free(buf);
        return;
    }

    /* Grab a reference to this client until the sender_send has
     * completed processing. Is this a passthrough subclient ? */
    if ((pass_id = subclient(id))) {
        int64_t client_id = id & 0xffffffffll;

        /* Make sure the passthrough exists for passthrough subclients */
        client = ref_client_by_id(cdata, pass_id);
        if (unlikely(!client)) {
            LOGINFO(
                "Connector failed to find passthrough id %" PRId64 " of client id %" PRId64 " to send to", pass_id,
                client_id);
            /* Now see if the subclient exists */
            client = ref_client_by_id(cdata, client_id);
            if (client) {
                invalidate_client(cdata, client);
                dec_instance_ref(cdata, client);
            } else
                stratifier_drop_id(id);
            free(buf);
            return;
        }
    } else {
        client = ref_client_by_id(cdata, id);
        if (unlikely(!client)) {
            LOGINFO("Connector failed to find client id %" PRId64 " to send to", id);
            stratifier_drop_id(id);
            free(buf);
            return;
        }
        if (ckpool.redirector && !client->redirected && client->authorised) {
            /* If clients match the IP of clients that have already
             * been whitelisted as finding valid shares then
             * redirect them immediately. */
            if (redirect_matches(cdata, client))
                redirect = true;
            else
                redirect = test_redirector_shares(cdata, client, buf);
        }
    }

    sender_send = ckzalloc(sizeof(sender_send_t));
    sender_send->client = client;
    sender_send->buf = buf;
    sender_send->len = len;

    queue_sender_send(cdata, client, sender_send);

    /* Redirect after sending response to shares and authorise */
    if (unlikely(redirect))
        redirect_client(client);
}

static void _send_client_yyjson(
    cdata_t*        cdata,
    int64_t         client_id,
    yyjson_mut_doc* doc,
    const char*     file,
    const char*     func,
    const int       line) {
    client_instance_t* client;
    char*              msg;

    if (unlikely(!doc)) {
        LOGWARNING("_send_client_yyjson received NULL doc from %s %s:%d", file, func, line);
        return;
    }

    if (ckpool.node && (client = ref_client_by_id(cdata, client_id))) {
        yyjson_mut_doc* tmp_doc = yyjson_mut_doc_mut_copy(doc, &ckyyalc);
        yyjson_mut_val* root = yyjson_mut_doc_get_root(tmp_doc);

        strip_reserved_keys(root, true);
        yyjson_mut_obj_add_sint(tmp_doc, root, "client_id", client_id);
        yyjson_mut_obj_add_str(tmp_doc, root, "address", client->address_name);
        yyjson_mut_obj_add_sint(tmp_doc, root, "server", client->server);
        dec_instance_ref(cdata, client);
        stratifier_add_yyrecv(tmp_doc);
    }
    msg = yyjson_mut_write(doc, YYJSON_WRITE_NEWLINE_AT_END, NULL);
    send_client(cdata, client_id, msg);
    yyjson_mut_doc_free(doc);
}

#define send_client_yyjson(cdata, client_id, doc) \
    _send_client_yyjson(cdata, client_id, doc, __FILE__, __func__, __LINE__)

/* When testing if a client exists, passthrough clients don't exist when their
 * parent no longer exists. */
static bool client_exists(cdata_t* cdata, int64_t id) {
    int64_t            parent_id = subclient(id);
    client_instance_t* client;

    if (parent_id)
        id = parent_id;

    ck_rlock(&cdata->lock);
    HASH_FIND_I64(cdata->clients, &id, client);
    ck_runlock(&cdata->lock);

    return !!client;
}

static void passthrough_client(cdata_t* cdata, client_instance_t* client) {
    yyjson_mut_doc* doc;

    LOGINFO("Connector adding passthrough client %" PRId64, client->id);
    client->passthrough = true;
    doc = yyjson_mut_pack("{sb}", "result", true);
    send_client_yyjson(cdata, client->id, doc);
    if (!ckpool.rmem_warn)
        set_recvbufsize(client->fd, 1048576);
    if (!ckpool.wmem_warn)
        client->sendbufsize = set_sendbufsize(client->fd, 1048576);
}

static bool connect_upstream(connsock_t* cs) {
    yyjson_val *    res_val, *err_val;
    yyjson_doc*     val = NULL;
    bool            res, ret = false;
    yyjson_mut_doc* req;
    float           timeout = 10;

    cksem_wait(&cs->sem);
    cs->fd = connect_socket(cs->url, cs->port);
    if (cs->fd < 0) {
        LOGWARNING("Failed to connect to upstream server %s:%s", cs->url, cs->port);
        goto out;
    }
    keep_sockalive(cs->fd);

    /* We want large send buffers for upstreaming messages */
    if (!ckpool.rmem_warn)
        set_recvbufsize(cs->fd, 2097152);
    if (!ckpool.wmem_warn)
        cs->sendbufsiz = set_sendbufsize(cs->fd, 2097152);

    req = yyjson_mut_pack("{ss,s[s]}", "method", "mining.remote", "params", PACKAGE "/" VERSION);
    res = send_yyjson_msg(cs, req);
    yyjson_mut_doc_free(req);
    if (!res) {
        LOGWARNING("Failed to send message in connect_upstream");
        goto out;
    }
    if (read_socket_line(cs, &timeout) < 1) {
        LOGWARNING("Failed to receive line in connect_upstream");
        goto out;
    }
    val = yyjson_msg_result(cs->buf, &res_val, &err_val);
    if (!val || !res_val) {
        LOGWARNING("Failed to get a json result in connect_upstream, got: %s", cs->buf);
        goto out;
    }
    ret = yyjson_is_true(res_val);
    if (!ret) {
        LOGWARNING("Denied upstream trusted connection");
        goto out;
    }
    LOGWARNING("Connected to upstream server %s:%s as trusted remote", cs->url, cs->port);
    ret = true;
out:
    if (val)
        yyjson_doc_free(val);
    cksem_post(&cs->sem);

    return ret;
}

static void usend_process(char* buf) {
    cdata_t*    cdata = ckpool.cdata;
    connsock_t* cs = &cdata->upstream_cs;
    int         len, sent;

    if (unlikely(!buf || !strlen(buf))) {
        LOGERR("Send empty message to usend_process");
        goto out;
    }
    LOGDEBUG("Sending upstream msg: %s", buf);
    len = strlen(buf);
    while (42) {
        sent = write_socket(cs->fd, buf, len);
        if (sent == len)
            break;
        if (cs->fd > 0) {
            LOGWARNING("Upstream pool failed, attempting reconnect while caching messages");
            Close(cs->fd);
        }
        do
            sleep(5);
        while (!connect_upstream(cs));
    }
out:
    free(buf);
}

static void ping_upstream(cdata_t* cdata) {
    char* buf;

    ASPRINTF(&buf, "{\"method\":\"ping\"}\n");
    ckmsgq_add(cdata->upstream_sends, buf);
}

static void* urecv_process(void __maybe_unused* arg) {
    cdata_t*    cdata = ckpool.cdata;
    connsock_t* cs = &cdata->upstream_cs;
    bool        alive = true;

    rename_proc("ureceiver");

    pthread_detach(pthread_self());

    while (42) {
        yyjson_mut_val* root;
        yyjson_mut_doc* doc;
        const char*     method;
        float           timeout = 5;
        yyjson_doc*     idoc;
        int             ret;

        cksem_wait(&cs->sem);
        ret = read_socket_line(cs, &timeout);
        if (ret < 1) {
            ping_upstream(cdata);
            if (likely(!ret)) {
                LOGDEBUG("No message from upstream pool");
            } else {
                LOGNOTICE("Failed to read from upstream pool");
                alive = false;
            }
            goto nomsg;
        }
        alive = true;
        idoc = yyjson_read(cs->buf, strlen(cs->buf), 0);
        if (unlikely(!idoc)) {
            LOGWARNING("Received non-json msg from upstream pool %s", cs->buf);
            goto nomsg;
        }
        doc = yyjson_doc_mut_copy(idoc, &ckyyalc);
        yyjson_doc_free(idoc);
        root = yyjson_mut_doc_get_root(doc);
        method = yyjson_mut_get_str(yyjson_mut_obj_get(root, "method"));
        if (unlikely(!method)) {
            LOGWARNING("Failed to find method from upstream pool json %s", cs->buf);
            goto decref;
        }
        if (!safecmp(method, stratum_msgs[SM_TRANSACTIONS]))
            parse_upstream_txns(root);
        else if (!safecmp(method, stratum_msgs[SM_AUTHRESULT]))
            parse_upstream_auth(root);
        else if (!safecmp(method, stratum_msgs[SM_WORKINFO]))
            parse_upstream_workinfo(root);
        else if (!safecmp(method, stratum_msgs[SM_BLOCK]))
            parse_upstream_block(doc, root);
        else if (!safecmp(method, stratum_msgs[SM_REQTXNS]))
            parse_upstream_reqtxns(root);
        else if (!safecmp(method, "pong"))
            LOGDEBUG("Received upstream pong");
        else
            LOGWARNING("Unrecognised upstream method %s", method);
    decref:
        yyjson_mut_doc_free(doc);
    nomsg:
        cksem_post(&cs->sem);

        if (!alive)
            sleep(5);
    }
    return NULL;
}

static bool setup_upstream(cdata_t* cdata) {
    connsock_t* cs = &cdata->upstream_cs;
    bool        ret = false;
    pthread_t   pth;

    if (!ckpool.upstream) {
        LOGEMERG("No upstream server set in remote trusted server mode");
        goto out;
    }
    if (!extract_sockaddr(ckpool.upstream, &cs->url, &cs->port)) {
        LOGEMERG("Failed to extract upstream address from %s", ckpool.upstream);
        goto out;
    }

    cksem_init(&cs->sem);
    cksem_post(&cs->sem);

    while (!connect_upstream(cs))
        cksleep_ms(5000);

    create_pthread(&pth, urecv_process, NULL);
    cdata->upstream_sends = create_ckmsgq("usender", &usend_process);
    ret = true;
out:
    return ret;
}

static void client_yymessage_processor(yyjson_mut_doc* doc) {
    cdata_t*           cdata = ckpool.cdata;
    client_instance_t* client;
    yyjson_mut_val*    root;
    int64_t            client_id;

    if (unlikely(!doc)) {
        LOGWARNING("client_yymessage_processor received NULL doc");
        return;
    }
    root = yyjson_mut_doc_get_root(doc);
    /* Extract the client id from the json message and remove its entry */
    client_id = yyjson_mut_get_num(yyjson_mut_obj_get(root, "client_id"));
    yyjson_mut_obj_remove_key(root, "client_id");
    /* Put client_id back in for a passthrough subclient, passing its
     * upstream client_id instead of the passthrough's. */
    if (subclient(client_id))
        yyjson_mut_obj_add_sint(doc, root, "client_id", client_id & 0xffffffffll);

    /* Flag redirector clients once they've been authorised */
    if (ckpool.redirector && (client = ref_client_by_id(cdata, client_id))) {
        if (!client->redirected && !client->authorised) {
            yyjson_mut_val* method_val = yyjson_mut_obj_get(root, "node.method");
            const char*     method = yyjson_mut_get_str(method_val);

            if (!safecmp(method, stratum_msgs[SM_AUTHRESULT]))
                client->authorised = true;
        }
        dec_instance_ref(cdata, client);
    }
    send_client_yyjson(cdata, client_id, doc);
}

void _connector_add_yymessage(yyjson_mut_doc* doc, const char* file, const char* func, const int line) {
    cdata_t* cdata = ckpool.cdata;

    if (unlikely(!doc)) {
        LOGWARNING("_connector_add_yymessage received NULL doc from %s %s:%d", file, func, line);
        return;
    }

    ckmsgq_add(cdata->cympq, doc);
}

/* Send the passthrough the terminate node.method */
static void drop_passthrough_client(cdata_t* cdata, const int64_t id) {
    int64_t client_id;
    char*   msg;

    LOGINFO("Asked to drop passthrough client %" PRId64 ", forwarding to passthrough", id);
    client_id = id & 0xffffffffll;
    /* We have a direct connection to the passthrough's connector so we
     * can send it any regular commands. */
    ASPRINTF(&msg, "dropclient=%" PRId64 "\n", client_id);
    send_client(cdata, id, msg);
}

char* connector_stats(void* data, const int runtime) {
    yyjson_mut_doc*    doc = yyjson_mut_doc_new(&ckyyalc);
    yyjson_mut_val *   root = yyjson_mut_obj(doc), *subval;
    client_instance_t* client;
    int                objects, generated;
    cdata_t*           cdata = data;
    sender_send_t*     send;
    int64_t            memsize;
    char*              buf;

    yyjson_mut_doc_set_root(doc, root);

    /* If called in passthrough mode we log stats instead of the stratifier */
    if (runtime)
        yyjson_mut_obj_add_int(doc, root, "runtime", runtime);

    ck_rlock(&cdata->lock);
    objects = HASH_COUNT(cdata->clients);
    memsize = SAFE_HASH_OVERHEAD(cdata->clients) + sizeof(client_instance_t) * objects;
    generated = cdata->clients_generated;
    ck_runlock(&cdata->lock);

    subval = yyjson_mut_pack_val(doc, "{si,sI,si}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "clients", subval);

    ck_rlock(&cdata->lock);
    DL_COUNT2(cdata->dead_clients, client, objects, dead_next);
    generated = cdata->dead_generated;
    ck_runlock(&cdata->lock);

    memsize = objects * sizeof(client_instance_t);
    subval = yyjson_mut_pack_val(doc, "{si,sI,si}", "count", objects, "memory", memsize, "generated", generated);
    yyjson_mut_obj_add_val(doc, root, "dead", subval);

    objects = 0;
    memsize = 0;

    /* Aggregate the pending-send and stats counters across all shards. */
    {
        int64_t sends_generated = 0, sends_queued = 0, sends_size = 0, sends_delayed = 0;
        int     i;

        for (i = 0; i < cdata->nsenders; i++) {
            csender_t* cs = &cdata->csenders[i];

            mutex_lock(&cs->lock);
            DL_FOREACH(cs->sends, send) {
                objects++;
                memsize += sizeof(sender_send_t) + send->len + 1;
            }
            sends_generated += cs->sends_generated;
            sends_queued += cs->sends_queued;
            sends_size += cs->sends_size;
            sends_delayed += cs->sends_delayed;
            mutex_unlock(&cs->lock);
        }
        subval =
            yyjson_mut_pack_val(doc, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", sends_generated);
        yyjson_mut_obj_add_val(doc, root, "sends", subval);

        subval = yyjson_mut_pack_val(
            doc, "{sI,sI,sI}", "count", sends_queued, "memory", sends_size, "generated", sends_delayed);
        yyjson_mut_obj_add_val(doc, root, "delays", subval);
    }

    buf = yyjson_mut_write(doc, 0, NULL);
    yyjson_mut_doc_free(doc);
    if (runtime)
        LOGNOTICE("Passthrough:%s", buf);
    else
        LOGNOTICE("Connector stats: %s", buf);
    return buf;
}

void connector_send_fd(const int fdno, const int sockd) {
    cdata_t* cdata = ckpool.cdata;

    if (fdno > -1 && fdno < ckpool.serverurls)
        send_fd(cdata->serverfd[fdno], sockd);
    else
        LOGWARNING("Connector asked to send invalid fd %d", fdno);
}

static void connector_loop(proc_instance_t* pi, cdata_t* cdata) {
    unix_msg_t* umsg = NULL;
    time_t      last_stats;
    int64_t     client_id;
    int         ret = 0;
    char*       buf;

    last_stats = cdata->start_time;

retry:
    if (ckpool.passthrough) {
        time_t diff = time(NULL);

        if (diff - last_stats >= 60) {
            last_stats = diff;
            diff -= cdata->start_time;
            buf = connector_stats(cdata, diff);
            dealloc(buf);
        }
    }

    if (umsg) {
        Close(umsg->sockd);
        free(umsg->buf);
        dealloc(umsg);
    }

    do {
        umsg = get_unix_msg(pi);
    } while (!umsg);

    buf = umsg->buf;
    LOGDEBUG("Connector received message: %s", buf);
    /* The bulk of the messages will be json messages to send to clients
     * so look for them first. */
    if (likely(buf[0] == '{')) {
        yyjson_doc* sdoc = yyjson_read(buf, strlen(buf), YYJSON_READ_STOP_WHEN_DONE);

        if (likely(sdoc)) {
            yyjson_mut_doc* doc = yyjson_doc_mut_copy(sdoc, &ckyyalc);
            yyjson_doc_free(sdoc);
            ckmsgq_add(cdata->cympq, doc);
        }
    } else if (cmdmatch(buf, "dropclient")) {
        client_instance_t* client;

        ret = sscanf(buf, "dropclient=%" PRId64, &client_id);
        if (ret < 0) {
            LOGDEBUG("Connector failed to parse dropclient command: %s", buf);
            goto retry;
        }
        /* A passthrough client */
        if (subclient(client_id)) {
            drop_passthrough_client(cdata, client_id);
            goto retry;
        }
        client = ref_client_by_id(cdata, client_id);
        if (unlikely(!client)) {
            LOGINFO("Connector failed to find client id %" PRId64 " to drop", client_id);
            goto retry;
        }
        ret = invalidate_client(cdata, client);
        dec_instance_ref(cdata, client);
        if (ret >= 0)
            LOGINFO("Connector dropped client id: %" PRId64, client_id);
    } else if (cmdmatch(buf, "testclient")) {
        ret = sscanf(buf, "testclient=%" PRId64, &client_id);
        if (unlikely(ret < 0)) {
            LOGDEBUG("Connector failed to parse testclient command: %s", buf);
            goto retry;
        }
        if (client_exists(cdata, client_id))
            goto retry;
        LOGINFO("Connector detected non-existent client id: %" PRId64, client_id);
        stratifier_drop_id(client_id);
    } else if (cmdmatch(buf, "ping")) {
        LOGDEBUG("Connector received ping request");
        send_unix_msg(umsg->sockd, "pong");
    } else if (cmdmatch(buf, "accept")) {
        LOGDEBUG("Connector received accept signal");
        cdata->accept = true;
    } else if (cmdmatch(buf, "reject")) {
        LOGDEBUG("Connector received reject signal");
        cdata->accept = false;
        if (ckpool.passthrough)
            drop_all_clients(cdata);
    } else if (cmdmatch(buf, "stats")) {
        char* msg;

        LOGDEBUG("Connector received stats request");
        msg = connector_stats(cdata, 0);
        send_unix_msg(umsg->sockd, msg);
    } else if (cmdmatch(buf, "loglevel")) {
        sscanf(buf, "loglevel=%d", &ckpool.loglevel);
    } else if (cmdmatch(buf, "passthrough")) {
        client_instance_t* client;

        ret = sscanf(buf, "passthrough=%" PRId64, &client_id);
        if (ret < 0) {
            LOGDEBUG("Connector failed to parse passthrough command: %s", buf);
            goto retry;
        }
        client = ref_client_by_id(cdata, client_id);
        if (unlikely(!client)) {
            LOGINFO("Connector failed to find client id %" PRId64 " to pass through", client_id);
            goto retry;
        }
        passthrough_client(cdata, client);
        dec_instance_ref(cdata, client);
    } else if (cmdmatch(buf, "getxfd")) {
        int fdno = -1;

        sscanf(buf, "getxfd%d", &fdno);
        if (fdno > -1 && fdno < ckpool.serverurls)
            send_fd(cdata->serverfd[fdno], umsg->sockd);
    } else if (cmdmatch(buf, "remote")) {
        int64_t client = -1;

        sscanf(buf, "remote=%ld", &client);
        add_remote_client(cdata, client);
    } else
        LOGWARNING("Unhandled connector message: %s", buf);
    goto retry;
}

void* connector(void* arg) {
    proc_instance_t* pi = (proc_instance_t*)arg;
    cdata_t*         cdata = ckzalloc(sizeof(cdata_t));
    char             newurl[INET6_ADDRSTRLEN], newport[8];
    int              threads, sockd, i, tries = 0;

    rename_proc(pi->processname);
    LOGWARNING("%s connector starting", ckpool.name);
    ckpool.cdata = cdata;

    /* serverurls always set in main: explicit SV1 and/or SV2, or one default SV1. */
    cdata->serverfd = ckalloc(sizeof(int*) * ckpool.serverurls);

    for (i = 0; i < ckpool.serverurls; i++) {
        char        oldurl[INET6_ADDRSTRLEN], oldport[8];
        char*       serverurl = ckpool.serverurl[i];
        const char* kind = "SV1";
        int         port;

#ifdef HAVE_SV2
        if (ckpool.server_sv2 && ckpool.server_sv2[i]) {
            if (ckpool.server_sv2_jd && ckpool.server_sv2_jd[i])
                kind = "SV2 Job Declaration";
            else
                kind = "SV2 mining";
        }
#endif
        if (!url_from_serverurl(serverurl, newurl, newport)) {
            LOGWARNING("Failed to extract resolved url from %s", serverurl);
            goto out;
        }
        port = atoi(newport);
        /* All even-numbered port servers are treated as highdiff ports
         * (SV1 and SV2). Port 0 means the port failed to parse, so fail
         * closed. Mirrored by report_config() for --testconfig, which
         * never gets this far; keep the two rules in step. */
        if (port && port % 2 == 0 && ckpool.server_highdiff) {
            LOGNOTICE("Highdiff server %s", serverurl);
            ckpool.server_highdiff[i] = true;
        }
        sockd = ckpool.oldconnfd[i];
        if (url_from_socket(sockd, oldurl, oldport)) {
            if (strcmp(newurl, oldurl) || strcmp(newport, oldport)) {
                LOGWARNING(
                    "Handed over socket url %s:%s does not match config %s:%s, creating new socket", oldurl, oldport,
                    newurl, newport);
                Close(sockd);
            }
        }

        do {
            if (sockd > 0)
                break;
            sockd = bind_socket(newurl, newport);
            if (sockd > 0)
                break;
            LOGWARNING("Connector failed to bind to socket, retrying in 5s");
            sleep(5);
        } while (++tries < 25);

        if (sockd < 0) {
            LOGERR("Connector failed to bind to socket for 2 minutes");
            goto out;
        }
        if (listen(sockd, 8192) < 0) {
            LOGERR("Connector failed to listen on socket");
            Close(sockd);
            goto out;
        }
        cdata->serverfd[i] = sockd;
        LOGWARNING("Bound serverurl[%d]: %s (%s)", i, serverurl, kind);
    }

    if (tries)
        LOGWARNING("Connector successfully bound after retries");

#ifdef HAVE_SV2
    /* Paths defaulted in parse_config to {socket_dir}sv2_{authority,static}.key */
    if (ckpool.sv2urls || ckpool.sv2jdurls) {
        const char* auth_path = ckpool.sv2_authority_key;
        const char* stat_path = ckpool.sv2_static_key;

        if (!sv2_init_server_keys(auth_path, stat_path))
            LOGERR("SV2 Noise key init failed — SV2 clients will be rejected");
        else
            LOGWARNING(
                "SV2 Noise server keys ready (auth=%s static=%s)", auth_path ? auth_path : "(null)",
                stat_path ? stat_path : "(null)");
    }
#endif

    cdata->cympq = create_ckmsgq("cympq", &client_yymessage_processor);

    if (ckpool.remote && !setup_upstream(cdata))
        goto out;

    cklock_init(&cdata->lock);
    cdata->pi = pi;
    cdata->nfds = 0;
    /* Set the client id to the highest serverurl count to distinguish
     * them from the server fds in epoll. */
    cdata->client_ids = ckpool.serverurls;
    threads = sysconf(_SC_NPROCESSORS_ONLN) / 2 ?: 1;
    cdata->nsenders = threads;
    cdata->csenders = ckzalloc(sizeof(csender_t) * cdata->nsenders);
    for (i = 0; i < cdata->nsenders; i++) {
        csender_t* cs = &cdata->csenders[i];

        cs->cdata = cdata;
        cs->id = i;
        mutex_init(&cs->lock);
        cond_init(&cs->cond);
        create_pthread(&cs->pth, sender, cs);
    }
    cdata->cevents = create_ckmsgqs("cevent", &client_event_processor, threads);
    create_pthread(&cdata->pth_receiver, receiver, cdata);
    cdata->start_time = time(NULL);

    ckpool.connector_ready = true;
    LOGWARNING("%s connector ready", ckpool.name);

    connector_loop(pi, cdata);
out:
    /* We should never get here unless there's a fatal error */
    LOGEMERG("Connector failure, shutting down");
    exit(1);
    return NULL;
}
