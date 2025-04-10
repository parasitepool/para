/*
 * Copyright 2014-2018,2023 Con Kolivas
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

/* We use a weak function as a simple printf within the library that can be
 * overridden by however the outside executable wishes to do its logging. */
void __attribute__((weak)) logmsg(int __maybe_unused loglevel, const char *fmt, ...)
{
	va_list ap;
	char *buf;

	va_start(ap, fmt);
	VASPRINTF(&buf, fmt, ap);
	va_end(ap);

	printf("%s\n", buf);
	free(buf);
}

void rename_proc(const char *name)
{
	char buf[16];

	snprintf(buf, 15, "ckp@%s", name);
	buf[15] = '\0';
	prctl(PR_SET_NAME, buf, 0, 0, 0);
}

void create_pthread(pthread_t *thread, void *(*start_routine)(void *), void *arg)
{
	int ret = pthread_create(thread, NULL, start_routine,  arg);

	if (unlikely(ret))
		quit(1, "Failed to pthread_create");
}

void join_pthread(pthread_t thread)
{
	if (!pthread_kill(thread, 0))
		pthread_join(thread, NULL);
}

struct ck_completion {
	sem_t sem;
	void (*fn)(void *fnarg);
	void *fnarg;
};

static void *completion_thread(void *arg)
{
	struct ck_completion *ckc = (struct ck_completion *)arg;

	ckc->fn(ckc->fnarg);
	cksem_post(&ckc->sem);

	return NULL;
}

bool ck_completion_timeout(void *fn, void *fnarg, int timeout)
{
	struct ck_completion ckc;
	pthread_t pthread;
	bool ret = false;

	cksem_init(&ckc.sem);
	ckc.fn = fn;
	ckc.fnarg = fnarg;

	pthread_create(&pthread, NULL, completion_thread, (void *)&ckc);

	ret = cksem_mswait(&ckc.sem, timeout);
	if (!ret)
		pthread_join(pthread, NULL);
	else
		pthread_cancel(pthread);
	return !ret;
}

int _cond_wait(pthread_cond_t *cond, mutex_t *lock, const char *file, const char *func, const int line)
{
	int ret;

	ret = pthread_cond_wait(cond, &lock->mutex);
	lock->file = file;
	lock->func = func;
	lock->line = line;
	return ret;
}

int _cond_timedwait(pthread_cond_t *cond, mutex_t *lock, const struct timespec *abstime, const char *file, const char *func, const int line)
{
	int ret;

	ret = pthread_cond_timedwait(cond, &lock->mutex, abstime);
	lock->file = file;
	lock->func = func;
	lock->line = line;
	return ret;
}


