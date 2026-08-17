/*
 * Copyright 2014-2020,2023,2025-2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#include "config.h"

#include <sys/ioctl.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <ctype.h>
#include <fenv.h>
#include <getopt.h>
#include <grp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "ckpool.h"
#include "libckpool.h"
#include "generator.h"
#include "stratifier.h"
#include "connector.h"
#ifdef HAVE_SV2
#include "sv2_jdc.h"
#endif

ckpool_t                     ckpool;
static volatile sig_atomic_t ckpool_shutdown;

static bool open_logfile(void) {
    if (ckpool.logfd > 0) {
        flock(ckpool.logfd, LOCK_EX);
        fflush(ckpool.logfp);
        Close(ckpool.logfd);
    }
    ckpool.logfp = fopen(ckpool.logfilename, "ae");
    if (unlikely(!ckpool.logfp)) {
        LOGEMERG("Failed to make open log file %s", ckpool.logfilename);
        return false;
    }
    /* Make logging line buffered */
    setvbuf(ckpool.logfp, NULL, _IOLBF, 0);
    ckpool.logfd = fileno(ckpool.logfp);
    ckpool.lastopen_t = time(NULL);
    return true;
}

/* Use ckmsgqs for logging to console and files to prevent logmsg from blocking
 * on any delays. */
static void console_log(char* msg) {
    /* Add clear line only if stderr is going to console */
    if (isatty(fileno(stderr)))
        fprintf(stderr, "\33[2K\r");
    fprintf(stderr, "%s", msg);
    fflush(stderr);

    free(msg);
}

static void proclog(char* msg) {
    time_t log_t = time(NULL);

    /* Reopen log file every minute, allowing us to move/rename it and
     * create a new logfile */
    if (log_t > ckpool.lastopen_t + 60) {
        LOGDEBUG("Reopening logfile");
        open_logfile();
    }

    flock(ckpool.logfd, LOCK_EX);
    fprintf(ckpool.logfp, "%s", msg);
    flock(ckpool.logfd, LOCK_UN);

    free(msg);
}

void get_timestamp(char* stamp) {
    struct tm tm;
    tv_t      now_tv;
    int       ms;

    tv_time(&now_tv);
    ms = (int)(now_tv.tv_usec / 1000);
    localtime_r(&(now_tv.tv_sec), &tm);
    sprintf(
        stamp, "[%d-%02d-%02d %02d:%02d:%02d.%03d]", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour,
        tm.tm_min, tm.tm_sec, ms);
}

/* Log everything to the logfile, but display warnings on the console as well */
void logmsg(int loglevel, const char* fmt, ...) {
    int     logfd = ckpool.logfd;
    char *  log, *buf = NULL;
    char    stamp[128];
    va_list ap;

    if (ckpool.loglevel < loglevel || !fmt)
        return;

    va_start(ap, fmt);
    VASPRINTF(&buf, fmt, ap);
    va_end(ap);

    if (unlikely(!buf)) {
        fprintf(stderr, "Null buffer sent to logmsg\n");
        return;
    }
    if (unlikely(!strlen(buf))) {
        fprintf(stderr, "Zero length string sent to logmsg\n");
        goto out;
    }
    get_timestamp(stamp);
    if (loglevel <= LOG_ERR && errno != 0)
        ASPRINTF(&log, "%s %s with errno %d: %s\n", stamp, buf, errno, strerror(errno));
    else
        ASPRINTF(&log, "%s %s\n", stamp, buf);

    if (unlikely(!ckpool.console_logger)) {
        fprintf(stderr, "%s", log);
        goto out_free;
    }
    if (unlikely(loglevel <= LOG_WARNING))
        ckmsgq_add(ckpool.console_logger, strdup(log));
    if (likely(logfd > 0)) {
        /* Hand log over to the ckmsgq to free */
        ckmsgq_add(ckpool.logger, log);
        goto out;
    }
out_free:
    free(log);
out:
    free(buf);
}

/* Generic function for creating a message queue receiving and parsing thread */
static void* ckmsg_queue(void* arg) {
    ckmsgq_t* ckmsgq = (ckmsgq_t*)arg;
    ckmsgq_t* primary = ckmsgq->primary;

    pthread_detach(pthread_self());
    rename_proc(ckmsgq->name);
    ckmsgq->active = true;

    while (42) {
        ckmsg_t* msg;
        tv_t     now;
        ts_t     abs;

        mutex_lock(primary->lock);
        tv_time(&now);
        tv_to_ts(&abs, &now);
        abs.tv_sec++;
        if (!primary->msgs)
            cond_timedwait(primary->cond, primary->lock, &abs);
        msg = primary->msgs;
        if (msg)
            DL_DELETE(primary->msgs, msg);
        mutex_unlock(primary->lock);

        if (!msg)
            continue;
        ckmsgq->func(msg->data);
        free(msg);
    }
    return NULL;
}

ckmsgq_t* create_ckmsgq(const char* name, const void* func) {
    ckmsgq_t* ckmsgq = ckzalloc(sizeof(ckmsgq_t));

    strncpy(ckmsgq->name, name, 15);
    ckmsgq->func = func;
    ckmsgq->lock = ckalloc(sizeof(mutex_t));
    ckmsgq->cond = ckalloc(sizeof(pthread_cond_t));
    mutex_init(ckmsgq->lock);
    cond_init(ckmsgq->cond);
    ckmsgq->primary = ckmsgq;
    create_pthread(&ckmsgq->pth, ckmsg_queue, ckmsgq);

    return ckmsgq;
}

ckmsgq_t* create_ckmsgqs(const char* name, const void* func, const int count) {
    ckmsgq_t*       ckmsgq = ckzalloc(sizeof(ckmsgq_t) * count);
    mutex_t*        lock;
    pthread_cond_t* cond;
    int             i;

    lock = ckalloc(sizeof(mutex_t));
    cond = ckalloc(sizeof(pthread_cond_t));
    mutex_init(lock);
    cond_init(cond);

    for (i = 0; i < count; i++) {
        snprintf(ckmsgq[i].name, 15, "%.6s%x", name, i);
        ckmsgq[i].func = func;
        ckmsgq[i].lock = lock;
        ckmsgq[i].cond = cond;
        ckmsgq[i].primary = &ckmsgq[0]; /* all workers consume from [0] */
        create_pthread(&ckmsgq[i].pth, ckmsg_queue, &ckmsgq[i]);
    }

    return ckmsgq;
}

/* Generic function for adding messages to a ckmsgq linked list and signal the
 * ckmsgq parsing thread(s) to wake up and process it. head adds to the head
 * of the queue for high priority work. */
bool _ckmsgq_add(ckmsgq_t* ckmsgq, void* data, bool head, const char* file, const char* func, const int line) {
    ckmsg_t* msg;

    if (unlikely(!ckmsgq)) {
        LOGWARNING("Sending messages to no queue from %s %s:%d", file, func, line);
        /* Discard data if we're unlucky enough to be sending it to
         * msg queues not set up during start up */
        free(data);
        return false;
    }
    while (unlikely(!ckmsgq->active))
        cksleep_ms(10);

    msg = ckalloc(sizeof(ckmsg_t));
    msg->data = data;

    mutex_lock(ckmsgq->lock);
    ckmsgq->messages++;
    if (head)
        DL_PREPEND(ckmsgq->msgs, msg);
    else
        DL_APPEND(ckmsgq->msgs, msg);
    pthread_cond_broadcast(ckmsgq->cond);
    mutex_unlock(ckmsgq->lock);

    return true;
}

/* Return whether there are any messages queued in the ckmsgq linked list. */
bool ckmsgq_empty(ckmsgq_t* ckmsgq) {
    bool ret = true;

    if (unlikely(!ckmsgq || !ckmsgq->active))
        goto out;

    mutex_lock(ckmsgq->lock);
    if (ckmsgq->msgs)
        ret = (ckmsgq->msgs->next == ckmsgq->msgs->prev);
    mutex_unlock(ckmsgq->lock);
out:
    return ret;
}

/* Create a standalone thread that queues received unix messages for a proc
 * instance and adds them to linked list of received messages with their
 * associated receive socket, then signal the associated rmsg_cond for the
 * process to know we have more queued messages. The unix_msg_t ram must be
 * freed by the code that removes the entry from the list. */
static void* unix_receiver(void* arg) {
    proc_instance_t* pi = (proc_instance_t*)arg;
    int              rsockd = pi->us.sockd, sockd;
    char             qname[16];

    sprintf(qname, "%cunixrq", pi->processname[0]);
    rename_proc(qname);
    pthread_detach(pthread_self());

    while (42) {
        unix_msg_t* umsg;
        char*       buf;

        sockd = accept(rsockd, NULL, NULL);
        if (unlikely(sockd < 0)) {
            LOGEMERG("Failed to accept on %s socket, exiting", qname);
            break;
        }
        buf = recv_unix_msg(sockd);
        if (unlikely(!buf)) {
            Close(sockd);
            LOGWARNING("Failed to get message on %s socket", qname);
            continue;
        }
        umsg = ckalloc(sizeof(unix_msg_t));
        umsg->sockd = sockd;
        umsg->buf = buf;

        mutex_lock(&pi->rmsg_lock);
        DL_APPEND(pi->unix_msgs, umsg);
        pthread_cond_signal(&pi->rmsg_cond);
        mutex_unlock(&pi->rmsg_lock);
    }

    return NULL;
}

/* Get the next message in the receive queue, or wait up to 5 seconds for
 * the next message, returning NULL if no message is received in that time. */
unix_msg_t* get_unix_msg(proc_instance_t* pi) {
    unix_msg_t* umsg;

    mutex_lock(&pi->rmsg_lock);
    if (!pi->unix_msgs) {
        tv_t now;
        ts_t abs;

        tv_time(&now);
        tv_to_ts(&abs, &now);
        abs.tv_sec += 5;
        cond_timedwait(&pi->rmsg_cond, &pi->rmsg_lock, &abs);
    }
    umsg = pi->unix_msgs;
    if (umsg)
        DL_DELETE(pi->unix_msgs, umsg);
    mutex_unlock(&pi->rmsg_lock);

    return umsg;
}

static void create_unix_receiver(proc_instance_t* pi) {
    pthread_t pth;

    mutex_init(&pi->rmsg_lock);
    cond_init(&pi->rmsg_cond);

    create_pthread(&pth, unix_receiver, pi);
}

/* Put a sanity check on kill calls to make sure we are not sending them to
 * pid 0. */
static int kill_pid(const int pid, const int sig) {
    if (pid < 1)
        return -1;
    return kill(pid, sig);
}

static int pid_wait(const pid_t pid, const int ms) {
    tv_t start, now;
    int  ret;

    tv_time(&start);
    do {
        ret = kill_pid(pid, 0);
        if (ret)
            break;
        tv_time(&now);
    } while (ms_tvdiff(&now, &start) < ms);
    return ret;
}

#if 0
static void api_message(char **buf, int *sockd)
{
	apimsg_t *apimsg = ckalloc(sizeof(apimsg_t));

	apimsg->buf = *buf;
	*buf = NULL;
	apimsg->sockd = *sockd;
	*sockd = -1;
	ckmsgq_add(ckpool.ckpapi, apimsg);
}
#endif

