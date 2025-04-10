/*
 * Copyright 2014-2020,2023 Con Kolivas
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
#include <jansson.h>
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

ckpool_t *global_ckp;

static bool open_logfile(ckpool_t *ckp)
{
	if (ckp->logfd > 0) {
		flock(ckp->logfd, LOCK_EX);
		fflush(ckp->logfp);
		Close(ckp->logfd);
	}
	ckp->logfp = fopen(ckp->logfilename, "ae");
	if (unlikely(!ckp->logfp)) {
		LOGEMERG("Failed to make open log file %s", ckp->logfilename);
		return false;
	}
	/* Make logging line buffered */
	setvbuf(ckp->logfp, NULL, _IOLBF, 0);
	ckp->logfd = fileno(ckp->logfp);
	ckp->lastopen_t = time(NULL);
	return true;
}

/* Use ckmsgqs for logging to console and files to prevent logmsg from blocking
 * on any delays. */
static void console_log(ckpool_t __maybe_unused *ckp, char *msg)
{
	/* Add clear line only if stderr is going to console */
	if (isatty(fileno(stderr)))
		fprintf(stderr, "\33[2K\r");
	fprintf(stderr, "%s", msg);
	fflush(stderr);

	free(msg);
}

static void proclog(ckpool_t *ckp, char *msg)
{
	time_t log_t = time(NULL);

	/* Reopen log file every minute, allowing us to move/rename it and
	 * create a new logfile */
	if (log_t > ckp->lastopen_t + 60) {
		LOGDEBUG("Reopening logfile");
		open_logfile(ckp);
	}

	flock(ckp->logfd, LOCK_EX);
	fprintf(ckp->logfp, "%s", msg);
	flock(ckp->logfd, LOCK_UN);

	free(msg);
}

void get_timestamp(char *stamp)
{
	struct tm tm;
	tv_t now_tv;
	int ms;

	tv_time(&now_tv);
	ms = (int)(now_tv.tv_usec / 1000);
	localtime_r(&(now_tv.tv_sec), &tm);
	sprintf(stamp, "[%d-%02d-%02d %02d:%02d:%02d.%03d]",
			tm.tm_year + 1900,
			tm.tm_mon + 1,
			tm.tm_mday,
			tm.tm_hour,
			tm.tm_min,
			tm.tm_sec, ms);
}

/* Log everything to the logfile, but display warnings on the console as well */
void logmsg(int loglevel, const char *fmt, ...)
{
	int logfd = global_ckp->logfd;
	char *log, *buf = NULL;
	char stamp[128];
	va_list ap;

	if (global_ckp->loglevel < loglevel || !fmt)
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

	if (unlikely(!global_ckp->console_logger)) {
		fprintf(stderr, "%s", log);
		goto out_free;
	}
	if (loglevel <= LOG_WARNING)
		ckmsgq_add(global_ckp->console_logger, strdup(log));
	if (logfd > 0)
		ckmsgq_add(global_ckp->logger, strdup(log));
out_free:
	free(log);
out:
	free(buf);
}

/* Generic function for creating a message queue receiving and parsing thread */
static void *ckmsg_queue(void *arg)
{
	ckmsgq_t *ckmsgq = (ckmsgq_t *)arg;
	ckpool_t *ckp = ckmsgq->ckp;

	pthread_detach(pthread_self());
	rename_proc(ckmsgq->name);
	ckmsgq->active = true;

	while (42) {
		ckmsg_t *msg;
		tv_t now;
		ts_t abs;

		mutex_lock(ckmsgq->lock);
		tv_time(&now);
		tv_to_ts(&abs, &now);
		abs.tv_sec++;
		if (!ckmsgq->msgs)
			cond_timedwait(ckmsgq->cond, ckmsgq->lock, &abs);
		msg = ckmsgq->msgs;
		if (msg)
			DL_DELETE(ckmsgq->msgs, msg);
		mutex_unlock(ckmsgq->lock);

		if (!msg)
			continue;
		ckmsgq->func(ckp, msg->data);
		free(msg);
	}
	return NULL;
}

ckmsgq_t *create_ckmsgq(ckpool_t *ckp, const char *name, const void *func)
{
	ckmsgq_t *ckmsgq = ckzalloc(sizeof(ckmsgq_t));

	strncpy(ckmsgq->name, name, 15);
	ckmsgq->func = func;
	ckmsgq->ckp = ckp;
	ckmsgq->lock = ckalloc(sizeof(mutex_t));
	ckmsgq->cond = ckalloc(sizeof(pthread_cond_t));
	mutex_init(ckmsgq->lock);
	cond_init(ckmsgq->cond);
	create_pthread(&ckmsgq->pth, ckmsg_queue, ckmsgq);

	return ckmsgq;
}

ckmsgq_t *create_ckmsgqs(ckpool_t *ckp, const char *name, const void *func, const int count)
{
	ckmsgq_t *ckmsgq = ckzalloc(sizeof(ckmsgq_t) * count);
	mutex_t *lock;
	pthread_cond_t *cond;
	int i;

	lock = ckalloc(sizeof(mutex_t));
	cond = ckalloc(sizeof(pthread_cond_t));
	mutex_init(lock);
	cond_init(cond);

	for (i = 0; i < count; i++) {
		snprintf(ckmsgq[i].name, 15, "%.6s%x", name, i);
		ckmsgq[i].func = func;
		ckmsgq[i].ckp = ckp;
		ckmsgq[i].lock = lock;
		ckmsgq[i].cond = cond;
		create_pthread(&ckmsgq[i].pth, ckmsg_queue, &ckmsgq[i]);
	}

	return ckmsgq;
}

/* Generic function for adding messages to a ckmsgq linked list and signal the
 * ckmsgq parsing thread(s) to wake up and process it. */
bool _ckmsgq_add(ckmsgq_t *ckmsgq, void *data, const char *file, const char *func, const int line)
{
	ckmsg_t *msg;

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
	DL_APPEND(ckmsgq->msgs, msg);
	pthread_cond_broadcast(ckmsgq->cond);
	mutex_unlock(ckmsgq->lock);

	return true;
}

