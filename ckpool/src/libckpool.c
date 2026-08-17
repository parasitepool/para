/*
 * Copyright 2014-2018,2023,2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#include "config.h"

#include <sys/types.h>
#include <sys/socket.h>
#ifdef HAVE_LINUX_UN_H
#include <linux/un.h>
#else
#include <sys/un.h>
#endif
#include <sys/epoll.h>
#include <sys/file.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <netdb.h>
#include <unistd.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#include <math.h>
#include <poll.h>
#include <arpa/inet.h>

#include "libckpool.h"
#include "sha2.h"
#include "utlist.h"

#ifndef UNIX_PATH_MAX
#define UNIX_PATH_MAX 108
#endif

/* Only ever set by ckpool's --testconfig; see the quit() macro. */
bool quit_zero_is_failure;

/* We use a weak function as a simple printf within the library that can be
 * overridden by however the outside executable wishes to do its logging. */
void __attribute__((weak)) logmsg(int __maybe_unused loglevel, const char* fmt, ...) {
    va_list ap;

    va_start(ap, fmt);
    vprintf(fmt, ap);
    va_end(ap);

    printf("\n");
    fflush(stdout);
}

void rename_proc(const char* name) {
    char buf[16];

    snprintf(buf, 15, "ckp@%s", name);
    buf[15] = '\0';
    prctl(PR_SET_NAME, buf, 0, 0, 0);
}

void create_pthread(pthread_t* thread, void* (*start_routine)(void*), void* arg) {
    int ret = pthread_create(thread, NULL, start_routine, arg);

    if (unlikely(ret))
        quit(1, "Failed to pthread_create");
}

void join_pthread(pthread_t thread) {
    if (!pthread_kill(thread, 0))
        pthread_join(thread, NULL);
}

struct ck_completion {
    sem_t sem;
    void (*fn)(void* fnarg);
    void* fnarg;
};

static void* completion_thread(void* arg) {
    struct ck_completion* ckc = (struct ck_completion*)arg;

    ckc->fn(ckc->fnarg);
    cksem_post(&ckc->sem);

    return NULL;
}

bool ck_completion_timeout(void* fn, void* fnarg, int timeout) {
    struct ck_completion ckc;
    pthread_t            pthread;
    bool                 ret = false;

    cksem_init(&ckc.sem);
    ckc.fn = fn;
    ckc.fnarg = fnarg;

    pthread_create(&pthread, NULL, completion_thread, (void*)&ckc);

    ret = cksem_mswait(&ckc.sem, timeout);
    if (!ret)
        pthread_join(pthread, NULL);
    else
        pthread_cancel(pthread);
    return !ret;
}

int _cond_wait(pthread_cond_t* cond, mutex_t* lock, const char* file, const char* func, const int line) {
    int ret;

    ret = pthread_cond_wait(cond, &lock->mutex);
    lock->file = file;
    lock->func = func;
    lock->line = line;
    return ret;
}

int _cond_timedwait(
    pthread_cond_t*        cond,
    mutex_t*               lock,
    const struct timespec* abstime,
    const char*            file,
    const char*            func,
    const int              line) {
    int ret;

    ret = pthread_cond_timedwait(cond, &lock->mutex, abstime);
    lock->file = file;
    lock->func = func;
    lock->line = line;
    return ret;
}

int _mutex_timedlock(mutex_t* lock, int timeout, const char* file, const char* func, const int line) {
    tv_t now;
    ts_t abs;
    int  ret;

    tv_time(&now);
    tv_to_ts(&abs, &now);
    abs.tv_sec += timeout;

    ret = pthread_mutex_timedlock(&lock->mutex, &abs);
    if (!ret) {
        lock->file = file;
        lock->func = func;
        lock->line = line;
    }

    return ret;
}

/* Make every locking attempt warn if we're unable to get the lock for more
 * than 10 seconds and fail if we can't get it for longer than a minute. */
void _mutex_lock(mutex_t* lock, const char* file, const char* func, const int line) {
    int ret, retries = 0;

retry:
    ret = _mutex_timedlock(lock, 10, file, func, line);
    if (unlikely(ret)) {
        if (likely(ret == ETIMEDOUT)) {
            LOGERR(
                "WARNING: Prolonged mutex lock contention from %s %s:%d, held by %s %s:%d", file, func, line,
                lock->file, lock->func, lock->line);
            if (++retries < 6)
                goto retry;
            quitfrom(1, file, func, line, "FAILED TO GRAB MUTEX!");
        }
        quitfrom(1, file, func, line, "WTF MUTEX ERROR ON LOCK!");
    }
}

/* Does not unset lock->file/func/line since they're only relevant when the lock is held */
void _mutex_unlock(mutex_t* lock, const char* file, const char* func, const int line) {
    if (unlikely(pthread_mutex_unlock(&lock->mutex)))
        quitfrom(1, file, func, line, "WTF MUTEX ERROR ON UNLOCK!");
}

int _mutex_trylock(
    mutex_t*                   lock,
    __maybe_unused const char* file,
    __maybe_unused const char* func,
    __maybe_unused const int   line) {
    int ret;

    ret = pthread_mutex_trylock(&lock->mutex);
    if (!ret) {
        lock->file = file;
        lock->func = func;
        lock->line = line;
    }
    return ret;
}

void mutex_destroy(mutex_t* lock) {
    pthread_mutex_destroy(&lock->mutex);
}

static int wr_timedlock(pthread_rwlock_t* lock, int timeout) {
    tv_t now;
    ts_t abs;
    int  ret;

    tv_time(&now);
    tv_to_ts(&abs, &now);
    abs.tv_sec += timeout;

    ret = pthread_rwlock_timedwrlock(lock, &abs);

    return ret;
}

void _wr_lock(rwlock_t* lock, const char* file, const char* func, const int line) {
    int ret, retries = 0;

retry:
    ret = wr_timedlock(&lock->rwlock, 10);
    if (unlikely(ret)) {
        if (likely(ret == ETIMEDOUT)) {
            LOGERR(
                "WARNING: Prolonged write lock contention from %s %s:%d, held by %s %s:%d", file, func, line,
                lock->file, lock->func, lock->line);
            if (++retries < 6)
                goto retry;
            quitfrom(1, file, func, line, "FAILED TO GRAB WRITE LOCK!");
        }
        quitfrom(1, file, func, line, "WTF ERROR ON WRITE LOCK!");
    }
    lock->file = file;
    lock->func = func;
    lock->line = line;
}

int _wr_trylock(
    rwlock_t*                  lock,
    __maybe_unused const char* file,
    __maybe_unused const char* func,
    __maybe_unused const int   line) {
    int ret = pthread_rwlock_trywrlock(&lock->rwlock);

    if (!ret) {
        lock->file = file;
        lock->func = func;
        lock->line = line;
    }
    return ret;
}

static int rd_timedlock(pthread_rwlock_t* lock, int timeout) {
    tv_t now;
    ts_t abs;
    int  ret;

    tv_time(&now);
    tv_to_ts(&abs, &now);
    abs.tv_sec += timeout;

    ret = pthread_rwlock_timedrdlock(lock, &abs);

    return ret;
}

void _rd_lock(rwlock_t* lock, const char* file, const char* func, const int line) {
    int ret, retries = 0;

retry:
    ret = rd_timedlock(&lock->rwlock, 10);
    if (unlikely(ret)) {
        if (likely(ret == ETIMEDOUT)) {
            LOGERR(
                "WARNING: Prolonged read lock contention from %s %s:%d, held by %s %s:%d", file, func, line, lock->file,
                lock->func, lock->line);
            if (++retries < 6)
                goto retry;
            quitfrom(1, file, func, line, "FAILED TO GRAB READ LOCK!");
        }
        quitfrom(1, file, func, line, "WTF ERROR ON READ LOCK!");
    }
    lock->file = file;
    lock->func = func;
    lock->line = line;
}

void _rw_unlock(rwlock_t* lock, const char* file, const char* func, const int line) {
    if (unlikely(pthread_rwlock_unlock(&lock->rwlock)))
        quitfrom(1, file, func, line, "WTF RWLOCK ERROR ON UNLOCK!");
}

void _rd_unlock(rwlock_t* lock, const char* file, const char* func, const int line) {
    _rw_unlock(lock, file, func, line);
}

void _wr_unlock(rwlock_t* lock, const char* file, const char* func, const int line) {
    _rw_unlock(lock, file, func, line);
}

void _mutex_init(mutex_t* lock, const char* file, const char* func, const int line) {
    if (unlikely(pthread_mutex_init(&lock->mutex, NULL)))
        quitfrom(1, file, func, line, "Failed to pthread_mutex_init");
}

void _rwlock_init(rwlock_t* lock, const char* file, const char* func, const int line) {
    if (unlikely(pthread_rwlock_init(&lock->rwlock, NULL)))
        quitfrom(1, file, func, line, "Failed to pthread_rwlock_init");
}

void _cond_init(pthread_cond_t* cond, const char* file, const char* func, const int line) {
    if (unlikely(pthread_cond_init(cond, NULL)))
        quitfrom(1, file, func, line, "Failed to pthread_cond_init!");
}

void _cklock_init(cklock_t* lock, const char* file, const char* func, const int line) {
    _mutex_init(&lock->mutex, file, func, line);
    _rwlock_init(&lock->rwlock, file, func, line);
}

/* Read lock variant of cklock. Cannot be promoted. */
void _ck_rlock(cklock_t* lock, const char* file, const char* func, const int line) {
    _mutex_lock(&lock->mutex, file, func, line);
    _rd_lock(&lock->rwlock, file, func, line);
    _mutex_unlock(&lock->mutex, file, func, line);
}

/* Write lock variant of cklock */
void _ck_wlock(cklock_t* lock, const char* file, const char* func, const int line) {
    _mutex_lock(&lock->mutex, file, func, line);
    _wr_lock(&lock->rwlock, file, func, line);
}

/* Downgrade write variant to a read lock */
void _ck_dwlock(cklock_t* lock, const char* file, const char* func, const int line) {
    _wr_unlock(&lock->rwlock, file, func, line);
    _rd_lock(&lock->rwlock, file, func, line);
    _mutex_unlock(&lock->mutex, file, func, line);
}

/* Demote a write variant to an intermediate variant */
void _ck_dwilock(cklock_t* lock, const char* file, const char* func, const int line) {
    _wr_unlock(&lock->rwlock, file, func, line);
}

