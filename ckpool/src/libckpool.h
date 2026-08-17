/*
 * Copyright 2014-2018,2023,2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

/* This file should contain all exported functions of libckpool */

#ifndef LIBCKPOOL_H
#define LIBCKPOOL_H

#include <errno.h>
#include <inttypes.h>
#include <netdb.h>
#include <stdarg.h>
#include "yyjson.h"
#include <pthread.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <syslog.h>
#include <semaphore.h>

#if HAVE_BYTESWAP_H
#include <byteswap.h>
#endif

#if HAVE_ENDIAN_H
#include <endian.h>
#elif HAVE_SYS_ENDIAN_H
#include <sys/endian.h>
#endif

#include <sys/socket.h>
#include <sys/types.h>

#include "utlist.h"

#ifndef bswap_16
#define bswap_16 __builtin_bswap16
#define bswap_32 __builtin_bswap32
#define bswap_64 __builtin_bswap64
#endif

/* This assumes htobe32 is a macro in endian.h, and if it doesn't exist, then
 * htobe64 also won't exist */
#ifndef htobe32
#if __BYTE_ORDER == __LITTLE_ENDIAN
#define htole16(x) (x)
#define le16toh(x) (x)
#define htole32(x) (x)
#define htole64(x) (x)
#define le32toh(x) (x)
#define le64toh(x) (x)
#define be32toh(x) bswap_32(x)
#define be64toh(x) bswap_64(x)
#define htobe16(x) bswap_16(x)
#define htobe32(x) bswap_32(x)
#define htobe64(x) bswap_64(x)
#elif __BYTE_ORDER == __BIG_ENDIAN
#define htole16(x) bswap_16(x)
#define le16toh(x) bswap_16(x)
#define htole32(x) bswap_32(x)
#define le32toh(x) bswap_32(x)
#define le64toh(x) bswap_64(x)
#define htole64(x) bswap_64(x)
#define be32toh(x) (x)
#define be64toh(x) (x)
#define htobe16(x) (x)
#define htobe32(x) (x)
#define htobe64(x) (x)
#endif
#endif

#define unlikely(expr) (__builtin_expect(!!(expr), 0))
#define likely(expr) (__builtin_expect(!!(expr), 1))
#define __maybe_unused __attribute__((unused))
#define uninitialised_var(x) x = x

/* Portable case fallthrough marker: C23 [[fallthrough]] where available,
 * GCC/Clang attribute on older compilers, and a no-op on anything else. */
#if defined(__has_c_attribute)
#if __has_c_attribute(fallthrough)
#define fallthrough [[fallthrough]]
#endif
#endif
#ifndef fallthrough
#if defined(__GNUC__) && __GNUC__ >= 7
#define fallthrough __attribute__((fallthrough))
#else
#define fallthrough ((void)0)
#endif
#endif

#ifndef MAX
#define MAX(a, b)               \
    ({                          \
        __typeof__(a) _a = (a); \
        __typeof__(b) _b = (b); \
        _a > _b ? _a : _b;      \
    })
#endif
#ifndef MIN
#define MIN(a, b)               \
    ({                          \
        __typeof__(a) _a = (a); \
        __typeof__(b) _b = (b); \
        _a < _b ? _a : _b;      \
    })
#endif

typedef unsigned char uchar;

typedef struct timeval  tv_t;
typedef struct timespec ts_t;

/* memcpy-based so buffers need not be naturally aligned (SV2 frames, etc.) */
static inline uint32_t read_u32(const void* p) {
    uint32_t v;

    memcpy(&v, p, sizeof(v));
    return v;
}

static inline void write_u32(void* p, uint32_t v) {
    memcpy(p, &v, sizeof(v));
}

static inline uint64_t read_u64(const void* p) {
    uint64_t v;

    memcpy(&v, p, sizeof(v));
    return v;
}