/* Listen for incoming global requests. Always returns a response if possible */
static void* listener(void* arg) {
    proc_instance_t* pi = (proc_instance_t*)arg;
    unixsock_t*      us = &pi->us;
    char *           buf = NULL, *msg;
    int              sockd;

    rename_proc(pi->sockname);
retry:
    dealloc(buf);
    sockd = accept(us->sockd, NULL, NULL);
    if (sockd < 0) {
        if (!ckpool_shutdown)
            LOGERR("Failed to accept on socket in listener");
        goto out;
    }

    buf = recv_unix_msg(sockd);
    if (!buf) {
        LOGWARNING("Failed to get message in listener");
        send_unix_msg(sockd, "failed");
#if 0
	} else if (buf[0] == '{') {
		/* Any JSON messages received are for the RPC API to handle */
		api_message(&buf, &sockd);
#endif
    } else if (cmdmatch(buf, "shutdown")) {
        LOGWARNING("Listener received shutdown message, terminating ckpool");
        send_unix_msg(sockd, "exiting");
        goto out;
    } else if (cmdmatch(buf, "ping")) {
        LOGDEBUG("Listener received ping request");
        send_unix_msg(sockd, "pong");
    } else if (cmdmatch(buf, "loglevel")) {
        int loglevel;

        if (sscanf(buf, "loglevel=%d", &loglevel) != 1) {
            LOGWARNING("Failed to parse loglevel message %s", buf);
            send_unix_msg(sockd, "Failed");
        } else if (loglevel < LOG_EMERG || loglevel > LOG_DEBUG) {
            LOGWARNING("Invalid loglevel %d sent", loglevel);
            send_unix_msg(sockd, "Invalid");
        } else {
            ckpool.loglevel = loglevel;
            send_unix_msg(sockd, "success");
        }
    } else if (cmdmatch(buf, "getxfd")) {
        int fdno = -1;

        sscanf(buf, "getxfd%d", &fdno);
        connector_send_fd(fdno, sockd);
    } else if (cmdmatch(buf, "accept")) {
        LOGWARNING("Listener received accept message, accepting clients");
        send_proc(ckpool.connector, "accept");
        send_unix_msg(sockd, "accepting");
    } else if (cmdmatch(buf, "reject")) {
        LOGWARNING("Listener received reject message, rejecting clients");
        send_proc(ckpool.connector, "reject");
        send_unix_msg(sockd, "rejecting");
    } else if (cmdmatch(buf, "dropall")) {
        LOGWARNING("Listener received dropall message, disconnecting all clients");
        send_proc(ckpool.stratifier, buf);
        send_unix_msg(sockd, "dropping all");
    } else if (cmdmatch(buf, "reconnect")) {
        LOGWARNING("Listener received request to send reconnect to clients");
        send_proc(ckpool.stratifier, buf);
        send_unix_msg(sockd, "reconnecting");
    } else if (cmdmatch(buf, "restart")) {
        LOGWARNING("Listener received restart message, attempting handover");
        send_unix_msg(sockd, "restarting");
        if (!fork()) {
            if (!ckpool.handover) {
                ckpool.initial_args[ckpool.args++] = strdup("-H");
                ckpool.initial_args[ckpool.args] = NULL;
            }
            execv(ckpool.initial_args[0], (char* const*)ckpool.initial_args);
        }
    } else if (cmdmatch(buf, "stratifierstats")) {
        LOGDEBUG("Listener received stratifierstats request");
        msg = stratifier_stats(ckpool.sdata);
        send_unix_msg(sockd, msg);
        dealloc(msg);
    } else if (cmdmatch(buf, "connectorstats")) {
        LOGDEBUG("Listener received connectorstats request");
        msg = connector_stats(ckpool.cdata, 0);
        send_unix_msg(sockd, msg);
        dealloc(msg);
    } else if (cmdmatch(buf, "resetshares")) {
        LOGWARNING("Resetting best shares");
        send_proc(ckpool.stratifier, buf);
        send_unix_msg(sockd, "resetting");
    } else {
        LOGINFO("Listener received unhandled message: %s", buf);
        send_unix_msg(sockd, "unknown");
    }
    Close(sockd);
    goto retry;
out:
    dealloc(buf);
    close_unix_socket(us->sockd, us->path);
    return NULL;
}

void empty_buffer(connsock_t* cs) {
    if (cs->buf)
        cs->buf[0] = '\0';
    cs->buflen = cs->bufofs = 0;
}

int set_sendbufsize(const int fd, const int len) {
    socklen_t optlen;
    int       opt;

    optlen = sizeof(opt);
    opt = len * 4 / 3;
    setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &opt, optlen);
    getsockopt(fd, SOL_SOCKET, SO_SNDBUF, &opt, &optlen);
    opt /= 2;
    if (opt < len) {
        LOGDEBUG("Failed to set desired sendbufsize of %d unprivileged, only got %d", len, opt);
        optlen = sizeof(opt);
        opt = len * 4 / 3;
        setsockopt(fd, SOL_SOCKET, SO_SNDBUFFORCE, &opt, optlen);
        getsockopt(fd, SOL_SOCKET, SO_SNDBUF, &opt, &optlen);
        opt /= 2;
    }
    if (opt < len) {
        LOGNOTICE(
            "Failed to increase sendbufsize to %d, increase wmem_max or start %s privileged if using a remote btcd",
            len, ckpool.name);
        ckpool.wmem_warn = true;
    } else
        LOGDEBUG("Increased sendbufsize to %d of desired %d", opt, len);
    return opt;
}

int set_recvbufsize(const int fd, const int len) {
    socklen_t optlen;
    int       opt;

    optlen = sizeof(opt);
    opt = len * 4 / 3;
    setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &opt, optlen);
    getsockopt(fd, SOL_SOCKET, SO_RCVBUF, &opt, &optlen);
    opt /= 2;
    if (opt < len) {
        LOGDEBUG("Failed to set desired rcvbufsiz of %d unprivileged, only got %d", len, opt);
        optlen = sizeof(opt);
        opt = len * 4 / 3;
        setsockopt(fd, SOL_SOCKET, SO_RCVBUFFORCE, &opt, optlen);
        getsockopt(fd, SOL_SOCKET, SO_RCVBUF, &opt, &optlen);
        opt /= 2;
    }
    if (opt < len) {
        LOGNOTICE(
            "Failed to increase rcvbufsiz to %d, increase rmem_max or start %s privileged if using a remote btcd", len,
            ckpool.name);
        ckpool.rmem_warn = true;
    } else
        LOGDEBUG("Increased rcvbufsiz to %d of desired %d", opt, len);
    return opt;
}

/* If there is any cs->buflen it implies a full line was received on the last
 * pass through read_socket_line and subsequently processed, leaving
 * unprocessed data beyond cs->bufofs. Otherwise a zero buflen means there is
 * only unprocessed data of bufofs length. */
static void clear_bufline(connsock_t* cs) {
    if (unlikely(!cs->buf)) {
        socklen_t optlen = sizeof(cs->rcvbufsiz);

        cs->buf = ckzalloc(PAGESIZE);
        cs->bufsize = PAGESIZE;
        getsockopt(cs->fd, SOL_SOCKET, SO_RCVBUF, &cs->rcvbufsiz, &optlen);
        cs->rcvbufsiz /= 2;
        LOGDEBUG("connsock rcvbufsiz detected as %d", cs->rcvbufsiz);
    } else if (cs->buflen) {
        memmove(cs->buf, cs->buf + cs->bufofs, cs->buflen);
        memset(cs->buf + cs->buflen, 0, cs->bufofs);
        cs->bufofs = cs->buflen;
        cs->buflen = 0;
        cs->buf[cs->bufofs] = '\0';
    }
}

/* Maximum we will buffer from one socket before treating it as hostile or
 * broken. Only bitcoind RPC and upstream pool connections use a connsock_t;
 * miners are bounded separately at MAX_MSGSIZE by the connector. A full
 * mainnet getblocktemplate response is the largest legitimate message here at
 * roughly 10MB of JSON, so this is deliberately generous - a false positive
 * costs a template outage while any finite ceiling stops the unbounded growth
 * an upstream that never sends a newline would otherwise cause. */
#define MAX_SOCKBUF (64 * 1024 * 1024)

static bool add_buflen(connsock_t* cs, const char* readbuf, const int len) {
    int backoff = 1;
    int buflen;

    if (unlikely(cs->bufofs + len + 1 > MAX_SOCKBUF)) {
        LOGWARNING("Oversize message of %d bytes exceeds %d limit in add_buflen", cs->bufofs + len, MAX_SOCKBUF);
        return false;
    }
    buflen = round_up_page(cs->bufofs + len + 1);
    while (cs->bufsize < buflen) {
        char* newbuf = realloc(cs->buf, buflen);

        if (likely(newbuf)) {
            cs->bufsize = buflen;
            cs->buf = newbuf;
            break;
        }
        if (backoff == 1)
            fprintf(stderr, "Failed to realloc %d in read_socket_line, retrying\n", (int)buflen);
        cksleep_ms(backoff);
        /* Cap the backoff. Doubling without bound is undefined once it
         * overflows and turns a transient allocation failure into
         * effectively permanent sleep. */
        if (backoff < 1000)
            backoff <<= 1;
    }
    /* Increase receive buffer if possible to larger than the largest
     * message we're likely to buffer */
    if (unlikely(!ckpool.rmem_warn && buflen > cs->rcvbufsiz))
        cs->rcvbufsiz = set_recvbufsize(cs->fd, buflen);

    memcpy(cs->buf + cs->bufofs, readbuf, len);
    cs->bufofs += len;
    cs->buf[cs->bufofs] = '\0';
    return true;
}

/* Receive as much data is currently available without blocking into a connsock
 * buffer. Returns total length of data read. */
static int recv_available(connsock_t* cs) {
    char readbuf[PAGESIZE];
    int  len = 0, ret;

    do {
        ret = recv(cs->fd, readbuf, PAGESIZE - 4, MSG_DONTWAIT);
        if (ret > 0) {
            /* Stop as soon as the buffer ceiling is hit rather than
             * draining the socket into an ever growing buffer. */
            if (unlikely(!add_buflen(cs, readbuf, ret)))
                return -1;
            len += ret;
        }
    } while (ret > 0);

    return len;
}

/* Read from a socket into cs->buf till we get an '\n', converting it to '\0'
 * and storing how much extra data we've received, to be moved to the beginning
 * of the buffer for use on the next receive. Returns length of the line if a
 * whole line is received, zero if none/some data is received without an EOL
 * and -1 on error. */
int read_socket_line(connsock_t* cs, float* timeout) {
    bool  quiet = ckpool.proxy | ckpool.remote;
    char* eom = NULL;
    tv_t  start, now;
    float diff;
    int   ret;

    clear_bufline(cs);
    /* Only a buffer overflow is fatal here; no data yet is normal and
     * returns zero. */
    if (unlikely(recv_available(cs) < 0)) {
        ret = -1;
        goto out;
    }
    eom = memchr(cs->buf, '\n', cs->bufofs);

    tv_time(&start);

    while (!eom) {
        if (unlikely(cs->fd < 0)) {
            ret = -1;
            goto out;
        }

        if (*timeout < 0) {
            if (quiet)
                LOGINFO("Timed out in read_socket_line");
            else
                LOGERR("Timed out in read_socket_line");
            ret = 0;
            goto out;
        }
        ret = wait_read_select(cs->fd, *timeout);
        if (ret < 1) {
            if (quiet)
                LOGINFO("Select %s in read_socket_line", !ret ? "timed out" : "failed");
            else
                LOGERR("Select %s in read_socket_line", !ret ? "timed out" : "failed");
            goto out;
        }
        ret = recv_available(cs);
        if (ret < 1) {
            /* If we have done wait_read_select there should be
             * something to read and if we get nothing it means the
             * socket is closed. */
            if (quiet)
                LOGINFO("Failed to recv in read_socket_line");
            else
                LOGERR("Failed to recv in read_socket_line");
            ret = -1;
            goto out;
        }
        eom = memchr(cs->buf, '\n', cs->bufofs);
        tv_time(&now);
        diff = tvdiff(&now, &start);
        copy_tv(&start, &now);
        *timeout -= diff;
    }
    ret = eom - cs->buf;

    cs->buflen = cs->buf + cs->bufofs - eom - 1;
    if (cs->buflen)
        cs->bufofs = eom - cs->buf + 1;
    else
        cs->bufofs = 0;
    *eom = '\0';
out:
    if (ret < 0) {
        empty_buffer(cs);
        dealloc(cs->buf);
    }
    return ret;
}

/* We used to send messages between each proc_instance via unix sockets when
 * ckpool was a multi-process model but that is no longer required so we can
 * place the messages directly on the other proc_instance's queue until we
 * deprecate this mechanism. */