/* Return whether there are any messages queued in the ckmsgq linked list. */
bool ckmsgq_empty(ckmsgq_t *ckmsgq)
{
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
static void *unix_receiver(void *arg)
{
	proc_instance_t *pi = (proc_instance_t *)arg;
	int rsockd = pi->us.sockd, sockd;
	char qname[16];

	sprintf(qname, "%cunixrq", pi->processname[0]);
	rename_proc(qname);
	pthread_detach(pthread_self());

	while (42) {
		unix_msg_t *umsg;
		char *buf;

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
unix_msg_t *get_unix_msg(proc_instance_t *pi)
{
	unix_msg_t *umsg;

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

static void create_unix_receiver(proc_instance_t *pi)
{
	pthread_t pth;

	mutex_init(&pi->rmsg_lock);
	cond_init(&pi->rmsg_cond);

	create_pthread(&pth, unix_receiver, pi);
}

/* Put a sanity check on kill calls to make sure we are not sending them to
 * pid 0. */
static int kill_pid(const int pid, const int sig)
{
	if (pid < 1)
		return -1;
	return kill(pid, sig);
}

static int pid_wait(const pid_t pid, const int ms)
{
	tv_t start, now;
	int ret;

	tv_time(&start);
	do {
		ret = kill_pid(pid, 0);
		if (ret)
			break;
		tv_time(&now);
	} while (ms_tvdiff(&now, &start) < ms);
	return ret;
}

static void api_message(ckpool_t *ckp, char **buf, int *sockd)
{
	apimsg_t *apimsg = ckalloc(sizeof(apimsg_t));

	apimsg->buf = *buf;
	*buf = NULL;
	apimsg->sockd = *sockd;
	*sockd = -1;
	ckmsgq_add(ckp->ckpapi, apimsg);
}

/* Listen for incoming global requests. Always returns a response if possible */
static void *listener(void *arg)
{
	proc_instance_t *pi = (proc_instance_t *)arg;
	unixsock_t *us = &pi->us;
	ckpool_t *ckp = pi->ckp;
	char *buf = NULL, *msg;
	int sockd;

	rename_proc(pi->sockname);
retry:
	dealloc(buf);
	sockd = accept(us->sockd, NULL, NULL);
	if (sockd < 0) {
		LOGERR("Failed to accept on socket in listener");
		goto out;
	}

	buf = recv_unix_msg(sockd);
	if (!buf) {
		LOGWARNING("Failed to get message in listener");
		send_unix_msg(sockd, "failed");
	} else if (buf[0] == '{') {
		/* Any JSON messages received are for the RPC API to handle */
		api_message(ckp, &buf, &sockd);
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
			ckp->loglevel = loglevel;
			send_unix_msg(sockd, "success");
		}
	} else if (cmdmatch(buf, "getxfd")) {
		int fdno = -1;

		sscanf(buf, "getxfd%d", &fdno);
		connector_send_fd(ckp, fdno, sockd);
	} else if (cmdmatch(buf, "accept")) {
		LOGWARNING("Listener received accept message, accepting clients");
		send_proc(ckp->connector, "accept");
		send_unix_msg(sockd, "accepting");
	} else if (cmdmatch(buf, "reject")) {
		LOGWARNING("Listener received reject message, rejecting clients");
		send_proc(ckp->connector, "reject");
		send_unix_msg(sockd, "rejecting");
	} else if (cmdmatch(buf, "reconnect")) {
		LOGWARNING("Listener received request to send reconnect to clients");
		send_proc(ckp->stratifier, buf);
		send_unix_msg(sockd, "reconnecting");
	} else if (cmdmatch(buf, "restart")) {
		LOGWARNING("Listener received restart message, attempting handover");
		send_unix_msg(sockd, "restarting");
		if (!fork()) {
			if (!ckp->handover) {
				ckp->initial_args[ckp->args++] = strdup("-H");
				ckp->initial_args[ckp->args] = NULL;
			}
			execv(ckp->initial_args[0], (char *const *)ckp->initial_args);
		}
	} else if (cmdmatch(buf, "stratifierstats")) {
		LOGDEBUG("Listener received stratifierstats request");
		msg = stratifier_stats(ckp, ckp->sdata);
		send_unix_msg(sockd, msg);
		dealloc(msg);
	} else if (cmdmatch(buf, "connectorstats")) {
		LOGDEBUG("Listener received connectorstats request");
		msg = connector_stats(ckp->cdata, 0);
		send_unix_msg(sockd, msg);
		dealloc(msg);
	} else if (cmdmatch(buf, "resetshares")) {
		LOGWARNING("Resetting best shares");
		send_proc(ckp->stratifier, buf);
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

void empty_buffer(connsock_t *cs)
{
	if (cs->buf)
		cs->buf[0] = '\0';
	cs->buflen = cs->bufofs = 0;
}

int set_sendbufsize(ckpool_t *ckp, const int fd, const int len)
{
	socklen_t optlen;
	int opt;

	optlen = sizeof(opt);
	opt = len * 4 / 3;
	setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &opt, optlen);
	getsockopt(fd, SOL_SOCKET, SO_SNDBUF, &opt, &optlen);
	opt /= 2;
	if (opt < len) {
		LOGDEBUG("Failed to set desired sendbufsize of %d unprivileged, only got %d",
			 len, opt);
		optlen = sizeof(opt);
		opt = len * 4 / 3;
		setsockopt(fd, SOL_SOCKET, SO_SNDBUFFORCE, &opt, optlen);
		getsockopt(fd, SOL_SOCKET, SO_SNDBUF, &opt, &optlen);
		opt /= 2;
	}
	if (opt < len) {
		LOGNOTICE("Failed to increase sendbufsize to %d, increase wmem_max or start %s privileged if using a remote btcd",
			   len, ckp->name);
		ckp->wmem_warn = true;
	} else
		LOGDEBUG("Increased sendbufsize to %d of desired %d", opt, len);
	return opt;
}

int set_recvbufsize(ckpool_t *ckp, const int fd, const int len)
{
	socklen_t optlen;
	int opt;

	optlen = sizeof(opt);
	opt = len * 4 / 3;
	setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &opt, optlen);
	getsockopt(fd, SOL_SOCKET, SO_RCVBUF, &opt, &optlen);
	opt /= 2;
	if (opt < len) {
		LOGDEBUG("Failed to set desired rcvbufsiz of %d unprivileged, only got %d",
			 len, opt);
		optlen = sizeof(opt);
		opt = len * 4 / 3;
		setsockopt(fd, SOL_SOCKET, SO_RCVBUFFORCE, &opt, optlen);
		getsockopt(fd, SOL_SOCKET, SO_RCVBUF, &opt, &optlen);
		opt /= 2;
	}
	if (opt < len) {
		LOGNOTICE("Failed to increase rcvbufsiz to %d, increase rmem_max or start %s privileged if using a remote btcd",
			   len, ckp->name);
		ckp->rmem_warn = true;
	} else
		LOGDEBUG("Increased rcvbufsiz to %d of desired %d", opt, len);
	return opt;
}

/* If there is any cs->buflen it implies a full line was received on the last
 * pass through read_socket_line and subsequently processed, leaving
 * unprocessed data beyond cs->bufofs. Otherwise a zero buflen means there is
 * only unprocessed data of bufofs length. */
static void clear_bufline(connsock_t *cs)
{
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

static void add_buflen(ckpool_t *ckp, connsock_t *cs, const char *readbuf, const int len)
{
	int backoff = 1;
	int buflen;

	buflen = round_up_page(cs->bufofs + len + 1);
	while (cs->bufsize < buflen) {
		char *newbuf = realloc(cs->buf, buflen);

		if (likely(newbuf)) {
			cs->bufsize = buflen;
			cs->buf = newbuf;
			break;
		}
		if (backoff == 1)
			fprintf(stderr, "Failed to realloc %d in read_socket_line, retrying\n", (int)buflen);
		cksleep_ms(backoff);
		backoff <<= 1;
	}
	/* Increase receive buffer if possible to larger than the largest
	 * message we're likely to buffer */
	if (unlikely(!ckp->rmem_warn && buflen > cs->rcvbufsiz))
		cs->rcvbufsiz = set_recvbufsize(ckp, cs->fd, buflen);

	memcpy(cs->buf + cs->bufofs, readbuf, len);
	cs->bufofs += len;
	cs->buf[cs->bufofs] = '\0';
}

/* Receive as much data is currently available without blocking into a connsock
 * buffer. Returns total length of data read. */
static int recv_available(ckpool_t *ckp, connsock_t *cs)
{
	char readbuf[PAGESIZE];
	int len = 0, ret;

	do {
		ret = recv(cs->fd, readbuf, PAGESIZE - 4, MSG_DONTWAIT);
		if (ret > 0) {
			add_buflen(ckp, cs, readbuf, ret);
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
int read_socket_line(connsock_t *cs, float *timeout)
{
	ckpool_t *ckp = cs->ckp;
	bool quiet = ckp->proxy | ckp->remote;
	char *eom = NULL;
	tv_t start, now;
	float diff;
	int ret;

	clear_bufline(cs);
	recv_available(ckp, cs); // Intentionally ignore return value
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
		ret = recv_available(ckp, cs);
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
void _queue_proc(proc_instance_t *pi, const char *msg, const char *file, const char *func, const int line)
{
	unix_msg_t *umsg;

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
char *_send_recv_proc(const proc_instance_t *pi, const char *msg, int writetimeout, int readtimedout,
		      const char *file, const char *func, const int line)
{
	char *path = pi->us.path, *buf = NULL;
	int sockd;

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

static const char *rpc_method(const char *rpc_req)
{
	const char *ptr = strchr(rpc_req, ':');
	if (ptr)
		return ptr+1;
	return rpc_req;
}

/* All of these calls are made to bitcoind which prefers open/close instead
 * of persistent connections so cs->fd is always invalid. */
static json_t *_json_rpc_call(connsock_t *cs, const char *rpc_req, const bool info_only)
{
	float timeout = RPC_TIMEOUT;
	char *http_req = NULL;
	json_error_t err_val;
	char *warning = NULL;
	json_t *val = NULL;
	tv_t stt_tv, fin_tv;
	double elapsed;
	int len, ret;

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
	http_req = ckalloc(len + 256); // Leave room for headers
	sprintf(http_req,
		 "POST / HTTP/1.1\n"
		 "Authorization: Basic %s\n"
		 "Host: %s:%s\n"
		 "Content-type: application/json\n"
		 "Content-Length: %d\n\n%s",
		 cs->auth, cs->url, cs->port, len, rpc_req);

	len = strlen(http_req);
	tv_time(&stt_tv);
	ret = write_socket(cs->fd, http_req, len);
	if (ret != len) {
		tv_time(&fin_tv);
		elapsed = tvdiff(&fin_tv, &stt_tv);
		ASPRINTF(&warning, "Failed to write to socket in %s (%.10s...) %.3fs",
			 __func__, rpc_method(rpc_req), elapsed);
		goto out_empty;
	}
	ret = read_socket_line(cs, &timeout);
	if (ret < 1) {
		tv_time(&fin_tv);
		elapsed = tvdiff(&fin_tv, &stt_tv);
		ASPRINTF(&warning, "Failed to read socket line in %s (%.10s...) %.3fs",
			 __func__, rpc_method(rpc_req), elapsed);
		goto out_empty;
	}
	if (strncasecmp(cs->buf, "HTTP/1.1 200 OK", 15)) {
		tv_time(&fin_tv);
		elapsed = tvdiff(&fin_tv, &stt_tv);
		ASPRINTF(&warning, "HTTP response to (%.10s...) %.3fs not ok: %s",
			 rpc_method(rpc_req), elapsed, cs->buf);
		timeout = 0;
		/* Look for a json response if there is one */
		while (read_socket_line(cs, &timeout) > 0) {
			timeout = 0;
			if (*cs->buf != '{')
				continue;
			free(warning);
			/* Replace the warning with the json response */
			ASPRINTF(&warning, "JSON response to (%.10s...) %.3fs not ok: %s",
				 rpc_method(rpc_req), elapsed, cs->buf);
			break;
		}
		goto out_empty;
	}
	do {
		ret = read_socket_line(cs, &timeout);
		if (ret < 1) {
			tv_time(&fin_tv);
			elapsed = tvdiff(&fin_tv, &stt_tv);
			ASPRINTF(&warning, "Failed to read http socket lines in %s (%.10s...) %.3fs",
				 __func__, rpc_method(rpc_req), elapsed);
			goto out_empty;
		}
	} while (strncmp(cs->buf, "{", 1));
	tv_time(&fin_tv);
	elapsed = tvdiff(&fin_tv, &stt_tv);
	if (elapsed > 5.0) {
		ASPRINTF(&warning, "HTTP socket read+write took %.3fs in %s (%.10s...)",
			 elapsed, __func__, rpc_method(rpc_req));
	}

	val = json_loads(cs->buf, 0, &err_val);
	if (!val) {
		ASPRINTF(&warning, "JSON decode (%.10s...) failed(%d): %s",
			 rpc_method(rpc_req), err_val.line, err_val.text);
	}
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
	return val;
}

json_t *json_rpc_call(connsock_t *cs, const char *rpc_req)
{
	return _json_rpc_call(cs, rpc_req, false);
}

json_t *json_rpc_response(connsock_t *cs, const char *rpc_req)
{
	return _json_rpc_call(cs, rpc_req, true);
}

/* For when we are submitting information that is not important and don't care
 * about the response. */
void json_rpc_msg(connsock_t *cs, const char *rpc_req)
{
	json_t *val = _json_rpc_call(cs, rpc_req, true);

	/* We don't care about the result */
	json_decref(val);
}

static void terminate_oldpid(const ckpool_t *ckp, proc_instance_t *pi, const pid_t oldpid)
{
	if (!ckp->killold) {
		quit(1, "Process %s pid %d still exists, start ckpool with -H to get a handover or -k if you wish to kill it",
				pi->processname, oldpid);
	}
	LOGNOTICE("Terminating old process %s pid %d", pi->processname, oldpid);
	if (kill_pid(oldpid, 15))
		quit(1, "Unable to kill old process %s pid %d", pi->processname, oldpid);
	LOGWARNING("Terminating old process %s pid %d", pi->processname, oldpid);
	if (pid_wait(oldpid, 500))
		return;
	LOGWARNING("Old process %s pid %d failed to respond to terminate request, killing",
			pi->processname, oldpid);
	if (kill_pid(oldpid, 9) || !pid_wait(oldpid, 3000))
		quit(1, "Unable to kill old process %s pid %d", pi->processname, oldpid);
}

/* This is for blocking sends of json messages */
bool _send_json_msg(connsock_t *cs, const json_t *json_msg, const char *file, const char *func, const int line)
{
	bool ret = false;
	int len, sent;
	char *s;

	if (unlikely(!json_msg)) {
		LOGWARNING("Empty json msg in send_json_msg from %s %s:%d", file, func, line);
		goto out;
	}
	s = json_dumps(json_msg, JSON_ESCAPE_SLASH | JSON_EOL);
	if (unlikely(!s)) {
		LOGWARNING("Empty json dump in send_json_msg from %s %s:%d", file, func, line);
		goto out;
	}
	LOGDEBUG("Sending json msg: %s", s);
	len = strlen(s);
	if (unlikely(!len)) {
		LOGWARNING("Zero length string in send_json_msg from %s %s:%d", file, func, line);
		goto out;
	}
	sent = write_socket(cs->fd, s, len);
	dealloc(s);
	if (sent != len) {
		LOGNOTICE("Failed to send %d bytes sent %d in send_json_msg", len, sent);
		goto out;
	}
	ret = true;
out:
	return ret;
}

/* Decode a string that should have a json message and return just the contents
 * of the result key or NULL. */
static json_t *json_result(json_t *val)
{
	json_t *res_val = NULL, *err_val;

	res_val = json_object_get(val, "result");
	/* (null) is a valid result while no value is an error, so mask out
	 * (null) and only handle lack of result */
	if (json_is_null(res_val))
		res_val = NULL;
	else if (!res_val) {
		char *ss;

		err_val = json_object_get(val, "error");
		if (err_val)
			ss = json_dumps(err_val, 0);
		else
			ss = strdup("(unknown reason)");

		LOGNOTICE("JSON-RPC decode of json_result failed: %s", ss);
		free(ss);
	}
	return res_val;
}

/* Return the error value if one exists */
static json_t *json_errval(json_t *val)
{
	json_t *err_val = json_object_get(val, "error");

	return err_val;
}

/* Parse a string and return the json value it contains, if any, and the
 * result in res_val. Return NULL if no result key is found. */
json_t *json_msg_result(const char *msg, json_t **res_val, json_t **err_val)
{
	json_error_t err;
	json_t *val;

	*res_val = NULL;
	val = json_loads(msg, 0, &err);
	if (!val) {
		LOGWARNING("Json decode failed(%d): %s", err.line, err.text);
		goto out;
	}
	*res_val = json_result(val);
	*err_val = json_errval(val);

out:
	return val;
}

/* Open the file in path, check if there is a pid in there that still exists
 * and if not, write the pid into that file. */
static bool write_pid(ckpool_t *ckp, const char *path, proc_instance_t *pi, const pid_t pid, const pid_t oldpid)
{
	FILE *fp;

	if (ckp->handover && oldpid && !pid_wait(oldpid, 500)) {
		LOGWARNING("Old process pid %d failed to shutdown cleanly, terminating", oldpid);
		terminate_oldpid(ckp, pi, oldpid);
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

static void name_process_sockname(unixsock_t *us, const proc_instance_t *pi)
{
	us->path = strdup(pi->ckp->socket_dir);
	realloc_strcat(&us->path, pi->sockname);
}

static void open_process_sock(ckpool_t *ckp, const proc_instance_t *pi, unixsock_t *us)
{
	LOGDEBUG("Opening %s", us->path);
	us->sockd = open_unix_server(us->path);
	if (unlikely(us->sockd < 0))
		quit(1, "Failed to open %s socket", pi->sockname);
	if (chown(us->path, -1, ckp->gr_gid))
		quit(1, "Failed to set %s to group id %d", us->path, ckp->gr_gid);
}

static void create_process_unixsock(proc_instance_t *pi)
{
	unixsock_t *us = &pi->us;
	ckpool_t *ckp = pi->ckp;

	name_process_sockname(us, pi);
	open_process_sock(ckp, pi, us);
}

static void write_namepid(proc_instance_t *pi)
{
	char s[256];

	pi->pid = getpid();
	sprintf(s, "%s%s.pid", pi->ckp->socket_dir, pi->processname);
	if (!write_pid(pi->ckp, s, pi, pi->pid, pi->oldpid))
		quit(1, "Failed to write %s pid %d", pi->processname, pi->pid);
}

static void rm_namepid(const proc_instance_t *pi)
{
	char s[256];

	sprintf(s, "%s%s.pid", pi->ckp->socket_dir, pi->processname);
	unlink(s);
}

static void launch_logger(ckpool_t *ckp)
{
	ckp->logger = create_ckmsgq(ckp, "logger", &proclog);
	ckp->console_logger = create_ckmsgq(ckp, "conlog", &console_log);
}

static void clean_up(ckpool_t *ckp)
{
	rm_namepid(&ckp->main);
	dealloc(ckp->socket_dir);
}

static void cancel_pthread(pthread_t *pth)
{
	if (!pth || !*pth)
		return;
	pthread_cancel(*pth);
	pth = NULL;
}

static void sighandler(const int sig)
{
	ckpool_t *ckp = global_ckp;

	signal(sig, SIG_IGN);
	signal(SIGTERM, SIG_IGN);
	LOGWARNING("Process %s received signal %d, shutting down",
		   ckp->name, sig);

	cancel_pthread(&ckp->pth_listener);
	exit(0);
}

static bool _json_get_string(char **store, const json_t *entry, const char *res)
{
	bool ret = false;
	const char *buf;

	*store = NULL;
	if (!entry || json_is_null(entry)) {
		LOGDEBUG("Json did not find entry %s", res);
		goto out;
	}
	if (!json_is_string(entry)) {
		LOGWARNING("Json entry %s is not a string", res);
		goto out;
	}
	buf = json_string_value(entry);
	LOGDEBUG("Json found entry %s: %s", res, buf);
	*store = strdup(buf);
	ret = true;
out:
	return ret;
}

bool json_get_string(char **store, const json_t *val, const char *res)
{
	return _json_get_string(store, json_object_get(val, res), res);
}

/* Used when there must be a valid string */
static void json_get_configstring(char **store, const json_t *val, const char *res)
{
	bool ret = _json_get_string(store, json_object_get(val, res), res);

	if (!ret) {
		LOGEMERG("Invalid config string or missing object for %s", res);
		exit(1);
	}
}

bool json_get_int64(int64_t *store, const json_t *val, const char *res)
{
	json_t *entry = json_object_get(val, res);
	bool ret = false;

	if (!entry) {
		LOGDEBUG("Json did not find entry %s", res);
		goto out;
	}
	if (!json_is_integer(entry)) {
		LOGINFO("Json entry %s is not an integer", res);
		goto out;
	}
	*store = json_integer_value(entry);
	LOGDEBUG("Json found entry %s: %"PRId64, res, *store);
	ret = true;
out:
	return ret;
}

bool json_get_int(int *store, const json_t *val, const char *res)
{
	json_t *entry = json_object_get(val, res);
	bool ret = false;

	if (!entry) {
		LOGDEBUG("Json did not find entry %s", res);
		goto out;
	}
	if (!json_is_integer(entry)) {
		LOGWARNING("Json entry %s is not an integer", res);
		goto out;
	}
	*store = json_integer_value(entry);
	LOGDEBUG("Json found entry %s: %d", res, *store);
	ret = true;
out:
	return ret;
}

bool json_get_double(double *store, const json_t *val, const char *res)
{
	json_t *entry = json_object_get(val, res);
	bool ret = false;

	if (!entry) {
		LOGDEBUG("Json did not find entry %s", res);
		goto out;
	}
	if (!json_is_real(entry)) {
		LOGWARNING("Json entry %s is not a double", res);
		goto out;
	}
	*store = json_real_value(entry);
	LOGDEBUG("Json found entry %s: %f", res, *store);
	ret = true;
out:
	return ret;
}

bool json_get_uint32(uint32_t *store, const json_t *val, const char *res)
{
	json_t *entry = json_object_get(val, res);
	bool ret = false;

	if (!entry) {
		LOGDEBUG("Json did not find entry %s", res);
		goto out;
	}
	if (!json_is_integer(entry)) {
		LOGWARNING("Json entry %s is not an integer", res);
		goto out;
	}
	*store = json_integer_value(entry);
	LOGDEBUG("Json found entry %s: %u", res, *store);
	ret = true;
out:
	return ret;
}

bool json_get_bool(bool *store, const json_t *val, const char *res)
{
	json_t *entry = json_object_get(val, res);
	bool ret = false;

	if (!entry) {
		LOGDEBUG("Json did not find entry %s", res);
		goto out;
	}
	if (!json_is_boolean(entry)) {
		LOGINFO("Json entry %s is not a boolean", res);
		goto out;
	}
	*store = json_is_true(entry);
	LOGDEBUG("Json found entry %s: %s", res, *store ? "true" : "false");
	ret = true;
out:
	return ret;
}

bool json_getdel_int(int *store, json_t *val, const char *res)
{
	bool ret;

	ret = json_get_int(store, val, res);
	if (ret)
		json_object_del(val, res);
	return ret;
}

bool json_getdel_int64(int64_t *store, json_t *val, const char *res)
{
	bool ret;

	ret = json_get_int64(store, val, res);
	if (ret)
		json_object_del(val, res);
	return ret;
}

static void parse_btcds(ckpool_t *ckp, const json_t *arr_val, const int arr_size)
{
	json_t *val;
	int i;

	ckp->btcds = arr_size;
	ckp->btcdurl = ckzalloc(sizeof(char *) * arr_size);
	ckp->btcdauth = ckzalloc(sizeof(char *) * arr_size);
	ckp->btcdpass = ckzalloc(sizeof(char *) * arr_size);
	ckp->btcdnotify = ckzalloc(sizeof(bool *) * arr_size);
	for (i = 0; i < arr_size; i++) {
		val = json_array_get(arr_val, i);
		json_get_configstring(&ckp->btcdurl[i], val, "url");
		json_get_configstring(&ckp->btcdauth[i], val, "auth");
		json_get_configstring(&ckp->btcdpass[i], val, "pass");
		json_get_bool(&ckp->btcdnotify[i], val, "notify");
	}
}

static void parse_proxies(ckpool_t *ckp, const json_t *arr_val, const int arr_size)
{
	json_t *val;
	int i;

	ckp->proxies = arr_size;
	ckp->proxyurl = ckzalloc(sizeof(char *) * arr_size);
	ckp->proxyauth = ckzalloc(sizeof(char *) * arr_size);
	ckp->proxypass = ckzalloc(sizeof(char *) * arr_size);
	for (i = 0; i < arr_size; i++) {
		val = json_array_get(arr_val, i);
		json_get_configstring(&ckp->proxyurl[i], val, "url");
		json_get_configstring(&ckp->proxyauth[i], val, "auth");
		if (!json_get_string(&ckp->proxypass[i], val, "pass"))
			ckp->proxypass[i] = strdup("");
	}
}

static bool parse_serverurls(ckpool_t *ckp, const json_t *arr_val)
{
	bool ret = false;
	int arr_size, i;

	if (!arr_val)
		goto out;
	if (!json_is_array(arr_val)) {
		LOGINFO("Unable to parse serverurl entries as an array");
		goto out;
	}
	arr_size = json_array_size(arr_val);
	if (!arr_size) {
		LOGWARNING("Serverurl array empty");
		goto out;
	}
	ckp->serverurls = arr_size;
	ckp->serverurl = ckalloc(sizeof(char *) * arr_size);
	ckp->server_highdiff = ckzalloc(sizeof(bool) * arr_size);
	ckp->nodeserver = ckzalloc(sizeof(bool) * arr_size);
	ckp->trusted = ckzalloc(sizeof(bool) * arr_size);
	for (i = 0; i < arr_size; i++) {
		json_t *val = json_array_get(arr_val, i);

		if (!_json_get_string(&ckp->serverurl[i], val, "serverurl"))
			LOGWARNING("Invalid serverurl entry number %d", i);
	}
	ret = true;
out:
	return ret;
}

static void parse_nodeservers(ckpool_t *ckp, const json_t *arr_val)
{
	int arr_size, i, j, total_urls;

	if (!arr_val)
		return;
	if (!json_is_array(arr_val)) {
		LOGWARNING("Unable to parse nodeservers entries as an array");
		return;
	}
	arr_size = json_array_size(arr_val);
	if (!arr_size) {
		LOGWARNING("Nodeserver array empty");
		return;
	}
	total_urls = ckp->serverurls + arr_size;
	ckp->serverurl = realloc(ckp->serverurl, sizeof(char *) * total_urls);
	ckp->nodeserver = realloc(ckp->nodeserver, sizeof(bool) * total_urls);
	ckp->trusted = realloc(ckp->trusted, sizeof(bool) * total_urls);
	for (i = 0, j = ckp->serverurls; j < total_urls; i++, j++) {
		json_t *val = json_array_get(arr_val, i);

		if (!_json_get_string(&ckp->serverurl[j], val, "nodeserver"))
			LOGWARNING("Invalid nodeserver entry number %d", i);
		ckp->nodeserver[j] = true;
		ckp->nodeservers++;
	}
	ckp->serverurls = total_urls;
}

static void parse_trusted(ckpool_t *ckp, const json_t *arr_val)
{
	int arr_size, i, j, total_urls;

	if (!arr_val)
		return;
	if (!json_is_array(arr_val)) {
		LOGWARNING("Unable to parse trusted server entries as an array");
		return;
	}
	arr_size = json_array_size(arr_val);
	if (!arr_size) {
		LOGWARNING("Trusted array empty");
		return;
	}
	total_urls = ckp->serverurls + arr_size;
	ckp->serverurl = realloc(ckp->serverurl, sizeof(char *) * total_urls);
	ckp->nodeserver = realloc(ckp->nodeserver, sizeof(bool) * total_urls);
	ckp->trusted = realloc(ckp->trusted, sizeof(bool) * total_urls);
	for (i = 0, j = ckp->serverurls; j < total_urls; i++, j++) {
		json_t *val = json_array_get(arr_val, i);

		if (!_json_get_string(&ckp->serverurl[j], val, "trusted"))
			LOGWARNING("Invalid trusted server entry number %d", i);
		ckp->trusted[j] = true;
	}
	ckp->serverurls = total_urls;
}


static bool parse_redirecturls(ckpool_t *ckp, const json_t *arr_val)
{
	bool ret = false;
	int arr_size, i;
	char *redirecturl, url[INET6_ADDRSTRLEN], port[8];
	redirecturl = alloca(INET6_ADDRSTRLEN);

	if (!arr_val)
		goto out;
	if (!json_is_array(arr_val)) {
		LOGNOTICE("Unable to parse redirecturl entries as an array");
		goto out;
	}
	arr_size = json_array_size(arr_val);
	if (!arr_size) {
		LOGWARNING("redirecturl array empty");
		goto out;
	}
	ckp->redirecturls = arr_size;
	ckp->redirecturl = ckalloc(sizeof(char *) * arr_size);
	ckp->redirectport = ckalloc(sizeof(char *) * arr_size);
	for (i = 0; i < arr_size; i++) {
		json_t *val = json_array_get(arr_val, i);

		strncpy(redirecturl, json_string_value(val), INET6_ADDRSTRLEN - 1);
		/* See that the url properly resolves */
		if (!url_from_serverurl(redirecturl, url, port))
			quit(1, "Invalid redirecturl entry %d %s", i, redirecturl);
		ckp->redirecturl[i] = strdup(strsep(&redirecturl, ":"));
		ckp->redirectport[i] = strdup(port);
	}
	ret = true;
out:
	return ret;
}


static void parse_config(ckpool_t *ckp)
{
	json_t *json_conf, *arr_val;
	json_error_t err_val;
	char *url, *vmask;
	int arr_size;

	json_conf = json_load_file(ckp->config, JSON_DISABLE_EOF_CHECK, &err_val);
	if (!json_conf) {
		LOGWARNING("Json decode error for config file %s: (%d): %s", ckp->config,
			   err_val.line, err_val.text);
		return;
	}
	arr_val = json_object_get(json_conf, "btcd");
	if (arr_val && json_is_array(arr_val)) {
		arr_size = json_array_size(arr_val);
		if (arr_size)
			parse_btcds(ckp, arr_val, arr_size);
	}
	json_get_string(&ckp->btcaddress, json_conf, "btcaddress");
	json_get_string(&ckp->btcsig, json_conf, "btcsig");
	if (ckp->btcsig && strlen(ckp->btcsig) > 38) {
		LOGWARNING("Signature %s too long, truncating to 38 bytes", ckp->btcsig);
		ckp->btcsig[38] = '\0';
	}
	json_get_int(&ckp->blockpoll, json_conf, "blockpoll");
	json_get_int(&ckp->nonce1length, json_conf, "nonce1length");
	json_get_int(&ckp->nonce2length, json_conf, "nonce2length");
	json_get_int(&ckp->update_interval, json_conf, "update_interval");
	json_get_string(&vmask, json_conf, "version_mask");
	if (vmask && strlen(vmask) && validhex(vmask))
		sscanf(vmask, "%x", &ckp->version_mask);
	else
		ckp->version_mask = 0x1fffe000;
	/* Look for an array first and then a single entry */
	arr_val = json_object_get(json_conf, "serverurl");
	if (!parse_serverurls(ckp, arr_val)) {
		if (json_get_string(&url, json_conf, "serverurl")) {
			ckp->serverurl = ckalloc(sizeof(char *));
			ckp->serverurl[0] = url;
			ckp->serverurls = 1;
		}
	}
	arr_val = json_object_get(json_conf, "nodeserver");
	parse_nodeservers(ckp, arr_val);
	arr_val = json_object_get(json_conf, "trusted");
	parse_trusted(ckp, arr_val);
	json_get_string(&ckp->upstream, json_conf, "upstream");
	json_get_int64(&ckp->mindiff, json_conf, "mindiff");
	json_get_int64(&ckp->startdiff, json_conf, "startdiff");
	json_get_int64(&ckp->highdiff, json_conf, "highdiff");
	json_get_int64(&ckp->maxdiff, json_conf, "maxdiff");
	json_get_string(&ckp->logdir, json_conf, "logdir");
	json_get_int(&ckp->maxclients, json_conf, "maxclients");
	json_get_double(&ckp->donation, json_conf, "donation");
	/* Avoid dust-sized donations */
	if (ckp->donation < 0.1)
		ckp->donation = 0;
	else if (ckp->donation > 99.9)
		ckp->donation = 99.9;
	arr_val = json_object_get(json_conf, "proxy");
	if (arr_val && json_is_array(arr_val)) {
		arr_size = json_array_size(arr_val);
		if (arr_size)
			parse_proxies(ckp, arr_val, arr_size);
	}
	arr_val = json_object_get(json_conf, "redirecturl");
	if (arr_val)
		parse_redirecturls(ckp, arr_val);
	json_get_string(&ckp->zmqblock, json_conf, "zmqblock");

	json_decref(json_conf);
}

static void manage_old_instance(ckpool_t *ckp, proc_instance_t *pi)
{
	struct stat statbuf;
	char path[256];
	FILE *fp;

	sprintf(path, "%s%s.pid", pi->ckp->socket_dir, pi->processname);
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
			if (ckp->handover) {
				LOGINFO("Saving pid to be handled at handover");
				pi->oldpid = oldpid;
				return;
			}
			terminate_oldpid(ckp, pi, oldpid);
		}
	}
}

static void prepare_child(ckpool_t *ckp, proc_instance_t *pi, void *process, char *name)
{
	pi->ckp = ckp;
	pi->processname = name;
	pi->sockname = pi->processname;
	create_process_unixsock(pi);
	create_pthread(&pi->pth_process, process, pi);
	create_unix_receiver(pi);
}

static struct option long_options[] = {
	{"btcsolo",	no_argument,		0,	'B'},
	{"config",	required_argument,	0,	'c'},
	{"daemonise",	no_argument,		0,	'D'},
	{"group",	required_argument,	0,	'g'},
	{"handover",	no_argument,		0,	'H'},
	{"help",	no_argument,		0,	'h'},
	{"killold",	no_argument,		0,	'k'},
	{"log-shares",	no_argument,		0,	'L'},
	{"loglevel",	required_argument,	0,	'l'},
	{"name",	required_argument,	0,	'n'},
	{"node",	no_argument,		0,	'N'},
	{"passthrough",	no_argument,		0,	'P'},
	{"proxy",	no_argument,		0,	'p'},
	{"quiet",	no_argument,		0,	'q'},
	{"redirector",	no_argument,		0,	'R'},
	{"sockdir",	required_argument,	0,	's'},
	{"trusted",	no_argument,		0,	't'},
	{"userproxy",	no_argument,		0,	'u'},
	{0, 0, 0, 0}
};

static bool send_recv_path(const char *path, const char *msg)
{
	int sockd = open_unix_client(path);
	bool ret = false;
	char *response;

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

int main(int argc, char **argv)
{
	struct sigaction handler;
	int c, ret, i = 0, j;
	char buf[512] = {};
	char *appname;
	ckpool_t ckp;

	/* Make significant floating point errors fatal to avoid subtle bugs being missed */
	feenableexcept(FE_DIVBYZERO | FE_INVALID);
	json_set_alloc_funcs(json_ckalloc, free);

	global_ckp = &ckp;
	memset(&ckp, 0, sizeof(ckp));
	ckp.starttime = time(NULL);
	ckp.startpid = getpid();
	ckp.loglevel = LOG_NOTICE;
	ckp.initial_args = ckalloc(sizeof(char *) * (argc + 2)); /* Leave room for extra -H */
	for (ckp.args = 0; ckp.args < argc; ckp.args++)
		ckp.initial_args[ckp.args] = strdup(argv[ckp.args]);
	ckp.initial_args[ckp.args] = NULL;

	appname = basename(argv[0]);
	if (!strcmp(appname, "ckproxy"))
		ckp.proxy = true;

	while ((c = getopt_long(argc, argv, "Bc:Dd:g:HhkLl:Nn:PpqRS:s:tu", long_options, &i)) != -1) {
		switch (c) {
			case 'B':
				if (ckp.proxy)
					quit(1, "Cannot set both proxy and btcsolo mode");
				ckp.btcsolo = true;
				break;
			case 'c':
				ckp.config = optarg;
				break;
			case 'D':
				ckp.daemon = true;
				break;
			case 'g':
				ckp.grpnam = optarg;
				break;
			case 'H':
				ckp.handover = true;
				ckp.killold = true;
				break;
			case 'h':
				for (j = 0; long_options[j].val; j++) {
					struct option *jopt = &long_options[j];

					if (jopt->has_arg) {
						char *upper = alloca(strlen(jopt->name) + 1);
						int offset = 0;

						do {
							upper[offset] = toupper(jopt->name[offset]);
						} while (upper[offset++] != '\0');
						printf("-%c %s | --%s %s\n", jopt->val,
						       upper, jopt->name, upper);
					} else
						printf("-%c | --%s\n", jopt->val, jopt->name);
				}
				exit(0);
			case 'k':
				ckp.killold = true;
				break;
			case 'L':
				ckp.logshares = true;
				break;
			case 'l':
				ckp.loglevel = atoi(optarg);
				if (ckp.loglevel < LOG_EMERG || ckp.loglevel > LOG_DEBUG) {
					quit(1, "Invalid loglevel (range %d - %d): %d",
					     LOG_EMERG, LOG_DEBUG, ckp.loglevel);
				}
				break;
			case 'N':
				if (ckp.proxy || ckp.redirector || ckp.userproxy || ckp.passthrough)
					quit(1, "Cannot set another proxy type or redirector and node mode");
				ckp.proxy = ckp.passthrough = ckp.node = true;
				break;
			case 'n':
				ckp.name = optarg;
				break;
			case 'P':
				if (ckp.proxy || ckp.redirector || ckp.userproxy || ckp.node)
					quit(1, "Cannot set another proxy type or redirector and passthrough mode");
				ckp.proxy = ckp.passthrough = true;
				break;
			case 'p':
				if (ckp.passthrough || ckp.redirector || ckp.userproxy || ckp.node)
					quit(1, "Cannot set another proxy type or redirector and proxy mode");
				ckp.proxy = true;
				break;
			case 'q':
				ckp.quiet = true;
				break;
			case 'R':
				if (ckp.proxy || ckp.passthrough || ckp.userproxy || ckp.node)
					quit(1, "Cannot set a proxy type or passthrough and redirector modes");
				ckp.proxy = ckp.passthrough = ckp.redirector = true;
				break;
			case 's':
				ckp.socket_dir = strdup(optarg);
				break;
			case 't':
				if (ckp.proxy)
					quit(1, "Cannot set a proxy type and trusted remote mode");
				ckp.remote = true;
				break;
			case 'u':
				if (ckp.proxy || ckp.redirector || ckp.passthrough || ckp.node)
					quit(1, "Cannot set both userproxy and another proxy type or redirector");
				ckp.userproxy = ckp.proxy = true;
				break;
		}
	}

	if (!ckp.name) {
		if (ckp.node)
			ckp.name = "cknode";
		else if (ckp.redirector)
			ckp.name = "ckredirector";
		else if (ckp.passthrough)
			ckp.name = "ckpassthrough";
		else if (ckp.proxy)
			ckp.name = "ckproxy";
		else
			ckp.name = "ckpool";
	}
	snprintf(buf, 15, "%s", ckp.name);
	prctl(PR_SET_NAME, buf, 0, 0, 0);
	memset(buf, 0, 15);

	if (ckp.grpnam) {
		struct group *group = getgrnam(ckp.grpnam);

		if (!group)
			quit(1, "Failed to find group %s", ckp.grpnam);
		ckp.gr_gid = group->gr_gid;
	} else
		ckp.gr_gid = getegid();

	if (!ckp.config) {
		ckp.config = strdup(ckp.name);
		realloc_strcat(&ckp.config, ".conf");
	}
	if (!ckp.socket_dir) {
		ckp.socket_dir = strdup("/tmp/");
		realloc_strcat(&ckp.socket_dir, ckp.name);
	}
	trail_slash(&ckp.socket_dir);

	/* Ignore sigpipe */
	signal(SIGPIPE, SIG_IGN);

	ret = mkdir(ckp.socket_dir, 0750);
	if (ret && errno != EEXIST)
		quit(1, "Failed to make directory %s", ckp.socket_dir);

	parse_config(&ckp);
	/* Set defaults if not found in config file */
	if (!ckp.btcds) {
		ckp.btcds = 1;
		ckp.btcdurl = ckzalloc(sizeof(char *));
		ckp.btcdauth = ckzalloc(sizeof(char *));
		ckp.btcdpass = ckzalloc(sizeof(char *));
		ckp.btcdnotify = ckzalloc(sizeof(bool));
	}
	for (i = 0; i < ckp.btcds; i++) {
		if (!ckp.btcdurl[i])
			ckp.btcdurl[i] = strdup("localhost:8332");
		if (!ckp.btcdauth[i])
			ckp.btcdauth[i] = strdup("user");
		if (!ckp.btcdpass[i])
			ckp.btcdpass[i] = strdup("pass");
	}

	ckp.donaddress = "bc1q28kkr5hk4gnqe3evma6runjrd2pvqyp8fpwfzu";

	/* Donations on testnet are meaningless but required for complete
	 * testing. Testnet and regtest addresses */
	ckp.tndonaddress = "tb1q5fyv7tue73y4zxezh2c685qpwx0cfngfxlrgxh";
	ckp.rtdonaddress = "bcrt1qlk935ze2fsu86zjp395uvtegztrkaezawxx0wf";

	if (!ckp.btcaddress && !ckp.btcsolo && !ckp.proxy)
		quit(0, "Non solo mining must have a btcaddress in config, aborting!");
	if (!ckp.blockpoll)
		ckp.blockpoll = 100;
	if (!ckp.nonce1length)
		ckp.nonce1length = 4;
	else if (ckp.nonce1length < 2 || ckp.nonce1length > 8)
		quit(0, "Invalid nonce1length %d specified, must be 2~8", ckp.nonce1length);
	if (!ckp.nonce2length) {
		/* nonce2length is zero by default in proxy mode */
		if (!ckp.proxy)
			ckp.nonce2length = 8;
	} else if (ckp.nonce2length < 2 || ckp.nonce2length > 8)
		quit(0, "Invalid nonce2length %d specified, must be 2~8", ckp.nonce2length);
	if (!ckp.update_interval)
		ckp.update_interval = 30;
	if (!ckp.mindiff)
		ckp.mindiff = 1;
	if (!ckp.startdiff)
		ckp.startdiff = 42;
	if (!ckp.highdiff)
		ckp.highdiff = 1000000;
	if (!ckp.logdir)
		ckp.logdir = strdup("logs");
	if (!ckp.serverurls)
		ckp.serverurl = ckzalloc(sizeof(char *));
	if (ckp.proxy && !ckp.proxies)
		quit(0, "No proxy entries found in config file %s", ckp.config);
	if (ckp.redirector && !ckp.redirecturls)
		quit(0, "No redirect entries found in config file %s", ckp.config);
	if (!ckp.zmqblock)
		ckp.zmqblock = "tcp://127.0.0.1:28332";

	/* Create the log directory */
	trail_slash(&ckp.logdir);
	ret = mkdir(ckp.logdir, 0750);
	if (ret && errno != EEXIST)
		quit(1, "Failed to make log directory %s", ckp.logdir);

	/* Create the workers logdir */
	sprintf(buf, "%s/workers", ckp.logdir);
	ret = mkdir(buf, 0750);
	if (ret && errno != EEXIST)
		quit(1, "Failed to make workers log directory %s", buf);

	/* Create the user logdir */
	sprintf(buf, "%s/users", ckp.logdir);
	ret = mkdir(buf, 0750);
	if (ret && errno != EEXIST)
		quit(1, "Failed to make user log directory %s", buf);

	/* Create the pool logdir */
	sprintf(buf, "%s/pool", ckp.logdir);
	ret = mkdir(buf, 0750);
	if (ret && errno != EEXIST)
		quit(1, "Failed to make pool log directory %s", buf);

	/* Create the logfile */
	ASPRINTF(&ckp.logfilename, "%s%s.log", ckp.logdir, ckp.name);
	if (!open_logfile(&ckp))
		quit(1, "Failed to make open log file %s", buf);
	launch_logger(&ckp);

	ckp.main.ckp = &ckp;
	ckp.main.processname = strdup("main");
	ckp.main.sockname = strdup("listener");
	name_process_sockname(&ckp.main.us, &ckp.main);
	ckp.oldconnfd = ckzalloc(sizeof(int *) * ckp.serverurls);
	manage_old_instance(&ckp, &ckp.main);
	if (ckp.handover) {
		const char *path = ckp.main.us.path;

		if (send_recv_path(path, "ping")) {
			for (i = 0; i < ckp.serverurls; i++) {
				char oldurl[INET6_ADDRSTRLEN], oldport[8];
				char getfd[16];
				int sockd;

				snprintf(getfd, 15, "getxfd%d", i);
				sockd = open_unix_client(path);
				if (sockd < 1)
					break;
				if (!send_unix_msg(sockd, getfd))
					break;
				ckp.oldconnfd[i] = get_fd(sockd);
				Close(sockd);
				sockd = ckp.oldconnfd[i];
				if (!sockd)
					break;
				if (url_from_socket(sockd, oldurl, oldport)) {
					LOGWARNING("Inherited old server socket %d url %s:%s !",
						   i, oldurl, oldport);
				} else {
					LOGWARNING("Inherited old server socket %d with new file descriptor %d!",
						   i, ckp.oldconnfd[i]);
				}
			}
			send_recv_path(path, "reject");
			send_recv_path(path, "reconnect");
			send_recv_path(path, "shutdown");
		}
	}

	if (ckp.daemon) {
		int fd;

		if (fork())
			exit(0);
		setsid();
		fd = open("/dev/null",O_RDWR, 0);
		if (fd != -1) {
			dup2(fd, STDIN_FILENO);
			dup2(fd, STDOUT_FILENO);
			dup2(fd, STDERR_FILENO);
		}
	}

	write_namepid(&ckp.main);
	open_process_sock(&ckp, &ckp.main, &ckp.main.us);

	ret = sysconf(_SC_OPEN_MAX);
	if (ckp.maxclients > ret * 9 / 10) {
		LOGWARNING("Cannot set maxclients to %d due to max open file limit of %d, reducing to %d",
			   ckp.maxclients, ret, ret * 9 / 10);
		ckp.maxclients = ret * 9 / 10;
	} else if (!ckp.maxclients) {
		LOGNOTICE("Setting maxclients to %d due to max open file limit of %d",
			  ret * 9 / 10, ret);
		ckp.maxclients = ret * 9 / 10;
	}

	// ckp.ckpapi = create_ckmsgq(&ckp, "api", &ckpool_api);
	create_pthread(&ckp.pth_listener, listener, &ckp.main);

	handler.sa_handler = &sighandler;
	handler.sa_flags = 0;
	sigemptyset(&handler.sa_mask);
	sigaction(SIGTERM, &handler, NULL);
	sigaction(SIGINT, &handler, NULL);

	/* Launch separate processes from here */
	prepare_child(&ckp, &ckp.generator, generator, "generator");
	prepare_child(&ckp, &ckp.stratifier, stratifier, "stratifier");
	prepare_child(&ckp, &ckp.connector, connector, "connector");

	/* Shutdown from here if the listener is sent a shutdown message */
	if (ckp.pth_listener)
		join_pthread(ckp.pth_listener);

	clean_up(&ckp);

	return 0;
}