void _ck_runlock(cklock_t* lock, const char* file, const char* func, const int line) {
    _rd_unlock(&lock->rwlock, file, func, line);
}

void _ck_wunlock(cklock_t* lock, const char* file, const char* func, const int line) {
    _wr_unlock(&lock->rwlock, file, func, line);
    _mutex_unlock(&lock->mutex, file, func, line);
}

void cklock_destroy(cklock_t* lock) {
    pthread_rwlock_destroy(&lock->rwlock.rwlock);
    pthread_mutex_destroy(&lock->mutex.mutex);
}

void _cksem_init(sem_t* sem, const char* file, const char* func, const int line) {
    int ret;
    if ((ret = sem_init(sem, 0, 0)))
        quitfrom(1, file, func, line, "Failed to sem_init ret=%d errno=%d", ret, errno);
}

void _cksem_post(sem_t* sem, const char* file, const char* func, const int line) {
    if (unlikely(sem_post(sem)))
        quitfrom(1, file, func, line, "Failed to sem_post errno=%d sem=0x%p", errno, sem);
}

void _cksem_wait(sem_t* sem, const char* file, const char* func, const int line) {
    if (unlikely(sem_wait(sem))) {
        if (errno == EINTR)
            return;
        quitfrom(1, file, func, line, "Failed to sem_wait errno=%d sem=0x%p", errno, sem);
    }
}

int _cksem_trywait(sem_t* sem, const char* file, const char* func, const int line) {
    int ret = sem_trywait(sem);

    if (unlikely(ret && errno != EAGAIN && errno != EINTR))
        quitfrom(1, file, func, line, "Failed to sem_trywait errno=%d sem=0x%p", errno, sem);
    return ret;
}

int _cksem_mswait(sem_t* sem, int ms, const char* file, const char* func, const int line) {
    ts_t abs_timeout, ts_now;
    tv_t tv_now;
    int  ret;

    tv_time(&tv_now);
    tv_to_ts(&ts_now, &tv_now);
    ms_to_ts(&abs_timeout, ms);
    timeraddspec(&abs_timeout, &ts_now);
    ret = sem_timedwait(sem, &abs_timeout);

    if (ret) {
        if (likely(errno == ETIMEDOUT))
            return ETIMEDOUT;
        if (errno == EINTR)
            return EINTR;
        quitfrom(1, file, func, line, "Failed to sem_timedwait errno=%d sem=0x%p", errno, sem);
    }
    return 0;
}

void _cksem_destroy(sem_t* sem, const char* file, const char* func, const int line) {
    if (unlikely(sem_destroy(sem)))
        quitfrom(1, file, func, line, "Failed to sem_destroy errno=%d sem=0x%p", errno, sem);
}

/* Extract just the url and port information from a url string, allocating
 * heap memory for sockaddr_url and sockaddr_port. */
bool extract_sockaddr(char* url, char** sockaddr_url, char** sockaddr_port) {
    char * url_begin, *url_end, *ipv6_begin, *ipv6_end, *port_start = NULL;
    char * url_address, *port, *tmp;
    int    url_len, port_len = 0;
    size_t hlen;

    if (!url) {
        LOGWARNING("Null length url string passed to extract_sockaddr");
        return false;
    }
    url_begin = strstr(url, "//");
    if (!url_begin)
        url_begin = url;
    else
        url_begin += 2;

    /* Look for numeric ipv6 entries */
    ipv6_begin = strstr(url_begin, "[");
    ipv6_end = strstr(url_begin, "]");
    if (ipv6_begin && ipv6_end && ipv6_end > ipv6_begin)
        url_end = strstr(ipv6_end, ":");
    else
        url_end = strstr(url_begin, ":");
    if (url_end) {
        url_len = url_end - url_begin;
        port_len = strlen(url_begin) - url_len - 1;
        if (port_len < 1)
            return false;
        port_start = url_end + 1;
    } else
        url_len = strlen(url_begin);

    /* Get rid of the [] */
    if (ipv6_begin && ipv6_end && ipv6_end > ipv6_begin) {
        url_len -= 2;
        url_begin++;
    }

    if (url_len < 1) {
        LOGWARNING("Null length URL passed to extract_sockaddr");
        return false;
    }

    hlen = url_len + 1;
    url_address = ckalloc(hlen);
    sprintf(url_address, "%.*s", url_len, url_begin);

    port = ckalloc(8);
    if (port_len) {
        char* slash;

        snprintf(port, 6, "%.*s", port_len, port_start);
        slash = strchr(port, '/');
        if (slash)
            *slash = '\0';
    } else
        strcpy(port, "80");

    /*
     * This function may be called with sockaddr_* already set as it may
     * be getting updated so we need to free the old entries safely.
     * Use a temporary variable so they never dereference */
    if (*sockaddr_port && !safecmp(*sockaddr_port, port))
        free(port);
    else {
        tmp = *sockaddr_port;
        *sockaddr_port = port;
        free(tmp);
    }
    if (*sockaddr_url && !safecmp(*sockaddr_url, url_address))
        free(url_address);
    else {
        tmp = *sockaddr_url;
        *sockaddr_url = url_address;
        free(tmp);
    }

    return true;
}

/* Convert a sockaddr structure into a url and port. URL should be a string of
 * INET6_ADDRSTRLEN size, port at least a string of 6 bytes */
bool url_from_sockaddr(const struct sockaddr* addr, char* url, char* port) {
    int port_no = 0;

    switch (addr->sa_family) {
        const struct sockaddr_in*  inet4_in;
        const struct sockaddr_in6* inet6_in;

        case AF_INET:
            inet4_in = (struct sockaddr_in*)addr;
            inet_ntop(AF_INET, &inet4_in->sin_addr, url, INET6_ADDRSTRLEN);
            port_no = htons(inet4_in->sin_port);
            break;
        case AF_INET6:
            inet6_in = (struct sockaddr_in6*)addr;
            inet_ntop(AF_INET6, &inet6_in->sin6_addr, url, INET6_ADDRSTRLEN);
            port_no = htons(inet6_in->sin6_port);
            break;
        default:
            return false;
    }
    sprintf(port, "%d", port_no);
    return true;
}

/* Helper for getaddrinfo with the same API that retries while getting
 * EAI_AGAIN error */
static int addrgetinfo(const char* node, const char* service, const struct addrinfo* hints, struct addrinfo** res) {
    int ret;

    do {
        ret = getaddrinfo(node, service, hints, res);
    } while (ret == EAI_AGAIN);

    return ret;
}

bool addrinfo_from_url(const char* url, const char* port, struct addrinfo* addrinfo) {
    struct addrinfo *servinfo, hints;

    memset(&hints, 0, sizeof(struct addrinfo));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    servinfo = addrinfo;
    if (addrgetinfo(url, port, &hints, &servinfo) != 0)
        return false;
    if (!servinfo)
        return false;
    memcpy(addrinfo, servinfo->ai_addr, servinfo->ai_addrlen);
    freeaddrinfo(servinfo);
    return true;
}

/* Extract a resolved url and port from a serverurl string. newurl must be
 * a string of at least INET6_ADDRSTRLEN and newport at least 6 bytes. */
bool url_from_serverurl(char* serverurl, char* newurl, char* newport) {
    char *          url = NULL, *port = NULL;
    struct addrinfo addrinfo;
    bool            ret = false;

    if (!extract_sockaddr(serverurl, &url, &port)) {
        LOGWARNING("Failed to extract server address from %s", serverurl);
        goto out;
    }
    if (!addrinfo_from_url(url, port, &addrinfo)) {
        LOGWARNING("Failed to extract addrinfo from url %s:%s", url, port);
        goto out;
    }
    if (!url_from_sockaddr((const struct sockaddr*)&addrinfo, newurl, newport)) {
        LOGWARNING("Failed to extract url from sockaddr for original url: %s:%s", url, port);
        goto out;
    }
    ret = true;
out:
    dealloc(url);
    dealloc(port);
    return ret;
}

/* Convert a socket into a url and port. URL should be a string of
 * INET6_ADDRSTRLEN size, port at least a string of 6 bytes */
bool url_from_socket(const int sockd, char* url, char* port) {
    struct sockaddr_storage storage;
    socklen_t               addrlen = sizeof(struct sockaddr_storage);
    struct sockaddr*        addr = (struct sockaddr*)&storage;

    if (sockd < 1)
        return false;
    if (getsockname(sockd, addr, &addrlen))
        return false;
    if (!url_from_sockaddr(addr, url, port))
        return false;
    return true;
}

void keep_sockalive(int fd) {
    const int tcp_one = 1;
    const int tcp_keepidle = 45;
    const int tcp_keepintvl = 30;

    setsockopt(fd, SOL_SOCKET, SO_KEEPALIVE, (const void*)&tcp_one, sizeof(tcp_one));
    setsockopt(fd, SOL_TCP, TCP_NODELAY, (const void*)&tcp_one, sizeof(tcp_one));
    setsockopt(fd, SOL_TCP, TCP_KEEPCNT, &tcp_one, sizeof(tcp_one));
    setsockopt(fd, SOL_TCP, TCP_KEEPIDLE, &tcp_keepidle, sizeof(tcp_keepidle));
    setsockopt(fd, SOL_TCP, TCP_KEEPINTVL, &tcp_keepintvl, sizeof(tcp_keepintvl));
}

void nolinger_socket(int fd) {
    const struct linger so_linger = {1, 0};

    setsockopt(fd, SOL_SOCKET, SO_LINGER, &so_linger, sizeof(so_linger));
}

void noblock_socket(int fd) {
    int flags = fcntl(fd, F_GETFL, 0);

    fcntl(fd, F_SETFL, O_NONBLOCK | flags);
}

void block_socket(int fd) {
    int flags = fcntl(fd, F_GETFL, 0);

    fcntl(fd, F_SETFL, flags & ~O_NONBLOCK);
}

void _close(int* fd, const char* file, const char* func, const int line) {
    int sockd;

    if (*fd < 0)
        return;
    sockd = *fd;
    LOGDEBUG("Closing file handle %d", sockd);
    *fd = -1;
    if (unlikely(close(sockd))) {
        LOGWARNING(
            "Close of fd %d failed with errno %d:%s from %s %s:%d", sockd, errno, strerror(errno), file, func, line);
    }
}