void _queue_proc(proc_instance_t* pi, const char* msg, const char* file, const char* func, const int line) {
    unix_msg_t* umsg;

    if (unlikely(!msg || !strlen(msg))) {
        LOGWARNING("Null msg passed to queue_proc from %s %s:%d", file, func, line);
        return;
    }
    umsg = ckalloc(sizeof(unix_msg_t));
    umsg->sockd = -1;
    umsg->buf = strdup(msg);

    mutex_lock(&pi->rmsg_lock);
    DL_APPEND(pi->unix_msgs, umsg);
    pthread_cond_signal(&pi->rmsg_cond);
    mutex_unlock(&pi->rmsg_lock);
}

/* Send a single message to a process instance and retrieve the response, then
 * close the socket. */
char* _send_recv_proc(
    const proc_instance_t* pi,
    const char*            msg,
    int                    writetimeout,
    int                    readtimedout,
    const char*            file,
    const char*            func,
    const int              line) {
    char *path = pi->us.path, *buf = NULL;
    int   sockd;

    if (unlikely(!path || !strlen(path))) {
        LOGERR("Attempted to send message %s to null path in send_proc", msg ? msg : "");
        goto out;
    }
    if (unlikely(!msg || !strlen(msg))) {
        LOGERR("Attempted to send null message to socket %s in send_proc", path);
        goto out;
    }
    sockd = open_unix_client(path);
    if (unlikely(sockd < 0)) {
        LOGWARNING("Failed to open socket %s in send_recv_proc", path);
        goto out;
    }
    if (unlikely(!_send_unix_msg(sockd, msg, writetimeout, file, func, line)))
        LOGWARNING("Failed to send %s to socket %s", msg, path);
    else
        buf = _recv_unix_msg(sockd, readtimedout, readtimedout, file, func, line);
    Close(sockd);
out:
    if (unlikely(!buf))
        LOGERR("Failure in send_recv_proc from %s %s:%d", file, func, line);
    return buf;
}

static const char* rpc_method(const char* rpc_req) {
    const char* ptr = strchr(rpc_req, ':');
    if (ptr)
        return ptr + 1;
    return rpc_req;
}

/* All of these calls are made to bitcoind which prefers open/close instead
 * of persistent connections so cs->fd is always invalid. */
static yyjson_doc* _yyjson_rpc_call(connsock_t* cs, const char* rpc_req, const bool info_only) {
    float       timeout = RPC_TIMEOUT;
    char*       http_req = NULL;
    char*       warning = NULL;
    yyjson_doc* doc = NULL;
    tv_t        stt_tv, fin_tv;
    double      elapsed;
    int         len, ret;

    /* Serialise all calls in case we use cs from multiple threads */
    cksem_wait(&cs->sem);
    cs->fd = connect_socket(cs->url, cs->port);
    if (unlikely(cs->fd < 0)) {
        ASPRINTF(&warning, "Unable to connect socket to %s:%s in %s", cs->url, cs->port, __func__);
        goto out;
    }
    if (unlikely(!cs->url)) {
        ASPRINTF(&warning, "No URL in %s", __func__);
        goto out;
    }
    if (unlikely(!cs->port)) {
        ASPRINTF(&warning, "No port in %s", __func__);
        goto out;
    }
    if (unlikely(!cs->auth)) {
        ASPRINTF(&warning, "No auth in %s", __func__);
        goto out;
    }
    if (unlikely(!rpc_req)) {
        ASPRINTF(&warning, "Null rpc_req passed to %s", __func__);
        goto out;
    }
    len = strlen(rpc_req);
    if (unlikely(!len)) {
        ASPRINTF(&warning, "Zero length rpc_req passed to %s", __func__);
        goto out;
    }
    /* ASPRINTF sizes for auth/url/port of any length; avoids fixed
     * ckalloc+sprintf and the null-destination format-overflow warning. */
    ASPRINTF(
        &http_req,
        "POST / HTTP/1.1\r\n"
        "Authorization: Basic %s\r\n"
        "Host: %s:%s\r\n"
        "Content-type: application/json\n"
        "Content-Length: %d\r\n\r\n%s",
        cs->auth, cs->url, cs->port, len, rpc_req);

    len = strlen(http_req);
    tv_time(&stt_tv);
    ret = write_socket(cs->fd, http_req, len);
    if (ret != len) {
        tv_time(&fin_tv);
        elapsed = tvdiff(&fin_tv, &stt_tv);
        ASPRINTF(&warning, "Failed to write to socket in %s (%.10s...) %.3fs", __func__, rpc_method(rpc_req), elapsed);
        goto out_empty;
    }
    ret = read_socket_line(cs, &timeout);
    if (ret < 1) {
        tv_time(&fin_tv);
        elapsed = tvdiff(&fin_tv, &stt_tv);
        ASPRINTF(&warning, "Failed to read socket line in %s (%.10s...) %.3fs", __func__, rpc_method(rpc_req), elapsed);
        goto out_empty;
    }
    if (strncasecmp(cs->buf, "HTTP/1.1 200 OK", 15)) {
        tv_time(&fin_tv);
        elapsed = tvdiff(&fin_tv, &stt_tv);
        ASPRINTF(&warning, "HTTP response to (%.10s...) %.3fs not ok: %s", rpc_method(rpc_req), elapsed, cs->buf);
        timeout = 0;
        /* Look for a json response if there is one */
        while (read_socket_line(cs, &timeout) > 0) {
            timeout = 0;
            if (*cs->buf != '{')
                continue;
            free(warning);
            /* Replace the warning with the json response */
            ASPRINTF(&warning, "JSON response to (%.10s...) %.3fs not ok: %s", rpc_method(rpc_req), elapsed, cs->buf);
            break;
        }
        goto out_empty;
    }
    do {
        ret = read_socket_line(cs, &timeout);
        if (ret < 1) {
            tv_time(&fin_tv);
            elapsed = tvdiff(&fin_tv, &stt_tv);
            ASPRINTF(
                &warning, "Failed to read http socket lines in %s (%.10s...) %.3fs", __func__, rpc_method(rpc_req),
                elapsed);
            goto out_empty;
        }
    } while (strncmp(cs->buf, "{", 1));
    tv_time(&fin_tv);
    elapsed = tvdiff(&fin_tv, &stt_tv);
    if (elapsed > 5.0) {
        ASPRINTF(
            &warning, "HTTP socket read+write took %.3fs in %s (%.10s...)", elapsed, __func__, rpc_method(rpc_req));
    }

    doc = yyjson_read(cs->buf, strlen(cs->buf), 0);
    if (!doc)
        ASPRINTF(&warning, "JSON decode (%.10s...) failed", rpc_method(rpc_req));
out_empty:
    empty_socket(cs->fd);
    empty_buffer(cs);
out:
    if (warning) {
        if (info_only)
            LOGINFO("%s", warning);
        else
            LOGWARNING("%s", warning);
        free(warning);
    }
    Close(cs->fd);
    free(http_req);
    dealloc(cs->buf);
    cksem_post(&cs->sem);
    return doc;
}

yyjson_doc* yyjson_rpc_call(connsock_t* cs, const char* rpc_req) {
    return _yyjson_rpc_call(cs, rpc_req, false);
}

yyjson_doc* yyjson_rpc_response(connsock_t* cs, const char* rpc_req) {
    return _yyjson_rpc_call(cs, rpc_req, true);
}

/* For when we are submitting information that is not important and don't care
 * about the response. */
void yyjson_rpc_msg(connsock_t* cs, const char* rpc_req) {
    yyjson_doc* doc = _yyjson_rpc_call(cs, rpc_req, true);

    /* We don't care about the result */
    yyjson_doc_free(doc);
}

static void terminate_oldpid(const proc_instance_t* pi, const pid_t oldpid) {
    if (!ckpool.killold) {
        quit(
            1, "Process %s pid %d still exists, start ckpool with -H to get a handover or -k if you wish to kill it",
            pi->processname, oldpid);
    }
    LOGNOTICE("Terminating old process %s pid %d", pi->processname, oldpid);
    if (kill_pid(oldpid, 15))
        quit(1, "Unable to kill old process %s pid %d", pi->processname, oldpid);
    LOGWARNING("Terminating old process %s pid %d", pi->processname, oldpid);
    if (pid_wait(oldpid, 500))
        return;
    LOGWARNING("Old process %s pid %d failed to respond to terminate request, killing", pi->processname, oldpid);
    if (kill_pid(oldpid, 9) || !pid_wait(oldpid, 3000))
        quit(1, "Unable to kill old process %s pid %d", pi->processname, oldpid);
}

/* As _send_json_msg but for yyjson docs */
bool _send_yyjson_msg(connsock_t* cs, yyjson_mut_doc* doc, const char* file, const char* func, const int line) {
    bool   ret = false;
    size_t len;
    int    sent;
    char*  s;

    if (unlikely(!doc)) {
        LOGWARNING("Empty doc in send_yyjson_msg from %s %s:%d", file, func, line);
        goto out;
    }
    s = yyjson_mut_write(doc, YYJSON_WRITE_NEWLINE_AT_END, &len);
    if (unlikely(!s)) {
        LOGWARNING("Empty yyjson write in send_yyjson_msg from %s %s:%d", file, func, line);
        goto out;
    }
    LOGDEBUG("Sending json msg: %s", s);
    sent = write_socket(cs->fd, s, len);
    if (sent != (int)len) {
        LOGNOTICE("Failed to send %d bytes sent %d in send_yyjson_msg", (int)len, sent);
        goto out_free;
    }
    ret = true;
out_free:
    dealloc(s);
out:
    return ret;
}

/* As json_msg_result but parsing into an immutable yyjson doc */
yyjson_doc* yyjson_msg_result(const char* msg, yyjson_val** res_val, yyjson_val** err_val) {
    yyjson_val* root;
    yyjson_doc* doc;

    *res_val = *err_val = NULL;
    doc = yyjson_read(msg, strlen(msg), 0);
    if (!doc) {
        LOGWARNING("Json decode failed: %s", msg);
        return NULL;
    }
    root = yyjson_doc_get_root(doc);
    *err_val = yyjson_obj_get(root, "error");
    *res_val = yyjson_obj_get(root, "result");
    /* (null) is a valid result while no value is an error, so mask out
     * (null) and only handle lack of result */
    if (yyjson_is_null(*res_val))
        *res_val = NULL;
    else if (!*res_val) {
        char* ss;

        if (*err_val)
            ss = yyjson_val_write(*err_val, 0, NULL);
        else
            ss = strdup("(unknown reason)");

        LOGNOTICE("JSON-RPC decode of json_result failed: %s", ss);
        free(ss);
    }
    return doc;
}

/* Open the file in path, check if there is a pid in there that still exists
 * and if not, write the pid into that file. */
static bool write_pid(const char* path, proc_instance_t* pi, const pid_t pid, const pid_t oldpid) {
    FILE* fp;

    if (ckpool.handover && oldpid && !pid_wait(oldpid, 500)) {
        LOGWARNING("Old process pid %d failed to shutdown cleanly, terminating", oldpid);
        terminate_oldpid(pi, oldpid);
    }

    fp = fopen(path, "we");
    if (!fp) {
        LOGERR("Failed to open file %s", path);
        return false;
    }
    fprintf(fp, "%d", pid);
    fclose(fp);

    return true;
}

static void name_process_sockname(unixsock_t* us, const proc_instance_t* pi) {
    us->path = strdup(ckpool.socket_dir);
    realloc_strcat(&us->path, pi->sockname);
}

static void open_process_sock(const proc_instance_t* pi, unixsock_t* us) {
    LOGDEBUG("Opening %s", us->path);
    us->sockd = open_unix_server(us->path);
    if (unlikely(us->sockd < 0))
        quit(1, "Failed to open %s socket", pi->sockname);
    if (chown(us->path, -1, ckpool.gr_gid))
        quit(1, "Failed to set %s to group id %d", us->path, ckpool.gr_gid);
}