static inline void write_u64(void* p, uint64_t v) {
    memcpy(p, &v, sizeof(v));
}

static inline void swap_256(void* dest_p, const void* src_p) {
    const uchar* src = src_p;
    uchar*       dest = dest_p;
    int          i;

    for (i = 0; i < 8; i++)
        write_u32(dest + i * 4, read_u32(src + (7 - i) * 4));
}

static inline void bswap_256(void* dest_p, const void* src_p) {
    const uchar* src = src_p;
    uchar*       dest = dest_p;
    int          i;

    for (i = 0; i < 8; i++)
        write_u32(dest + i * 4, bswap_32(read_u32(src + (7 - i) * 4)));
}

static inline void flip_32(void* dest_p, const void* src_p) {
    const uchar* src = src_p;
    uchar*       dest = dest_p;
    int          i;

    for (i = 0; i < 8; i++)
        write_u32(dest + i * 4, bswap_32(read_u32(src + i * 4)));
}

static inline void flip_80(void* dest_p, const void* src_p) {
    const uchar* src = src_p;
    uchar*       dest = dest_p;
    int          i;

    for (i = 0; i < 20; i++)
        write_u32(dest + i * 4, bswap_32(read_u32(src + i * 4)));
}

#define cond_wait(_cond, _lock) _cond_wait(_cond, _lock, __FILE__, __func__, __LINE__)
#define cond_timedwait(_cond, _lock, _abstime) _cond_timedwait(_cond, _lock, _abstime, __FILE__, __func__, __LINE__)
#define mutex_timedlock(_lock, _timeout) _mutex_timedlock(_lock, _timeout, __FILE__, __func__, __LINE__)
#define mutex_lock(_lock) _mutex_lock(_lock, __FILE__, __func__, __LINE__)
#define mutex_unlock_noyield(_lock) _mutex_unlock_noyield(_lock, __FILE__, __func__, __LINE__)
#define mutex_unlock(_lock) _mutex_unlock(_lock, __FILE__, __func__, __LINE__)
#define mutex_trylock(_lock) _mutex_trylock(_lock, __FILE__, __func__, __LINE__)
#define wr_lock(_lock) _wr_lock(_lock, __FILE__, __func__, __LINE__)
#define wr_trylock(_lock) _wr_trylock(_lock, __FILE__, __func__, __LINE__)
#define rd_lock(_lock) _rd_lock(_lock, __FILE__, __func__, __LINE__)
#define rw_unlock(_lock) _rw_unlock(_lock, __FILE__, __func__, __LINE__)
#define rd_unlock_noyield(_lock) _rd_unlock_noyield(_lock, __FILE__, __func__, __LINE__)
#define wr_unlock_noyield(_lock) _wr_unlock_noyield(_lock, __FILE__, __func__, __LINE__)
#define rd_unlock(_lock) _rd_unlock(_lock, __FILE__, __func__, __LINE__)
#define wr_unlock(_lock) _wr_unlock(_lock, __FILE__, __func__, __LINE__)
#define mutex_init(_lock) _mutex_init(_lock, __FILE__, __func__, __LINE__)
#define rwlock_init(_lock) _rwlock_init(_lock, __FILE__, __func__, __LINE__)
#define cond_init(_cond) _cond_init(_cond, __FILE__, __func__, __LINE__)

#define cklock_init(_lock) _cklock_init(_lock, __FILE__, __func__, __LINE__)
#define ck_rlock(_lock) _ck_rlock(_lock, __FILE__, __func__, __LINE__)
#define ck_wlock(_lock) _ck_wlock(_lock, __FILE__, __func__, __LINE__)
#define ck_dwlock(_lock) _ck_dwlock(_lock, __FILE__, __func__, __LINE__)
#define ck_dlock(_lock) _ck_dlock(_lock, __FILE__, __func__, __LINE__)
#define ck_runlock(_lock) _ck_runlock(_lock, __FILE__, __func__, __LINE__)
#define ck_wunlock(_lock) _ck_wunlock(_lock, __FILE__, __func__, __LINE__)