int _mutex_timedlock(mutex_t *lock, int timeout, const char *file, const char *func, const int line)
{
	tv_t now;
	ts_t abs;
	int ret;

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
void _mutex_lock(mutex_t *lock, const char *file, const char *func, const int line)
{
	int ret, retries = 0;

retry:
	ret = _mutex_timedlock(lock, 10, file, func, line);
	if (unlikely(ret)) {
		if (likely(ret == ETIMEDOUT)) {
			LOGERR("WARNING: Prolonged mutex lock contention from %s %s:%d, held by %s %s:%d",
			       file, func, line, lock->file, lock->func, lock->line);
			if (++retries < 6)
				goto retry;
			quitfrom(1, file, func, line, "FAILED TO GRAB MUTEX!");
		}
		quitfrom(1, file, func, line, "WTF MUTEX ERROR ON LOCK!");
	}
}

/* Does not unset lock->file/func/line since they're only relevant when the lock is held */
void _mutex_unlock(mutex_t *lock, const char *file, const char *func, const int line)
{
	if (unlikely(pthread_mutex_unlock(&lock->mutex)))
		quitfrom(1, file, func, line, "WTF MUTEX ERROR ON UNLOCK!");
}

int _mutex_trylock(mutex_t *lock, __maybe_unused const char *file, __maybe_unused const char *func, __maybe_unused const int line)
{
	int ret;

	ret = pthread_mutex_trylock(&lock->mutex);
	if (!ret) {
		lock->file = file;
		lock->func = func;
		lock->line = line;
	}
	return ret;
}

void mutex_destroy(mutex_t *lock)
{
	pthread_mutex_destroy(&lock->mutex);
}


static int wr_timedlock(pthread_rwlock_t *lock, int timeout)
{
	tv_t now;
	ts_t abs;
	int ret;

	tv_time(&now);
	tv_to_ts(&abs, &now);
	abs.tv_sec += timeout;

	ret = pthread_rwlock_timedwrlock(lock, &abs);

	return ret;
}

void _wr_lock(rwlock_t *lock, const char *file, const char *func, const int line)
{
	int ret, retries = 0;

retry:
	ret = wr_timedlock(&lock->rwlock, 10);
	if (unlikely(ret)) {
		if (likely(ret == ETIMEDOUT)) {
			LOGERR("WARNING: Prolonged write lock contention from %s %s:%d, held by %s %s:%d",
			       file, func, line, lock->file, lock->func, lock->line);
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

int _wr_trylock(rwlock_t *lock, __maybe_unused const char *file, __maybe_unused const char *func, __maybe_unused const int line)
{
	int ret = pthread_rwlock_trywrlock(&lock->rwlock);

	if (!ret) {
		lock->file = file;
		lock->func = func;
		lock->line = line;
	}
	return ret;
}

static int rd_timedlock(pthread_rwlock_t *lock, int timeout)
{
	tv_t now;
	ts_t abs;
	int ret;

	tv_time(&now);
	tv_to_ts(&abs, &now);
	abs.tv_sec += timeout;

	ret = pthread_rwlock_timedrdlock(lock, &abs);

	return ret;
}

void _rd_lock(rwlock_t *lock, const char *file, const char *func, const int line)
{
	int ret, retries = 0;

retry:
	ret = rd_timedlock(&lock->rwlock, 10);
	if (unlikely(ret)) {
		if (likely(ret == ETIMEDOUT)) {
			LOGERR("WARNING: Prolonged read lock contention from %s %s:%d, held by %s %s:%d",
			       file, func, line, lock->file, lock->func, lock->line);
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

void _rw_unlock(rwlock_t *lock, const char *file, const char *func, const int line)
{
	if (unlikely(pthread_rwlock_unlock(&lock->rwlock)))
		quitfrom(1, file, func, line, "WTF RWLOCK ERROR ON UNLOCK!");
}

void _rd_unlock(rwlock_t *lock, const char *file, const char *func, const int line)
{
	_rw_unlock(lock, file, func, line);
}

void _wr_unlock(rwlock_t *lock, const char *file, const char *func, const int line)
{
	_rw_unlock(lock, file, func, line);
}

void _mutex_init(mutex_t *lock, const char *file, const char *func, const int line)
{
	if (unlikely(pthread_mutex_init(&lock->mutex, NULL)))
		quitfrom(1, file, func, line, "Failed to pthread_mutex_init");
}

void _rwlock_init(rwlock_t *lock, const char *file, const char *func, const int line)
{
	if (unlikely(pthread_rwlock_init(&lock->rwlock, NULL)))
		quitfrom(1, file, func, line, "Failed to pthread_rwlock_init");
}


void _cond_init(pthread_cond_t *cond, const char *file, const char *func, const int line)
{
	if (unlikely(pthread_cond_init(cond, NULL)))
		quitfrom(1, file, func, line, "Failed to pthread_cond_init!");
}

void _cklock_init(cklock_t *lock, const char *file, const char *func, const int line)
{
	_mutex_init(&lock->mutex, file, func, line);
	_rwlock_init(&lock->rwlock, file, func, line);
}


/* Read lock variant of cklock. Cannot be promoted. */
void _ck_rlock(cklock_t *lock, const char *file, const char *func, const int line)
{
	_mutex_lock(&lock->mutex, file, func, line);
	_rd_lock(&lock->rwlock, file, func, line);
	_mutex_unlock(&lock->mutex, file, func, line);
}

/* Write lock variant of cklock */
void _ck_wlock(cklock_t *lock, const char *file, const char *func, const int line)
{
	_mutex_lock(&lock->mutex, file, func, line);
	_wr_lock(&lock->rwlock, file, func, line);
}

/* Downgrade write variant to a read lock */
void _ck_dwlock(cklock_t *lock, const char *file, const char *func, const int line)
{
	_wr_unlock(&lock->rwlock, file, func, line);
	_rd_lock(&lock->rwlock, file, func, line);
	_mutex_unlock(&lock->mutex, file, func, line);
}

/* Demote a write variant to an intermediate variant */
void _ck_dwilock(cklock_t *lock, const char *file, const char *func, const int line)
{
	_wr_unlock(&lock->rwlock, file, func, line);
}

void _ck_runlock(cklock_t *lock, const char *file, const char *func, const int line)
{
	_rd_unlock(&lock->rwlock, file, func, line);
}

void _ck_wunlock(cklock_t *lock, const char *file, const char *func, const int line)
{
	_wr_unlock(&lock->rwlock, file, func, line);
	_mutex_unlock(&lock->mutex, file, func, line);
}

void cklock_destroy(cklock_t *lock)
{
	pthread_rwlock_destroy(&lock->rwlock.rwlock);
	pthread_mutex_destroy(&lock->mutex.mutex);
}


void _cksem_init(sem_t *sem, const char *file, const char *func, const int line)
{
	int ret;
	if ((ret = sem_init(sem, 0, 0)))
		quitfrom(1, file, func, line, "Failed to sem_init ret=%d errno=%d", ret, errno);
}

void _cksem_post(sem_t *sem, const char *file, const char *func, const int line)
{
	if (unlikely(sem_post(sem)))
		quitfrom(1, file, func, line, "Failed to sem_post errno=%d sem=0x%p", errno, sem);
}

void _cksem_wait(sem_t *sem, const char *file, const char *func, const int line)
{
	if (unlikely(sem_wait(sem))) {
		if (errno == EINTR)
			return;
		quitfrom(1, file, func, line, "Failed to sem_wait errno=%d sem=0x%p", errno, sem);
	}
}

int _cksem_trywait(sem_t *sem, const char *file, const char *func, const int line)
{
	int ret = sem_trywait(sem);

	if (unlikely(ret && errno != EAGAIN && errno != EINTR))
		quitfrom(1, file, func, line, "Failed to sem_trywait errno=%d sem=0x%p", errno, sem);
	return ret;
}

int _cksem_mswait(sem_t *sem, int ms, const char *file, const char *func, const int line)
{
	ts_t abs_timeout, ts_now;
	tv_t tv_now;
	int ret;

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

void _cksem_destroy(sem_t *sem, const char *file, const char *func, const int line)
{

	if (unlikely(sem_destroy(sem)))
		quitfrom(1, file, func, line, "Failed to sem_destroy errno=%d sem=0x%p", errno, sem);
}

/* Extract just the url and port information from a url string, allocating
 * heap memory for sockaddr_url and sockaddr_port. */
bool extract_sockaddr(char *url, char **sockaddr_url, char **sockaddr_port)
{
	char *url_begin, *url_end, *ipv6_begin, *ipv6_end, *port_start = NULL;
	char *url_address, *port, *tmp;
	int url_len, port_len = 0;
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
	if (ipv6_begin && ipv6_end && ipv6_end > ipv6_begin){
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
		char *slash;

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
bool url_from_sockaddr(const struct sockaddr *addr, char *url, char *port)
{
	int port_no = 0;

	switch(addr->sa_family) {
		const struct sockaddr_in *inet4_in;
		const struct sockaddr_in6 *inet6_in;

		case AF_INET:
			inet4_in = (struct sockaddr_in *)addr;
			inet_ntop(AF_INET, &inet4_in->sin_addr, url, INET6_ADDRSTRLEN);
			port_no = htons(inet4_in->sin_port);
			break;
		case AF_INET6:
			inet6_in = (struct sockaddr_in6 *)addr;
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
static int addrgetinfo(const char *node, const char *service,
                       const struct addrinfo *hints,
                       struct addrinfo **res)
{
	int ret;

	do {
		ret = getaddrinfo(node, service, hints, res);
	} while (ret == EAI_AGAIN);

	return ret;
}


bool addrinfo_from_url(const char *url, const char *port, struct addrinfo *addrinfo)
{
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
bool url_from_serverurl(char *serverurl, char *newurl, char *newport)
{
	char *url = NULL, *port = NULL;
	struct addrinfo addrinfo;
	bool ret = false;

	if (!extract_sockaddr(serverurl, &url, &port)) {
		LOGWARNING("Failed to extract server address from %s", serverurl);
		goto out;
	}
	if (!addrinfo_from_url(url, port, &addrinfo)) {
		LOGWARNING("Failed to extract addrinfo from url %s:%s", url, port);
		goto out;
	}
	if (!url_from_sockaddr((const struct sockaddr *)&addrinfo, newurl, newport)) {
		LOGWARNING("Failed to extract url from sockaddr for original url: %s:%s",
			   url, port);
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
bool url_from_socket(const int sockd, char *url, char *port)
{
	struct sockaddr_storage storage;
	socklen_t addrlen = sizeof(struct sockaddr_storage);
	struct sockaddr *addr = (struct sockaddr *)&storage;

	if (sockd < 1)
		return false;
	if (getsockname(sockd, addr, &addrlen))
		return false;
	if (!url_from_sockaddr(addr, url, port))
		return false;
	return true;
}


void keep_sockalive(int fd)
{
	const int tcp_one = 1;
	const int tcp_keepidle = 45;
	const int tcp_keepintvl = 30;

	setsockopt(fd, SOL_SOCKET, SO_KEEPALIVE, (const void *)&tcp_one, sizeof(tcp_one));
	setsockopt(fd, SOL_TCP, TCP_NODELAY, (const void *)&tcp_one, sizeof(tcp_one));
	setsockopt(fd, SOL_TCP, TCP_KEEPCNT, &tcp_one, sizeof(tcp_one));
	setsockopt(fd, SOL_TCP, TCP_KEEPIDLE, &tcp_keepidle, sizeof(tcp_keepidle));
	setsockopt(fd, SOL_TCP, TCP_KEEPINTVL, &tcp_keepintvl, sizeof(tcp_keepintvl));
}

void nolinger_socket(int fd)
{
	const struct linger so_linger = { 1, 0 };

	setsockopt(fd, SOL_SOCKET, SO_LINGER, &so_linger, sizeof(so_linger));
}

void noblock_socket(int fd)
{
	int flags = fcntl(fd, F_GETFL, 0);

	fcntl(fd, F_SETFL, O_NONBLOCK | flags);
}

void block_socket(int fd)
{
	int flags = fcntl(fd, F_GETFL, 0);

	fcntl(fd, F_SETFL, flags & ~O_NONBLOCK);
}

void _close(int *fd, const char *file, const char *func, const int line)
{
	int sockd;

	if (*fd < 0)
		return;
	sockd = *fd;
	LOGDEBUG("Closing file handle %d", sockd);
	*fd = -1;
	if (unlikely(close(sockd))) {
		LOGWARNING("Close of fd %d failed with errno %d:%s from %s %s:%d",
			   sockd, errno, strerror(errno), file, func, line);
	}
}

int bind_socket(char *url, char *port)
{
	struct addrinfo servinfobase, *servinfo, hints, *p;
	int ret, sockd = -1;
	const int on = 1;

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

int connect_socket(char *url, char *port)
{
	struct addrinfo servinfobase, *servinfo, hints, *p;
	int sockd = -1;

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
			if  (selret > 0) {
				socklen_t len;
				int err, n;

				len = sizeof(err);
				n = getsockopt(sockd, SOL_SOCKET, SO_ERROR, (void *)&err, &len);
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
int round_trip(char *url)
{
	struct addrinfo servinfobase, *p, hints;
	int sockd = -1, ret = 0, i, diff;
	tv_t start_tv, end_tv;
	char port[] = "1042";

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
			LOGINFO("Unable to get round trip due to %s:%s connect not being refused",
				url, port);
			goto out;
		}
		tv_time(&end_tv);
		diff = ms_tvdiff(&end_tv, &start_tv);
		if (!ret || diff < ret)
			ret = diff;
	}
	if (ret > 500) {
		LOGINFO("Round trip to %s:%s greater than 500ms at %d, clamping to 500",
			url, port, diff);
		diff = 500;
	}
	LOGINFO("Minimum round trip to %s:%s calculated as %dms", url, port, ret);
out:
	Close(sockd);
	freeaddrinfo(p);
	return ret;
}

int write_socket(int fd, const void *buf, size_t nbyte)
{
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

void empty_socket(int fd)
{
	char buf[PAGESIZE];
	int ret;

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

void _close_unix_socket(int *sockd, const char *server_path)
{
	LOGDEBUG("Closing unix socket %d %s", *sockd, server_path);
	_Close(sockd);
}

int _open_unix_server(const char *server_path, const char *file, const char *func, const int line)
{
	mode_t mode = S_IRWXU | S_IRWXG; // Owner+Group RWX
	struct sockaddr_un serveraddr;
	int sockd = -1, len, ret;
	struct stat buf;

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
			LOGWARNING("%s already exists and is not a socket, not removing",
				   server_path);
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

	ret = bind(sockd, (struct sockaddr *)&serveraddr, sizeof(serveraddr));
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

int _open_unix_client(const char *server_path, const char *file, const char *func, const int line)
{
	struct sockaddr_un serveraddr;
	int sockd = -1, len, ret;

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

	ret = connect(sockd, (struct sockaddr *)&serveraddr, sizeof(serveraddr));
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
int wait_close(int sockd, int timeout)
{
	struct pollfd sfd;
	int ret;

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
int wait_read_select(int sockd, float timeout)
{
	struct epoll_event event = {0, {NULL}};
	int epfd, ret;

	epfd = epoll_create1(EPOLL_CLOEXEC);
	event.events = EPOLLIN | EPOLLRDHUP;
	epoll_ctl(epfd, EPOLL_CTL_ADD, sockd, &event);
	timeout *= 1000;
	ret = epoll_wait(epfd, &event, 1, timeout);
	close(epfd);
	return ret;
}

int read_length(int sockd, void *buf, int len)
{
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
char *_recv_unix_msg(int sockd, int timeout1, int timeout2, const char *file, const char *func, const int line)
{
	char *buf = NULL;
	uint32_t msglen;
	int ret, ern;

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
	if (unlikely(msglen < 1 || msglen > 0x80000000)) {
		LOGWARNING("Invalid message length %u sent to recv_unix_msg", msglen);
		goto out;
	}
	ret = wait_read_select(sockd, timeout2);
	if (unlikely(ret < 1)) {
		ern = errno;
		LOGERR("Select2 failed in recv_unix_msg (%d)", ern);
		goto out;
	}
	buf = ckzalloc(msglen + 1);
	ret = read_length(sockd, buf, msglen);
	if (unlikely(ret < (int)msglen)) {
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
int wait_write_select(int sockd, float timeout)
{
	struct epoll_event event = {0, {NULL}};
	int epfd, ret;

	epfd = epoll_create1(EPOLL_CLOEXEC);
	event.events = EPOLLOUT | EPOLLRDHUP ;
	epoll_ctl(epfd, EPOLL_CTL_ADD, sockd, &event);
	timeout *= 1000;
	ret = epoll_wait(epfd, &event, 1, timeout);
	close(epfd);
	return ret;
}

int _write_length(int sockd, const void *buf, int len, const char *file, const char *func, const int line)
{
	int ret, ofs = 0, ern;

	if (unlikely(len < 1)) {
		LOGWARNING("Invalid write length of %d requested in write_length from %s %s:%d",
			   len, file, func, line);
		return -1;
	}
	if (unlikely(sockd < 0)) {
		ern = errno;
		LOGWARNING("Attempt to write to invalidated sock in write_length from %s %s:%d",
			   file, func, line);
		return -1;
	}
	while (len) {
		ret = write(sockd, buf + ofs, len);
		if (unlikely(ret < 0)) {
			ern = errno;
			LOGERR("Failed to write %d bytes in write_length (%d) from %s %s:%d",
			       len, ern, file, func, line);
			return -1;
		}
		ofs += ret;
		len -= ret;
	}
	return ofs;
}

bool _send_unix_msg(int sockd, const char *buf, int timeout, const char *file, const char *func, const int line)
{
	uint32_t msglen, len;
	bool retval = false;
	int ret, ern;

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

bool _send_unix_data(int sockd, const struct msghdr *msg, const char *file, const char *func, const int line)
{
	bool retval = false;
	int ret;

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

bool _recv_unix_data(int sockd, struct msghdr *msg, const char *file, const char *func, const int line)
{
	bool retval = false;
	int ret;

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
bool _send_fd(int fd, int sockd, const char *file, const char *func, const int line)
{
	struct cmsghdr *cmptr = ckzalloc(CONTROLLLEN);
	struct iovec iov[1];
	struct msghdr msg;
	char buf[2];
	bool ret;
	int *cm;

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
	cm = (int *)CMSG_DATA(cmptr);
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
int _get_fd(int sockd, const char *file, const char *func, const int line)
{
	int newfd = -1;
	char buf[MAXLINE];
	struct iovec iov[1];
	struct msghdr msg;
	struct cmsghdr *cmptr = ckzalloc(CONTROLLLEN);
	int *cm;

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
		goto out;
	}
out:
	cm = (int *)CMSG_DATA(cmptr);
	newfd = *cm;
	free(cmptr);
	return newfd;
}


void _json_check(json_t *val, json_error_t *err, const char *file, const char *func, const int line)
{
	if (likely(val))
		return;

	LOGERR("Invalid json line:%d col:%d pos:%d text: %s from %s %s:%d",
	       err->line, err->column, err->position, err->text,
	       file, func, line);
}

/* Extracts a string value from a json array with error checking. To be used
 * when the value of the string returned is only examined and not to be stored.
 * See json_array_string below */
const char *__json_array_string(json_t *val, unsigned int entry)
{
	json_t *arr_entry;

	if (json_is_null(val))
		return NULL;
	if (!json_is_array(val))
		return NULL;
	if (entry > json_array_size(val))
		return NULL;
	arr_entry = json_array_get(val, entry);
	if (!json_is_string(arr_entry))
		return NULL;

	return json_string_value(arr_entry);
}

/* Creates a freshly malloced dup of __json_array_string */
char *json_array_string(json_t *val, unsigned int entry)
{
	const char *buf = __json_array_string(val, entry);

	if (buf)
		return strdup(buf);
	return NULL;
}

json_t *json_object_dup(json_t *val, const char *entry)
{
	return json_copy(json_object_get(val, entry));
}

char *rotating_filename(const char *path, time_t when)
{
	char *filename;
	struct tm tm;

	gmtime_r(&when, &tm);
	ASPRINTF(&filename, "%s%04d%02d%02d%02d.log", path, tm.tm_year + 1900, tm.tm_mon + 1,
		 tm.tm_mday, tm.tm_hour);
	return filename;
}

/* Creates a logfile entry which changes filename hourly with exclusive access */
bool rotating_log(const char *path, const char *msg)
{
	mode_t mode = S_IRUSR | S_IWUSR | S_IRGRP | S_IROTH;
	char *filename;
	FILE *fp;
	int fd;
	bool ok = false;

	filename = rotating_filename(path, time(NULL));
	fd = open(filename, O_CREAT | O_RDWR | O_CLOEXEC , mode);
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
void align_len(size_t *len)
{
	if (*len % 4)
		*len += 4 - (*len % 4);
}

/* Malloc failure should be fatal but keep backing off and retrying as the OS
 * will kill us eventually if it can't recover. */
void realloc_strcat(char **ptr, const char *s)
{
	size_t old, new, len;
	int backoff = 1;
	void *new_ptr;
	char *ofs;

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
			fprintf(stderr, "Failed to realloc %d, retrying\n", (int)len);
		cksleep_ms(backoff);
		backoff <<= 1;
	}
	*ptr = new_ptr;
	ofs = *ptr + old;
	sprintf(ofs, "%s", s);
}

void trail_slash(char **buf)
{
	int ofs;

	ofs = strlen(*buf) - 1;
	if (memcmp(*buf + ofs, "/", 1))
		realloc_strcat(buf, "/");
}

void *_ckalloc(size_t len, const char *file, const char *func, const int line)
{
	int backoff = 1;
	void *ptr;

	align_len(&len);
	while (42) {
		ptr = malloc(len);
		if (likely(ptr))
			break;
		if (backoff == 1) {
			fprintf(stderr, "Failed to ckalloc %d, retrying from %s %s:%d\n",
				(int)len, file, func, line);
		}
		cksleep_ms(backoff);
		backoff <<= 1;
	}
	return ptr;
}

void *json_ckalloc(size_t size)
{
	return _ckalloc(size, __FILE__, __func__, __LINE__);
}

void *_ckzalloc(size_t len, const char *file, const char *func, const int line)
{
	int backoff = 1;
	void *ptr;

	align_len(&len);
	while (42) {
		ptr = calloc(len, 1);
		if (likely(ptr))
			break;
		if (backoff == 1) {
			fprintf(stderr, "Failed to ckzalloc %d, retrying from %s %s:%d\n",
				(int)len, file, func, line);
		}
		cksleep_ms(backoff);
		backoff <<= 1;
	}
	return ptr;
}

/* Round up to the nearest page size for efficient malloc */
size_t round_up_page(size_t len)
{
	int rem = len % PAGESIZE;

	if (rem)
		len += PAGESIZE - rem;
	return len;
}



/* Adequate size s==len*2 + 1 must be alloced to use this variant */
void __bin2hex(void *vs, const void *vp, size_t len)
{
	static const char hex[16] = {'0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f'};
	const uchar *p = vp;
	uchar *s = vs;
	int i;

	for (i = 0; i < (int)len; i++) {
		*s++ = hex[p[i] >> 4];
		*s++ = hex[p[i] & 0xF];
	}
	*s++ = '\0';
}

/* Returns a malloced array string of a binary value of arbitrary length. The
 * array is rounded up to a 4 byte size to appease architectures that need
 * aligned array  sizes */
void *bin2hex(const void *vp, size_t len)
{
	const uchar *p = vp;
	size_t slen;
	uchar *s;

	slen = len * 2 + 1;
	s = ckzalloc(slen);
	__bin2hex(s, p, len);

	return s;
}

const int hex2bin_tbl[256] = {
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	 0,  1,  2,  3,  4,  5,  6,  7,  8,  9, -1, -1, -1, -1, -1, -1,
	-1, 10, 11, 12, 13, 14, 15, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, 10, 11, 12, 13, 14, 15, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
};

bool _validhex(const char *buf, const char *file, const char *func, const int line)
{
	unsigned int i, slen;
	bool ret = false;

	slen = strlen(buf);
	if (!slen || slen % 2) {
		LOGDEBUG("Invalid hex due to length %u from %s %s:%d", slen, file, func, line);
		goto out;
	}
	for (i = 0; i < slen; i++) {
		uchar idx = buf[i];

		if (hex2bin_tbl[idx] == -1) {
			LOGDEBUG("Invalid hex due to value %u at offset %d from %s %s:%d",
				 idx, i, file, func, line);
			goto out;
		}
	}
	ret = true;
out:
	return ret;
}

/* Does the reverse of bin2hex but does not allocate any ram */
bool _hex2bin(void *vp, const void *vhexstr, size_t len, const char *file, const char *func, const int line)
{
	const uchar *hexstr = vhexstr;
	int nibble1, nibble2;
	bool ret = false;
	uchar *p = vp;
	uchar idx;

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

static const int b58tobin_tbl[] = {
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1,  0,  1,  2,  3,  4,  5,  6,  7,  8, -1, -1, -1, -1, -1, -1,
	-1,  9, 10, 11, 12, 13, 14, 15, 16, -1, 17, 18, 19, 20, 21, -1,
	22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, -1, -1, -1, -1, -1,
	-1, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, -1, 44, 45, 46,
	47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57
};

/* b58bin should always be at least 25 bytes long and already checked to be
 * valid. */
void b58tobin(char *b58bin, const char *b58)
{
	uint32_t c, bin32[7];
	int len, i, j;
	uint64_t t;

	memset(bin32, 0, 7 * sizeof(uint32_t));
	len = strlen((const char *)b58);
	for (i = 0; i < len; i++) {
		c = b58[i];
		c = b58tobin_tbl[c];
		for (j = 6; j >= 0; j--) {
			t = ((uint64_t)bin32[j]) * 58 + c;
			c = (t & 0x3f00000000ull) >> 32;
			bin32[j] = t & 0xffffffffull;
		}
	}
	*(b58bin++) = bin32[0] & 0xff;
	for (i = 1; i < 7; i++) {
		*((uint32_t *)b58bin) = htobe32(bin32[i]);
		b58bin += sizeof(uint32_t);
	}
}

/* Does a safe string comparison tolerating zero length and NULL strings */
int safecmp(const char *a, const char *b)
{
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
bool cmdmatch(const char *buf, const char *cmd)
{
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
char *http_base64(const char *src)
{
	char *str, *dst;
	size_t l, hlen;
	int t, r;

	l = strlen((const char *)src);
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
		src += 3; l -= 3;
		dst += 4; r += 4;
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
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
	15, -1, 10, 17, 21, 20, 26, 30,  7,  5, -1, -1, -1, -1, -1, -1,
	-1, 29, -1, 24, 13, 25,  9,  8, 23, -1, 18, 22, 31, 27, 19, -1,
	1,  0,  3, 16, 11, 28, 12, 14,  6,  4,  2, -1, -1, -1, -1, -1,
	-1, 29, -1, 24, 13, 25,  9,  8, 23, -1, 18, 22, 31, 27, 19, -1,
	1,  0,  3, 16, 11, 28, 12, 14,  6,  4,  2, -1, -1, -1, -1, -1
};

/* It's assumed that there is no chance of sending invalid chars to these
 * functions as they should have been checked beforehand. */
static void bech32_decode(uint8_t *data, int *data_len, const char *input)
{
	int input_len = strlen(input), hrp_len, i;

	*data_len = 0;
	while (*data_len < input_len && input[(input_len - 1) - *data_len] != '1')
		++(*data_len);
	hrp_len = input_len - (1 + *data_len);
	*(data_len) -= 6;
	for (i = hrp_len + 1; i < input_len; i++) {
		int v = (input[i] & 0x80) ? -1 : charset_rev[(int)input[i]];

		if (i + 6 < input_len)
			data[i - (1 + hrp_len)] = v;
	}
}

static void convert_bits(char *out, int *outlen, const uint8_t *in,
			 int inlen)
{
	const int outbits = 8, inbits = 5;
	uint32_t val = 0, maxv = (((uint32_t)1) << outbits) - 1;
	int bits = 0;

	while (inlen--) {
		val = (val << inbits) | *(in++);
		bits += inbits;
		while (bits >= outbits) {
			bits -= outbits;
			out[(*outlen)++] = (val >> bits) & maxv;
		}
	}
}

static int address_to_pubkeytxn(char *pkh, const char *addr)
{
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

static int address_to_scripttxn(char *psh, const char *addr)
{
	char b58bin[25] = {};

	b58tobin(b58bin, addr);
	psh[0] = 0xa9;
	psh[1] = 0x14;
	memcpy(&psh[2], &b58bin[1], 20);
	psh[22] = 0x87;
	return 23;
}

static int segaddress_to_txn(char *p2h, const char *addr)
{
	int data_len, witdata_len = 0;
	char *witdata = &p2h[2];
	uint8_t data[84];

	bech32_decode(data, &data_len, addr);
	p2h[0] = data[0];
	/* Witness version is > 0 */
	if (p2h[0])
		p2h[0] += 0x50;
	convert_bits(witdata, &witdata_len, data + 1, data_len - 1);
	p2h[1] = witdata_len;
	return witdata_len + 2;
}

/* Convert an address to a transaction and return the length of the transaction */
int address_to_txn(char *p2h, const char *addr, const bool script, const bool segwit)
{
	if (segwit)
		return segaddress_to_txn(p2h, addr);
	if (script)
		return address_to_scripttxn(p2h, addr);
	return address_to_pubkeytxn(p2h, addr);
}

/*  For encoding nHeight into coinbase, return how many bytes were used */
int ser_number(uchar *s, int32_t val)
{
	int32_t *i32 = (int32_t *)&s[1];
	int len;

	if (val < 0x80)
		len = 1;
	else if (val < 0x8000)
		len = 2;
	else if (val < 0x800000)
		len = 3;
	else
		len = 4;
	*i32 = htole32(val);
	s[0] = len++;
	return len;
}

int get_sernumber(uchar *s)
{
	int32_t val = 0;
	int len;

	len = s[0];
	if (unlikely(len < 1 || len > 4))
		return 0;
	memcpy(&val, &s[1], len);
	return le32toh(val);
}

/* For testing a le encoded 256 byte hash against a target */
bool fulltest(const uchar *hash, const uchar *target)
{
	uint32_t *hash32 = (uint32_t *)hash;
	uint32_t *target32 = (uint32_t *)target;
	bool ret = true;
	int i;

	for (i = 28 / 4; i >= 0; i--) {
		uint32_t h32tmp = le32toh(hash32[i]);
		uint32_t t32tmp = le32toh(target32[i]);

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

void copy_tv(tv_t *dest, const tv_t *src)
{
	memcpy(dest, src, sizeof(tv_t));
}

void ts_to_tv(tv_t *val, const ts_t *spec)
{
	val->tv_sec = spec->tv_sec;
	val->tv_usec = spec->tv_nsec / 1000;
}

void tv_to_ts(ts_t *spec, const tv_t *val)
{
	spec->tv_sec = val->tv_sec;
	spec->tv_nsec = val->tv_usec * 1000;
}

void us_to_tv(tv_t *val, int64_t us)
{
	lldiv_t tvdiv = lldiv(us, 1000000);

	val->tv_sec = tvdiv.quot;
	val->tv_usec = tvdiv.rem;
}

void us_to_ts(ts_t *spec, int64_t us)
{
	lldiv_t tvdiv = lldiv(us, 1000000);

	spec->tv_sec = tvdiv.quot;
	spec->tv_nsec = tvdiv.rem * 1000;
}

void ms_to_ts(ts_t *spec, int64_t ms)
{
	lldiv_t tvdiv = lldiv(ms, 1000);

	spec->tv_sec = tvdiv.quot;
	spec->tv_nsec = tvdiv.rem * 1000000;
}

void ms_to_tv(tv_t *val, int64_t ms)
{
	lldiv_t tvdiv = lldiv(ms, 1000);

	val->tv_sec = tvdiv.quot;
	val->tv_usec = tvdiv.rem * 1000;
}

void tv_time(tv_t *tv)
{
	gettimeofday(tv, NULL);
}

void ts_realtime(ts_t *ts)
{
	clock_gettime(CLOCK_REALTIME, ts);
}

void cksleep_prepare_r(ts_t *ts)
{
	clock_gettime(CLOCK_MONOTONIC, ts);
}

void nanosleep_abstime(ts_t *ts_end)
{
	clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, ts_end, NULL);
}

void timeraddspec(ts_t *a, const ts_t *b)
{
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
void cksleep_ms_r(ts_t *ts_start, int ms)
{
	ts_t ts_end;

	ms_to_ts(&ts_end, ms);
	timeraddspec(&ts_end, ts_start);
	nanosleep_abstime(&ts_end);
}

void cksleep_us_r(ts_t *ts_start, int64_t us)
{
	ts_t ts_end;

	us_to_ts(&ts_end, us);
	timeraddspec(&ts_end, ts_start);
	nanosleep_abstime(&ts_end);
}

void cksleep_ms(int ms)
{
	ts_t ts_start;

	cksleep_prepare_r(&ts_start);
	cksleep_ms_r(&ts_start, ms);
}

void cksleep_us(int64_t us)
{
	ts_t ts_start;

	cksleep_prepare_r(&ts_start);
	cksleep_us_r(&ts_start, us);
}

/* Returns the microseconds difference between end and start times as a double */
double us_tvdiff(tv_t *end, tv_t *start)
{
	/* Sanity check. We should only be using this for small differences so
	 * limit the max to 60 seconds. */
	if (unlikely(end->tv_sec - start->tv_sec > 60))
		return 60000000;
	return (end->tv_sec - start->tv_sec) * 1000000 + (end->tv_usec - start->tv_usec);
}

/* Returns the milliseconds difference between end and start times */
int ms_tvdiff(tv_t *end, tv_t *start)
{
	/* Like us_tdiff, limit to 1 hour. */
	if (unlikely(end->tv_sec - start->tv_sec > 3600))
		return 3600000;
	return (end->tv_sec - start->tv_sec) * 1000 + (end->tv_usec - start->tv_usec) / 1000;
}

/* Returns the seconds difference between end and start times as a double */
double tvdiff(tv_t *end, tv_t *start)
{
	return end->tv_sec - start->tv_sec + (end->tv_usec - start->tv_usec) / 1000000.0;
}

/* Create an exponentially decaying average over interval */
void decay_time(double *f, double fadd, double fsecs, double interval)
{
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
	 * eventually underflow libjansson's real number interpretation. */
	if (unlikely(*f < 2E-16))
		*f = 0;
}

/* Sanity check to prevent clock adjustments backwards from screwing up stats */
double sane_tdiff(tv_t *end, tv_t *start)
{
	double tdiff = tvdiff(end, start);

	if (unlikely(tdiff < 0.001))
		tdiff = 0.001;
	return tdiff;
}

/* Convert a double value into a truncated string for displaying with its
 * associated suitable for Mega, Giga etc. Buf array needs to be long enough */
void suffix_string(double val, char *buf, size_t bufsiz, int sigdigits)
{
	const double kilo = 1000;
	const double mega = 1000000;
	const double giga = 1000000000;
	const double tera = 1000000000000;
	const double peta = 1000000000000000;
	const double exa  = 1000000000000000000;
	char suffix[2] = "";
	bool decimal = true;
	double dval;

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
double le256todouble(const uchar *target)
{
	uint64_t *data64;
	double dcut64;

	data64 = (uint64_t *)(target + 24);
	dcut64 = le64toh(*data64) * bits192;

	data64 = (uint64_t *)(target + 16);
	dcut64 += le64toh(*data64) * bits128;

	data64 = (uint64_t *)(target + 8);
	dcut64 += le64toh(*data64) * bits64;

	data64 = (uint64_t *)(target);
	dcut64 += le64toh(*data64);

	return dcut64;
}

/* Converts a big endian 256 bit value to a double */
double be256todouble(const uchar *target)
{
	uint64_t *data64;
	double dcut64;

	data64 = (uint64_t *)(target);
	dcut64 = be64toh(*data64) * bits192;

	data64 = (uint64_t *)(target + 8);
	dcut64 += be64toh(*data64) * bits128;

	data64 = (uint64_t *)(target + 16);
	dcut64 += be64toh(*data64) * bits64;

	data64 = (uint64_t *)(target + 24);
	dcut64 += be64toh(*data64);

	return dcut64;
}

/* Return a difficulty from a binary target */
double diff_from_target(uchar *target)
{
	double dcut64;

	dcut64 = le256todouble(target);
	if (unlikely(dcut64 <= 0))
		dcut64 = 1;
	return truediffone / dcut64;
}

/* Return a difficulty from a binary big endian target */
double diff_from_betarget(uchar *target)
{
	double dcut64;

	dcut64 = be256todouble(target);
	if (unlikely(dcut64 <= 0))
		dcut64 = 1;
	return truediffone / dcut64;
}

/* Return the network difficulty from the block header which is in packed form,
 * as a double. */
double diff_from_nbits(char *nbits)
{
	uint8_t shift = nbits[0];
	uchar target[32] = {};
	char *nb;

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

void target_from_diff(uchar *target, double diff)
{
	uint64_t *data64, h64;
	double d64, dcut64;

	if (unlikely(diff == 0.0)) {
		/* This shouldn't happen but best we check to prevent a crash */
		memset(target, 0xff, 32);
		return;
	}

	d64 = truediffone;
	d64 /= diff;

	dcut64 = d64 / bits192;
	h64 = dcut64;
	data64 = (uint64_t *)(target + 24);
	*data64 = htole64(h64);
	dcut64 = h64;
	dcut64 *= bits192;
	d64 -= dcut64;

	dcut64 = d64 / bits128;
	h64 = dcut64;
	data64 = (uint64_t *)(target + 16);
	*data64 = htole64(h64);
	dcut64 = h64;
	dcut64 *= bits128;
	d64 -= dcut64;

	dcut64 = d64 / bits64;
	h64 = dcut64;
	data64 = (uint64_t *)(target + 8);
	*data64 = htole64(h64);
	dcut64 = h64;
	dcut64 *= bits64;
	d64 -= dcut64;

	h64 = d64;
	data64 = (uint64_t *)(target);
	*data64 = htole64(h64);
}

void gen_hash(uchar *data, uchar *hash, int len)
{
	uchar hash1[32];

	sha256(data, len, hash1);
	sha256(hash1, 32, hash);
}