static void create_process_unixsock(proc_instance_t* pi) {
    unixsock_t* us = &pi->us;

    name_process_sockname(us, pi);
    open_process_sock(pi, us);
}

static void write_namepid(proc_instance_t* pi) {
    char s[256];

    pi->pid = getpid();
    sprintf(s, "%s%s.pid", ckpool.socket_dir, pi->processname);
    if (!write_pid(s, pi, pi->pid, pi->oldpid))
        quit(1, "Failed to write %s pid %d", pi->processname, pi->pid);
}

static void rm_namepid(const proc_instance_t* pi) {
    char s[256];

    sprintf(s, "%s%s.pid", ckpool.socket_dir, pi->processname);
    unlink(s);
}

static void launch_logger(void) {
    ckpool.logger = create_ckmsgq("logger", &proclog);
    ckpool.console_logger = create_ckmsgq("conlog", &console_log);
}

static void clean_up(void) {
    rm_namepid(&ckpool.main);
    dealloc(ckpool.socket_dir);
}

static void sighandler(const int sig) {
    signal(sig, SIG_IGN);
    signal(SIGTERM, SIG_IGN);
    ckpool_shutdown = sig;
    /* Interrupt the blocking accept() in listener() without calling any
     * non-async-signal-safe functions. main() logs and cleans up after
     * join_pthread returns. */
    shutdown(ckpool.main.us.sockd, SHUT_RDWR);
}

/* As _json_get_string but for a yyjson entry that may not be within an
 * object */
static bool _yyjson_get_string(char** store, yyjson_val* entry, const char* res) {
    const char* buf;

    *store = NULL;
    if (!entry || yyjson_is_null(entry)) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_is_str(entry)) {
        LOGWARNING("Json entry %s is not a string", res);
        return false;
    }
    buf = yyjson_get_str(entry);
    LOGDEBUG("Json found entry %s: %s", res, buf);
    *store = strdup(buf);
    return true;
}

/* Used when there must be a valid string */
static void json_get_configstring(char** store, yyjson_val* val, const char* res) {
    bool ret = _yyjson_get_string(store, yyjson_obj_get(val, res), res);

    if (!ret) {
        LOGEMERG("Invalid config string or missing object for %s", res);
        exit(1);
    }
}

/* As the json_get_* helpers above but for immutable yyjson objects */
bool yyjson_obj_get_string(char** store, yyjson_val* val, const char* res) {
    yyjson_val* entry = yyjson_obj_get(val, res);

    *store = NULL;
    if (!entry || yyjson_is_null(entry)) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_is_str(entry)) {
        LOGWARNING("Json entry %s is not a string", res);
        return false;
    }
    *store = strdup(yyjson_get_str(entry));
    LOGDEBUG("Json found entry %s: %s", res, *store);
    return true;
}

bool yyjson_obj_get_int64(int64_t* store, yyjson_val* val, const char* res) {
    yyjson_val* entry = yyjson_obj_get(val, res);

    if (!entry) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_is_int(entry)) {
        LOGINFO("Json entry %s is not an integer", res);
        return false;
    }
    *store = yyjson_get_sint(entry);
    LOGDEBUG("Json found entry %s: %" PRId64, res, *store);
    return true;
}

bool yyjson_obj_get_int(int* store, yyjson_val* val, const char* res) {
    yyjson_val* entry = yyjson_obj_get(val, res);

    if (!entry) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_is_int(entry)) {
        LOGWARNING("Json entry %s is not an integer", res);
        return false;
    }
    *store = yyjson_get_sint(entry);
    LOGDEBUG("Json found entry %s: %d", res, *store);
    return true;
}

bool yyjson_obj_get_double(double* store, yyjson_val* val, const char* res) {
    yyjson_val* entry = yyjson_obj_get(val, res);

    if (!entry) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_is_num(entry)) {
        LOGWARNING("Json entry %s is not a double", res);
        return false;
    }
    *store = yyjson_get_num(entry);
    LOGDEBUG("Json found entry %s: %f", res, *store);
    return true;
}

bool yyjson_obj_get_uint32(uint32_t* store, yyjson_val* val, const char* res) {
    yyjson_val* entry = yyjson_obj_get(val, res);

    if (!entry) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_is_int(entry)) {
        LOGWARNING("Json entry %s is not an integer", res);
        return false;
    }
    *store = (uint32_t)yyjson_get_uint(entry);
    LOGDEBUG("Json found entry %s: %u", res, *store);
    return true;
}

bool yyjson_obj_get_bool(bool* store, yyjson_val* val, const char* res) {
    yyjson_val* entry = yyjson_obj_get(val, res);

    if (!entry) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_is_bool(entry)) {
        LOGINFO("Json entry %s is not a boolean", res);
        return false;
    }
    *store = yyjson_get_bool(entry);
    LOGDEBUG("Json found entry %s: %s", res, *store ? "true" : "false");
    return true;
}

/* As above but for mutable yyjson objects */
bool yyjson_mut_obj_get_string(char** store, yyjson_mut_val* val, const char* res) {
    yyjson_mut_val* entry = yyjson_mut_obj_get(val, res);

    *store = NULL;
    if (!entry || yyjson_mut_is_null(entry)) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_mut_is_str(entry)) {
        LOGWARNING("Json entry %s is not a string", res);
        return false;
    }
    *store = strdup(yyjson_mut_get_str(entry));
    LOGDEBUG("Json found entry %s: %s", res, *store);
    return true;
}

bool yyjson_mut_obj_get_int64(int64_t* store, yyjson_mut_val* val, const char* res) {
    yyjson_mut_val* entry = yyjson_mut_obj_get(val, res);

    if (!entry) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_mut_is_int(entry)) {
        LOGINFO("Json entry %s is not an integer", res);
        return false;
    }
    *store = yyjson_mut_get_sint(entry);
    LOGDEBUG("Json found entry %s: %" PRId64, res, *store);
    return true;
}

bool yyjson_mut_obj_get_int(int* store, yyjson_mut_val* val, const char* res) {
    yyjson_mut_val* entry = yyjson_mut_obj_get(val, res);

    if (!entry) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_mut_is_int(entry)) {
        LOGWARNING("Json entry %s is not an integer", res);
        return false;
    }
    *store = yyjson_mut_get_sint(entry);
    LOGDEBUG("Json found entry %s: %d", res, *store);
    return true;
}

bool yyjson_mut_obj_get_double(double* store, yyjson_mut_val* val, const char* res) {
    yyjson_mut_val* entry = yyjson_mut_obj_get(val, res);

    if (!entry) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_mut_is_num(entry)) {
        LOGWARNING("Json entry %s is not a double", res);
        return false;
    }
    *store = yyjson_mut_get_num(entry);
    LOGDEBUG("Json found entry %s: %f", res, *store);
    return true;
}

bool yyjson_mut_obj_get_uint32(uint32_t* store, yyjson_mut_val* val, const char* res) {
    yyjson_mut_val* entry = yyjson_mut_obj_get(val, res);

    if (!entry) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_mut_is_int(entry)) {
        LOGWARNING("Json entry %s is not an integer", res);
        return false;
    }
    *store = (uint32_t)yyjson_mut_get_uint(entry);
    LOGDEBUG("Json found entry %s: %u", res, *store);
    return true;
}

bool yyjson_mut_obj_get_bool(bool* store, yyjson_mut_val* val, const char* res) {
    yyjson_mut_val* entry = yyjson_mut_obj_get(val, res);

    if (!entry) {
        LOGDEBUG("Json did not find entry %s", res);
        return false;
    }
    if (!yyjson_mut_is_bool(entry)) {
        LOGINFO("Json entry %s is not a boolean", res);
        return false;
    }
    *store = yyjson_mut_get_bool(entry);
    LOGDEBUG("Json found entry %s: %s", res, *store ? "true" : "false");
    return true;
}

bool yyjson_mut_obj_getdel_int(int* store, yyjson_mut_val* val, const char* res) {
    bool ret;

    ret = yyjson_mut_obj_get_int(store, val, res);
    if (ret)
        yyjson_mut_obj_remove_key(val, res);
    return ret;
}

bool yyjson_mut_obj_getdel_int64(int64_t* store, yyjson_mut_val* val, const char* res) {
    bool ret;

    ret = yyjson_mut_obj_get_int64(store, val, res);
    if (ret)
        yyjson_mut_obj_remove_key(val, res);
    return ret;
}

static void parse_btcds(yyjson_val* arr_val, const int arr_size) {
    yyjson_val* val;
    int         i;

    ckpool.btcds = arr_size;
    ckpool.btcdurl = ckzalloc(sizeof(char*) * arr_size);
    ckpool.btcdauth = ckzalloc(sizeof(char*) * arr_size);
    ckpool.btcdpass = ckzalloc(sizeof(char*) * arr_size);
    ckpool.btcdnotify = ckzalloc(sizeof(bool*) * arr_size);
    for (i = 0; i < arr_size; i++) {
        val = yyjson_arr_get(arr_val, i);
        json_get_configstring(&ckpool.btcdurl[i], val, "url");
        json_get_configstring(&ckpool.btcdauth[i], val, "auth");
        json_get_configstring(&ckpool.btcdpass[i], val, "pass");
        yyjson_obj_get_bool(&ckpool.btcdnotify[i], val, "notify");
    }
}

/* Read one string from the database config object. Unlike the generic config
 * helpers this never echoes the value, because the object holds a password. */
static char* db_conf_str(yyjson_val* db_val, const char* key) {
    yyjson_val* entry = yyjson_obj_get(db_val, key);
    const char* str;

    if (!entry || !yyjson_is_str(entry))
        return NULL;
    str = yyjson_get_str(entry);
    if (!str || !strlen(str))
        return NULL;
    return strdup(str);
}

/* Build the libpq conninfo string from the optional "database" config object.
 * All four fields are required; a partial object leaves db_conn_str NULL and
 * -L is refused in main(). The assembled string holds the password, so it is
 * never logged. */
static void parse_database(yyjson_val* json_conf) {
    yyjson_val* db_val = yyjson_obj_get(json_conf, "database");
    int         len;

    if (!db_val || !yyjson_is_obj(db_val))
        return;

    /* Every field is optional here rather than fatal, so that a partial
     * database object only disables db logging; main() decides whether
     * that is an error based on whether --log-shares was given. */
    ckpool.database_host = db_conf_str(db_val, "hostname");
    ckpool.database_name = db_conf_str(db_val, "dbname");
    /* "dbtable" is the original spelling of this key. It has always been
     * passed to libpq as dbname, so accept it as an alias. */
    if (!ckpool.database_name)
        ckpool.database_name = db_conf_str(db_val, "dbtable");
    ckpool.database_user = db_conf_str(db_val, "user");
    ckpool.database_pass = db_conf_str(db_val, "pass");

    if (!ckpool.database_host || !ckpool.database_name || !ckpool.database_user || !ckpool.database_pass) {
        LOGWARNING("Incomplete database configuration, needs hostname, dbname, user and pass");
        return;
    }

    len = strlen(ckpool.database_host) + strlen(ckpool.database_name) + strlen(ckpool.database_user) +
          strlen(ckpool.database_pass) + 32;
    ckpool.db_conn_str = ckalloc(len);
    snprintf(
        ckpool.db_conn_str, len, "host=%s dbname=%s user=%s password=%s", ckpool.database_host, ckpool.database_name,
        ckpool.database_user, ckpool.database_pass);
    LOGDEBUG("Database configured for %s@%s/%s", ckpool.database_user, ckpool.database_host, ckpool.database_name);
}

static void parse_proxies(yyjson_val* arr_val, const int arr_size) {
    yyjson_val* val;
    int         i;

    ckpool.proxies = arr_size;
    ckpool.proxyurl = ckzalloc(sizeof(char*) * arr_size);
    ckpool.proxyauth = ckzalloc(sizeof(char*) * arr_size);
    ckpool.proxypass = ckzalloc(sizeof(char*) * arr_size);
    ckpool.proxyjds = ckzalloc(sizeof(char*) * arr_size);
    for (i = 0; i < arr_size; i++) {
        val = yyjson_arr_get(arr_val, i);
        json_get_configstring(&ckpool.proxyurl[i], val, "url");
        json_get_configstring(&ckpool.proxyauth[i], val, "auth");
        if (!yyjson_obj_get_string(&ckpool.proxypass[i], val, "pass"))
            ckpool.proxypass[i] = strdup("");
        yyjson_obj_get_string(&ckpool.proxyjds[i], val, "jds");
    }
}