int bind_socket(char* url, char* port) {
    struct addrinfo servinfobase, *servinfo, hints, *p;
    int             ret, sockd = -1;
    const int       on = 1;

    memset(&hints, 0, sizeof(struct addrinfo));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    servinfo = &servinfobase;

    if (addrgetinfo(url, port, &hints, &servinfo) != 0) {
        LOGWARNING("Failed to resolve (?wrong URL) %s:%s", url, port);
        return sockd;
    }
    for (p = servinfo; p != NULL; p = p->ai_next) {
        sockd = socket(p->ai_family, p->ai_socktype, p->ai_protocol);
        if (sockd > 0)
            break;
    }
    if (sockd < 1 || p == NULL) {
        LOGWARNING("Failed to open socket for %s:%s", url, port);
        goto out;
    }
    setsockopt(sockd, SOL_SOCKET, SO_REUSEADDR, &on, sizeof(on));
    ret = bind(sockd, p->ai_addr, p->ai_addrlen);
    if (ret < 0) {
        LOGWARNING("Failed to bind socket for %s:%s", url, port);
        Close(sockd);
        goto out;
    }

out:
    freeaddrinfo(servinfo);
    return sockd;
}

int connect_socket(char* url, char* port) {
    struct addrinfo servinfobase, *servinfo, hints, *p;
    int             sockd = -1;

    memset(&hints, 0, sizeof(struct addrinfo));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    memset(&servinfobase, 0, sizeof(struct addrinfo));
    servinfo = &servinfobase;

    if (addrgetinfo(url, port, &hints, &servinfo) != 0) {
        LOGWARNING("Failed to resolve (?wrong URL) %s:%s", url, port);
        goto out;
    }

    for (p = servinfo; p != NULL; p = p->ai_next) {
        sockd = socket(p->ai_family, p->ai_socktype, p->ai_protocol);
        if (sockd == -1) {
            LOGDEBUG("Failed socket");
            continue;
        }

        /* Iterate non blocking over entries returned by getaddrinfo
         * to cope with round robin DNS entries, finding the first one
         * we can connect to quickly. */
        noblock_socket(sockd);
        if (connect(sockd, p->ai_addr, p->ai_addrlen) == -1) {
            int selret;

            if (!sock_connecting()) {
                Close(sockd);
                LOGDEBUG("Failed sock connect");
                continue;
            }
            selret = wait_write_select(sockd, 5);
            if (selret > 0) {
                socklen_t len;
                int       err, n;

                len = sizeof(err);
                n = getsockopt(sockd, SOL_SOCKET, SO_ERROR, (void*)&err, &len);
                if (!n && !err) {
                    LOGDEBUG("Succeeded delayed connect");
                    block_socket(sockd);
                    break;
                }
            }
            Close(sockd);
            LOGDEBUG("Select timeout/failed connect");
            continue;
        }
        LOGDEBUG("Succeeded immediate connect");
        if (sockd >= 0)
            block_socket(sockd);

        break;
    }
    if (p == NULL) {
        LOGINFO("Failed to connect to %s:%s", url, port);
        sockd = -1;
    }
    freeaddrinfo(servinfo);
out:
    return sockd;
}

/* Measure the minimum round trip time it should take to get to a url by attempting
 * to connect to what should be a closed socket on port 1042. This is a blocking
 * function so can take many seconds. Returns 0 on failure */
int round_trip(char* url) {
    struct addrinfo servinfobase, *p, hints;
    int             sockd = -1, ret = 0, i, diff;
    tv_t            start_tv, end_tv;
    char            port[] = "1042";

    memset(&hints, 0, sizeof(struct addrinfo));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    memset(&servinfobase, 0, sizeof(struct addrinfo));
    p = &servinfobase;

    if (addrgetinfo(url, port, &hints, &p) != 0) {
        LOGWARNING("Failed to resolve (?wrong URL) %s:%s", url, port);
        return ret;
    }
    /* This function should be called only on already-resolved IP addresses so
     * we only need to use the first result from servinfobase */
    sockd = socket(p->ai_family, p->ai_socktype, p->ai_protocol);
    if (sockd == -1) {
        LOGERR("Failed socket");
        goto out;
    }
    /* Attempt to connect 5 times to what should be a closed port and measure
     * the time it takes to get a refused message */
    for (i = 0; i < 5; i++) {
        tv_time(&start_tv);
        if (!connect(sockd, p->ai_addr, p->ai_addrlen) || errno != ECONNREFUSED) {
            LOGINFO("Unable to get round trip due to %s:%s connect not being refused", url, port);
            goto out;
        }
        tv_time(&end_tv);
        diff = ms_tvdiff(&end_tv, &start_tv);
        if (!ret || diff < ret)
            ret = diff;
    }
    if (ret > 500) {
        LOGINFO("Round trip to %s:%s greater than 500ms at %d, clamping to 500", url, port, diff);
        diff = 500;
    }
    LOGINFO("Minimum round trip to %s:%s calculated as %dms", url, port, ret);
out:
    Close(sockd);
    freeaddrinfo(p);
    return ret;
}

int write_socket(int fd, const void* buf, size_t nbyte) {
    int ret;

    ret = wait_write_select(fd, 5);
    if (ret < 1) {
        if (!ret)
            LOGNOTICE("Select timed out in write_socket");
        else
            LOGNOTICE("Select failed in write_socket");
        goto out;
    }
    ret = write_length(fd, buf, nbyte);
    if (ret < 0)
        LOGNOTICE("Failed to write in write_socket");
out:
    return ret;
}

void empty_socket(int fd) {
    char buf[PAGESIZE];
    int  ret;

    if (fd < 1)
        return;

    do {
        ret = recv(fd, buf, PAGESIZE - 1, MSG_DONTWAIT);
        if (ret > 0) {
            buf[ret] = 0;
            LOGDEBUG("Discarding: %s", buf);
        }
    } while (ret > 0);
}

void _close_unix_socket(int* sockd, const char* server_path) {
    LOGDEBUG("Closing unix socket %d %s", *sockd, server_path);
    _Close(sockd);
}

int _open_unix_server(const char* server_path, const char* file, const char* func, const int line) {
    mode_t             mode = S_IRWXU | S_IRWXG;  // Owner+Group RWX
    struct sockaddr_un serveraddr;
    int                sockd = -1, len, ret;
    struct stat        buf;

    if (likely(server_path)) {
        len = strlen(server_path);
        if (unlikely(len < 1 || len >= UNIX_PATH_MAX)) {
            LOGERR("Invalid server path length %d in open_unix_server", len);
            goto out;
        }
    } else {
        LOGERR("Null passed as server_path to open_unix_server");
        goto out;
    }

    if (!stat(server_path, &buf)) {
        if ((buf.st_mode & S_IFMT) == S_IFSOCK) {
            ret = unlink(server_path);
            if (ret) {
                LOGERR("Unlink of %s failed in open_unix_server", server_path);
                goto out;
            }
            LOGDEBUG("Unlinked %s to recreate socket", server_path);
        } else {
            LOGWARNING("%s already exists and is not a socket, not removing", server_path);
            goto out;
        }
    }

    sockd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (unlikely(sockd < 0)) {
        LOGERR("Failed to open socket in open_unix_server");
        goto out;
    }
    memset(&serveraddr, 0, sizeof(serveraddr));
    serveraddr.sun_family = AF_UNIX;
    strcpy(serveraddr.sun_path, server_path);

    ret = bind(sockd, (struct sockaddr*)&serveraddr, sizeof(serveraddr));
    if (unlikely(ret < 0)) {
        LOGERR("Failed to bind to socket in open_unix_server");
        close_unix_socket(sockd, server_path);
        sockd = -1;
        goto out;
    }

    ret = chmod(server_path, mode);
    if (unlikely(ret < 0))
        LOGERR("Failed to set mode in open_unix_server - continuing");

    ret = listen(sockd, SOMAXCONN);
    if (unlikely(ret < 0)) {
        LOGERR("Failed to listen to socket in open_unix_server");
        close_unix_socket(sockd, server_path);
        sockd = -1;
        goto out;
    }

    LOGDEBUG("Opened server path %s successfully on socket %d", server_path, sockd);
out:
    if (unlikely(sockd == -1))
        LOGERR("Failure in open_unix_server from %s %s:%d", file, func, line);
    return sockd;
}

int _open_unix_client(const char* server_path, const char* file, const char* func, const int line) {
    struct sockaddr_un serveraddr;
    int                sockd = -1, len, ret;

    if (likely(server_path)) {
        len = strlen(server_path);
        if (unlikely(len < 1 || len >= UNIX_PATH_MAX)) {
            LOGERR("Invalid server path length %d in open_unix_client", len);
            goto out;
        }
    } else {
        LOGERR("Null passed as server_path to open_unix_client");
        goto out;
    }

    sockd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (unlikely(sockd < 0)) {
        LOGERR("Failed to open socket in open_unix_client");
        goto out;
    }
    memset(&serveraddr, 0, sizeof(serveraddr));
    serveraddr.sun_family = AF_UNIX;
    strcpy(serveraddr.sun_path, server_path);

    ret = connect(sockd, (struct sockaddr*)&serveraddr, sizeof(serveraddr));
    if (unlikely(ret < 0)) {
        LOGERR("Failed to bind to socket in open_unix_client");
        Close(sockd);
        goto out;
    }

    LOGDEBUG("Opened client path %s successfully on socket %d", server_path, sockd);
out:
    if (unlikely(sockd == -1))
        LOGERR("Failure in open_unix_client from %s %s:%d", file, func, line);
    return sockd;
}

/* Wait till a socket has been closed at the other end */
int wait_close(int sockd, int timeout) {
    struct pollfd sfd;
    int           ret;

    if (unlikely(sockd < 0))
        return -1;
    sfd.fd = sockd;
    sfd.events = POLLRDHUP;
    sfd.revents = 0;
    timeout *= 1000;
    ret = poll(&sfd, 1, timeout);
    if (ret < 1)
        return 0;
    return sfd.revents & (POLLHUP | POLLRDHUP | POLLERR);
}