#define ckalloc(len) _ckalloc(len, __FILE__, __func__, __LINE__)
#define ckzalloc(len) _ckzalloc(len, __FILE__, __func__, __LINE__)
#define ckrealloc(ptr, size) _ckrealloc(ptr, size, __FILE__, __func__, __LINE__);

#define dealloc(ptr) \
    do {             \
        free(ptr);   \
        ptr = NULL;  \
    } while (0)

#define VASPRINTF(strp, fmt, ...)                                             \
    do {                                                                      \
        if (unlikely(vasprintf(strp, fmt, ##__VA_ARGS__) < 0))                \
            quitfrom(1, __FILE__, __func__, __LINE__, "Failed to vasprintf"); \
    } while (0)

#define ASPRINTF(strp, fmt, ...)                                             \
    do {                                                                     \
        if (unlikely(asprintf(strp, fmt, ##__VA_ARGS__) < 0))                \
            quitfrom(1, __FILE__, __func__, __LINE__, "Failed to asprintf"); \
    } while (0)

void logmsg(int loglevel, const char* fmt, ...);

/* Truncates messages greater than DEFLOGBUFSIZ */
#define DEFLOGBUFSIZ 512

#define LOGMSGSIZ(__siz, __lvl, __fmt, ...)        \
    do {                                           \
        char* BUF;                                 \
        int   LEN, OFFSET = 0;                     \
        ASPRINTF(&BUF, __fmt, ##__VA_ARGS__);      \
        LEN = strlen(BUF);                         \
        while (LEN > 0) {                          \
            char tmp42[__siz] = {};                \
            int  CPY = MIN(LEN, DEFLOGBUFSIZ - 2); \
            memcpy(tmp42, BUF + OFFSET, CPY);      \
            logmsg(__lvl, "%s", tmp42);            \
            OFFSET += CPY;                         \
            LEN -= CPY;                            \
        }                                          \
        free(BUF);                                 \
    } while (0)

#define LOGMSG(_lvl, _fmt, ...) LOGMSGSIZ(DEFLOGBUFSIZ, _lvl, _fmt, ##__VA_ARGS__)

#define LOGEMERG(fmt, ...) LOGMSG(LOG_EMERG, fmt, ##__VA_ARGS__)
#define LOGALERT(fmt, ...) LOGMSG(LOG_ALERT, fmt, ##__VA_ARGS__)
#define LOGCRIT(fmt, ...) LOGMSG(LOG_CRIT, fmt, ##__VA_ARGS__)
#define LOGERR(fmt, ...) LOGMSG(LOG_ERR, fmt, ##__VA_ARGS__)
#define LOGWARNING(fmt, ...) LOGMSG(LOG_WARNING, fmt, ##__VA_ARGS__)
#define LOGNOTICE(fmt, ...) LOGMSG(LOG_NOTICE, fmt, ##__VA_ARGS__)
#define LOGINFO(fmt, ...) LOGMSG(LOG_INFO, fmt, ##__VA_ARGS__)
#define LOGDEBUG(fmt, ...) LOGMSG(LOG_DEBUG, fmt, ##__VA_ARGS__)

#define IN_FMT_FFL " in %s %s():%d"
#define quitfrom(status, _file, _func, _line, fmt, ...)                          \
    do {                                                                         \
        if (fmt) {                                                               \
            fprintf(stderr, fmt IN_FMT_FFL, ##__VA_ARGS__, _file, _func, _line); \
            fprintf(stderr, "\n");                                               \
            fflush(stderr);                                                      \
        }                                                                        \
        exit(status);                                                            \
    } while (0)

/* Many configuration aborts pass status 0. Set this for a config test run so
 * they exit non zero instead and the check can be used from a script. It is
 * false everywhere else, leaving every existing exit status untouched. */
extern bool quit_zero_is_failure;

#define quit(status, fmt, ...)                                      \
    do {                                                            \
        if (fmt) {                                                  \
            fprintf(stderr, fmt, ##__VA_ARGS__);                    \
            fprintf(stderr, "\n");                                  \
            fflush(stderr);                                         \
        }                                                           \
        exit((status) ? (status) : (quit_zero_is_failure ? 1 : 0)); \
    } while (0)

#define PAGESIZE (4096)

/* Default timeouts for unix socket reads and writes in seconds. Set write
 * timeout to double the read timeout in case of one read blocking the next
 * writer. */
#define UNIX_READ_TIMEOUT 5
#define UNIX_WRITE_TIMEOUT 10

#define MIN1 60
#define MIN5 300
#define MIN15 900
#define HOUR 3600
#define HOUR6 21600
#define DAY 86400
#define WEEK 604800

/* Share error values */

enum share_err {
    SE_INVALID_NONCE2 = -9,
    SE_WORKER_MISMATCH,
    SE_NO_NONCE,
    SE_NO_NTIME,
    SE_NO_NONCE2,
    SE_NO_JOBID,
    SE_NO_USERNAME,
    SE_INVALID_SIZE,
    SE_NOT_ARRAY,
    SE_NONE,  // 0
    SE_INVALID_JOBID,
    SE_STALE,
    SE_NTIME_INVALID,
    SE_DUPE,
    SE_HIGH_DIFF,
    SE_INVALID_VERSION_MASK,
    SE_NO_WORKBASE
};

static const char __maybe_unused* share_errs[] = {
    "Invalid nonce2 length",
    "Worker mismatch",
    "No nonce",
    "No ntime",
    "No nonce2",
    "No job_id",
    "No username",
    "Invalid array size",
    "Params not array",
    "Valid",
    "Invalid JobID",
    "Stale",
    "Ntime out of range",
    "Duplicate",
    "Above target",
    "Invalid version mask",
    "No workbase"};

#define SHARE_ERR(x) share_errs[((x) + 9)]

/* Stratum V1 error codes for mining.submit responses.
 * Origin: slush pool Stratum mining protocol specification
 * (https://web.archive.org/web/20150307191254/http://mining.bitcoin.cz/stratum-mining)
 *
 *   20 - Other/Unknown
 *   21 - Job not found (=stale)
 *   22 - Duplicate share
 *   23 - Low difficulty share
 *   24 - Unauthorized worker
 *   25 - Not subscribed
 */
static const int __maybe_unused share_err_codes[] = {
    20,  // SE_INVALID_NONCE2       - Other/Unknown
    24,  // SE_WORKER_MISMATCH      - Unauthorized worker
    20,  // SE_NO_NONCE             - Other/Unknown
    20,  // SE_NO_NTIME             - Other/Unknown
    20,  // SE_NO_NONCE2            - Other/Unknown
    20,  // SE_NO_JOBID             - Other/Unknown
    20,  // SE_NO_USERNAME          - Other/Unknown
    20,  // SE_INVALID_SIZE         - Other/Unknown
    20,  // SE_NOT_ARRAY            - Other/Unknown
    0,   // SE_NONE
    21,  // SE_INVALID_JOBID        - Job not found
    21,  // SE_STALE                - Job not found (stale)
    20,  // SE_NTIME_INVALID        - Other/Unknown
    22,  // SE_DUPE                 - Duplicate share
    23,  // SE_HIGH_DIFF            - Low difficulty share
    20,  // SE_INVALID_VERSION_MASK - Other/Unknown
    20,  // SE_NO_WORKBASE          - Other/Unknown
};

#define SHARE_ERR_CODE(x) share_err_codes[((x) + 9)]

typedef struct ckmutex mutex_t;

struct ckmutex {
    pthread_mutex_t mutex;
    const char*     file;
    const char*     func;
    int             line;
};

typedef struct ckrwlock rwlock_t;

struct ckrwlock {
    pthread_rwlock_t rwlock;
    const char*      file;
    const char*      func;
    int              line;
};

/* ck locks, a write biased variant of rwlocks */
struct cklock {
    mutex_t     mutex;
    rwlock_t    rwlock;
    const char* file;
    const char* func;
    int         line;
};

typedef struct cklock cklock_t;

struct unixsock {
    int   sockd;
    char* path;
};

typedef struct unixsock unixsock_t;

/* As the json_*cpy helpers above but for mutable yyjson objects, bounded to
 * size bytes including the null terminator for copying into fixed sized
 * buffers. */
static inline void yyjson_mut_obj_strncpy(char* buf, yyjson_mut_val* val, const char* key, const size_t size) {
    const char* str = yyjson_mut_get_str(yyjson_mut_obj_get(val, key)) ?: "";

    if (likely(size)) {
        strncpy(buf, str, size - 1);
        buf[size - 1] = '\0';
    }
}

static inline void yyjson_mut_obj_dblcpy(double* dbl, yyjson_mut_val* val, const char* key) {
    *dbl = yyjson_mut_get_num(yyjson_mut_obj_get(val, key));
}

static inline void yyjson_mut_obj_uintcpy(uint32_t* u32, yyjson_mut_val* val, const char* key) {
    *u32 = (uint32_t)yyjson_mut_get_uint(yyjson_mut_obj_get(val, key));
}

static inline void yyjson_mut_obj_uint64cpy(uint64_t* u64, yyjson_mut_val* val, const char* key) {
    *u64 = yyjson_mut_get_uint(yyjson_mut_obj_get(val, key));
}

static inline void yyjson_mut_obj_int64cpy(int64_t* i64, yyjson_mut_val* val, const char* key) {
    *i64 = yyjson_mut_get_sint(yyjson_mut_obj_get(val, key));
}

static inline void yyjson_mut_obj_intcpy(int* i, yyjson_mut_val* val, const char* key) {
    *i = yyjson_mut_get_sint(yyjson_mut_obj_get(val, key));
}

static inline void yyjson_mut_obj_strdup(char** buf, yyjson_mut_val* val, const char* key) {
    *buf = strdup(yyjson_mut_get_str(yyjson_mut_obj_get(val, key)) ?: "");
}

/* As above but for immutable yyjson objects */
static inline void yyjson_obj_strncpy(char* buf, yyjson_val* val, const char* key, const size_t size) {
    const char* str = yyjson_get_str(yyjson_obj_get(val, key)) ?: "";

    if (likely(size)) {
        strncpy(buf, str, size - 1);
        buf[size - 1] = '\0';
    }
}

static inline void yyjson_obj_dblcpy(double* dbl, yyjson_val* val, const char* key) {
    *dbl = yyjson_get_num(yyjson_obj_get(val, key));
}

static inline void yyjson_obj_int64cpy(int64_t* i64, yyjson_val* val, const char* key) {
    *i64 = yyjson_get_sint(yyjson_obj_get(val, key));
}

static inline void yyjson_obj_intcpy(int* i, yyjson_val* val, const char* key) {
    *i = yyjson_get_sint(yyjson_obj_get(val, key));
}

static inline void yyjson_obj_strdup(char** buf, yyjson_val* val, const char* key) {
    *buf = strdup(yyjson_get_str(yyjson_obj_get(val, key)) ?: "");
}

void rename_proc(const char* name);
void create_pthread(pthread_t* thread, void* (*start_routine)(void*), void* arg);
void join_pthread(pthread_t thread);
bool ck_completion_timeout(void* fn, void* fnarg, int timeout);

int _cond_wait(pthread_cond_t* cond, mutex_t* lock, const char* file, const char* func, const int line);
int _cond_timedwait(
    pthread_cond_t*        cond,
    mutex_t*               lock,
    const struct timespec* abstime,
    const char*            file,
    const char*            func,
    const int              line);
int  _mutex_timedlock(mutex_t* lock, int timeout, const char* file, const char* func, const int line);
void _mutex_lock(mutex_t* lock, const char* file, const char* func, const int line);
void _mutex_unlock_noyield(mutex_t* lock, const char* file, const char* func, const int line);
void _mutex_unlock(mutex_t* lock, const char* file, const char* func, const int line);
int  _mutex_trylock(
     mutex_t*                   lock,
     __maybe_unused const char* file,
     __maybe_unused const char* func,
     __maybe_unused const int   line);
void mutex_destroy(mutex_t* lock);

void _wr_lock(rwlock_t* lock, const char* file, const char* func, const int line);
int  _wr_trylock(
     rwlock_t*                  lock,
     __maybe_unused const char* file,
     __maybe_unused const char* func,
     __maybe_unused const int   line);
void _rd_lock(rwlock_t* lock, const char* file, const char* func, const int line);
void _rw_unlock(rwlock_t* lock, const char* file, const char* func, const int line);
void _rd_unlock_noyield(rwlock_t* lock, const char* file, const char* func, const int line);
void _wr_unlock_noyield(rwlock_t* lock, const char* file, const char* func, const int line);
void _rd_unlock(rwlock_t* lock, const char* file, const char* func, const int line);
void _wr_unlock(rwlock_t* lock, const char* file, const char* func, const int line);
void _mutex_init(mutex_t* lock, const char* file, const char* func, const int line);
void _rwlock_init(rwlock_t* lock, const char* file, const char* func, const int line);
void _cond_init(pthread_cond_t* cond, const char* file, const char* func, const int line);

void _cklock_init(cklock_t* lock, const char* file, const char* func, const int line);
void _ck_rlock(cklock_t* lock, const char* file, const char* func, const int line);
void _ck_ilock(cklock_t* lock, const char* file, const char* func, const int line);
void _ck_uilock(cklock_t* lock, const char* file, const char* func, const int line);
void _ck_ulock(cklock_t* lock, const char* file, const char* func, const int line);
void _ck_wlock(cklock_t* lock, const char* file, const char* func, const int line);
void _ck_dwlock(cklock_t* lock, const char* file, const char* func, const int line);
void _ck_dwilock(cklock_t* lock, const char* file, const char* func, const int line);
void _ck_dlock(cklock_t* lock, const char* file, const char* func, const int line);
void _ck_runlock(cklock_t* lock, const char* file, const char* func, const int line);
void _ck_wunlock(cklock_t* lock, const char* file, const char* func, const int line);
void cklock_destroy(cklock_t* lock);

void _cksem_init(sem_t* sem, const char* file, const char* func, const int line);
void _cksem_post(sem_t* sem, const char* file, const char* func, const int line);
void _cksem_wait(sem_t* sem, const char* file, const char* func, const int line);
int  _cksem_trywait(sem_t* sem, const char* file, const char* func, const int line);
int  _cksem_mswait(sem_t* sem, int ms, const char* file, const char* func, const int line);
void _cksem_destroy(sem_t* sem, const char* file, const char* func, const int line);

#define cksem_init(SEM) _cksem_init(SEM, __FILE__, __func__, __LINE__)
#define cksem_post(SEM) _cksem_post(SEM, __FILE__, __func__, __LINE__)
#define cksem_wait(SEM) _cksem_wait(SEM, __FILE__, __func__, __LINE__)
#define cksem_trywait(SEM) _cksem_trywait(SEM, __FILE__, __func__, __LINE__)
#define cksem_mswait(SEM, _timeout) _cksem_mswait(SEM, _timeout, __FILE__, __func__, __LINE__)
#define cksem_destroy(SEM) _cksem_destroy(SEM, __FILE__, __func__, __LINE__)

static inline bool sock_connecting(void) {
    return errno == EINPROGRESS;
}

static inline bool sock_blocks(void) {
    return (errno == EAGAIN || errno == EWOULDBLOCK);
}

static inline bool sock_timeout(void) {
    return (errno == ETIMEDOUT);
}

bool extract_sockaddr(char* url, char** sockaddr_url, char** sockaddr_port);
bool url_from_sockaddr(const struct sockaddr* addr, char* url, char* port);
bool addrinfo_from_url(const char* url, const char* port, struct addrinfo* addrinfo);
bool url_from_serverurl(char* serverurl, char* newurl, char* newport);
bool url_from_socket(const int sockd, char* url, char* port);

void keep_sockalive(int fd);
void nolinger_socket(int fd);
void noblock_socket(int fd);
void block_socket(int fd);
void _close(int* fd, const char* file, const char* func, const int line);
#define _Close(FD) _close(FD, __FILE__, __func__, __LINE__)
#define Close(FD) _close(&FD, __FILE__, __func__, __LINE__)
int  bind_socket(char* url, char* port);
int  connect_socket(char* url, char* port);
int  round_trip(char* url);
int  write_socket(int fd, const void* buf, size_t nbyte);
void empty_socket(int fd);
void _close_unix_socket(int* sockd, const char* server_path);
#define close_unix_socket(sockd, server_path) _close_unix_socket(&sockd, server_path)
int _open_unix_server(const char* server_path, const char* file, const char* func, const int line);
#define open_unix_server(server_path) _open_unix_server(server_path, __FILE__, __func__, __LINE__)
int _open_unix_client(const char* server_path, const char* file, const char* func, const int line);
#define open_unix_client(server_path) _open_unix_client(server_path, __FILE__, __func__, __LINE__)
int wait_close(int sockd, int timeout);
int wait_read_select(int sockd, float timeout);
int read_length(int sockd, void* buf, int len);
/* Largest message accepted over the unix socket IPC. Control plane messages
 * are tiny; a block submission carries the full block as hex and is the
 * largest, at roughly 8MB for a full mainnet block, so leave ample headroom. */
#define MAX_UNIX_MSGSIZE (64 * 1024 * 1024)
char* _recv_unix_msg(int sockd, int timeout1, int timeout2, const char* file, const char* func, const int line);
#define RECV_UNIX_TIMEOUT1 30
#define RECV_UNIX_TIMEOUT2 5
#define recv_unix_msg(sockd) _recv_unix_msg(sockd, UNIX_READ_TIMEOUT, UNIX_READ_TIMEOUT, __FILE__, __func__, __LINE__)
#define recv_unix_msg_tmo(sockd, tmo) _recv_unix_msg(sockd, tmo, UNIX_READ_TIMEOUT, __FILE__, __func__, __LINE__)
#define recv_unix_msg_tmo2(sockd, tmo1, tmo2) _recv_unix_msg(sockd, tmo1, tmo2, __FILE__, __func__, __LINE__)
int wait_write_select(int sockd, float timeout);
#define write_length(sockd, buf, len) _write_length(sockd, buf, len, __FILE__, __func__, __LINE__)
int  _write_length(int sockd, const void* buf, int len, const char* file, const char* func, const int line);
bool _send_unix_msg(int sockd, const char* buf, int timeout, const char* file, const char* func, const int line);
#define send_unix_msg(sockd, buf) _send_unix_msg(sockd, buf, UNIX_WRITE_TIMEOUT, __FILE__, __func__, __LINE__)
bool _send_unix_data(int sockd, const struct msghdr* msg, const char* file, const char* func, const int line);
#define send_unix_data(sockd, msg) _send_unix_data(sockd, msg, __FILE__, __func__, __LINE__)
bool _recv_unix_data(int sockd, struct msghdr* msg, const char* file, const char* func, const int line);
#define recv_unix_data(sockd, msg) _recv_unix_data(sockd, msg, __FILE__, __func__, __LINE__)
bool _send_fd(int fd, int sockd, const char* file, const char* func, const int line);
#define send_fd(fd, sockd) _send_fd(fd, sockd, __FILE__, __func__, __LINE__)
int _get_fd(int sockd, const char* file, const char* func, const int line);
#define get_fd(sockd) _get_fd(sockd, __FILE__, __func__, __LINE__)

char* rotating_filename(const char* path, time_t when);
bool  rotating_log(const char* path, const char* msg);

void   align_len(size_t* len);
void   realloc_strcat(char** ptr, const char* s);
void   trail_slash(char** buf);
void*  _ckrealloc(void* ptr, size_t size, const char* file, const char* func, const int line);
void*  _ckalloc(size_t len, const char* file, const char* func, const int line);
void*  _ckzalloc(size_t len, const char* file, const char* func, const int line);
size_t round_up_page(size_t len);

extern const int hex2bin_tbl[];
void             __bin2hex(void* vs, const void* vp, size_t len);
void*            bin2hex(const void* vp, size_t len);
bool             _validhex(const char* buf, const char* file, const char* func, const int line);
#define validhex(buf) _validhex(buf, __FILE__, __func__, __LINE__)
bool _hex2bin(void* p, const void* vhexstr, size_t len, const char* file, const char* func, const int line);
#define hex2bin(p, vhexstr, len) _hex2bin(p, vhexstr, len, __FILE__, __func__, __LINE__)
char* http_base64(const char* src);
void  b58tobin(char* b58bin, const char* b58);
int   safecmp(const char* a, const char* b);
bool  cmdmatch(const char* buf, const char* cmd);

int  address_to_txn(char* p2h, const char* addr, const bool script, const bool segwit);
bool txn_to_address(char* addr, const size_t alen, const uchar* script, const int slen, const char* ref);
int  coinbase_payout_script(
     uchar*       script,
     int*         slen,
     int64_t*     value,
     const uchar* cb1,
     const int    cb1len,
     const int    holelen,
     const uchar* cb2,
     const int    cb2len);
int  ser_number(uchar* s, int32_t val);
int  get_sernumber(uchar* s);
bool fulltest(const uchar* hash, const uchar* target);

void copy_tv(tv_t* dest, const tv_t* src);
void ts_to_tv(tv_t* val, const ts_t* spec);
void tv_to_ts(ts_t* spec, const tv_t* val);
void us_to_tv(tv_t* val, int64_t us);
void us_to_ts(ts_t* spec, int64_t us);
void ms_to_ts(ts_t* spec, int64_t ms);
void ms_to_tv(tv_t* val, int64_t ms);
void tv_time(tv_t* tv);
void ts_realtime(ts_t* ts);

void cksleep_prepare_r(ts_t* ts);
void nanosleep_abstime(ts_t* ts_end);
void timeraddspec(ts_t* a, const ts_t* b);
void cksleep_ms_r(ts_t* ts_start, int ms);
void cksleep_us_r(ts_t* ts_start, int64_t us);
void cksleep_ms(int ms);
void cksleep_us(int64_t us);

double us_tvdiff(tv_t* end, tv_t* start);
int    ms_tvdiff(tv_t* end, tv_t* start);
double tvdiff(tv_t* end, tv_t* start);

void   decay_time(double* f, double fadd, double fsecs, double interval);
double sane_tdiff(tv_t* end, tv_t* start);
void   suffix_string(double val, char* buf, size_t bufsiz, int sigdigits);

double le256todouble(const uchar* target);
double be256todouble(const uchar* target);
double diff_from_target(uchar* target);
double diff_from_betarget(uchar* target);
double diff_from_nbits(char* nbits);
void   target_from_diff(uchar* target, double diff);

void gen_hash(uchar* data, uchar* hash, int len);

#endif /* LIBCKPOOL_H */