static bool parse_serverurls(yyjson_val* arr_val) {
    bool ret = false;
    int  arr_size, i;

    if (!arr_val)
        goto out;
    if (!yyjson_is_arr(arr_val)) {
        LOGINFO("Unable to parse serverurl entries as an array");
        goto out;
    }
    arr_size = yyjson_arr_size(arr_val);
    if (!arr_size) {
        LOGWARNING("Serverurl array empty");
        goto out;
    }
    ckpool.serverurls = arr_size;
    ckpool.serverurl = ckalloc(sizeof(char*) * arr_size);
    ckpool.server_highdiff = ckzalloc(sizeof(bool) * arr_size);
    ckpool.nodeserver = ckzalloc(sizeof(bool) * arr_size);
    ckpool.trusted = ckzalloc(sizeof(bool) * arr_size);
#ifdef HAVE_SV2
    ckpool.server_sv2 = ckzalloc(sizeof(bool) * arr_size);
    ckpool.server_sv2_jd = ckzalloc(sizeof(bool) * arr_size);
#endif
    for (i = 0; i < arr_size; i++) {
        yyjson_val* val = yyjson_arr_get(arr_val, i);

        if (!_yyjson_get_string(&ckpool.serverurl[i], val, "serverurl"))
            LOGWARNING("Invalid serverurl entry number %d", i);
    }
    ret = true;
out:
    return ret;
}

/* True if host means "all interfaces" for clash purposes. */
static bool listen_host_is_wildcard(const char* host) {
    if (!host || !host[0])
        return true;
    if (!strcmp(host, "0.0.0.0") || !strcmp(host, "*") || !strcmp(host, "::") || !strcmp(host, "[::]"))
        return true;
    return false;
}

/*
 * Ensure no two serverurl[] entries fight for the same listen port on
 * overlapping hosts (exact host match, or either side is 0.0.0.0/::).
 * Call after SV1 + SV2/JD urls are merged. Aborts on clash.
 */
static void check_listen_port_clashes(void) {
    int i, j;

    for (i = 0; i < ckpool.serverurls; i++) {
        char *host_i = NULL, *port_i = NULL;
        int   p_i;

        if (!ckpool.serverurl[i] || !ckpool.serverurl[i][0])
            continue;
        if (!extract_sockaddr(ckpool.serverurl[i], &host_i, &port_i)) {
            LOGWARNING("Cannot parse listen URL for clash check: %s", ckpool.serverurl[i]);
            continue;
        }
        p_i = atoi(port_i);
        for (j = i + 1; j < ckpool.serverurls; j++) {
            char *host_j = NULL, *port_j = NULL;
            int   p_j;
            bool  clash;

            if (!ckpool.serverurl[j] || !ckpool.serverurl[j][0])
                continue;
            if (!extract_sockaddr(ckpool.serverurl[j], &host_j, &port_j)) {
                LOGWARNING("Cannot parse listen URL for clash check: %s", ckpool.serverurl[j]);
                continue;
            }
            p_j = atoi(port_j);
            clash = false;
            if (p_i > 0 && p_i == p_j) {
                if (listen_host_is_wildcard(host_i) || listen_host_is_wildcard(host_j) || !strcasecmp(host_i, host_j))
                    clash = true;
            }
            if (clash) {
                quit(
                    0,
                    "Listen port clash: \"%s\" and \"%s\" both "
                    "use port %d (use distinct ports for SV1 "
                    "serverurl / SV2 sv2url / SV2 sv2jdurl, "
                    "defaults 3333 / 3336 / 3337)",
                    ckpool.serverurl[i], ckpool.serverurl[j], p_i);
            }
            dealloc(host_j);
            dealloc(port_j);
        }
        dealloc(host_i);
        dealloc(port_i);
    }
}

#ifdef HAVE_SV2
/* Parse an array of URL strings into *urls / *nurls (like serverurl). */
static bool parse_sv2_urllist(yyjson_val* arr_val, char*** urls, int* nurls, const char* name) {
    int arr_size, i;

    if (!arr_val)
        return false;
    if (!yyjson_is_arr(arr_val)) {
        LOGINFO("Unable to parse %s entries as an array", name);
        return false;
    }
    arr_size = yyjson_arr_size(arr_val);
    if (!arr_size) {
        LOGWARNING("%s array empty", name);
        return false;
    }
    *nurls = arr_size;
    *urls = ckalloc(sizeof(char*) * arr_size);
    for (i = 0; i < arr_size; i++) {
        yyjson_val* val = yyjson_arr_get(arr_val, i);

        if (!_yyjson_get_string(&(*urls)[i], val, name))
            LOGWARNING("Invalid %s entry number %d", name, i);
    }
    return true;
}

static void free_sv2_urllist(char*** urls, int* nurls) {
    int i;

    if (*urls) {
        for (i = 0; i < *nurls; i++)
            dealloc((*urls)[i]);
        dealloc(*urls);
    }
    *nurls = 0;
}

/* Grow bool parallel arrays to new_n, zero-filling any new slots. */
static bool* grow_bools(bool* old, int old_n, int new_n) {
    bool* p = ckzalloc(sizeof(bool) * new_n);

    if (old && old_n > 0)
        memcpy(p, old, sizeof(bool) * ((old_n < new_n) ? old_n : new_n));
    free(old);
    return p;
}

/*
 * SV2 mining/JD *server* binds are allowed only in normal pool mode or
 * btcsolo. All proxy-family modes set ckpool.proxy (proxy, userproxy,
 * passthrough, node, redirector). Upstream SV2 as a *client* (ckproxy) is
 * independent of this.
 */
static bool sv2_server_listen_allowed(void) {
    return !ckpool.proxy;
}

/* Append SV2 mining (jd=false) or JD (jd=true) binds onto serverurl[]. */
static void append_sv2_serverurls(char** urls, int nurl, bool jd, int defport) {
    int i, n, total, added = 0;

    if (!urls || nurl <= 0)
        return;
    if (!sv2_server_listen_allowed()) {
        LOGWARNING("Not binding SV2 %s: server listen is pool/solo only", jd ? "Job Declaration" : "mining");
        return;
    }

    n = ckpool.serverurls;
    total = n + nurl;
    ckpool.serverurl = realloc(ckpool.serverurl, sizeof(char*) * total);
    ckpool.server_highdiff = grow_bools(ckpool.server_highdiff, n, total);
    ckpool.nodeserver = grow_bools(ckpool.nodeserver, n, total);
    ckpool.trusted = grow_bools(ckpool.trusted, n, total);
    ckpool.server_sv2 = grow_bools(ckpool.server_sv2, n, total);
    ckpool.server_sv2_jd = grow_bools(ckpool.server_sv2_jd, n, total);

    for (i = 0; i < nurl; i++) {
        char* url;
        int   idx;

        if (!urls[i] || !urls[i][0]) {
            LOGWARNING("Empty SV2 %s entry %d, skipping", jd ? "jdurl" : "url", i);
            continue;
        }
        if (!strchr(urls[i], ':'))
            ASPRINTF(&url, "%s:%d", urls[i], defport);
        else
            url = strdup(urls[i]);
        idx = n + added;
        ckpool.serverurl[idx] = url;
        ckpool.server_highdiff[idx] = false;
        ckpool.nodeserver[idx] = false;
        ckpool.trusted[idx] = false;
        ckpool.server_sv2[idx] = true;
        ckpool.server_sv2_jd[idx] = jd;
        added++;
        LOGWARNING("SV2 %s listen enabled on %s", jd ? "Job Declaration" : "mining", url);
    }
    ckpool.serverurls = n + added;
}
#endif

static void parse_nodeservers(yyjson_val* arr_val) {
    int arr_size, i, j, total_urls;

    if (!arr_val)
        return;
    if (!yyjson_is_arr(arr_val)) {
        LOGWARNING("Unable to parse nodeservers entries as an array");
        return;
    }
    arr_size = yyjson_arr_size(arr_val);
    if (!arr_size) {
        LOGWARNING("Nodeserver array empty");
        return;
    }
    total_urls = ckpool.serverurls + arr_size;
    ckpool.serverurl = realloc(ckpool.serverurl, sizeof(char*) * total_urls);
    ckpool.nodeserver = realloc(ckpool.nodeserver, sizeof(bool) * total_urls);
    ckpool.trusted = realloc(ckpool.trusted, sizeof(bool) * total_urls);
    for (i = 0, j = ckpool.serverurls; j < total_urls; i++, j++) {
        yyjson_val* val = yyjson_arr_get(arr_val, i);

        if (!_yyjson_get_string(&ckpool.serverurl[j], val, "nodeserver"))
            LOGWARNING("Invalid nodeserver entry number %d", i);
        ckpool.nodeserver[j] = true;
        ckpool.nodeservers++;
    }
    ckpool.serverurls = total_urls;
}

static void parse_trusted(yyjson_val* arr_val) {
    int arr_size, i, j, total_urls;

    if (!arr_val)
        return;
    if (!yyjson_is_arr(arr_val)) {
        LOGWARNING("Unable to parse trusted server entries as an array");
        return;
    }
    arr_size = yyjson_arr_size(arr_val);
    if (!arr_size) {
        LOGWARNING("Trusted array empty");
        return;
    }
    total_urls = ckpool.serverurls + arr_size;
    ckpool.serverurl = realloc(ckpool.serverurl, sizeof(char*) * total_urls);
    ckpool.nodeserver = realloc(ckpool.nodeserver, sizeof(bool) * total_urls);
    ckpool.trusted = realloc(ckpool.trusted, sizeof(bool) * total_urls);
    for (i = 0, j = ckpool.serverurls; j < total_urls; i++, j++) {
        yyjson_val* val = yyjson_arr_get(arr_val, i);

        if (!_yyjson_get_string(&ckpool.serverurl[j], val, "trusted"))
            LOGWARNING("Invalid trusted server entry number %d", i);
        ckpool.trusted[j] = true;
    }
    ckpool.serverurls = total_urls;
}

static bool parse_redirecturls(yyjson_val* arr_val) {
    bool  ret = false;
    int   arr_size, i;
    char *redirecturl, url[INET6_ADDRSTRLEN], port[8];
    redirecturl = alloca(INET6_ADDRSTRLEN);

    if (!arr_val)
        goto out;
    if (!yyjson_is_arr(arr_val)) {
        LOGNOTICE("Unable to parse redirecturl entries as an array");
        goto out;
    }
    arr_size = yyjson_arr_size(arr_val);
    if (!arr_size) {
        LOGWARNING("redirecturl array empty");
        goto out;
    }
    ckpool.redirecturls = arr_size;
    ckpool.redirecturl = ckalloc(sizeof(char*) * arr_size);
    ckpool.redirectport = ckalloc(sizeof(char*) * arr_size);
    for (i = 0; i < arr_size; i++) {
        yyjson_val* val = yyjson_arr_get(arr_val, i);

        strncpy(redirecturl, yyjson_get_str(val), INET6_ADDRSTRLEN - 1);
        /* See that the url properly resolves */
        if (!url_from_serverurl(redirecturl, url, port))
            quit(1, "Invalid redirecturl entry %d %s", i, redirecturl);
        ckpool.redirecturl[i] = strdup(strsep(&redirecturl, ":"));
        ckpool.redirectport[i] = strdup(port);
    }
    ret = true;
out:
    return ret;
}