/* Emulate a select read wait for high fds that select doesn't support. */
int wait_read_select(int sockd, float timeout) {
    struct epoll_event event = {0, {NULL}};
    int                epfd, ret;

    epfd = epoll_create1(EPOLL_CLOEXEC);
    event.events = EPOLLIN | EPOLLRDHUP;
    epoll_ctl(epfd, EPOLL_CTL_ADD, sockd, &event);
    timeout *= 1000;
    ret = epoll_wait(epfd, &event, 1, timeout);
    close(epfd);
    return ret;
}

int read_length(int sockd, void* buf, int len) {
    int ret, ofs = 0;

    if (unlikely(len < 1)) {
        LOGWARNING("Invalid read length of %d requested in read_length", len);
        return -1;
    }
    if (unlikely(sockd < 0))
        return -1;
    while (len) {
        ret = recv(sockd, buf + ofs, len, MSG_WAITALL);
        if (unlikely(ret < 1))
            return -1;
        ofs += ret;
        len -= ret;
    }
    return ofs;
}

/* Use a standard message across the unix sockets:
 * 4 byte length of message as little endian encoded uint32_t followed by the
 * string. Return NULL in case of failure. */
char* _recv_unix_msg(int sockd, int timeout1, int timeout2, const char* file, const char* func, const int line) {
    char*    buf = NULL;
    uint32_t msglen;
    int      ret, ern;

    ret = wait_read_select(sockd, timeout1);
    if (unlikely(ret < 1)) {
        ern = errno;
        LOGERR("Select1 failed in recv_unix_msg (%d)", ern);
        goto out;
    }
    /* Get message length */
    ret = read_length(sockd, &msglen, 4);
    if (unlikely(ret < 4)) {
        ern = errno;
        LOGERR("Failed to read 4 byte length in recv_unix_msg (%d?)", ern);
        goto out;
    }
    msglen = le32toh(msglen);
    /* Cap the length rather than trusting it. The old 0x80000000 bound
     * both allowed a ~2GB allocation per connection and was itself
     * inclusive, at which point the (int)msglen comparison below could not
     * detect a failed read because read_length returns -1 and -1 < INT_MIN
     * is false, so 2GB of zeroes was returned as a valid message. */
    if (unlikely(msglen < 1 || msglen > MAX_UNIX_MSGSIZE)) {
        LOGWARNING("Invalid message length %u sent to recv_unix_msg", msglen);
        goto out;
    }
    ret = wait_read_select(sockd, timeout2);
    if (unlikely(ret < 1)) {
        ern = errno;
        LOGERR("Select2 failed in recv_unix_msg (%d)", ern);
        goto out;
    }
    /* Bound the body read. It is a blocking MSG_WAITALL loop issued from
     * the single threaded accept loops, so a peer that sends a length then
     * stalls would otherwise freeze all pool IPC indefinitely. */
    {
        struct timeval tv;

        tv.tv_sec = timeout2 > 0 ? timeout2 : 60;
        tv.tv_usec = 0;
        setsockopt(sockd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    }
    buf = ckzalloc(msglen + 1);
    ret = read_length(sockd, buf, msglen);
    if (unlikely(ret < 0 || (uint32_t)ret < msglen)) {
        ern = errno;
        LOGERR("Failed to read %u bytes in recv_unix_msg (%d?)", msglen, ern);
        dealloc(buf);
    }
out:
    shutdown(sockd, SHUT_RD);
    if (unlikely(!buf))
        LOGERR("Failure in recv_unix_msg from %s %s:%d", file, func, line);
    return buf;
}

/* Emulate a select write wait for high fds that select doesn't support */
int wait_write_select(int sockd, float timeout) {
    struct epoll_event event = {0, {NULL}};
    int                epfd, ret;

    epfd = epoll_create1(EPOLL_CLOEXEC);
    event.events = EPOLLOUT | EPOLLRDHUP;
    epoll_ctl(epfd, EPOLL_CTL_ADD, sockd, &event);
    timeout *= 1000;
    ret = epoll_wait(epfd, &event, 1, timeout);
    close(epfd);
    return ret;
}

int _write_length(int sockd, const void* buf, int len, const char* file, const char* func, const int line) {
    int ret, ofs = 0, ern;

    if (unlikely(len < 1)) {
        LOGWARNING("Invalid write length of %d requested in write_length from %s %s:%d", len, file, func, line);
        return -1;
    }
    if (unlikely(sockd < 0)) {
        ern = errno;
        LOGWARNING("Attempt to write to invalidated sock in write_length from %s %s:%d", file, func, line);
        return -1;
    }
    while (len) {
        ret = write(sockd, buf + ofs, len);
        if (unlikely(ret < 0)) {
            ern = errno;
            LOGERR("Failed to write %d bytes in write_length (%d) from %s %s:%d", len, ern, file, func, line);
            return -1;
        }
        ofs += ret;
        len -= ret;
    }
    return ofs;
}

bool _send_unix_msg(int sockd, const char* buf, int timeout, const char* file, const char* func, const int line) {
    uint32_t msglen, len;
    bool     retval = false;
    int      ret, ern;

    if (unlikely(sockd < 0)) {
        LOGWARNING("Attempting to send unix message to invalidated sockd %d", sockd);
        goto out;
    }
    if (unlikely(!buf)) {
        LOGWARNING("Null message sent to send_unix_msg");
        goto out;
    }
    len = strlen(buf);
    if (unlikely(!len)) {
        LOGWARNING("Zero length message sent to send_unix_msg");
        goto out;
    }
    msglen = htole32(len);
    ret = wait_write_select(sockd, timeout);
    if (unlikely(ret < 1)) {
        ern = errno;
        LOGERR("Select1 failed in send_unix_msg (%d)", ern);
        goto out;
    }
    ret = _write_length(sockd, &msglen, 4, file, func, line);
    if (unlikely(ret < 4)) {
        LOGERR("Failed to write 4 byte length in send_unix_msg");
        goto out;
    }
    ret = wait_write_select(sockd, timeout);
    if (unlikely(ret < 1)) {
        ern = errno;
        LOGERR("Select2 failed in send_unix_msg (%d)", ern);
        goto out;
    }
    ret = _write_length(sockd, buf, len, file, func, line);
    if (unlikely(ret < 0)) {
        LOGERR("Failed to write %d bytes in send_unix_msg", len);
        goto out;
    }
    retval = true;
out:
    shutdown(sockd, SHUT_WR);
    if (unlikely(!retval))
        LOGERR("Failure in send_unix_msg from %s %s:%d", file, func, line);
    return retval;
}

bool _send_unix_data(int sockd, const struct msghdr* msg, const char* file, const char* func, const int line) {
    bool retval = false;
    int  ret;

    if (unlikely(!msg)) {
        LOGWARNING("Null message sent to send_unix_data");
        goto out;
    }
    ret = wait_write_select(sockd, UNIX_WRITE_TIMEOUT);
    if (unlikely(ret < 1)) {
        LOGERR("Select1 failed in send_unix_data");
        goto out;
    }
    ret = sendmsg(sockd, msg, 0);
    if (unlikely(ret < 1)) {
        LOGERR("Failed to send in send_unix_data");
        goto out;
    }
    retval = true;
out:
    shutdown(sockd, SHUT_WR);
    if (unlikely(!retval))
        LOGERR("Failure in send_unix_data from %s %s:%d", file, func, line);
    return retval;
}

bool _recv_unix_data(int sockd, struct msghdr* msg, const char* file, const char* func, const int line) {
    bool retval = false;
    int  ret;

    ret = wait_read_select(sockd, UNIX_READ_TIMEOUT);
    if (unlikely(ret < 1)) {
        LOGERR("Select1 failed in recv_unix_data");
        goto out;
    }
    ret = recvmsg(sockd, msg, MSG_WAITALL);
    if (unlikely(ret < 0)) {
        LOGERR("Failed to recv in recv_unix_data");
        goto out;
    }
    retval = true;
out:
    shutdown(sockd, SHUT_RD);
    if (unlikely(!retval))
        LOGERR("Failure in recv_unix_data from %s %s:%d", file, func, line);
    return retval;
}

#define CONTROLLLEN CMSG_LEN(sizeof(int))
#define MAXLINE 4096

/* Send a msghdr containing fd via the unix socket sockd */
bool _send_fd(int fd, int sockd, const char* file, const char* func, const int line) {
    struct cmsghdr* cmptr = ckzalloc(CONTROLLLEN);
    struct iovec    iov[1];
    struct msghdr   msg;
    char            buf[2];
    bool            ret;
    int*            cm;

    memset(&msg, 0, sizeof(struct msghdr));
    iov[0].iov_base = buf;
    iov[0].iov_len = 2;
    msg.msg_iov = iov;
    msg.msg_iovlen = 1;
    msg.msg_name = NULL;
    msg.msg_namelen = 0;
    msg.msg_controllen = CONTROLLLEN;
    msg.msg_control = cmptr;
    cmptr->cmsg_level = SOL_SOCKET;
    cmptr->cmsg_type = SCM_RIGHTS;
    cmptr->cmsg_len = CONTROLLLEN;
    cm = (int*)CMSG_DATA(cmptr);
    *cm = fd;
    buf[1] = 0;
    buf[0] = 0;
    ret = send_unix_data(sockd, &msg);
    free(cmptr);
    if (!ret)
        LOGERR("Failed to send_unix_data in send_fd from %s %s:%d", file, func, line);
    return ret;
}

/* Receive an fd by reading a msghdr from the unix socket sockd */
int _get_fd(int sockd, const char* file, const char* func, const int line) {
    int             newfd = -1;
    char            buf[MAXLINE];
    struct iovec    iov[1];
    struct msghdr   msg;
    struct cmsghdr* cmptr = ckzalloc(CONTROLLLEN);
    int*            cm;

    memset(&msg, 0, sizeof(struct msghdr));
    iov[0].iov_base = buf;
    iov[0].iov_len = sizeof(buf);
    msg.msg_iov = iov;
    msg.msg_name = NULL;
    msg.msg_namelen = 0;
    msg.msg_control = cmptr;
    msg.msg_controllen = CONTROLLLEN;
    if (!recv_unix_data(sockd, &msg)) {
        LOGERR("Failed to recv_unix_data in get_fd from %s %s:%d", file, func, line);
        /* cmptr was zeroed by ckzalloc; reading CMSG_DATA here would
         * return fd 0 (stdin) as if the transfer succeeded. */
        goto out;
    }
    if (unlikely(msg.msg_controllen < sizeof(struct cmsghdr) || cmptr->cmsg_len < CONTROLLLEN)) {
        LOGERR("Missing fd control message in get_fd from %s %s:%d", file, func, line);
        goto out;
    }
    cm = (int*)CMSG_DATA(cmptr);
    newfd = *cm;
out:
    free(cmptr);
    return newfd;
}

char* rotating_filename(const char* path, time_t when) {
    char*     filename;
    struct tm tm;

    gmtime_r(&when, &tm);
    ASPRINTF(&filename, "%s%04d%02d%02d%02d.log", path, tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour);
    return filename;
}

/* Creates a logfile entry which changes filename hourly with exclusive access */
bool rotating_log(const char* path, const char* msg) {
    mode_t mode = S_IRUSR | S_IWUSR | S_IRGRP | S_IROTH;
    char*  filename;
    FILE*  fp;
    int    fd;
    bool   ok = false;

    filename = rotating_filename(path, time(NULL));
    fd = open(filename, O_CREAT | O_RDWR | O_CLOEXEC, mode);
    if (unlikely(fd == -1)) {
        LOGERR("Failed to open %s in rotating_log!", filename);
        goto stageleft;
    }
    fp = fdopen(fd, "ae");
    if (unlikely(!fp)) {
        Close(fd);
        LOGERR("Failed to fdopen %s in rotating_log!", filename);
        goto stageleft;
    }
    if (unlikely(flock(fd, LOCK_EX))) {
        fclose(fp);
        LOGERR("Failed to flock %s in rotating_log!", filename);
        goto stageleft;
    }
    fprintf(fp, "%s\n", msg);
    fclose(fp);
    ok = true;

stageleft:
    free(filename);

    return ok;
}

/* Align a size_t to 4 byte boundaries for fussy arches */
void align_len(size_t* len) {
    if (*len % 4)
        *len += 4 - (*len % 4);
}

void* _ckrealloc(void* ptr, size_t size, const char* file, const char* func, const int line) {
    int   backoff = 1;
    void* new_ptr;

    while (42) {
        new_ptr = realloc(ptr, size);
        if (likely(new_ptr))
            break;
        if (backoff == 1)
            fprintf(stderr, "Failed to realloc %d, retrying from %s %s:%d\n", (int)size, file, func, line);
        cksleep_ms(backoff);
        /* Cap the backoff. Doubling without bound is undefined once it
         * overflows and turns a transient allocation failure into
         * effectively permanent sleep. */
        if (backoff < 1000)
            backoff <<= 1;
    }
    return new_ptr;
}

/* Malloc failure should be fatal but keep backing off and retrying as the OS
 * will kill us eventually if it can't recover. */
void realloc_strcat(char** ptr, const char* s) {
    size_t old, new, len;
    int    backoff = 1;
    void*  new_ptr;
    char*  ofs;

    if (unlikely(!*s)) {
        LOGWARNING("Passed empty pointer to realloc_strcat");
        return;
    }
    new = strlen(s);
    if (unlikely(!new)) {
        LOGWARNING("Passed empty string to realloc_strcat");
        return;
    }
    if (!*ptr)
        old = 0;
    else
        old = strlen(*ptr);
    len = old + new + 1;
    len = round_up_page(len);
    while (42) {
        new_ptr = realloc(*ptr, len);
        if (likely(new_ptr))
            break;
        if (backoff == 1)
            fprintf(stderr, "Failed to realloc_strcat %d, retrying\n", (int)len);
        cksleep_ms(backoff);
        /* Cap the backoff. Doubling without bound is undefined once it
         * overflows and turns a transient allocation failure into
         * effectively permanent sleep. */
        if (backoff < 1000)
            backoff <<= 1;
    }
    *ptr = new_ptr;
    ofs = *ptr + old;
    sprintf(ofs, "%s", s);
}

void trail_slash(char** buf) {
    int ofs;

    ofs = strlen(*buf) - 1;
    if (memcmp(*buf + ofs, "/", 1))
        realloc_strcat(buf, "/");
}

void* _ckalloc(size_t len, const char* file, const char* func, const int line) {
    int   backoff = 1;
    void* ptr;

    align_len(&len);
    while (42) {
        ptr = malloc(len);
        if (likely(ptr))
            break;
        if (backoff == 1) {
            fprintf(stderr, "Failed to ckalloc %d, retrying from %s %s:%d\n", (int)len, file, func, line);
        }
        cksleep_ms(backoff);
        /* Cap the backoff. Doubling without bound is undefined once it
         * overflows and turns a transient allocation failure into
         * effectively permanent sleep. */
        if (backoff < 1000)
            backoff <<= 1;
    }
    return ptr;
}

void* _ckzalloc(size_t len, const char* file, const char* func, const int line) {
    int   backoff = 1;
    void* ptr;

    align_len(&len);
    while (42) {
        ptr = calloc(len, 1);
        if (likely(ptr))
            break;
        if (backoff == 1) {
            fprintf(stderr, "Failed to ckzalloc %d, retrying from %s %s:%d\n", (int)len, file, func, line);
        }
        cksleep_ms(backoff);
        /* Cap the backoff. Doubling without bound is undefined once it
         * overflows and turns a transient allocation failure into
         * effectively permanent sleep. */
        if (backoff < 1000)
            backoff <<= 1;
    }
    return ptr;
}

/* Round up to the nearest page size for efficient malloc */
size_t round_up_page(size_t len) {
    int rem = len % PAGESIZE;

    if (rem)
        len += PAGESIZE - rem;
    return len;
}

/* Adequate size s==len*2 + 1 must be alloced to use this variant */
void __bin2hex(void* vs, const void* vp, size_t len) {
    static const char hex[16] = {'0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f'};
    const uchar*      p = vp;
    uchar*            s = vs;
    int               i;

    for (i = 0; i < (int)len; i++) {
        *s++ = hex[p[i] >> 4];
        *s++ = hex[p[i] & 0xF];
    }
    *s++ = '\0';
}

/* Returns a malloced array string of a binary value of arbitrary length. The
 * array is rounded up to a 4 byte size to appease architectures that need
 * aligned array  sizes */
void* bin2hex(const void* vp, size_t len) {
    const uchar* p = vp;
    size_t       slen;
    uchar*       s;

    slen = len * 2 + 1;
    s = ckzalloc(slen);
    __bin2hex(s, p, len);

    return s;
}

const int hex2bin_tbl[256] = {
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 0,  1,  2,  3,  4,  5,  6,  7,  8,  9,
    -1, -1, -1, -1, -1, -1, -1, 10, 11, 12, 13, 14, 15, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 10, 11, 12, 13, 14, 15, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
};

bool _validhex(const char* buf, const char* file, const char* func, const int line) {
    unsigned int i, slen;
    bool         ret = false;

    slen = strlen(buf);
    if (!slen || slen % 2) {
        LOGDEBUG("Invalid hex due to length %u from %s %s:%d", slen, file, func, line);
        goto out;
    }
    for (i = 0; i < slen; i++) {
        uchar idx = buf[i];

        if (hex2bin_tbl[idx] == -1) {
            LOGDEBUG("Invalid hex due to value %u at offset %d from %s %s:%d", idx, i, file, func, line);
            goto out;
        }
    }
    ret = true;
out:
    return ret;
}

/* Does the reverse of bin2hex but does not allocate any ram */
bool _hex2bin(void* vp, const void* vhexstr, size_t len, const char* file, const char* func, const int line) {
    const uchar* hexstr = vhexstr;
    int          nibble1, nibble2;
    bool         ret = false;
    uchar*       p = vp;
    uchar        idx;

    while (*hexstr && len) {
        if (unlikely(!hexstr[1])) {
            LOGWARNING("Early end of string in hex2bin from %s %s:%d", file, func, line);
            return ret;
        }

        idx = *hexstr++;
        nibble1 = hex2bin_tbl[idx];
        idx = *hexstr++;
        nibble2 = hex2bin_tbl[idx];

        if (unlikely((nibble1 < 0) || (nibble2 < 0))) {
            LOGWARNING("Invalid binary encoding in hex2bin from %s %s:%d", file, func, line);
            return ret;
        }

        *p++ = (((uchar)nibble1) << 4) | ((uchar)nibble2);
        --len;
    }

    if (likely(len == 0 && *hexstr == 0))
        ret = true;
    if (!ret)
        LOGWARNING("Failed hex2bin decode from %s %s:%d", file, func, line);
    return ret;
}

static const int b58tobin_tbl[] = {-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
                                   -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
                                   -1, -1, -1, -1, -1, -1, -1, 0,  1,  2,  3,  4,  5,  6,  7,  8,  -1, -1, -1, -1, -1,
                                   -1, -1, 9,  10, 11, 12, 13, 14, 15, 16, -1, 17, 18, 19, 20, 21, -1, 22, 23, 24, 25,
                                   26, 27, 28, 29, 30, 31, 32, -1, -1, -1, -1, -1, -1, 33, 34, 35, 36, 37, 38, 39, 40,
                                   41, 42, 43, -1, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57};

/* b58bin should always be at least 25 bytes long and already checked to be
 * valid. */
void b58tobin(char* b58bin, const char* b58) {
    uint32_t c, bin32[7];
    int      len, i, j;
    uint64_t t;

    memset(bin32, 0, 7 * sizeof(uint32_t));
    len = strlen((const char*)b58);
    for (i = 0; i < len; i++) {
        /* char may be signed and the table only covers up to 'z', so
         * bound the index rather than trusting the caller to have
         * validated the string. */
        c = (uint8_t)b58[i];
        if (unlikely(c >= sizeof(b58tobin_tbl) / sizeof(b58tobin_tbl[0])))
            c = (uint32_t)-1;
        else
            c = b58tobin_tbl[c];
        for (j = 6; j >= 0; j--) {
            t = ((uint64_t)bin32[j]) * 58 + c;
            c = (t & 0x3f00000000ull) >> 32;
            bin32[j] = t & 0xffffffffull;
        }
    }
    *(b58bin++) = bin32[0] & 0xff;
    for (i = 1; i < 7; i++) {
        uint32_t val = htobe32(bin32[i]);

        memcpy(b58bin, &val, sizeof(uint32_t));
        b58bin += sizeof(uint32_t);
    }
}

/* Does a safe string comparison tolerating zero length and NULL strings */
int safecmp(const char* a, const char* b) {
    int lena, lenb;

    if (unlikely(!a || !b)) {
        if (a != b)
            return -1;
        return 0;
    }
    lena = strlen(a);
    lenb = strlen(b);
    if (unlikely(!lena || !lenb)) {
        if (lena != lenb)
            return -1;
        return 0;
    }
    return (strcmp(a, b));
}

/* Returns whether there is a case insensitive match of buf to cmd, safely
 * handling NULL or zero length strings. */
bool cmdmatch(const char* buf, const char* cmd) {
    int cmdlen, buflen;

    if (!buf)
        return false;
    buflen = strlen(buf);
    if (!buflen)
        return false;
    cmdlen = strlen(cmd);
    if (buflen < cmdlen)
        return false;
    return !strncasecmp(buf, cmd, cmdlen);
}

static const char base64[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/* Return a malloced string of *src encoded into mime base 64 */
char* http_base64(const char* src) {
    char * str, *dst;
    size_t l, hlen;
    int    t, r;

    l = strlen((const char*)src);
    hlen = ((l + 2) / 3) * 4 + 1;
    str = ckalloc(hlen);
    dst = str;
    r = 0;

    while (l >= 3) {
        t = (src[0] << 16) | (src[1] << 8) | src[2];
        dst[0] = base64[(t >> 18) & 0x3f];
        dst[1] = base64[(t >> 12) & 0x3f];
        dst[2] = base64[(t >> 6) & 0x3f];
        dst[3] = base64[(t >> 0) & 0x3f];
        src += 3;
        l -= 3;
        dst += 4;
        r += 4;
    }

    switch (l) {
        case 2:
            t = (src[0] << 16) | (src[1] << 8);
            dst[0] = base64[(t >> 18) & 0x3f];
            dst[1] = base64[(t >> 12) & 0x3f];
            dst[2] = base64[(t >> 6) & 0x3f];
            dst[3] = '=';
            dst += 4;
            r += 4;
            break;
        case 1:
            t = src[0] << 16;
            dst[0] = base64[(t >> 18) & 0x3f];
            dst[1] = base64[(t >> 12) & 0x3f];
            dst[2] = dst[3] = '=';
            dst += 4;
            r += 4;
            break;
        case 0:
            break;
    }
    *dst = 0;
    return (str);
}

static const int8_t charset_rev[128] = {
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, 15, -1, 10, 17,
    21, 20, 26, 30, 7,  5,  -1, -1, -1, -1, -1, -1, -1, 29, -1, 24, 13, 25, 9,  8,  23, -1, 18, 22, 31, 27,
    19, -1, 1,  0,  3,  16, 11, 28, 12, 14, 6,  4,  2,  -1, -1, -1, -1, -1, -1, 29, -1, 24, 13, 25, 9,  8,
    23, -1, 18, 22, 31, 27, 19, -1, 1,  0,  3,  16, 11, 28, 12, 14, 6,  4,  2,  -1, -1, -1, -1, -1};

/* It's assumed that there is no chance of sending invalid chars to these
 * functions as they should have been checked beforehand. */
/* Decode the data part of a bech32 address into data, which must be at least
 * data_size bytes. Returns false without writing anything if the input cannot
 * be a valid bech32 string. Callers currently only reach this after bitcoind
 * has validated the address, but this must not depend on that: without the
 * length and separator checks a long input containing no '1' yields a negative
 * hrp_len and writes far past the end of the caller's buffer. */
static bool bech32_decode(uint8_t* data, const int data_size, int* data_len, const char* input) {
    int         input_len = strlen(input), hrp_len, dlen, i;
    const char* sep;

    *data_len = 0;
    /* BIP173 caps the whole string at 90 characters. */
    if (unlikely(input_len < 8 || input_len > 90))
        return false;
    /* The separator is the last '1' in the string, and there must be one
     * with a human readable part before it and a checksum after it. */
    sep = strrchr(input, '1');
    if (unlikely(!sep))
        return false;
    hrp_len = sep - input;
    dlen = input_len - (hrp_len + 1);
    /* The data part carries a 6 character checksum we do not return. */
    if (unlikely(hrp_len < 1 || dlen < 6))
        return false;
    dlen -= 6;
    if (unlikely(dlen > data_size))
        return false;
    for (i = 0; i < dlen; i++) {
        char c = input[hrp_len + 1 + i];
        int  v = (c & 0x80) ? -1 : charset_rev[(int)c];

        if (unlikely(v < 0))
            return false;
        data[i] = v;
    }
    *data_len = dlen;
    return true;
}

static void convert_bits(char* out, int* outlen, const uint8_t* in, int inlen) {
    const int outbits = 8, inbits = 5;
    uint32_t  val = 0, maxv = (((uint32_t)1) << outbits) - 1;
    int       bits = 0;

    while (inlen--) {
        val = (val << inbits) | *(in++);
        bits += inbits;
        while (bits >= outbits) {
            bits -= outbits;
            out[(*outlen)++] = (val >> bits) & maxv;
        }
    }
}

static int address_to_pubkeytxn(char* pkh, const char* addr) {
    char b58bin[25] = {};

    b58tobin(b58bin, addr);
    pkh[0] = 0x76;
    pkh[1] = 0xa9;
    pkh[2] = 0x14;
    memcpy(&pkh[3], &b58bin[1], 20);
    pkh[23] = 0x88;
    pkh[24] = 0xac;
    return 25;
}

static int address_to_scripttxn(char* psh, const char* addr) {
    char b58bin[25] = {};

    b58tobin(b58bin, addr);
    psh[0] = 0xa9;
    psh[1] = 0x14;
    memcpy(&psh[2], &b58bin[1], 20);
    psh[22] = 0x87;
    return 23;
}

static int segaddress_to_txn(char* p2h, const char* addr) {
    int     data_len, witdata_len = 0;
    char*   witdata = &p2h[2];
    uint8_t data[84];

    if (unlikely(!bech32_decode(data, sizeof(data), &data_len, addr) || data_len < 1)) {
        LOGWARNING("Failed to decode bech32 address %s in segaddress_to_txn", addr);
        return 0;
    }
    p2h[0] = data[0];
    /* Witness version is > 0 */
    if (p2h[0])
        p2h[0] += 0x50;
    convert_bits(witdata, &witdata_len, data + 1, data_len - 1);
    p2h[1] = witdata_len;
    return witdata_len + 2;
}

/* Convert an address to a transaction and return the length of the transaction */
int address_to_txn(char* p2h, const char* addr, const bool script, const bool segwit) {
    if (segwit)
        return segaddress_to_txn(p2h, addr);
    if (script)
        return address_to_scripttxn(p2h, addr);
    return address_to_pubkeytxn(p2h, addr);
}

static const char b58_charset[] = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/* base58check(ver || hash20) — the inverse of the b58tobin() the *_to_txn()
 * functions above rely on. */
static bool b58check_encode(char* out, const size_t outsz, const uint8_t ver, const uchar* hash20) {
    uchar payload[25], check[32], digits[40] = {};
    int   digitslen = 1, zeros = 0, i, j;
    char* p = out;

    payload[0] = ver;
    memcpy(&payload[1], hash20, 20);
    gen_hash(payload, check, 21);
    memcpy(&payload[21], check, 4);

    while (zeros < 25 && !payload[zeros])
        zeros++;
    for (i = zeros; i < 25; i++) {
        unsigned int carry = payload[i];

        for (j = 0; j < digitslen; j++) {
            carry += (unsigned int)digits[j] << 8;
            digits[j] = carry % 58;
            carry /= 58;
        }
        while (carry) {
            digits[digitslen++] = carry % 58;
            carry /= 58;
        }
    }
    while (digitslen > 0 && !digits[digitslen - 1])
        digitslen--;
    if ((size_t)(zeros + digitslen) >= outsz)
        return false;
    for (i = 0; i < zeros; i++)
        *p++ = '1';
    for (j = digitslen; j > 0; j--)
        *p++ = b58_charset[digits[j - 1]];
    *p = '\0';
    return true;
}

/*
 * Version byte of a base58check address, or -1 if s is not one. The charset,
 * length and checksum are all verified here because b58tobin() assumes a string
 * that has already been checked, and because the whole point of asking is that
 * s may be an arbitrary worker name.
 */
static int b58check_version(const char* s) {
    uchar bin[25], check[32];
    int   len, i;

    if (!s)
        return -1;
    len = strlen(s);
    /*
     * 25 bytes is at most 35 base58 digits - which is what a 0xc4 version
     * (testnet P2SH) takes - and fewer as the leading byte shrinks. Anything
     * longer cannot be a 25 byte payload, and b58tobin() has nowhere to put it.
     */
    if (len < 26 || len > 35)
        return -1;
    for (i = 0; i < len; i++) {
        if (!memchr(b58_charset, s[i], sizeof(b58_charset) - 1))
            return -1;
    }
    b58tobin((char*)bin, s);
    gen_hash(bin, check, 21);
    if (memcmp(check, &bin[21], 4))
        return -1;
    return bin[0];
}

static const char bech32_charset[] = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";

static uint32_t bech32_polymod_step(const uint32_t pre) {
    uint8_t b = pre >> 25;

    return ((pre & 0x1ffffff) << 5) ^ (-((b >> 0) & 1) & 0x3b6a57b2UL) ^ (-((b >> 1) & 1) & 0x26508e6dUL) ^
           (-((b >> 2) & 1) & 0x1ea119faUL) ^ (-((b >> 3) & 1) & 0x3d4233ddUL) ^ (-((b >> 4) & 1) & 0x2a1462b3UL);
}

/*
 * Whether s is a bech32 or bech32m string under hrp. The prefix alone cannot
 * tell an address from a worker name that happens to begin with it, so the
 * checksum decides.
 */
static bool bech32_hrp_is(const char* s, const char* hrp) {
    size_t   hrplen = strlen(hrp), len, i;
    uint32_t chk = 1;

    if (!s)
        return false;
    len = strlen(s);
    /* hrp, separator, at least a version char, and the 6 checksum chars. */
    if (len < hrplen + 8 || len > 90)
        return false;
    if (strncasecmp(s, hrp, hrplen) || s[hrplen] != '1')
        return false;
    for (i = 0; i < hrplen; i++)
        chk = bech32_polymod_step(chk) ^ (hrp[i] >> 5);
    chk = bech32_polymod_step(chk);
    for (i = 0; i < hrplen; i++)
        chk = bech32_polymod_step(chk) ^ (hrp[i] & 0x1f);
    for (i = hrplen + 1; i < len; i++) {
        char        c = s[i] >= 'A' && s[i] <= 'Z' ? s[i] + ('a' - 'A') : s[i];
        const char* pos = memchr(bech32_charset, c, sizeof(bech32_charset) - 1);

        if (!pos)
            return false;
        chk = bech32_polymod_step(chk) ^ (uint32_t)(pos - bech32_charset);
    }
    /* bech32 (witness v0) and bech32m (v1+) differ only in the constant. */
    return chk == 1 || chk == 0x2bc830a3;
}

/* Inverse of the convert_bits() above: 8 bit groups to 5. */
static int convert_bits_to5(uint8_t* out, const uchar* in, int inlen) {
    uint32_t val = 0;
    int      bits = 0, outlen = 0;

    while (inlen--) {
        val = (val << 8) | *(in++);
        bits += 8;
        while (bits >= 5) {
            bits -= 5;
            out[outlen++] = (val >> bits) & 0x1f;
        }
    }
    if (bits)
        out[outlen++] = (val << (5 - bits)) & 0x1f;
    return outlen;
}

/* xorval is 1 for bech32 (witness v0) and 0x2bc830a3 for bech32m (v1+). */
static bool bech32_encode(
    char*          out,
    const size_t   outsz,
    const char*    hrp,
    const uint8_t* data,
    const int      datalen,
    const uint32_t xorval) {
    size_t   hrplen = strlen(hrp), i;
    uint32_t chk = 1;
    char*    p = out;

    if (hrplen + 1 + datalen + 6 >= outsz)
        return false;
    for (i = 0; i < hrplen; i++)
        chk = bech32_polymod_step(chk) ^ (hrp[i] >> 5);
    chk = bech32_polymod_step(chk);
    for (i = 0; i < hrplen; i++)
        chk = bech32_polymod_step(chk) ^ (hrp[i] & 0x1f);
    memcpy(p, hrp, hrplen);
    p += hrplen;
    *p++ = '1';
    for (i = 0; i < (size_t)datalen; i++) {
        chk = bech32_polymod_step(chk) ^ data[i];
        *p++ = bech32_charset[data[i]];
    }
    for (i = 0; i < 6; i++)
        chk = bech32_polymod_step(chk);
    chk ^= xorval;
    for (i = 0; i < 6; i++)
        *p++ = bech32_charset[(chk >> ((5 - i) * 5)) & 0x1f];
    *p = '\0';
    return true;
}

/* Convert a standard scriptPubKey back to the address that it pays, the inverse
 * of address_to_txn(). A script does not say which chain it belongs to, so the
 * chain is taken from ref, an address we already know (typically our own payout
 * address); mainnet is assumed if ref is not an address, or is one on mainnet.
 * Returns false, and leaves addr untouched, for anything that is not P2PKH,
 * P2SH or a witness program - only ever used for logging, so callers can fall
 * back to the hex. */
bool txn_to_address(char* addr, const size_t alen, const uchar* script, const int slen, const char* ref) {
    /* A witness program is at most a 40 byte push, which is 64 groups of 5
     * bits, plus the version. */
    uint8_t     p2pkh = 0x00, p2sh = 0x05, data[72];
    const char* hrp = "bc";
    int         datalen, plen, ver;

    /*
     * Which chain ref names, but only once ref is known to be an address at
     * all: it is whatever we authorise as, which is a payout address only
     * against a solo pool and a worker name against any other. A name that
     * merely begins with m, n, 2 or "tb1" must not be read as testnet, or the
     * very log line this exists to let an operator check would name an address
     * on the wrong chain. Base58 does not distinguish testnet from regtest, so
     * only a bech32 ref can select the latter.
     */
    if (ref && *ref) {
        int refver = b58check_version(ref);

        if (bech32_hrp_is(ref, "bcrt")) {
            hrp = "bcrt";
            p2pkh = 0x6f;
            p2sh = 0xc4;
        } else if (bech32_hrp_is(ref, "tb") || refver == 0x6f || refver == 0xc4) {
            /* Testnet and signet share their prefixes. */
            hrp = "tb";
            p2pkh = 0x6f;
            p2sh = 0xc4;
        }
    }
    if (slen == 25 && script[0] == 0x76 && script[1] == 0xa9 && script[2] == 0x14 && script[23] == 0x88 &&
        script[24] == 0xac)
        return b58check_encode(addr, alen, p2pkh, script + 3);
    if (slen == 23 && script[0] == 0xa9 && script[1] == 0x14 && script[22] == 0x87)
        return b58check_encode(addr, alen, p2sh, script + 2);

    /* Witness program: version opcode, then a single 2-40 byte push. */
    if (slen < 4)
        return false;
    if (script[0] && (script[0] < 0x51 || script[0] > 0x60))
        return false;
    ver = script[0] ? script[0] - 0x50 : 0;
    plen = script[1];
    if (plen < 2 || plen > 40 || plen != slen - 2)
        return false;
    /* v0 is only defined for P2WPKH and P2WSH. */
    if (!ver && plen != 20 && plen != 32)
        return false;
    data[0] = ver;
    datalen = 1 + convert_bits_to5(&data[1], script + 2, plen);
    return bech32_encode(addr, alen, hrp, data, datalen, ver ? 0x2bc830a3 : 1);
}

/* Read a bitcoin varint, advancing *p, refusing to read past end. */
static bool read_varint(uint64_t* val, const uchar** p, const uchar* end) {
    const uchar* s = *p;
    int          i;

    if (s >= end)
        return false;
    switch (*s) {
        case 0xfd:
            if (end - s < 3)
                return false;
            *val = (uint64_t)s[1] | ((uint64_t)s[2] << 8);
            *p = s + 3;
            return true;
        case 0xfe:
            if (end - s < 5)
                return false;
            *val = 0;
            for (i = 0; i < 4; i++)
                *val |= (uint64_t)s[1 + i] << (8 * i);
            *p = s + 5;
            return true;
        case 0xff:
            if (end - s < 9)
                return false;
            *val = 0;
            for (i = 0; i < 8; i++)
                *val |= (uint64_t)s[1 + i] << (8 * i);
            *p = s + 9;
            return true;
        default:
            *val = *s;
            *p = s + 1;
            return true;
    }
}

/* Find the output being paid in a coinbase that arrives split either side of the
 * extranonce hole, which is how both an SV1 mining.notify and an SV2 extended
 * job deliver one. holelen is the total number of bytes that go in the hole
 * (enonce1 + enonce2), needed because the scriptSig may continue into cb2 past
 * it. The largest value output is taken as the payout; its scriptPubKey is
 * copied to script and *slen updated, its value to value. Returns the number of
 * outputs, or -1 if the coinbase does not parse or the payout does not fit in
 * *slen bytes - callers get either the real payout or nothing. */
int coinbase_payout_script(
    uchar*       script,
    int*         slen,
    int64_t*     value,
    const uchar* cb1,
    const int    cb1len,
    const int    holelen,
    const uchar* cb2,
    const int    cb2len) {
    const uchar *p = cb1, *end = cb1 + cb1len, *best = NULL;
    uint64_t     siglen, count, olen, bestlen = 0;
    int64_t      bestval = -1;
    int          sigleft, i, j;

    /* cb1: nVersion, exactly one input, its null outpoint, then the scriptSig
     * length and however much of the scriptSig precedes the hole. */
    if (cb1len < 42 || cb2len < 5 || holelen < 0)
        return -1;
    p += 4;
    if (*p++ != 1)
        return -1;
    p += 36;
    if (!read_varint(&siglen, &p, end) || siglen > (uint64_t)0x100)
        return -1;
    sigleft = (int)siglen - (int)(end - p) - holelen;
    if (sigleft < 0 || sigleft + 4 > cb2len)
        return -1;

    /* cb2: the rest of the scriptSig, nSequence, the outputs, nLockTime. */
    p = cb2 + sigleft + 4;
    end = cb2 + cb2len;
    if (!read_varint(&count, &p, end) || !count || count > 0x100)
        return -1;
    for (i = 0; i < (int)count; i++) {
        int64_t val = 0;

        if (end - p < 8)
            return -1;
        for (j = 0; j < 8; j++)
            val |= (int64_t)p[j] << (8 * j);
        p += 8;
        if (!read_varint(&olen, &p, end) || (uint64_t)(end - p) < olen)
            return -1;
        if (val > bestval) {
            bestval = val;
            bestlen = olen;
            best = p;
        }
        p += olen;
    }
    /* nLockTime, and nothing after it. */
    if (end - p != 4 || !best || !bestlen || bestlen > (uint64_t)*slen)
        return -1;
    memcpy(script, best, bestlen);
    *slen = bestlen;
    *value = bestval;
    return (int)count;
}

/*  For encoding nHeight into coinbase, return how many bytes were used */
int ser_number(uchar* s, int32_t val) {
    uint32_t v;
    int      len;

    if (val < 0x80)
        len = 1;
    else if (val < 0x8000)
        len = 2;
    else if (val < 0x800000)
        len = 3;
    else
        len = 4;

    v = htole32(val);
    memcpy(&s[1], &v, sizeof(uint32_t));

    s[0] = len++;
    return len;
}

int get_sernumber(uchar* s) {
    int32_t val = 0;
    int     len;

    len = s[0];
    if (unlikely(len < 1 || len > 4))
        return 0;
    memcpy(&val, &s[1], len);
    return le32toh(val);
}

/* For testing a le encoded 256 byte hash against a target */
bool fulltest(const uchar* hash, const uchar* target) {
    bool ret = true;
    int  i;

    for (i = 28 / 4; i >= 0; i--) {
        uint32_t h32tmp = le32toh(read_u32(hash + i * 4));
        uint32_t t32tmp = le32toh(read_u32(target + i * 4));

        if (h32tmp > t32tmp) {
            ret = false;
            break;
        }
        if (h32tmp < t32tmp) {
            ret = true;
            break;
        }
    }
    return ret;
}

void copy_tv(tv_t* dest, const tv_t* src) {
    memcpy(dest, src, sizeof(tv_t));
}

void ts_to_tv(tv_t* val, const ts_t* spec) {
    val->tv_sec = spec->tv_sec;
    val->tv_usec = spec->tv_nsec / 1000;
}

void tv_to_ts(ts_t* spec, const tv_t* val) {
    spec->tv_sec = val->tv_sec;
    spec->tv_nsec = val->tv_usec * 1000;
}

void us_to_tv(tv_t* val, int64_t us) {
    lldiv_t tvdiv = lldiv(us, 1000000);

    val->tv_sec = tvdiv.quot;
    val->tv_usec = tvdiv.rem;
}

void us_to_ts(ts_t* spec, int64_t us) {
    lldiv_t tvdiv = lldiv(us, 1000000);

    spec->tv_sec = tvdiv.quot;
    spec->tv_nsec = tvdiv.rem * 1000;
}

void ms_to_ts(ts_t* spec, int64_t ms) {
    lldiv_t tvdiv = lldiv(ms, 1000);

    spec->tv_sec = tvdiv.quot;
    spec->tv_nsec = tvdiv.rem * 1000000;
}

void ms_to_tv(tv_t* val, int64_t ms) {
    lldiv_t tvdiv = lldiv(ms, 1000);

    val->tv_sec = tvdiv.quot;
    val->tv_usec = tvdiv.rem * 1000;
}

void tv_time(tv_t* tv) {
    gettimeofday(tv, NULL);
}

void ts_realtime(ts_t* ts) {
    clock_gettime(CLOCK_REALTIME, ts);
}

void cksleep_prepare_r(ts_t* ts) {
    clock_gettime(CLOCK_MONOTONIC, ts);
}

void nanosleep_abstime(ts_t* ts_end) {
    clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, ts_end, NULL);
}

void timeraddspec(ts_t* a, const ts_t* b) {
    a->tv_sec += b->tv_sec;
    a->tv_nsec += b->tv_nsec;
    if (a->tv_nsec >= 1000000000) {
        a->tv_nsec -= 1000000000;
        a->tv_sec++;
    }
}

/* Reentrant version of cksleep functions allow start time to be set separately
 * from the beginning of the actual sleep, allowing scheduling delays to be
 * counted in the sleep. */
void cksleep_ms_r(ts_t* ts_start, int ms) {
    ts_t ts_end;

    ms_to_ts(&ts_end, ms);
    timeraddspec(&ts_end, ts_start);
    nanosleep_abstime(&ts_end);
}

void cksleep_us_r(ts_t* ts_start, int64_t us) {
    ts_t ts_end;

    us_to_ts(&ts_end, us);
    timeraddspec(&ts_end, ts_start);
    nanosleep_abstime(&ts_end);
}

void cksleep_ms(int ms) {
    ts_t ts_start;

    cksleep_prepare_r(&ts_start);
    cksleep_ms_r(&ts_start, ms);
}

void cksleep_us(int64_t us) {
    ts_t ts_start;

    cksleep_prepare_r(&ts_start);
    cksleep_us_r(&ts_start, us);
}

/* Returns the microseconds difference between end and start times as a double */
double us_tvdiff(tv_t* end, tv_t* start) {
    /* Sanity check. We should only be using this for small differences so
     * limit the max to 60 seconds. */
    if (unlikely(end->tv_sec - start->tv_sec > 60))
        return 60000000;
    return (end->tv_sec - start->tv_sec) * 1000000 + (end->tv_usec - start->tv_usec);
}

/* Returns the milliseconds difference between end and start times */
int ms_tvdiff(tv_t* end, tv_t* start) {
    /* Like us_tdiff, limit to 1 hour. */
    if (unlikely(end->tv_sec - start->tv_sec > 3600))
        return 3600000;
    return (end->tv_sec - start->tv_sec) * 1000 + (end->tv_usec - start->tv_usec) / 1000;
}

/* Returns the seconds difference between end and start times as a double */
double tvdiff(tv_t* end, tv_t* start) {
    return end->tv_sec - start->tv_sec + (end->tv_usec - start->tv_usec) / 1000000.0;
}

/* Create an exponentially decaying average over interval */
void decay_time(double* f, double fadd, double fsecs, double interval) {
    double ftotal, fprop, dexp;

    if (fsecs <= 0)
        return;
    dexp = fsecs / interval;
    /* Put Sanity bound on how large the denominator can get */
    if (unlikely(dexp > 36))
        dexp = 36;
    fprop = 1.0 - 1 / exp(dexp);
    ftotal = 1.0 + fprop;
    *f += (fadd / fsecs * fprop);
    *f /= ftotal;
    /* Sanity check to prevent meaningless super small numbers that
     * eventually underflow the json real number interpretation. */
    if (unlikely(*f < 2E-16))
        *f = 0;
}

/* Sanity check to prevent clock adjustments backwards from screwing up stats */
double sane_tdiff(tv_t* end, tv_t* start) {
    double tdiff = tvdiff(end, start);

    if (unlikely(tdiff < 0.001))
        tdiff = 0.001;
    return tdiff;
}

/* Convert a double value into a truncated string for displaying with its
 * associated suitable for Mega, Giga etc. Buf array needs to be long enough */
void suffix_string(double val, char* buf, size_t bufsiz, int sigdigits) {
    const double kilo = 1000;
    const double mega = 1000000;
    const double giga = 1000000000;
    const double tera = 1000000000000;
    const double peta = 1000000000000000;
    const double exa = 1000000000000000000;
    char         suffix[2] = "";
    bool         decimal = true;
    double       dval;

    if (val >= exa) {
        val /= peta;
        dval = val / kilo;
        strcpy(suffix, "E");
    } else if (val >= peta) {
        val /= tera;
        dval = val / kilo;
        strcpy(suffix, "P");
    } else if (val >= tera) {
        val /= giga;
        dval = val / kilo;
        strcpy(suffix, "T");
    } else if (val >= giga) {
        val /= mega;
        dval = val / kilo;
        strcpy(suffix, "G");
    } else if (val >= mega) {
        val /= kilo;
        dval = val / kilo;
        strcpy(suffix, "M");
    } else if (val >= kilo) {
        dval = val / kilo;
        strcpy(suffix, "K");
    } else {
        dval = val;
        decimal = false;
    }

    if (!sigdigits) {
        if (decimal)
            snprintf(buf, bufsiz, "%.3g%s", dval, suffix);
        else
            snprintf(buf, bufsiz, "%d%s", (unsigned int)dval, suffix);
    } else {
        /* Always show sigdigits + 1, padded on right with zeroes
         * followed by suffix */
        int ndigits = sigdigits - 1 - (dval > 0.0 ? floor(log10(dval)) : 0);

        snprintf(buf, bufsiz, "%*.*f%s", sigdigits + 1, ndigits, dval, suffix);
    }
}

/* truediffone == 0x00000000FFFF0000000000000000000000000000000000000000000000000000
 * Generate a 256 bit binary LE target by cutting up diff into 64 bit sized
 * portions or vice versa. */
static const double truediffone = 26959535291011309493156476344723991336010898738574164086137773096960.0;
static const double bits192 = 6277101735386680763835789423207666416102355444464034512896.0;
static const double bits128 = 340282366920938463463374607431768211456.0;
static const double bits64 = 18446744073709551616.0;

/* Converts a little endian 256 bit value to a double */
double le256todouble(const uchar* target) {
    double dcut64;

    dcut64 = le64toh(read_u64(target + 24)) * bits192;
    dcut64 += le64toh(read_u64(target + 16)) * bits128;
    dcut64 += le64toh(read_u64(target + 8)) * bits64;
    dcut64 += le64toh(read_u64(target));

    return dcut64;
}

/* Converts a big endian 256 bit value to a double */
double be256todouble(const uchar* target) {
    double dcut64;

    dcut64 = be64toh(read_u64(target)) * bits192;
    dcut64 += be64toh(read_u64(target + 8)) * bits128;
    dcut64 += be64toh(read_u64(target + 16)) * bits64;
    dcut64 += be64toh(read_u64(target + 24));

    return dcut64;
}

/* Return a difficulty from a binary target */
double diff_from_target(uchar* target) {
    double dcut64;

    dcut64 = le256todouble(target);
    if (unlikely(dcut64 <= 0))
        dcut64 = 1;
    return truediffone / dcut64;
}

/* Return a difficulty from a binary big endian target */
double diff_from_betarget(uchar* target) {
    double dcut64;

    dcut64 = be256todouble(target);
    if (unlikely(dcut64 <= 0))
        dcut64 = 1;
    return truediffone / dcut64;
}

/* Return the network difficulty from the block header which is in packed form,
 * as a double. */
double diff_from_nbits(char* nbits) {
    uint8_t shift = nbits[0];
    uchar   target[32] = {};
    char*   nb;

    nb = bin2hex(nbits, 4);
    LOGDEBUG("Nbits is %s", nb);
    free(nb);
    if (unlikely(shift < 3)) {
        LOGWARNING("Corrupt shift of %d in nbits", shift);
        shift = 3;
    } else if (unlikely(shift > 32)) {
        LOGWARNING("Corrupt shift of %d in nbits", shift);
        shift = 32;
    }
    memcpy(target + (32 - shift), nbits + 1, 3);
    return diff_from_betarget(target);
}

void target_from_diff(uchar* target, double diff) {
    uint64_t h64;
    double   d64, dcut64;

    if (unlikely(diff == 0.0)) {
        /* This shouldn't happen but best we check to prevent a crash */
        memset(target, 0xff, 32);
        return;
    }

    d64 = truediffone;
    d64 /= diff;

    dcut64 = d64 / bits192;
    h64 = dcut64;
    write_u64(target + 24, htole64(h64));
    dcut64 = h64;
    dcut64 *= bits192;
    d64 -= dcut64;

    dcut64 = d64 / bits128;
    h64 = dcut64;
    write_u64(target + 16, htole64(h64));
    dcut64 = h64;
    dcut64 *= bits128;
    d64 -= dcut64;

    dcut64 = d64 / bits64;
    h64 = dcut64;
    write_u64(target + 8, htole64(h64));
    dcut64 = h64;
    dcut64 *= bits64;
    d64 -= dcut64;

    h64 = d64;
    write_u64(target, htole64(h64));
}

void gen_hash(uchar* data, uchar* hash, int len) {
    uchar hash1[32];

    sha256(data, len, hash1);
    sha256(hash1, 32, hash);
}