static void parse_config(void) {
    yyjson_val *    json_conf, *arr_val;
    char *          url, *vmask = NULL;
    yyjson_read_err err_val;
    yyjson_doc*     doc;
    int             arr_size;

    /* Defaults for options where the zeroed struct is not the wanted value.
     * yyjson_obj_get_bool only writes when the key is present, so this
     * survives unless the config overrides it. */
    ckpool.reconnect = true;

    doc = yyjson_read_file(ckpool.config, YYJSON_READ_STOP_WHEN_DONE, NULL, &err_val);
    if (!doc) {
        LOGWARNING("Json decode error for config file %s: (%zu): %s", ckpool.config, err_val.pos, err_val.msg);
        return;
    }
    json_conf = yyjson_doc_get_root(doc);
    arr_val = yyjson_obj_get(json_conf, "btcd");
    if (arr_val && yyjson_is_arr(arr_val)) {
        arr_size = yyjson_arr_size(arr_val);
        if (arr_size)
            parse_btcds(arr_val, arr_size);
    }
    parse_database(json_conf);
    yyjson_obj_get_string(&ckpool.btcaddress, json_conf, "btcaddress");
    yyjson_obj_get_string(&ckpool.btcsig, json_conf, "btcsig");
    if (ckpool.btcsig && strlen(ckpool.btcsig) > 38) {
        LOGWARNING("Signature %s too long, truncating to 38 bytes", ckpool.btcsig);
        ckpool.btcsig[38] = '\0';
    }
    yyjson_obj_get_int(&ckpool.blockpoll, json_conf, "blockpoll");
    yyjson_obj_get_int(&ckpool.nonce1length, json_conf, "nonce1length");
    yyjson_obj_get_int(&ckpool.nonce2length, json_conf, "nonce2length");
    yyjson_obj_get_int(&ckpool.update_interval, json_conf, "update_interval");
    yyjson_obj_get_string(&vmask, json_conf, "version_mask");
    if (vmask && strlen(vmask) && validhex(vmask))
        sscanf(vmask, "%x", &ckpool.version_mask);
    else
        ckpool.version_mask = 0x1fffe000;
    dealloc(vmask);

    /* Default don't drop idle clients */
    yyjson_obj_get_int(&ckpool.dropidle, json_conf, "dropidle");
    /* Look for an array first and then a single entry */
    arr_val = yyjson_obj_get(json_conf, "serverurl");
    if (!parse_serverurls(arr_val)) {
        if (yyjson_obj_get_string(&url, json_conf, "serverurl")) {
            ckpool.serverurl = ckalloc(sizeof(char*));
            ckpool.serverurl[0] = url;
            ckpool.serverurls = 1;
            ckpool.server_highdiff = ckzalloc(sizeof(bool));
            ckpool.nodeserver = ckzalloc(sizeof(bool));
            ckpool.trusted = ckzalloc(sizeof(bool));
#ifdef HAVE_SV2
            ckpool.server_sv2 = ckzalloc(sizeof(bool));
            ckpool.server_sv2_jd = ckzalloc(sizeof(bool));
#endif
        }
    }
    arr_val = yyjson_obj_get(json_conf, "nodeserver");
    parse_nodeservers(arr_val);
    arr_val = yyjson_obj_get(json_conf, "trusted");
    parse_trusted(arr_val);
    yyjson_obj_get_string(&ckpool.upstream, json_conf, "upstream");
    yyjson_obj_get_int64(&ckpool.mindiff, json_conf, "mindiff");
    yyjson_obj_get_int64(&ckpool.startdiff, json_conf, "startdiff");
    yyjson_obj_get_int64(&ckpool.highdiff, json_conf, "highdiff");
    yyjson_obj_get_int64(&ckpool.maxdiff, json_conf, "maxdiff");
    yyjson_obj_get_string(&ckpool.logdir, json_conf, "logdir");
    yyjson_obj_get_int(&ckpool.maxclients, json_conf, "maxclients");
    yyjson_obj_get_int64(&ckpool.maxsendqueue, json_conf, "maxsendqueue");
    yyjson_obj_get_int(&ckpool.maxusers, json_conf, "maxusers");
    yyjson_obj_get_bool(&ckpool.reconnect, json_conf, "reconnect");
    yyjson_obj_get_double(&ckpool.donation, json_conf, "donation");
    /* Avoid dust-sized donations */
    if (ckpool.donation < 0.1)
        ckpool.donation = 0;
    else if (ckpool.donation > 99.9)
        ckpool.donation = 99.9;
    arr_val = yyjson_obj_get(json_conf, "proxy");
    if (arr_val && yyjson_is_arr(arr_val)) {
        arr_size = yyjson_arr_size(arr_val);
        if (arr_size)
            parse_proxies(arr_val, arr_size);
    }
    arr_val = yyjson_obj_get(json_conf, "redirecturl");
    if (arr_val)
        parse_redirecturls(arr_val);
    yyjson_obj_get_string(&ckpool.zmqblock, json_conf, "zmqblock");
    yyjson_obj_get_string(&ckpool.ipcmining, json_conf, "ipcmining");
#ifdef HAVE_SV2
    /* Prefer array form (like serverurl); fall back to a single string. */
    arr_val = yyjson_obj_get(json_conf, "sv2url");
    if (!parse_sv2_urllist(arr_val, &ckpool.sv2url, &ckpool.sv2urls, "sv2url")) {
        if (yyjson_obj_get_string(&url, json_conf, "sv2url")) {
            ckpool.sv2url = ckalloc(sizeof(char*));
            ckpool.sv2url[0] = url;
            ckpool.sv2urls = 1;
        }
    }
    arr_val = yyjson_obj_get(json_conf, "sv2jdurl");
    if (!parse_sv2_urllist(arr_val, &ckpool.sv2jdurl, &ckpool.sv2jdurls, "sv2jdurl")) {
        if (yyjson_obj_get_string(&url, json_conf, "sv2jdurl")) {
            ckpool.sv2jdurl = ckalloc(sizeof(char*));
            ckpool.sv2jdurl[0] = url;
            ckpool.sv2jdurls = 1;
        }
    }
    yyjson_obj_get_string(&ckpool.sv2_authority_key, json_conf, "sv2_authority_key");
    yyjson_obj_get_string(&ckpool.sv2_static_key, json_conf, "sv2_static_key");
    /*
     * SV2 *server* listen (sv2url / sv2jdurl) is pool or btcsolo only.
     * Proxy, userproxy, passthrough, node, and redirector all set
     * ckpool.proxy — drop SV2 binds there (upstream SV2 client is separate).
     */
    if (!sv2_server_listen_allowed()) {
        if (ckpool.sv2urls) {
            LOGWARNING(
                "Ignoring sv2url: SV2 server listen is pool/solo only "
                "(not proxy/passthrough/node/redirector/userproxy)");
            free_sv2_urllist(&ckpool.sv2url, &ckpool.sv2urls);
        }
        if (ckpool.sv2jdurls) {
            LOGWARNING(
                "Ignoring sv2jdurl: SV2 server listen is pool/solo only "
                "(not proxy/passthrough/node/redirector/userproxy)");
            free_sv2_urllist(&ckpool.sv2jdurl, &ckpool.sv2jdurls);
        }
    }
#endif

    yyjson_doc_free(doc);
}

static void manage_old_instance(proc_instance_t* pi) {
    struct stat statbuf;
    char        path[256];
    FILE*       fp;

    sprintf(path, "%s%s.pid", ckpool.socket_dir, pi->processname);
    if (!stat(path, &statbuf)) {
        int oldpid, ret;

        LOGNOTICE("File %s exists", path);
        fp = fopen(path, "re");
        if (!fp)
            quit(1, "Failed to open file %s", path);
        ret = fscanf(fp, "%d", &oldpid);
        fclose(fp);
        if (ret == 1 && !(kill_pid(oldpid, 0))) {
            LOGNOTICE("Old process %s pid %d still exists", pi->processname, oldpid);
            if (ckpool.handover) {
                LOGINFO("Saving pid to be handled at handover");
                pi->oldpid = oldpid;
                return;
            }
            terminate_oldpid(pi, oldpid);
        }
    }
}

static void prepare_child(proc_instance_t* pi, void* process, char* name) {
    pi->processname = name;
    pi->sockname = pi->processname;
    create_process_unixsock(pi);
    create_pthread(&pi->pth_process, process, pi);
    create_unix_receiver(pi);
}

static struct option long_options[] = {
    {"btcsolo", no_argument, 0, 'B'},
    {"config", required_argument, 0, 'c'},
    {"daemonise", no_argument, 0, 'D'},
    {"group", required_argument, 0, 'g'},
    {"handover", no_argument, 0, 'H'},
    {"help", no_argument, 0, 'h'},
    {"killold", no_argument, 0, 'k'},
    {"log-shares", no_argument, 0, 'L'},
    {"log-shares-file", no_argument, 0, 'F'},
    {"log-txns", no_argument, 0, 'X'},
    {"loglevel", required_argument, 0, 'l'},
    {"name", required_argument, 0, 'n'},
    {"node", no_argument, 0, 'N'},
    {"passthrough", no_argument, 0, 'P'},
    {"proxy", no_argument, 0, 'p'},
    {"quiet", no_argument, 0, 'q'},
    {"redirector", no_argument, 0, 'R'},
    {"signet", no_argument, 0, 'S'},
    {"sockdir", required_argument, 0, 's'},
    {"testconfig", no_argument, 0, 'T'},
    {"trusted", no_argument, 0, 't'},
    {"userproxy", no_argument, 0, 'u'},
    {0, 0, 0, 0}};

static bool send_recv_path(const char* path, const char* msg) {
    int   sockd = open_unix_client(path);
    bool  ret = false;
    char* response;

    send_unix_msg(sockd, msg);
    response = recv_unix_msg(sockd);
    if (response) {
        ret = true;
        LOGWARNING("Received: %s in response to %s request", response, msg);
        dealloc(response);
    } else
        LOGWARNING("Received no response to %s request", msg);
    Close(sockd);
    return ret;
}

/*
 * Port from a "host:port" serverurl, taken lexically so no name resolution is
 * needed. Handles the bracketed [::1]:port ipv6 form as well. 0 if unparseable.
 */
static int port_from_serverurl(const char* serverurl) {
    const char* colon;

    if (!serverurl)
        return 0;
    colon = strrchr(serverurl, ':');
    if (!colon)
        return 0;
    return atoi(colon + 1);
}

/*
 * Report every setting that has been resolved from the config file, the
 * command line and the built in defaults. Printed to stdout rather than the
 * log since the point is to diff two binaries' interpretation of the same
 * config file, so the format is one stable "key = value" line per setting.
 */
/*
 * Job Declaration client config sanity. A "jds" entry
 * needs an SV2 mining URL on the same entry, a mining IPC socket for local
 * templates, and an IPC-capable build. Fail here rather than discovering it
 * as a dead JD path at runtime.
 *
 * Whether the mining URL really is SV2 (valid base58check authority key) is
 * settled by the generator's proxy_parse_sv2_url(); only an explicitly SV1
 * scheme can be rejected this early.
 */
static void validate_jd_config(void) {
    int i;

    if (!ckpool.proxyjds)
        return;
    for (i = 0; i < ckpool.proxies; i++) {
        const char* url = ckpool.proxyurl[i];

        if (!ckpool.proxyjds[i])
            continue;
        if (!ckpool.proxy) {
            quit(1, "proxy[%d] has a jds entry but %s is not running as a proxy", i, ckpool.name);
        }
#ifndef HAVE_CAPNP
        quit(
            1,
            "proxy[%d] has a jds entry but this build has no mining IPC support "
            "(configure could not find capnp-rpc)",
            i);
#endif
        if (!url)
            quit(1, "proxy[%d] has a jds entry but no url", i);
        if (!strncasecmp(url, "stratum+tcp://", 14) || !strncasecmp(url, "stratum://", 10)) {
            quit(1, "proxy[%d] jds requires an SV2 (stratum2+tcp://) url, not %s", i, url);
        }
        if (!ckpool.ipcmining) {
            quit(
                1,
                "proxy[%d] has a jds entry but no ipcmining socket is configured; "
                "job declaration builds templates from the Bitcoin Core mining IPC",
                i);
        }
    }
}

static void report_config(void) {
    int i;

    printf(
        "mode = %s\n", ckpool.proxy     ? (ckpool.userproxy     ? "userproxy"
                                           : ckpool.passthrough ? (ckpool.node ? "node" : "passthrough")
                                           : ckpool.redirector  ? "redirector"
                                                                : "proxy")
                       : ckpool.btcsolo ? "btcsolo"
                                        : "pool");
    printf("name = %s\n", ckpool.name);
    printf("config = %s\n", ckpool.config ? ckpool.config : "(none)");
    printf("trusted_remote = %d\n", ckpool.remote);
    printf(
        "sv2_compiled = %d\n",
#ifdef HAVE_SV2
        1
#else
        0
#endif
    );

    printf("btcaddress = %s\n", ckpool.btcaddress ? ckpool.btcaddress : "(none)");
    printf("btcsig = %s\n", ckpool.btcsig ? ckpool.btcsig : "(none)");
    printf("donation = %.2f\n", ckpool.donation);

    printf("mindiff = %" PRId64 "\n", ckpool.mindiff);
    printf("startdiff = %" PRId64 "\n", ckpool.startdiff);
    printf("highdiff = %" PRId64 "\n", ckpool.highdiff);
    printf("maxdiff = %" PRId64 "\n", ckpool.maxdiff);
    printf("nonce1length = %d\n", ckpool.nonce1length);
    printf("nonce2length = %d\n", ckpool.nonce2length);
    printf("update_interval = %d\n", ckpool.update_interval);
    printf("blockpoll = %d\n", ckpool.blockpoll);
    printf("dropidle = %d\n", ckpool.dropidle);
    /* Resolved against the open file limit after this point, 0 = automatic. */
    printf("maxclients = %d\n", ckpool.maxclients);
    printf("maxsendqueue = %" PRId64 "\n", ckpool.maxsendqueue);
    printf("maxusers = %d\n", ckpool.maxusers);
    printf("reconnect = %s\n", ckpool.reconnect ? "true" : "false");

    printf("logdir = %s\n", ckpool.logdir);
    printf("socket_dir = %s\n", ckpool.socket_dir);
    printf("loglevel = %d\n", ckpool.loglevel);
    printf("logshares = %d\n", ckpool.logshares);
    printf("zmqblock = %s\n", ckpool.zmqblock);

    printf("btcds = %d\n", ckpool.btcds);
    for (i = 0; i < ckpool.btcds; i++) {
        printf(
            "btcdurl[%d] = %s notify=%d\n", i, ckpool.btcdurl[i] ? ckpool.btcdurl[i] : "(null)",
            ckpool.btcdnotify ? ckpool.btcdnotify[i] : 0);
    }

    printf("serverurls = %d\n", ckpool.serverurls);
    for (i = 0; i < ckpool.serverurls; i++) {
        const char* kind = "sv1";
        bool        highdiff;
        int         port;

#ifdef HAVE_SV2
        if (ckpool.server_sv2 && ckpool.server_sv2[i]) {
            if (ckpool.server_sv2_jd && ckpool.server_sv2_jd[i])
                kind = "sv2jd";
            else
                kind = "sv2";
        }
#endif
        /*
         * server_highdiff[] is not populated until the connector binds
         * its listening sockets, which a config test never reaches, so
         * apply the same rule it does rather than report the zeroed
         * array. Keep the two in step: see connector().
         */
        highdiff = ckpool.server_highdiff && ckpool.server_highdiff[i];
        port = port_from_serverurl(ckpool.serverurl[i]);
        if (port && port % 2 == 0)
            highdiff = true;
        printf(
            "serverurl[%d] = %s type=%s highdiff=%d node=%d trusted=%d\n", i,
            ckpool.serverurl[i] ? ckpool.serverurl[i] : "(null)", kind, highdiff,
            ckpool.nodeserver ? ckpool.nodeserver[i] : 0, ckpool.trusted ? ckpool.trusted[i] : 0);
    }
#ifdef HAVE_SV2
    printf("sv2urls = %d\n", ckpool.sv2urls);
    printf("sv2jdurls = %d\n", ckpool.sv2jdurls);
    if (ckpool.sv2urls || ckpool.sv2jdurls) {
        printf("sv2_authority_key = %s\n", ckpool.sv2_authority_key);
        printf("sv2_static_key = %s\n", ckpool.sv2_static_key);
    }
#endif
    printf("upstream = %s\n", ckpool.upstream ? ckpool.upstream : "(none)");
    printf("ipcmining = %s\n", ckpool.ipcmining ? ckpool.ipcmining : "(none)");
    printf("proxies = %d\n", ckpool.proxies);
    for (i = 0; i < ckpool.proxies; i++) {
        printf(
            "proxyurl[%d] = %s jds=%s\n", i, ckpool.proxyurl[i],
            ckpool.proxyjds && ckpool.proxyjds[i] ? ckpool.proxyjds[i] : "(none)");
    }
    printf("redirecturls = %d\n", ckpool.redirecturls);
    for (i = 0; i < ckpool.redirecturls; i++)
        printf("redirecturl[%d] = %s:%s\n", i, ckpool.redirecturl[i], ckpool.redirectport[i]);
}

int main(int argc, char** argv) {
    struct sigaction handler;
    int              c, ret, i = 0, j;
    char             buf[512] = {};
    char*            appname;

    /* Make significant floating point errors fatal to avoid subtle bugs being missed */
    feenableexcept(FE_DIVBYZERO | FE_INVALID);

    ckpool.starttime = time(NULL);
    ckpool.startpid = getpid();
    ckpool.loglevel = LOG_NOTICE;
    ckpool.initial_args = ckalloc(sizeof(char*) * (argc + 2)); /* Leave room for extra -H */
    for (ckpool.args = 0; ckpool.args < argc; ckpool.args++)
        ckpool.initial_args[ckpool.args] = strdup(argv[ckpool.args]);
    ckpool.initial_args[ckpool.args] = NULL;

    appname = basename(argv[0]);
    if (!strcmp(appname, "ckproxy"))
        ckpool.proxy = true;

    while ((c = getopt_long(argc, argv, "BFc:Dd:g:HhkLl:Nn:PpqRSs:TtuX", long_options, &i)) != -1) {
        switch (c) {
            case 'B':
                if (ckpool.proxy)
                    quit(1, "Cannot set both proxy and btcsolo mode");
                ckpool.btcsolo = true;
                break;
            case 'c':
                ckpool.config = optarg;
                break;
            case 'D':
                ckpool.daemon = true;
                break;
            case 'g':
                ckpool.grpnam = optarg;
                break;
            case 'H':
                ckpool.handover = true;
                ckpool.killold = true;
                break;
            case 'h':
                for (j = 0; long_options[j].val; j++) {
                    struct option* jopt = &long_options[j];

                    if (jopt->has_arg) {
                        char* upper = alloca(strlen(jopt->name) + 1);
                        int   offset = 0;

                        do {
                            upper[offset] = toupper(jopt->name[offset]);
                        } while (upper[offset++] != '\0');
                        printf("-%c %s | --%s %s\n", jopt->val, upper, jopt->name, upper);
                    } else
                        printf("-%c | --%s\n", jopt->val, jopt->name);
                }
                exit(0);
            case 'k':
                ckpool.killold = true;
                break;
            case 'L':
                ckpool.logshares = ckpool.logshares_db = true;
                break;
            case 'F':
                ckpool.logshares = ckpool.logshares_file = true;
                break;
            case 'X':
                ckpool.logtxns = true;
                break;
            case 'l':
                ckpool.loglevel = atoi(optarg);
                if (ckpool.loglevel < LOG_EMERG || ckpool.loglevel > LOG_DEBUG) {
                    quit(1, "Invalid loglevel (range %d - %d): %d", LOG_EMERG, LOG_DEBUG, ckpool.loglevel);
                }
                break;
            case 'N':
                if (ckpool.proxy || ckpool.redirector || ckpool.userproxy || ckpool.passthrough)
                    quit(1, "Cannot set another proxy type or redirector and node mode");
                ckpool.proxy = ckpool.passthrough = ckpool.node = true;
                break;
            case 'n':
                ckpool.name = optarg;
                break;
            case 'P':
                if (ckpool.proxy || ckpool.redirector || ckpool.userproxy || ckpool.node)
                    quit(1, "Cannot set another proxy type or redirector and passthrough mode");
                ckpool.proxy = ckpool.passthrough = true;
                break;
            case 'p':
                if (ckpool.passthrough || ckpool.redirector || ckpool.userproxy || ckpool.node)
                    quit(1, "Cannot set another proxy type or redirector and proxy mode");
                ckpool.proxy = true;
                break;
            case 'q':
                ckpool.quiet = true;
                break;
            case 'R':
                if (ckpool.proxy || ckpool.passthrough || ckpool.userproxy || ckpool.node)
                    quit(1, "Cannot set a proxy type or passthrough and redirector modes");
                ckpool.proxy = ckpool.passthrough = ckpool.redirector = true;
                break;
            case 'S':
                ckpool.signet = true;
                break;
            case 's':
                ckpool.socket_dir = strdup(optarg);
                break;
            case 'T':
                ckpool.testconfig = true;
                /* Make the config aborts that pass status 0 fail */
                quit_zero_is_failure = true;
                break;
            case 't':
                if (ckpool.proxy)
                    quit(1, "Cannot set a proxy type and trusted remote mode");
                ckpool.remote = true;
                break;
            case 'u':
                if (ckpool.proxy || ckpool.redirector || ckpool.passthrough || ckpool.node)
                    quit(1, "Cannot set both userproxy and another proxy type or redirector");
                ckpool.userproxy = ckpool.proxy = true;
                break;
        }
    }

    if (!ckpool.name) {
        if (ckpool.node)
            ckpool.name = "cknode";
        else if (ckpool.redirector)
            ckpool.name = "ckredirector";
        else if (ckpool.passthrough)
            ckpool.name = "ckpassthrough";
        else if (ckpool.proxy)
            ckpool.name = "ckproxy";
        else
            ckpool.name = "ckpool";
    }
    snprintf(buf, 15, "%s", ckpool.name);
    prctl(PR_SET_NAME, buf, 0, 0, 0);
    memset(buf, 0, 15);

    if (ckpool.grpnam) {
        struct group* group = getgrnam(ckpool.grpnam);

        if (!group)
            quit(1, "Failed to find group %s", ckpool.grpnam);
        ckpool.gr_gid = group->gr_gid;
    } else
        ckpool.gr_gid = getegid();

    if (!ckpool.config) {
        ckpool.config = strdup(ckpool.name);
        realloc_strcat(&ckpool.config, ".conf");
    }
    if (!ckpool.socket_dir) {
        ckpool.socket_dir = strdup("/tmp/");
        realloc_strcat(&ckpool.socket_dir, ckpool.name);
    }
    trail_slash(&ckpool.socket_dir);

    /* Ignore sigpipe */
    signal(SIGPIPE, SIG_IGN);

    /* A config test must not leave anything behind on the filesystem. */
    if (!ckpool.testconfig) {
        ret = mkdir(ckpool.socket_dir, 0750);
        if (ret && errno != EEXIST)
            quit(1, "Failed to make directory %s", ckpool.socket_dir);
    }

    parse_config();

    /* Refuse to start rather than handing a NULL conninfo to libpq later,
     * which would fault in the stratifier on the first share. */
    if (ckpool.logshares_db && !ckpool.db_conn_str) {
        quit(
            1,
            "--log-shares needs a complete \"database\" config object "
            "(hostname, dbname, user, pass); use --log-shares-file for file logging");
    }
#ifndef HAVE_LIBPQ
    if (ckpool.logshares_db)
        quit(1, "--log-shares needs ckpool built with PostgreSQL support (libpq not found at configure time)");
#endif

    /* Set defaults if not found in config file */
    if (!ckpool.btcds) {
        ckpool.btcds = 1;
        ckpool.btcdurl = ckzalloc(sizeof(char*));
        ckpool.btcdauth = ckzalloc(sizeof(char*));
        ckpool.btcdpass = ckzalloc(sizeof(char*));
        ckpool.btcdnotify = ckzalloc(sizeof(bool));
    }
    for (i = 0; i < ckpool.btcds; i++) {
        if (!ckpool.btcdurl[i])
            ckpool.btcdurl[i] = strdup("localhost:8332");
        if (!ckpool.btcdauth[i])
            ckpool.btcdauth[i] = strdup("user");
        if (!ckpool.btcdpass[i])
            ckpool.btcdpass[i] = strdup("pass");
    }

    ckpool.donaddress = "bc1q28kkr5hk4gnqe3evma6runjrd2pvqyp8fpwfzu";

    /* Donations on testnet are meaningless but required for complete
     * testing. Testnet and regtest addresses */
    ckpool.tndonaddress = "tb1qdxclx2qxdh0g67j27v6y6ls0xm9cl2w2xktjq2";
    ckpool.rtdonaddress = "bcrt1qlk935ze2fsu86zjp395uvtegztrkaezawxx0wf";

    if (!ckpool.btcaddress && !ckpool.btcsolo && !ckpool.proxy)
        quit(0, "Non solo mining must have a btcaddress in config, aborting!");
    if (!ckpool.blockpoll)
        ckpool.blockpoll = 100;
    if (!ckpool.nonce1length)
        ckpool.nonce1length = 4;
    else if (ckpool.nonce1length < 2 || ckpool.nonce1length > 8)
        quit(0, "Invalid nonce1length %d specified, must be 2~8", ckpool.nonce1length);
    if (!ckpool.nonce2length) {
        /* nonce2length is zero by default in proxy mode */
        if (!ckpool.proxy)
            ckpool.nonce2length = 8;
    } else if (ckpool.nonce2length < 2 || ckpool.nonce2length > 8)
        quit(0, "Invalid nonce2length %d specified, must be 2~8", ckpool.nonce2length);
    if (!ckpool.update_interval)
        ckpool.update_interval = 30;
    if (!ckpool.mindiff)
        ckpool.mindiff = 1;
    /* Modern default; match shipped example confs. Override with "startdiff". */
    if (!ckpool.startdiff)
        ckpool.startdiff = 10000;
    if (!ckpool.highdiff)
        ckpool.highdiff = 1000000;
    if (!ckpool.logdir)
        ckpool.logdir = strdup("logs");
#ifdef HAVE_SV2
    /* Append each SV2 mining / JD bind onto serverurl[] (default ports
     * 3336 / 3337 — must not collide with serverurl SV1 ports).
     * SV2-only configs need no serverurl (SV1) entries. */
    append_sv2_serverurls(ckpool.sv2url, ckpool.sv2urls, false, 3336);
    append_sv2_serverurls(ckpool.sv2jdurl, ckpool.sv2jdurls, true, 3337);
    /*
     * SV2 has no version-mask advertisement: NewExtendedMiningJob's
     * version_rolling_allowed grants every BIP320 general-purpose bit
     * (0x1fffe000), and clients cannot be told about a narrower policy the
     * way SV1 mining.configure does. Shares rolling bits outside the mask
     * are rejected invalid-share, so a narrowed mask silently costs SV2
     * miners work. Warn loudly; version_mask == 0 refuses
     * REQUIRES_VERSION_ROLLING clients outright at SetupConnection instead.
     */
    if ((ckpool.sv2urls || ckpool.sv2jdurls) && ckpool.version_mask && ckpool.version_mask != 0x1fffe000) {
        LOGWARNING(
            "version_mask 0x%08x is narrower than BIP320 0x1fffe000: "
            "SV2 clients are granted all BIP320 bits by version_rolling_allowed "
            "and cannot learn the narrower mask — shares rolling excluded bits "
            "will be rejected",
            ckpool.version_mask);
    }
    /*
     * Default Noise key paths when not set in the conf file.
     * socket_dir is /tmp/<name>/ (name from -n, else "ckpool") unless -s
     * overrides it — so keys land at e.g. /tmp/ckpool/sv2_authority.key.
     */
    if (ckpool.sv2urls || ckpool.sv2jdurls) {
        if (!ckpool.sv2_authority_key || !ckpool.sv2_authority_key[0]) {
            dealloc(ckpool.sv2_authority_key);
            ASPRINTF(&ckpool.sv2_authority_key, "%ssv2_authority.key", ckpool.socket_dir);
            LOGNOTICE("SV2 authority key defaulting to %s", ckpool.sv2_authority_key);
        }
        if (!ckpool.sv2_static_key || !ckpool.sv2_static_key[0]) {
            dealloc(ckpool.sv2_static_key);
            ASPRINTF(&ckpool.sv2_static_key, "%ssv2_static.key", ckpool.socket_dir);
            LOGNOTICE("SV2 static key defaulting to %s", ckpool.sv2_static_key);
        }
    }
#endif
    /*
     * Listen endpoints: serverurl (SV1) and/or sv2url/sv2jdurl (SV2).
     * Either family alone is enough. Only when nothing is configured do
     * we default to a single SV1 bind on 0.0.0.0:3333 (3334 in proxy).
     */
    if (!ckpool.serverurls) {
        int port = ckpool.proxy ? 3334 : 3333;

        ckpool.serverurl = ckalloc(sizeof(char*));
        ASPRINTF(&ckpool.serverurl[0], "0.0.0.0:%d", port);
        ckpool.serverurls = 1;
        ckpool.server_highdiff = ckzalloc(sizeof(bool));
        ckpool.nodeserver = ckzalloc(sizeof(bool));
        ckpool.trusted = ckzalloc(sizeof(bool));
#ifdef HAVE_SV2
        ckpool.server_sv2 = ckzalloc(sizeof(bool));
        ckpool.server_sv2_jd = ckzalloc(sizeof(bool));
#endif
        LOGWARNING("No serverurl/sv2url configured, defaulting SV1 listen to %s", ckpool.serverurl[0]);
    }
    /*
     * Fail early if two listens would bind the same host:port (or the same
     * port with a wildcard 0.0.0.0/:: host). Covers SV1 vs SV2 vs JD and
     * duplicate entries within one family.
     */
    check_listen_port_clashes();
    for (i = 0; i < ckpool.serverurls; i++) {
        const char* kind = "SV1";

#ifdef HAVE_SV2
        if (ckpool.server_sv2 && ckpool.server_sv2[i]) {
            if (ckpool.server_sv2_jd && ckpool.server_sv2_jd[i])
                kind = "SV2 Job Declaration";
            else
                kind = "SV2 mining";
        }
#endif
        LOGWARNING("Configured serverurl[%d]: %s (%s)", i, ckpool.serverurl[i] ? ckpool.serverurl[i] : "(null)", kind);
    }
    if (ckpool.proxy && !ckpool.proxies)
        quit(0, "No proxy entries found in config file %s", ckpool.config);
    validate_jd_config();
    if (ckpool.redirector && !ckpool.redirecturls)
        quit(0, "No redirect entries found in config file %s", ckpool.config);
    if (!ckpool.zmqblock)
        ckpool.zmqblock = "tcp://127.0.0.1:28332";

    /*
     * Everything reachable without touching the filesystem, the network or
     * an already running instance has now been parsed, defaulted and
     * validated, so this is as far as a config test can go.
     */
    if (ckpool.testconfig) {
        report_config();
        LOGWARNING("Configuration check of %s passed", ckpool.config ? ckpool.config : "defaults");
        exit(0);
    }

    /* Create the log directory */
    trail_slash(&ckpool.logdir);
    ret = mkdir(ckpool.logdir, 0750);
    if (ret && errno != EEXIST)
        quit(1, "Failed to make log directory %s", ckpool.logdir);

    /* Create the user logdir */
    sprintf(buf, "%s/users", ckpool.logdir);
    ret = mkdir(buf, 0750);
    if (ret && errno != EEXIST)
        quit(1, "Failed to make user log directory %s", buf);

    /* Create the pool logdir */
    sprintf(buf, "%s/pool", ckpool.logdir);
    ret = mkdir(buf, 0750);
    if (ret && errno != EEXIST)
        quit(1, "Failed to make pool log directory %s", buf);

    /* Create the logfile */
    ASPRINTF(&ckpool.logfilename, "%s%s.log", ckpool.logdir, ckpool.name);
    if (!open_logfile())
        quit(1, "Failed to make open log file %s", buf);
    launch_logger();

    ckpool.main.processname = strdup("main");
    ckpool.main.sockname = strdup("listener");
    name_process_sockname(&ckpool.main.us, &ckpool.main);
    ckpool.oldconnfd = ckzalloc(sizeof(int*) * ckpool.serverurls);
    manage_old_instance(&ckpool.main);
    if (ckpool.handover) {
        const char* path = ckpool.main.us.path;

        if (send_recv_path(path, "ping")) {
            for (i = 0; i < ckpool.serverurls; i++) {
                char oldurl[INET6_ADDRSTRLEN], oldport[8];
                char getfd[24];
                int  sockd;

                snprintf(getfd, sizeof(getfd), "getxfd%d", i);
                sockd = open_unix_client(path);
                if (sockd < 1)
                    break;
                if (!send_unix_msg(sockd, getfd))
                    break;
                ckpool.oldconnfd[i] = get_fd(sockd);
                Close(sockd);
                sockd = ckpool.oldconnfd[i];
                if (!sockd)
                    break;
                if (url_from_socket(sockd, oldurl, oldport)) {
                    LOGWARNING("Inherited old server socket %d url %s:%s !", i, oldurl, oldport);
                } else {
                    LOGWARNING("Inherited old server socket %d with new file descriptor %d!", i, ckpool.oldconnfd[i]);
                }
            }
            send_recv_path(path, "reject");
            send_recv_path(path, "reconnect");
            send_recv_path(path, "shutdown");
        }
    }

    if (ckpool.daemon) {
        int fd;

        if (fork())
            exit(0);
        setsid();
        fd = open("/dev/null", O_RDWR, 0);
        if (fd != -1) {
            dup2(fd, STDIN_FILENO);
            dup2(fd, STDOUT_FILENO);
            dup2(fd, STDERR_FILENO);
        }
    }

    write_namepid(&ckpool.main);
    open_process_sock(&ckpool.main, &ckpool.main.us);

    ret = sysconf(_SC_OPEN_MAX);
    if (ckpool.maxclients > ret * 9 / 10) {
        LOGWARNING(
            "Cannot set maxclients to %d due to max open file limit of %d, reducing to %d", ckpool.maxclients, ret,
            ret * 9 / 10);
        ckpool.maxclients = ret * 9 / 10;
    } else if (!ckpool.maxclients) {
        LOGNOTICE("Setting maxclients to %d due to max open file limit of %d", ret * 9 / 10, ret);
        ckpool.maxclients = ret * 9 / 10;
    }

    // ckpool.ckpapi = create_ckmsgq("api", &ckpool_api);
    create_pthread(&ckpool.pth_listener, listener, &ckpool.main);

    handler.sa_handler = &sighandler;
    handler.sa_flags = 0;
    sigemptyset(&handler.sa_mask);
    sigaction(SIGTERM, &handler, NULL);
    sigaction(SIGINT, &handler, NULL);

    /* Launch separate processes from here */
    prepare_child(&ckpool.generator, generator, "generator");
    prepare_child(&ckpool.stratifier, stratifier, "stratifier");
    prepare_child(&ckpool.connector, connector, "connector");

    /* Shutdown from here if the listener is sent a shutdown message */
    if (ckpool.pth_listener)
        join_pthread(ckpool.pth_listener);

    if (ckpool_shutdown)
        LOGWARNING("Process %s received signal %d, shutting down", ckpool.name, (int)ckpool_shutdown);

    /* Signal subsystems that we are shutting down and give the mining IPC
     * notifier a chance to disconnect from bitcoind cleanly before the
     * process exits and the socket is closed from under any in-flight
     * request. */
    ckpool.shutdown = true;
    stratifier_shutdown_ipc();
#ifdef HAVE_SV2
    sv2_jdc_stop();
#endif

    clean_up();

    return 0;
}
