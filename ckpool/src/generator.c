/*
 * Copyright 2014-2017,2023 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#include "config.h"

#include <sys/epoll.h>
#include <sys/socket.h>
#include <jansson.h>
#include <string.h>
#include <unistd.h>

#include "ckpool.h"
#include "libckpool.h"
#include "generator.h"
#include "stratifier.h"
#include "bitcoin.h"
#include "uthash.h"
#include "utlist.h"

struct notify_instance {
	/* Hash table data */
	UT_hash_handle hh;
	int64_t id64;

	char prevhash[68];
	json_t *jobid;
	char *coinbase1;
	char *coinbase2;
	int coinb1len;
	int merkles;
	char merklehash[16][68];
	char nbit[12];
	char ntime[12];
	char bbversion[12];
	bool clean;

	time_t notify_time;
};

typedef struct notify_instance notify_instance_t;

typedef struct proxy_instance proxy_instance_t;

struct share_msg {
	UT_hash_handle hh;
	int64_t id64; // Our own id for submitting upstream

	int64_t client_id;
	time_t submit_time;
	double diff;
};

typedef struct share_msg share_msg_t;

struct stratum_msg {
	struct stratum_msg *next;
	struct stratum_msg *prev;

	json_t *json_msg;
	int64_t client_id;
};

typedef struct stratum_msg stratum_msg_t;

struct pass_msg {
	proxy_instance_t *proxy;
	connsock_t *cs;
	char *msg;
};

typedef struct pass_msg pass_msg_t;
typedef struct cs_msg cs_msg_t;

/* Statuses of various proxy states - connect, subscribe and auth */
enum proxy_stat {
	STATUS_INIT = 0,
	STATUS_SUCCESS,
	STATUS_FAIL
};

static const char *proxy_status[] = {
	"Initial",
	"Success",
	"Failed"
};

/* Per proxied pool instance data */
struct proxy_instance {
	UT_hash_handle hh; /* Proxy list */
	UT_hash_handle sh; /* Subproxy list */
	proxy_instance_t *next; /* For dead proxy list */
	proxy_instance_t *prev; /* For dead proxy list */

	ckpool_t *ckp;
	connsock_t cs;
	bool passthrough;
	bool node;
	int id; /* Proxy server id*/
	int subid; /* Subproxy id */
	int userid; /* User id if this proxy is bound to a user */

	char *baseurl;
	char *url;
	char *auth;
	char *pass;

	char *enonce1;
	char *enonce1bin;
	int nonce1len;
	int nonce2len;

	tv_t last_message;

	double diff;
	double diff_accepted;
	double diff_rejected;
	double total_accepted; /* Used only by parent proxy structures */
	double total_rejected; /* "" */
	tv_t last_share;

	/* Diff shares per second for 1/5/60... minute rolling averages */
	double dsps1;
	double dsps5;
	double dsps60;
	double dsps360;
	double dsps1440;
	tv_t last_decay;

	/* Total diff shares per second for all subproxies */
	double tdsps1; /* Used only by parent proxy structures */
	double tdsps5; /* "" */
	double tdsps60; /* "" */
	double tdsps360; /* "" */
	double tdsps1440; /* "" */
	tv_t total_last_decay;

	bool no_params; /* Doesn't want any parameters on subscribe */

	bool global;	/* Part of the global list of proxies */
	bool disabled; /* Subproxy no longer to be used */
	bool reconnect; /* We need to drop and reconnect */
	bool reconnecting; /* Testing of parent in progress */
	int64_t recruit; /* No of recruiting requests in progress */
	bool alive;
	bool authorised;

	/* Which of STATUS_* states are these in */
	enum proxy_stat connect_status;
	enum proxy_stat subscribe_status;
	enum proxy_stat auth_status;

	/* Back off from retrying if we fail one of the above */
	int backoff;

	 /* Are we in the middle of a blocked write of this message? */
	cs_msg_t *sending;

	pthread_t pth_precv;

	ckmsgq_t *passsends;	// passthrough sends

	char_entry_t *recvd_lines; /* Linked list of unprocessed messages */

	int epfd; /* Epoll fd used by the parent proxy */

	mutex_t proxy_lock; /* Lock protecting hashlist of proxies */
	proxy_instance_t *parent; /* Parent proxy of subproxies */
	proxy_instance_t *subproxies; /* Hashlist of subproxies of this proxy */
	int64_t clients_per_proxy; /* Max number of clients of this proxy */
	int subproxy_count; /* Number of subproxies */
};

/* Private data for the generator */
struct generator_data {
	ckpool_t *ckp;
	mutex_t lock; /* Lock protecting linked lists */
	proxy_instance_t *proxies; /* Hash list of all proxies */
	proxy_instance_t *dead_proxies; /* Disabled proxies */
	int proxies_generated;
	int subproxies_generated;

	int64_t proxy_notify_id;	// Globally increasing notify id
	pthread_t pth_uprecv;	// User proxy receive thread
	pthread_t pth_psend;	// Combined proxy send thread

	mutex_t psend_lock;	// Lock associated with conditional below
	pthread_cond_t psend_cond;

	stratum_msg_t *psends;
	int psends_generated;

	mutex_t notify_lock;
	notify_instance_t *notify_instances;

	mutex_t share_lock;
	share_msg_t *shares;
	int64_t share_id;

	server_instance_t *current_si; // Current server instance

	proxy_instance_t *current_proxy;
};

typedef struct generator_data gdata_t;

/* Use a temporary fd when testing server_alive to avoid races on cs->fd */
static bool server_alive(ckpool_t *ckp, server_instance_t *si, bool pinging)
{
	char *userpass = NULL;
	bool ret = false;
	connsock_t *cs;
	gbtbase_t gbt;
	int fd;

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
			LOGINFO("Failed to get test block template from %s:%s!",
				cs->url, cs->port);
		}
		goto out;
	}
	clear_gbtbase(&gbt);
	if (unlikely(ckp->btcsolo && !ckp->btcaddress)) {
		/* If no btcaddress is specified in solobtc mode, choose one of
		 * the donation addresses from mainnet, testnet, or regtest for
		 * coinbase validation later on, although it will not be used
		 * for mining. */
		if (validate_address(cs, ckp->donaddress, &ckp->script, &ckp->segwit))
			ckp->btcaddress = ckp->donaddress;
		else if (validate_address(cs, ckp->tndonaddress, &ckp->script, &ckp->segwit))
			ckp->btcaddress = ckp->tndonaddress;
		else if (validate_address(cs, ckp->rtdonaddress, &ckp->script, &ckp->segwit))
			ckp->btcaddress = ckp->rtdonaddress;
	}

	if (!ckp->node && !validate_address(cs, ckp->btcaddress, &ckp->script, &ckp->segwit)) {
		LOGWARNING("Invalid btcaddress: %s !", ckp->btcaddress);
		goto out;
	}
	si->alive = cs->alive = ret = true;
	LOGNOTICE("Server alive: %s:%s", cs->url, cs->port);
out:
	/* Close the file handle */
	close(fd);
	return ret;
}

/* Find the highest priority server alive and return it */
static server_instance_t *live_server(ckpool_t *ckp, gdata_t *gdata)
{
	server_instance_t *alive = NULL;
	connsock_t *cs;
	int i;

	LOGDEBUG("Attempting to connect to bitcoind");
retry:
	/* First find a server that is already flagged alive if possible
	 * without blocking on server_alive() */
	for (i = 0; i < ckp->btcds; i++) {
		server_instance_t *si = ckp->servers[i];
		cs = &si->cs;

		if (si->alive) {
			alive = si;
			goto living;
		}
	}

	/* No servers flagged alive, try to connect to them blocking */
	for (i = 0; i < ckp->btcds; i++) {
		server_instance_t *si = ckp->servers[i];

		if (server_alive(ckp, si, false)) {
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
	send_proc(ckp->connector, alive ? "accept" : "reject");
	return alive;
}

static void kill_server(server_instance_t *si)
{
	connsock_t *cs;

	if (!si) // This shouldn't happen
		return;

	LOGNOTICE("Killing server");
	cs = &si->cs;
	Close(cs->fd);
	empty_buffer(cs);
	dealloc(cs->url);
	dealloc(cs->port);
	dealloc(cs->auth);
}

static void clear_unix_msg(unix_msg_t **umsg)
{
	if (*umsg) {
		Close((*umsg)->sockd);
		free((*umsg)->buf);
		free(*umsg);
		*umsg = NULL;
	}
}

bool generator_submitblock(ckpool_t *ckp, const char *buf)
{
	gdata_t *gdata = ckp->gdata;
	server_instance_t *si;
	bool warn = false;
	connsock_t *cs;

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

void generator_preciousblock(ckpool_t *ckp, const char *hash)
{
	gdata_t *gdata = ckp->gdata;
	server_instance_t *si;
	connsock_t *cs;

	if (unlikely(!(si = gdata->current_si))) {
		LOGWARNING("No live current server in generator_get_blockhash");
		return;
	}
	cs = &si->cs;
	precious_block(cs, hash);
}

bool generator_get_blockhash(ckpool_t *ckp, int height, char *hash)
{
	gdata_t *gdata = ckp->gdata;
	server_instance_t *si;
	connsock_t *cs;

	if (unlikely(!(si = gdata->current_si))) {
		LOGWARNING("No live current server in generator_get_blockhash");
		return false;
	}
	cs = &si->cs;
	return get_blockhash(cs, height, hash);
}

static void gen_loop(proc_instance_t *pi)
{
	server_instance_t *si = NULL, *old_si;
	unix_msg_t *umsg = NULL;
	ckpool_t *ckp = pi->ckp;
	char *buf = NULL;
	connsock_t *cs;
	gbtbase_t gbt;
	char hash[68];

reconnect:
	clear_unix_msg(&umsg);
	old_si = si;
	si = live_server(ckp, ckp->gdata);
	if (!si)
		goto out;
	if (unlikely(!ckp->generator_ready)) {
		ckp->generator_ready = true;
		LOGWARNING("%s generator ready", ckp->name);
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
			LOGWARNING("Failed to get block template from %s:%s",
				   cs->url, cs->port);
			si->alive = cs->alive = false;
			send_unix_msg(umsg->sockd, "Failed");
			goto reconnect;
		} else {
			char *s = json_dumps(gbt.json, JSON_NO_UTF8);

			send_unix_msg(umsg->sockd, s);
			free(s);
			clear_gbtbase(&gbt);
		}
	} else if (cmdmatch(buf, "getbest")) {
		if (si->notify)
			send_unix_msg(umsg->sockd, "notify");
		else if (!get_bestblockhash(cs, hash)) {
			LOGINFO("No best block hash support from %s:%s",
				cs->url, cs->port);
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
			send_unix_msg(umsg->sockd,  "failed");
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

		LOGNOTICE("Submitting block data!");
		ret = submit_block(cs, buf + 12 + 64 + 1);
		memset(buf + 12 + 64, 0, 1);
		sprintf(blockmsg, "%sblock:%s", ret ? "" : "no", buf + 12);
		send_proc(ckp->stratifier, blockmsg);
	} else if (cmdmatch(buf, "reconnect")) {
		goto reconnect;
	} else if (cmdmatch(buf, "loglevel")) {
		sscanf(buf, "loglevel=%d", &ckp->loglevel);
	} else if (cmdmatch(buf, "ping")) {
		LOGDEBUG("Generator received ping request");
		send_unix_msg(umsg->sockd, "pong");
	}
	goto retry;

out:
	kill_server(si);
}

static bool connect_proxy(ckpool_t *ckp, connsock_t *cs, proxy_instance_t *proxy)
{
	if (cs->fd > 0) {
		epoll_ctl(proxy->epfd, EPOLL_CTL_DEL, cs->fd, NULL);
		Close(cs->fd);
	}
	cs->fd = connect_socket(cs->url, cs->port);
	if (cs->fd < 0) {
		LOGINFO("Failed to connect socket to %s:%s in connect_proxy",
			cs->url, cs->port);
		return false;
	}
	keep_sockalive(cs->fd);
	if (!ckp->passthrough) {
		struct epoll_event event;

		event.events = EPOLLIN | EPOLLRDHUP;
		event.data.ptr = proxy;
		/* Add this connsock_t to the epoll list */
		if (unlikely(epoll_ctl(proxy->epfd, EPOLL_CTL_ADD, cs->fd, &event) == -1)) {
			LOGERR("Failed to add fd %d to epfd %d to epoll_ctl in proxy_alive",
				cs->fd, proxy->epfd);
			return false;
		}
	} else {
		/* We want large send/recv buffers on passthroughs */
		if (!ckp->rmem_warn)
			cs->rcvbufsiz = set_recvbufsize(ckp, cs->fd, 1048576);
		if (!ckp->wmem_warn)
			cs->sendbufsiz = set_sendbufsize(ckp, cs->fd, 1048576);
	}
	return true;
}

/* For some reason notify is buried at various different array depths so use
 * a reentrant function to try and find it. */
static json_t *find_notify(json_t *val)
{
	int arr_size, i;
	json_t *ret = NULL;
	const char *entry;

	if (!json_is_array(val))
		return NULL;
	arr_size = json_array_size(val);
	entry = json_string_value(json_array_get(val, 0));
	if (cmdmatch(entry, "mining.notify"))
		return val;
	for (i = 0; i < arr_size; i++) {
		json_t *arr_val;

		arr_val = json_array_get(val, i);
		ret = find_notify(arr_val);
		if (ret)
			break;
	}
	return ret;
}

/* Get stored line in the proxy linked list of messages if any exist or NULL */
static char *cached_proxy_line(proxy_instance_t *proxi)
{
	char *buf = NULL;

	if (proxi->recvd_lines) {
		char_entry_t *char_t = proxi->recvd_lines;

		DL_DELETE(proxi->recvd_lines, char_t);
		buf = char_t->buf;
		free(char_t);
	}
	return buf;
}

/* Get next line in the proxy linked list of messages or a new line from the
 * connsock if there are none. */
static char *next_proxy_line(connsock_t *cs, proxy_instance_t *proxi)
{
	char *buf = cached_proxy_line(proxi);
	float timeout = 10;

	if (!buf && read_socket_line(cs, &timeout) > 0)
		buf = strdup(cs->buf);
	return buf;
}

/* For appending a line to the proxy recv list */
static void append_proxy_line(proxy_instance_t *proxi, const char *buf)
{
	char_entry_t *char_t = ckalloc(sizeof(char_entry_t));
	char_t->buf = strdup(buf);
	DL_APPEND(proxi->recvd_lines, char_t);
}

/* Get a new line from the connsock and return a copy of it */
static char *new_proxy_line(connsock_t *cs)
{
	float timeout = 10;
	char *buf = NULL;

	if (read_socket_line(cs, &timeout) < 1)
		goto out;
	buf = strdup(cs->buf);
out:
	return buf;
}

static inline bool parent_proxy(const proxy_instance_t *proxy)
{
	return (proxy->parent == proxy);
}

static void recruit_subproxies(proxy_instance_t *proxi, const int recruits);

static bool parse_subscribe(connsock_t *cs, proxy_instance_t *proxi)
{
	json_t *val = NULL, *res_val, *notify_val, *tmp;
	bool parsed, ret = false;
	int retries = 0, size;
	const char *string;
	char *buf, *old;

retry:
	parsed = true;
	if (!(buf = new_proxy_line(cs))) {
		LOGNOTICE("Proxy %d:%d %s failed to receive line in parse_subscribe",
			   proxi->id, proxi->subid, proxi->url);
		goto out;
	}
	LOGDEBUG("parse_subscribe received %s", buf);
	/* Ignore err_val here stored in &tmp */
	val = json_msg_result(buf, &res_val, &tmp);
	if (!val || !res_val) {
		LOGINFO("Failed to get a json result in parse_subscribe, got: %s", buf);
		parsed = false;
	}
	if (!json_is_array(res_val)) {
		LOGINFO("Result in parse_subscribe not an array");
		parsed = false;
	}
	size = json_array_size(res_val);
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
			goto retry;
		}
		LOGNOTICE("Proxy %d:%d %s failed to parse subscribe response in parse_subscribe",
			  proxi->id, proxi->subid, proxi->url);
		goto out;
	}

	tmp = json_array_get(res_val, 1);
	if (!tmp || !json_is_string(tmp)) {
		LOGWARNING("Failed to parse enonce1 in parse_subscribe");
		goto out;
	}
	string = json_string_value(tmp);
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
	tmp = json_array_get(res_val, 2);
	if (!tmp || !json_is_integer(tmp)) {
		LOGWARNING("Failed to parse nonce2len in parse_subscribe");
		goto out;
	}
	size = json_integer_value(tmp);
	if (size < 1 || size > 8) {
		LOGWARNING("Invalid nonce2len %d in parse_subscribe", size);
		goto out;
	}
	if (size < 3) {
		if (!proxi->subid) {
			LOGWARNING("Proxy %d %s Nonce2 length %d too small for fast miners",
				   proxi->id, proxi->url, size);
		} else {
			LOGNOTICE("Proxy %d:%d Nonce2 length %d too small for fast miners",
				   proxi->id, proxi->subid, size);
		}
	}
	proxi->nonce2len = size;
	proxi->clients_per_proxy = 1ll << ((size - 3) * 8);

	LOGNOTICE("Found notify for new proxy %d:%d with enonce %s nonce2len %d", proxi->id,
		proxi->subid, proxi->enonce1, proxi->nonce2len);
	ret = true;

out:
	if (val)
		json_decref(val);
	free(buf);
	return ret;
}

/* cs semaphore must be held */
static bool subscribe_stratum(ckpool_t *ckp, connsock_t *cs, proxy_instance_t *proxi)
{
	bool ret = false;
	json_t *req;

retry:
	/* Attempt to connect with the client description g*/
	if (!proxi->no_params) {
		JSON_CPACK(req, "{s:i,s:s,s:[s]}",
				"id", 0,
				"method", "mining.subscribe",
				"params", PACKAGE"/"VERSION);
	/* Then try without any parameters */
	} else {
		JSON_CPACK(req, "{s:i,s:s,s:[]}",
				"id", 0,
				"method", "mining.subscribe",
				"params");
	}
	ret = send_json_msg(cs, req);
	json_decref(req);
	if (!ret) {
		LOGNOTICE("Proxy %d:%d %s failed to send message in subscribe_stratum",
			   proxi->id, proxi->subid, proxi->url);
		goto out;
	}
	ret = parse_subscribe(cs, proxi);
	if (ret)
		goto out;

	if (proxi->no_params) {
		LOGNOTICE("Proxy %d:%d %s failed all subscription options in subscribe_stratum",
			   proxi->id, proxi->subid, proxi->url);
		goto out;
	}
	LOGINFO("Proxy %d:%d %s failed connecting with parameters in subscribe_stratum, retrying without",
		proxi->id, proxi->subid, proxi->url);
	proxi->no_params = true;
	ret = connect_proxy(ckp, cs, proxi);
	if (!ret) {
		LOGNOTICE("Proxy %d:%d %s failed to reconnect in subscribe_stratum",
			   proxi->id, proxi->subid, proxi->url);
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
static bool passthrough_stratum(connsock_t *cs, proxy_instance_t *proxi)
{
	json_t *req, *val = NULL, *res_val, *err_val;
	bool res, ret = false;
	float timeout = 10;

	JSON_CPACK(req, "{ss,s[s]}",
			"method", "mining.passthrough",
			"params", PACKAGE"/"VERSION);
	res = send_json_msg(cs, req);
	json_decref(req);
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
	val = json_msg_result(cs->buf, &res_val, &err_val);
	if (!val || !res_val) {
		LOGWARNING("Failed to get a json result in passthrough_stratum, got: %s",
			   cs->buf);
		goto out;
	}
	ret = json_is_true(res_val);
	if (!ret) {
		LOGWARNING("Denied passthrough for stratum");
		goto out;
	}
	proxi->passthrough = true;
out:
	if (val)
		json_decref(val);
	if (!ret)
		Close(cs->fd);
	return ret;
}

/* cs semaphore must be held */
static bool node_stratum(connsock_t *cs, proxy_instance_t *proxi)
{
	json_t *req, *val = NULL, *res_val, *err_val;
	bool res, ret = false;
	float timeout = 10;

	JSON_CPACK(req, "{ss,s[s]}",
			"method", "mining.node",
			"params", PACKAGE"/"VERSION);

	res = send_json_msg(cs, req);
	json_decref(req);
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
	val = json_msg_result(cs->buf, &res_val, &err_val);
	if (!val || !res_val) {
		LOGWARNING("Failed to get a json result in node_stratum, got: %s",
			   cs->buf);
		goto out;
	}
	ret = json_is_true(res_val);
	if (!ret) {
		LOGWARNING("Denied node setup for stratum");
		goto out;
	}
	proxi->node = true;
out:
	if (val)
		json_decref(val);
	if (!ret)
		Close(cs->fd);
	return ret;
}

static void send_notify(ckpool_t *ckp, proxy_instance_t *proxi, notify_instance_t *ni);

static void reconnect_generator(ckpool_t *ckp)
{
	send_proc(ckp->generator, "reconnect");
}

struct genwork *generator_getbase(ckpool_t *ckp)
{
	gdata_t *gdata = ckp->gdata;
	gbtbase_t *gbt = NULL;
	server_instance_t *si;
	connsock_t *cs;

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
		reconnect_generator(ckp);
		dealloc(gbt);
	}
out:
	return gbt;
}

int generator_getbest(ckpool_t *ckp, char *hash)
{
	gdata_t *gdata = ckp->gdata;
	int ret = GETBEST_FAILED;
	server_instance_t *si;
	connsock_t *cs;

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

bool generator_checkaddr(ckpool_t *ckp, const char *addr, bool *script, bool *segwit)
{
	gdata_t *gdata = ckp->gdata;
	server_instance_t *si;
	int ret = false;
	connsock_t *cs;

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

bool generator_checktxn(const ckpool_t *ckp, const char *txn, json_t **val)
{
	gdata_t *gdata = ckp->gdata;
	server_instance_t *si;
	bool ret = false;
	connsock_t *cs;

	si = gdata->current_si;
	if (unlikely(!si)) {
		LOGWARNING("No live current server in generator_checkaddr");
		goto out;
	}
	cs = &si->cs;
	*val = validate_txn(cs, txn);
	if (*val)
		ret = true;
out:
	return ret;
}

char *generator_get_txn(ckpool_t *ckp, const char *hash)
{
	gdata_t *gdata = ckp->gdata;
	server_instance_t *si;
	char *ret = NULL;
	connsock_t *cs;

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

static bool parse_notify(ckpool_t *ckp, proxy_instance_t *proxi, json_t *val)
{
	const char *prev_hash, *bbversion, *nbit, *ntime;
	gdata_t *gdata = proxi->ckp->gdata;
	char *coinbase1, *coinbase2;
	const char *jobidbuf;
	bool clean, ret = false;
	notify_instance_t *ni;
	json_t *arr, *job_id;
	int merkles, i;

	arr = json_array_get(val, 4);
	if (!arr || !json_is_array(arr))
		goto out;

	merkles = json_array_size(arr);
	job_id = json_copy(json_array_get(val, 0));
	prev_hash = __json_array_string(val, 1);
	coinbase1 = json_array_string(val, 2);
	coinbase2 = json_array_string(val, 3);
	bbversion = __json_array_string(val, 5);
	nbit = __json_array_string(val, 6);
	ntime = __json_array_string(val, 7);
	clean = json_is_true(json_array_get(val, 8));
	if (!job_id || !prev_hash || !coinbase1 || !coinbase2 || !bbversion || !nbit || !ntime) {
		if (job_id)
			json_decref(job_id);
		if (coinbase1)
			free(coinbase1);
		if (coinbase2)
			free(coinbase2);
		goto out;
	}

	LOGDEBUG("Received new notify from proxy %d:%d", proxi->id, proxi->subid);
	ni = ckzalloc(sizeof(notify_instance_t));
	ni->jobid = job_id;
	jobidbuf = json_string_value(job_id);
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
		const char *merkle = __json_array_string(arr, i);

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

	send_notify(ckp, proxi, ni);
out:
	return ret;
}

static bool parse_diff(proxy_instance_t *proxi, json_t *val)
{
	double diff = json_number_value(json_array_get(val, 0));

	if (diff == 0 || diff == proxi->diff)
		return true;
	proxi->diff = diff;
	return true;
}

static bool send_version(proxy_instance_t *proxi, json_t *val)
{
	json_t *json_msg, *id_val = json_object_dup(val, "id");
	bool ret;

	JSON_CPACK(json_msg, "{sossso}", "id", id_val, "result", PACKAGE"/"VERSION,
			     "error", json_null());
	ret = send_json_msg(&proxi->cs, json_msg);
	json_decref(json_msg);
	return ret;
}

static bool show_message(json_t *val)
{
	const char *msg;

	if (!json_is_array(val))
		return false;
	msg = json_string_value(json_array_get(val, 0));
	if (!msg)
		return false;
	LOGNOTICE("Pool message: %s", msg);
	return true;
}

static bool send_pong(proxy_instance_t *proxi, json_t *val)
{
	json_t *json_msg, *id_val = json_object_dup(val, "id");
	bool ret;

	JSON_CPACK(json_msg, "{sossso}", "id", id_val, "result", "pong",
			     "error", json_null());
	ret = send_json_msg(&proxi->cs, json_msg);
	json_decref(json_msg);
	return ret;
}

static void prepare_proxy(proxy_instance_t *proxi);

/* Creates a duplicate instance or proxi to be used as a subproxy, ignoring
 * fields we don't use in the subproxy. */
static proxy_instance_t *create_subproxy(ckpool_t *ckp, gdata_t *gdata, proxy_instance_t *proxi,
					 const char *url, const char *baseurl)
{
	proxy_instance_t *subproxy;

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

	subproxy->cs.ckp = subproxy->ckp = ckp;

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
	cksem_init(&subproxy->cs.sem);
	cksem_post(&subproxy->cs.sem);
	return subproxy;
}

static void add_subproxy(proxy_instance_t *proxi, proxy_instance_t *subproxy)
{
	mutex_lock(&proxi->proxy_lock);
	HASH_ADD(sh, proxi->subproxies, subid, sizeof(int), subproxy);
	mutex_unlock(&proxi->proxy_lock);
}

static proxy_instance_t *__subproxy_by_id(proxy_instance_t *proxy, const int subid)
{
	proxy_instance_t *subproxy;

	HASH_FIND(sh, proxy->subproxies, &subid, sizeof(int), subproxy);
	return subproxy;
}

/* Add to the dead list to be recycled if possible */
static void store_proxy(gdata_t *gdata, proxy_instance_t *proxy)
{
	LOGINFO("Recycling data from proxy %d:%d", proxy->id, proxy->subid);

	mutex_lock(&gdata->lock);
	dealloc(proxy->enonce1);
	dealloc(proxy->url);
	dealloc(proxy->baseurl);
	dealloc(proxy->auth);
	dealloc(proxy->pass);
	memset(proxy, 0, sizeof(proxy_instance_t));
	DL_APPEND(gdata->dead_proxies, proxy);
	mutex_unlock(&gdata->lock);
}

/* The difference between a dead proxy and a deleted one is the parent proxy entry
 * is not removed from the stratifier as it assumes it is down whereas a deleted
 * proxy has had its entry removed from the generator. */
static void send_stratifier_deadproxy(ckpool_t *ckp, const int id, const int subid)
{
	char buf[256];

	if (ckp->passthrough)
		return;
	sprintf(buf, "deadproxy=%d:%d", id, subid);
	send_proc(ckp->stratifier, buf);
}

static void send_stratifier_delproxy(ckpool_t *ckp, const int id, const int subid)
{
	char buf[256];

	if (ckp->passthrough)
		return;
	sprintf(buf, "delproxy=%d:%d", id, subid);
	send_proc(ckp->stratifier, buf);
}

/* Close the subproxy socket if it's open and remove it from the epoll list */
static void close_proxy_socket(proxy_instance_t *proxy, proxy_instance_t *subproxy)
{
	if (subproxy->cs.fd > 0) {
		epoll_ctl(proxy->epfd, EPOLL_CTL_DEL, subproxy->cs.fd, NULL);
		Close(subproxy->cs.fd);
	}
}

/* Remove the subproxy from the proxi list and put it on the dead list.
 * Further use of the subproxy pointer may point to a new proxy but will not
 * dereference. This will only disable subproxies so parent proxies need to
 * have their disabled bool set manually. */
static void disable_subproxy(gdata_t *gdata, proxy_instance_t *proxi, proxy_instance_t *subproxy)
{
	subproxy->alive = false;
	send_stratifier_deadproxy(gdata->ckp, subproxy->id, subproxy->subid);
	close_proxy_socket(proxi, subproxy);
	if (parent_proxy(subproxy))
		return;

	subproxy->disabled = true;

	mutex_lock(&proxi->proxy_lock);
	/* Make sure subproxy is still in the list */
	subproxy = __subproxy_by_id(proxi, subproxy->subid);
	if (likely(subproxy))
		HASH_DELETE(sh, proxi->subproxies, subproxy);
	mutex_unlock(&proxi->proxy_lock);

	if (subproxy) {
		send_stratifier_deadproxy(gdata->ckp, subproxy->id, subproxy->subid);
		store_proxy(gdata, subproxy);
	}
}

static bool parse_reconnect(proxy_instance_t *proxy, json_t *val)
{
	bool sameurl = false, ret = false;
	ckpool_t *ckp = proxy->ckp;
	gdata_t *gdata = ckp->gdata;
	proxy_instance_t *parent;
	const char *new_url;
	int new_port;
	char *url;

	new_url = json_string_value(json_array_get(val, 0));
	new_port = json_integer_value(json_array_get(val, 1));
	/* See if we have an invalid entry listing port as a string instead of
	 * integer and handle that. */
	if (!new_port) {
		const char *newport_string = json_string_value(json_array_get(val, 1));

		if (newport_string)
			sscanf(newport_string, "%d", &new_port);
	}
	if (new_url && strlen(new_url) && new_port) {
		char *dot_pool, *dot_reconnect;
		int len;

		dot_pool = strchr(proxy->url, '.');
		if (!dot_pool) {
			LOGWARNING("Denied stratum reconnect request from server without domain %s",
				   proxy->url);
			goto out;
		}
		dot_reconnect = strchr(new_url, '.');
		if (!dot_reconnect) {
			LOGWARNING("Denied stratum reconnect request to url without domain %s",
				   new_url);
			goto out;
		}
		len = strlen(dot_reconnect);
		if (strncmp(dot_pool, dot_reconnect, len)) {
			LOGWARNING("Denied stratum reconnect request from %s to non-matching domain %s",
				   proxy->url, new_url);
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
	disable_subproxy(gdata, parent, proxy);
	if (parent != proxy) {
		/* If this is a subproxy we only need to create a new one if
		 * the url has changed. Otherwise automated recruiting will
		 * take care of creating one if needed. */
		if (!sameurl)
			create_subproxy(ckp, gdata, parent, url, parent->baseurl);
		goto out;
	}

	proxy->reconnect = true;
	LOGWARNING("Proxy %d:%s reconnect issue to %s, dropping existing connection",
		   proxy->id, proxy->url, url);
	if (!sameurl) {
		char *oldurl = proxy->url;

		proxy->url = url;
		free(oldurl);
	} else
		free(url);
out:
	return ret;
}

static void send_diff(ckpool_t *ckp, proxy_instance_t *proxi)
{
	proxy_instance_t *proxy = proxi->parent;
	json_t *json_msg;
	char *msg, *buf;

	/* Not set yet */
	if (!proxi->diff)
		return;

	JSON_CPACK(json_msg, "{sIsisf}",
		   "proxy", proxy->id,
		   "subproxy", proxi->subid,
		   "diff", proxi->diff);
	msg = json_dumps(json_msg, JSON_NO_UTF8);
	json_decref(json_msg);
	ASPRINTF(&buf, "diff=%s", msg);
	free(msg);
	send_proc(ckp->stratifier, buf);
	free(buf);
}

static void send_notify(ckpool_t *ckp, proxy_instance_t *proxi, notify_instance_t *ni)
{
	proxy_instance_t *proxy = proxi->parent;
	json_t *json_msg, *merkle_arr;
	char *msg, *buf;
	int i;

	merkle_arr = json_array();

	for (i = 0; i < ni->merkles; i++)
		json_array_append_new(merkle_arr, json_string(&ni->merklehash[i][0]));
	/* Use our own jobid instead of the server's one for easy lookup */
	JSON_CPACK(json_msg, "{sIsisisssisssssosssssssb}",
			     "proxy", proxy->id, "subproxy", proxi->subid,
			     "jobid", ni->id64, "prevhash", ni->prevhash, "coinb1len", ni->coinb1len,
			     "coinbase1", ni->coinbase1, "coinbase2", ni->coinbase2,
			     "merklehash", merkle_arr, "bbversion", ni->bbversion,
			     "nbit", ni->nbit, "ntime", ni->ntime,
			     "clean", ni->clean);

	msg = json_dumps(json_msg, JSON_NO_UTF8);
	json_decref(json_msg);
	ASPRINTF(&buf, "notify=%s", msg);
	free(msg);
	send_proc(ckp->stratifier, buf);
	free(buf);

	/* Send diff now as stratifier will not accept diff till it has a
	 * valid workbase */
	send_diff(ckp, proxi);
}

static bool parse_method(ckpool_t *ckp, proxy_instance_t *proxi, const char *msg)
{
	json_t *val = NULL, *method, *err_val, *params;
	json_error_t err;
	bool ret = false;
	const char *buf;

	if (!msg)
		goto out;
	memset(&err, 0, sizeof(err));
	val = json_loads(msg, 0, &err);
	if (!val) {
		if (proxi->global) {
			LOGWARNING("JSON decode of proxy %d:%s msg %s failed(%d): %s",
				   proxi->id, proxi->url, msg, err.line, err.text);
		} else {
			LOGNOTICE("JSON decode of proxy %d:%s msg %s failed(%d): %s",
				  proxi->id, proxi->url, msg, err.line, err.text);
		}
		goto out;
	}

	method = json_object_get(val, "method");
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
	err_val = json_object_get(val, "error");
	params = json_object_get(val, "params");

	if (err_val && !json_is_null(err_val)) {
		char *ss;

		if (err_val)
			ss = json_dumps(err_val, 0);
		else
			ss = strdup("(unknown reason)");

		LOGINFO("JSON-RPC method decode failed: %s", ss);
		free(ss);
		goto out;
	}

	if (!json_is_string(method)) {
		LOGINFO("Method is not string in parse_method");
		goto out;
	}
	buf = json_string_value(method);
	if (!buf || strlen(buf) < 1) {
		LOGINFO("Invalid string for method in parse_method");
		goto out;
	}

	LOGDEBUG("Proxy %d:%d received method %s", proxi->id, proxi->subid, buf);
	if (cmdmatch(buf, "mining.notify")) {
		ret = parse_notify(ckp, proxi, params);
		goto out;
	}

	if (cmdmatch(buf, "mining.set_difficulty")) {
		ret = parse_diff(proxi, params);
		if (likely(ret))
			send_diff(ckp, proxi);
		goto out;
	}

	if (cmdmatch(buf, "client.reconnect")) {
		ret = parse_reconnect(proxi, params);
		goto out;
	}

	if (cmdmatch(buf, "client.get_version")) {
		ret =  send_version(proxi, val);
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
	if (val)
		json_decref(val);
	return ret;
}

/* cs semaphore must be held */
static bool auth_stratum(ckpool_t *ckp, connsock_t *cs, proxy_instance_t *proxi)
{
	json_t *val = NULL, *res_val, *req, *err_val;
	char *buf = NULL;
	bool ret;

	JSON_CPACK(req, "{s:i,s:s,s:[s,s]}",
			"id", 42,
			"method", "mining.authorize",
			"params", proxi->auth, proxi->pass);
	ret = send_json_msg(cs, req);
	json_decref(req);
	if (!ret) {
		LOGNOTICE("Proxy %d:%d %s failed to send message in auth_stratum",
			  proxi->id, proxi->subid, proxi->url);
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
			LOGNOTICE("Proxy %d:%d %s failed to receive line in auth_stratum",
				  proxi->id, proxi->subid, proxi->url);
			ret = false;
			goto out;
		}
		ret = parse_method(ckp, proxi, buf);
	} while (ret);

	val = json_msg_result(buf, &res_val, &err_val);
	if (!val) {
		if (proxi->global) {
			LOGWARNING("Proxy %d:%d %s failed to get a json result in auth_stratum, got: %s",
				   proxi->id, proxi->subid, proxi->url, buf);
		} else {
			LOGNOTICE("Proxy %d:%d %s failed to get a json result in auth_stratum, got: %s",
				  proxi->id, proxi->subid, proxi->url, buf);
		}
		goto out;
	}

	if (err_val && !json_is_null(err_val)) {
		LOGWARNING("Proxy %d:%d %s failed to authorise in auth_stratum due to err_val, got: %s",
			   proxi->id, proxi->subid, proxi->url, buf);
		goto out;
	}
	if (res_val) {
		ret = json_is_true(res_val);
		if (!ret) {
			if (proxi->global) {
				LOGWARNING("Proxy %d:%d %s failed to authorise in auth_stratum, got: %s",
					   proxi->id, proxi->subid, proxi->url, buf);
			} else {
				LOGNOTICE("Proxy %d:%d %s failed to authorise in auth_stratum, got: %s",
					  proxi->id, proxi->subid, proxi->url, buf);
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
		json_decref(val);
	if (ret) {
		/* Now parse any cached responses so there are none in the
		 * queue and they can be managed one at a time from now on. */
		while(42) {
			dealloc(buf);
			buf = cached_proxy_line(proxi);
			if (!buf)
				break;
			parse_method(ckp, proxi, buf);
		};
	}
	return ret;
}

static proxy_instance_t *proxy_by_id(gdata_t *gdata, const int id)
{
	proxy_instance_t *proxi;

	mutex_lock(&gdata->lock);
	HASH_FIND_INT(gdata->proxies, &id, proxi);
	mutex_unlock(&gdata->lock);

	return proxi;
}

static void send_subscribe(ckpool_t *ckp, proxy_instance_t *proxi)
{
	json_t *json_msg;
	char *msg, *buf;

	JSON_CPACK(json_msg, "{ss,ss,ss,ss,sI,si,ss,si,sb,si}",
		   "baseurl", proxi->baseurl,
		   "url", proxi->url, "auth", proxi->auth, "pass", proxi->pass,
		   "proxy", proxi->id, "subproxy", proxi->subid,
		   "enonce1", proxi->enonce1, "nonce2len", proxi->nonce2len,
		   "global", proxi->global, "userid", proxi->userid);
	msg = json_dumps(json_msg, JSON_NO_UTF8);
	json_decref(json_msg);
	ASPRINTF(&buf, "subscribe=%s", msg);
	free(msg);
	send_proc(ckp->stratifier, buf);
	free(buf);
}

static proxy_instance_t *subproxy_by_id(proxy_instance_t *proxy, const int subid)
{
	proxy_instance_t *subproxy;

	mutex_lock(&proxy->proxy_lock);
	subproxy = __subproxy_by_id(proxy, subid);
	mutex_unlock(&proxy->proxy_lock);

	return subproxy;
}

static void drop_proxy(gdata_t *gdata, const char *buf)
{
	proxy_instance_t *proxy, *subproxy;
	int id = -1, subid = -1;

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

static void stratifier_reconnect_client(ckpool_t *ckp, const int64_t id)
{
	char buf[256];

	sprintf(buf, "reconnclient=%"PRId64, id);
	send_proc(ckp->stratifier, buf);
}

/* Add a share to the gdata share hashlist. Returns the share id */
static int add_share(gdata_t *gdata, const int64_t client_id, const double diff)
{
	share_msg_t *share = ckzalloc(sizeof(share_msg_t)), *tmpshare;
	time_t now;
	int ret;

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

static void submit_share(gdata_t *gdata, json_t *val)
{
	proxy_instance_t *proxy, *proxi;
	ckpool_t *ckp = gdata->ckp;
	int id, subid, share_id;
	bool success = false;
	stratum_msg_t *msg;
	int64_t client_id;

	/* Get the client id so we can tell the stratifier to drop it if the
	 * proxy it's bound to is not functional */
	if (unlikely(!json_get_int64(&client_id, val, "client_id"))) {
		LOGWARNING("Got no client_id in share");
		goto out;
	}
	if (unlikely(!json_get_int(&id, val, "proxy"))) {
		LOGWARNING("Got no proxy in share");
		goto out;
	}
	if (unlikely(!json_get_int(&subid, val, "subproxy"))) {
		LOGWARNING("Got no subproxy in share");
		goto out;
	}
	proxy = proxy_by_id(gdata, id);
	if (unlikely(!proxy)) {
		LOGINFO("Client %"PRId64" sending shares to non existent proxy %d, dropping",
			client_id, id);
		stratifier_reconnect_client(ckp, client_id);
		goto out;
	}
	proxi = subproxy_by_id(proxy, subid);
	if (unlikely(!proxi)) {
		LOGINFO("Client %"PRId64" sending shares to non existent subproxy %d:%d, dropping",
			client_id, id, subid);
		stratifier_reconnect_client(ckp, client_id);
		goto out;
	}
	if (!proxi->alive) {
		LOGINFO("Client %"PRId64" sending shares to dead subproxy %d:%d, dropping",
			client_id, id, subid);
		stratifier_reconnect_client(ckp, client_id);
		goto out;
	}

	success = true;
	msg = ckzalloc(sizeof(stratum_msg_t));
	msg->json_msg = val;
	share_id = add_share(gdata, client_id, proxi->diff);
	json_set_int(val, "id", share_id);

	/* Add the new message to the psend list */
	mutex_lock(&gdata->psend_lock);
	gdata->psends_generated++;
	DL_APPEND(gdata->psends, msg);
	pthread_cond_signal(&gdata->psend_cond);
	mutex_unlock(&gdata->psend_lock);

out:
	if (!success)
		json_decref(val);
}

static void clear_notify(notify_instance_t *ni)
{
	if (ni->jobid)
		json_decref(ni->jobid);
	free(ni->coinbase1);
	free(ni->coinbase2);
	free(ni);
}

/* Entered with proxy_lock held */
static void __decay_proxy(proxy_instance_t *proxy, proxy_instance_t * parent, const double diff)
{
	double tdiff;
	tv_t now_t;

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

static void account_shares(proxy_instance_t *proxy, const double diff, const bool result)
{
	proxy_instance_t *parent = proxy->parent;

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
static int parse_share(gdata_t *gdata, proxy_instance_t *proxi, const char *buf)
{
	json_t *val = NULL, *idval;
	bool result = false;
	share_msg_t *share;
	int ret = 0;
	int64_t id;

	val = json_loads(buf, 0, NULL);
	if (unlikely(!val)) {
		LOGINFO("Failed to parse upstream json msg: %s", buf);
		goto out;
	}
	idval = json_object_get(val, "id");
	if (unlikely(!idval)) {
		LOGINFO("Failed to find id in upstream json msg: %s", buf);
		goto out;
	}
	id = json_integer_value(idval);
	if (unlikely(!json_get_bool(&result, val, "result"))) {
		LOGINFO("Failed to find result in upstream json msg: %s", buf);
		goto out;
	}

	mutex_lock(&gdata->share_lock);
	HASH_FIND_I64(gdata->shares, &id, share);
	if (share) {
		HASH_DEL(gdata->shares, share);
		free(share);
	}
	mutex_unlock(&gdata->share_lock);

	if (!share) {
		LOGINFO("Proxy %d:%d failed to find matching share to result: %s",
			proxi->id, proxi->subid, buf);
		/* We don't know what diff these shares are so assume the
		 * current proxy diff. */
		account_shares(proxi, proxi->diff, result);
		ret = -1;
		goto out;
	}
	ret = 1;
	account_shares(proxi, share->diff, result);
	LOGINFO("Proxy %d:%d share result %s from client %"PRId64, proxi->id, proxi->subid,
		buf, share->client_id);
	free(share);
out:
	if (val)
		json_decref(val);
	return ret;
}

struct cs_msg {
	cs_msg_t *next;
	cs_msg_t *prev;
	proxy_instance_t *proxy;
	char *buf;
	int len;
	int ofs;
};

/* Sends all messages in the queue ready to be dispatched, leaving those that
 * would block to be handled next pass */
static void send_json_msgq(gdata_t *gdata, cs_msg_t **csmsgq)
{
	cs_msg_t *csmsg, *tmp;
	int ret;

	DL_FOREACH_SAFE(*csmsgq, csmsg, tmp) {
		proxy_instance_t *proxy = csmsg->proxy;

		/* Only try to send one message at a time to each proxy
		 * to avoid sending parts of different messages */
		if (proxy->sending  && proxy->sending != csmsg)
			continue;
		while (csmsg->len > 0) {
			int fd;

			if (unlikely(!proxy->alive)) {
				LOGDEBUG("Dropping send message to dead proxy %d:%d in send_json_msgq",
					 proxy->id, proxy->subid);
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
				LOGNOTICE("Proxy %d:%d %s failed to send msg in send_json_msgq, dropping",
					  proxy->id, proxy->subid, proxy->url);
				disable_subproxy(gdata, proxy->parent, proxy);
			}
			csmsg->ofs += ret;
			csmsg->len -= ret;
		}
		if (csmsg->len < 1) {
			proxy->sending = NULL;
			DL_DELETE(*csmsgq, csmsg);
			free(csmsg->buf);
			free(csmsg);
		}
	}
}

static void add_json_msgq(cs_msg_t **csmsgq, proxy_instance_t *proxy, json_t **val)
{
	cs_msg_t *csmsg = ckzalloc(sizeof(cs_msg_t));

	csmsg->buf = json_dumps(*val, JSON_ESCAPE_SLASH | JSON_EOL);
	json_decref(*val);
	*val = NULL;
	if (unlikely(!csmsg->buf)) {
		LOGWARNING("Failed to create json dump in add_json_msgq");
		return;
	}
	csmsg->len = strlen(csmsg->buf);
	csmsg->proxy = proxy;
	DL_APPEND(*csmsgq, csmsg);
}

/* For processing and sending shares. proxy refers to parent proxy here */
static void *proxy_send(void *arg)
{
	ckpool_t *ckp = (ckpool_t *)arg;
	gdata_t *gdata = ckp->gdata;
	stratum_msg_t *msg = NULL;
	cs_msg_t *csmsgq = NULL;

	rename_proc("proxysend");

	pthread_detach(pthread_self());

	while (42) {
		proxy_instance_t *proxy, *subproxy;
		int proxyid = 0, subid = 0;
		int64_t client_id = 0, id;
		notify_instance_t *ni;
		json_t *jobid = NULL;
		json_t *val;

		if (unlikely(msg)) {
			json_decref(msg->json_msg);
			free(msg);
		}

		mutex_lock(&gdata->psend_lock);
		if (!gdata->psends) {
			/* Poll every 10ms */
			const ts_t polltime = {0, 10000000};
			ts_t timeout_ts;

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

		if (unlikely(!json_get_int(&subid, msg->json_msg, "subproxy"))) {
			LOGWARNING("Failed to find subproxy in proxy_send msg");
			continue;
		}
		if (unlikely(!json_get_int64(&id, msg->json_msg, "jobid"))) {
			LOGWARNING("Failed to find jobid in proxy_send msg");
			continue;
		}
		if (unlikely(!json_get_int(&proxyid, msg->json_msg, "proxy"))) {
			LOGWARNING("Failed to find proxy in proxy_send msg");
			continue;
		}
		if (unlikely(!json_get_int64(&client_id, msg->json_msg, "client_id"))) {
			LOGWARNING("Failed to find client_id in proxy_send msg");
			continue;
		}
		proxy = proxy_by_id(gdata, proxyid);
		if (unlikely(!proxy)) {
			LOGWARNING("Proxysend for got message for non-existent proxy %d",
				   proxyid);
			continue;
		}
		subproxy = subproxy_by_id(proxy, subid);
		if (unlikely(!subproxy)) {
			LOGWARNING("Proxysend for got message for non-existent subproxy %d:%d",
				   proxyid, subid);
			continue;
		}

		mutex_lock(&gdata->notify_lock);
		HASH_FIND_INT(gdata->notify_instances, &id, ni);
		if (ni)
			jobid = json_copy(ni->jobid);
		mutex_unlock(&gdata->notify_lock);

		if (unlikely(!jobid)) {
			stratifier_reconnect_client(ckp, client_id);
			LOGNOTICE("Proxy %d:%s failed to find matching jobid in proxysend",
				  subproxy->id, subproxy->url);
			continue;
		}

		JSON_CPACK(val, "{s[soooo]soss}", "params", subproxy->auth, jobid,
				json_object_dup(msg->json_msg, "nonce2"),
				json_object_dup(msg->json_msg, "ntime"),
				json_object_dup(msg->json_msg, "nonce"),
				"id", json_object_dup(msg->json_msg, "id"),
				"method", "mining.submit");
		add_json_msgq(&csmsgq, subproxy, &val);
		send_json_msgq(gdata, &csmsgq);
	}
	return NULL;
}

static void passthrough_send(ckpool_t *ckp, pass_msg_t *pm)
{
	proxy_instance_t *proxy = pm->proxy;
	connsock_t *cs = pm->cs;
	int len, sent;

	if (unlikely(!proxy->alive || cs->fd < 0)) {
		LOGDEBUG("Dropping send to dead proxy of upstream json msg: %s", pm->msg);
		goto out;
	}
	LOGDEBUG("Sending upstream json msg: %s", pm->msg);
	len = strlen(pm->msg);
	sent = write_socket(cs->fd, pm->msg, len);
	if (unlikely(sent != len)) {
		LOGWARNING("Failed to passthrough %d bytes of message %s, attempting reconnect",
			   len, pm->msg);
		Close(cs->fd);
		proxy->alive = false;
		reconnect_generator(ckp);
	}
out:
	free(pm->msg);
	free(pm);
}

static void passthrough_add_send(proxy_instance_t *proxy, char *msg)
{
	pass_msg_t *pm = ckzalloc(sizeof(pass_msg_t));

	pm->proxy = proxy;
	pm->cs = &proxy->cs;
	pm->msg = msg;
	ckmsgq_add(proxy->passsends, pm);
}

void generator_add_send(ckpool_t *ckp, json_t *val)
{
	gdata_t *gdata = ckp->gdata;
	char *buf;

	if (!ckp->passthrough) {
		submit_share(gdata, val);
		return;
	}
	if (unlikely(!gdata->current_proxy)) {
		LOGWARNING("No current proxy to send passthrough data to");
		goto out;
	}
	buf = json_dumps(val, JSON_COMPACT | JSON_EOL);
	if (unlikely(!buf)) {
		LOGWARNING("Unable to decode json in generator_add_send");
		goto out;
	}
	passthrough_add_send(gdata->current_proxy, buf);
out:
	json_decref(val);
}

static void suggest_diff(ckpool_t *ckp, connsock_t *cs, proxy_instance_t *proxy)
{
	json_t *req;
	bool ret;

	JSON_CPACK(req, "{s:i,s:s, s:[I]}",
		        "id", 41,
		        "method", "mining.suggest",
		        "params", ckp->mindiff);
	ret = send_json_msg(cs, req);
	json_decref(req);
	if (!ret) {
		LOGNOTICE("Proxy %d:%d %s failed to send message in suggest_diff",
			  proxy->id, proxy->subid, proxy->url);
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
static void proxy_backoff(proxy_instance_t *proxy)
{
	/* Add 5 seconds with each backoff, up to maximum of 1 minute */
	if (proxy->backoff < 60)
		proxy->backoff += 5;
}

static bool proxy_alive(ckpool_t *ckp, proxy_instance_t *proxi, connsock_t *cs,
			bool pinging)
{
	proxy_instance_t *parent = proxi->parent;
	bool ret = false;

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
	if (!connect_proxy(ckp, cs, proxi)) {
		if (!pinging) {
			LOGINFO("Failed to connect to %s:%s in proxy_mode!",
				cs->url, cs->port);
		}
		parent->connect_status = STATUS_FAIL;
		proxy_backoff(parent);
		goto out;
	}
	parent->connect_status = STATUS_SUCCESS;

	if (ckp->node) {
		if (!node_stratum(cs, proxi)) {
			LOGWARNING("Failed initial node setup to %s:%s !",
				   cs->url, cs->port);
			goto out;
		}
		ret = true;
		goto out;
	}
	if (ckp->passthrough) {
		if (!passthrough_stratum(cs, proxi)) {
			LOGWARNING("Failed initial passthrough to %s:%s !",
				   cs->url, cs->port);
			goto out;
		}
		ret = true;
		goto out;
	}
	/* Test we can connect, authorise and get stratum information */
	if (!subscribe_stratum(ckp, cs, proxi)) {
		if (!pinging) {
			LOGWARNING("Failed initial subscribe to %s:%s !",
				   cs->url, cs->port);
		}
		parent->subscribe_status = STATUS_FAIL;
		proxy_backoff(parent);
		goto out;
	}
	parent->subscribe_status = STATUS_SUCCESS;

	if (!ckp->passthrough)
		send_subscribe(ckp, proxi);
	if (!auth_stratum(ckp, cs, proxi)) {
		if (!pinging) {
			LOGWARNING("Failed initial authorise to %s:%s with %s:%s !",
				   cs->url, cs->port, proxi->auth, proxi->pass);
		}
		parent->auth_status = STATUS_FAIL;
		proxy_backoff(parent);
		goto out;
	}
	parent->auth_status = STATUS_SUCCESS;
	proxi->authorised = ret = true;
	parent->backoff = 0;
	if (ckp->mindiff > 1)
		suggest_diff(ckp, cs, proxi);
out:
	if (!ret) {
		send_stratifier_deadproxy(ckp, proxi->id, proxi->subid);
		/* Close and invalidate the file handle */
		Close(cs->fd);
	}
	proxi->alive = ret;
	cksem_post(&cs->sem);

	/* Decrease the parent's recruit count after sending the stratifier the
	 * new subscribe so it can get an accurate headroom count before
	 * requesting more proxies. */
	if (ret) {
		proxy_instance_t *parent = proxi->parent;

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

static void *proxy_recruit(void *arg)
{
	proxy_instance_t *proxy, *parent = (proxy_instance_t *)arg;
	ckpool_t *ckp = parent->ckp;
	gdata_t *gdata = ckp->gdata;
	bool recruit, alive;

	pthread_detach(pthread_self());

	/* We do this in a separate thread so it's okay to sleep here */
	if (parent->backoff)
		sleep(parent->backoff);

retry:
	recruit = false;
	proxy = create_subproxy(ckp, gdata, parent, parent->url, parent->baseurl);
	alive = proxy_alive(ckp, proxy, &proxy->cs, false);
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

static void recruit_subproxies(proxy_instance_t *proxi, const int recruits)
{
	bool recruit = false;
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
static void recruit_subproxy(gdata_t *gdata, const char *buf)
{
	int recruits = 1, id = 0;
	proxy_instance_t *proxy;

	sscanf(buf, "recruit=%d:%d", &id, &recruits);
	proxy = proxy_by_id(gdata, id);
	if (unlikely(!proxy)) {
		LOGNOTICE("Generator failed to find proxy id %d to recruit subproxies",
			  id);
		return;
	}
	recruit_subproxies(proxy, recruits);
}

static void *proxy_reconnect(void *arg)
{
	proxy_instance_t *proxy = (proxy_instance_t *)arg;
	connsock_t *cs = &proxy->cs;
	ckpool_t *ckp = proxy->ckp;

	pthread_detach(pthread_self());
	if (proxy->parent->backoff)
		sleep(proxy->parent->backoff);
	proxy_alive(ckp, proxy, cs, true);
	proxy->reconnecting = false;
	return NULL;
}

/* For reconnecting the parent proxy instance async */
static void reconnect_proxy(proxy_instance_t *proxi)
{
	pthread_t pth;

	if (proxi->reconnecting)
		return;
	proxi->reconnecting = true;
	create_pthread(&pth, proxy_reconnect, proxi);
}

/* For receiving messages from an upstream pool to pass downstream. Responsible
 * for setting up the connection and testing pool is live. */
static void *passthrough_recv(void *arg)
{
	proxy_instance_t *proxi = (proxy_instance_t *)arg;
	connsock_t *cs = &proxi->cs;
	ckpool_t *ckp = proxi->ckp;
	bool alive;

	rename_proc("passrecv");

	proxi->parent = proxi;
	if (proxy_alive(ckp, proxi, cs, false))
		LOGWARNING("Passthrough proxy %d:%s connection established", proxi->id, proxi->url);
	alive = proxi->alive;

	while (42) {
		float timeout = 5;
		int ret;

		while (!proxy_alive(ckp, proxi, cs, true)) {
			alive = false;
			sleep(5);
		}
		if (!alive) {
			reconnect_generator(ckp);
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
			send_proc(ckp->connector, cs->buf);
		} else if (ret < 0) {
			/* Read failure */
			LOGWARNING("Passthrough %d:%s failed to read_socket_line in passthrough_recv, attempting reconnect",
				   proxi->id, proxi->url);
			alive = proxi->alive = false;
			Close(cs->fd);
			reconnect_generator(ckp);
		} else /* No messages during timeout */
			LOGDEBUG("Passthrough %d:%s no messages received", proxi->id, proxi->url);
		cksem_post(&cs->sem);
	}
	return NULL;
}

static bool subproxies_alive(proxy_instance_t *proxy)
{
	proxy_instance_t *subproxy, *tmp;
	bool ret = false;

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
static void *proxy_recv(void *arg)
{
	proxy_instance_t *proxi = (proxy_instance_t *)arg;
	connsock_t *cs = &proxi->cs;
	proxy_instance_t *subproxy;
	ckpool_t *ckp = proxi->ckp;
	gdata_t *gdata = ckp->gdata;
	struct epoll_event event;
	bool alive;
	int epfd;

	rename_proc("proxyrecv");
	pthread_detach(pthread_self());

	proxi->epfd = epfd = epoll_create1(EPOLL_CLOEXEC);
	if (epfd < 0){
		LOGEMERG("FATAL: Failed to create epoll in proxyrecv");
		return NULL;
	}

	if (proxy_alive(ckp, proxi, cs, false))
		LOGWARNING("Proxy %d:%s connection established", proxi->id, proxi->url);

	alive = proxi->alive;

	while (42) {
		bool message = false, hup = false;
		share_msg_t *share, *tmpshare;
		notify_instance_t *ni, *tmp;
		float timeout;
		time_t now;
		int ret;

		subproxy = proxi;
		if (!proxi->alive) {
			reconnect_proxy(proxi);
			while (!subproxies_alive(proxi)) {
				reconnect_proxy(proxi);
				if (alive) {
					reconnect_generator(ckp);
					LOGWARNING("Proxy %d:%s failed, attempting reconnect",
						   proxi->id, proxi->url);
					alive = false;
				}
				sleep(5);
			}
		}
		if (!alive) {
			reconnect_generator(ckp);
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
		 * has likely stopped responding. */
		ret = epoll_wait(epfd, &event, 1, 600000);
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
				timeout = 30;
				ret = read_socket_line(cs, &timeout);
				/* If we are unable to read anything within 30
				 * seconds at this point after EPOLLIN is set
				 * then the socket is dead. */
				if (ret < 1) {
					LOGNOTICE("Proxy %d:%d %s failed to read_socket_line in proxy_recv",
						  proxi->id, subproxy->subid, subproxy->url);
					hup = true;
				} else {
					message = true;
					timeout = 0;
				}
			}
			if (event.events & (EPOLLHUP | EPOLLERR | EPOLLRDHUP)) {
				LOGNOTICE("Proxy %d:%d %s epoll hangup in proxy_recv",
					  proxi->id, subproxy->subid, subproxy->url);
				hup = true;
			}
		} else {
			LOGNOTICE("Proxy %d:%d %s failed to epoll in proxy_recv",
				  proxi->id, subproxy->subid, subproxy->url);
			hup = true;
		}

		/* Parse any other messages already fully buffered with a zero
		 * timeout. */
		while (message || read_socket_line(cs, &timeout) > 0) {
			message = false;
			timeout = 0;
			/* subproxy may have been recycled here if it is not a
			 * parent and reconnect was issued */
			if (parse_method(ckp, subproxy, cs->buf))
				continue;
			/* If it's not a method it should be a share result */
			if (!parse_share(gdata, subproxy, cs->buf)) {
				LOGNOTICE("Proxy %d:%d unhandled stratum message: %s",
					  subproxy->id, subproxy->subid, cs->buf);
			}
		}

		/* Process hangup only after parsing messages */
		if (hup)
			disable_subproxy(gdata, proxi, subproxy);
		if (cs)
			cksem_post(&cs->sem);
	}

	return NULL;
}

/* Thread that handles all received messages from user proxies */
static void *userproxy_recv(void *arg)
{
	ckpool_t *ckp = (ckpool_t *)arg;
	gdata_t *gdata = ckp->gdata;
	struct epoll_event event;
	int epfd;

	rename_proc("uproxyrecv");
	pthread_detach(pthread_self());

	epfd = epoll_create1(EPOLL_CLOEXEC);
	if (epfd < 0){
		LOGEMERG("FATAL: Failed to create epoll in userproxy_recv");
		return NULL;
	}

	while (42) {
		proxy_instance_t *proxy, *tmpproxy;
		bool message = false, hup = false;
		share_msg_t *share, *tmpshare;
		notify_instance_t *ni, *tmp;
		connsock_t *cs;
		float timeout;
		time_t now;
		int ret;

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
			LOGNOTICE("Proxy %d:%d %s hangup in userproxy_recv", proxy->id,
				  proxy->subid, proxy->url);
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
				LOGNOTICE("Proxy %d:%d %s failed to read_socket_line in userproxy_recv",
					  proxy->id, proxy->subid, proxy->url);
				hup = true;
			} else {
				message = true;
				timeout = 0;
			}
			while (message || (ret = read_socket_line(cs, &timeout)) > 0) {
				message = false;
				timeout = 0;
				/* proxy may have been recycled here if it is not a
				 * parent and reconnect was issued */
				if (parse_method(ckp, proxy, cs->buf))
					continue;
				/* If it's not a method it should be a share result */
				if (!parse_share(gdata, proxy, cs->buf)) {
					LOGNOTICE("Proxy %d:%d unhandled stratum message: %s",
						  proxy->id, proxy->subid, cs->buf);
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

static void prepare_proxy(proxy_instance_t *proxi)
{
	proxi->parent = proxi;
	mutex_init(&proxi->proxy_lock);
	add_subproxy(proxi, proxi);
	if (proxi->global)
		create_pthread(&proxi->pth_precv, proxy_recv, proxi);
}

static proxy_instance_t *wait_best_proxy(ckpool_t *ckp, gdata_t *gdata)
{
	proxy_instance_t *ret = NULL, *proxi, *tmp;
	int retries = 0;

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
			send_proc(ckp->connector, "reject");
		sleep(1);
	}
	send_proc(ckp->connector, ret ? "accept" : "reject");
	return ret;
}

static void send_list(gdata_t *gdata, const int sockd)
{
	proxy_instance_t *proxy, *tmp;
	json_t *val, *array_val;

	array_val = json_array();

	mutex_lock(&gdata->lock);
	HASH_ITER(hh, gdata->proxies, proxy, tmp) {
		JSON_CPACK(val, "{si,sb,si,ss,ss,sf,sb,sb,si}",
			"id", proxy->id, "global", proxy->global, "userid", proxy->userid,
			"auth", proxy->auth, "pass", proxy->pass,
			"diff", proxy->diff,
			"disabled", proxy->disabled, "alive", proxy->alive,
			"subproxies", proxy->subproxy_count);
		if (proxy->enonce1) {
			json_set_string(val, "enonce1", proxy->enonce1);
			json_set_int(val, "nonce1len", proxy->nonce1len);
			json_set_int(val, "nonce2len", proxy->nonce2len);
		}
		json_array_append_new(array_val, val);
	}
	mutex_unlock(&gdata->lock);

	JSON_CPACK(val, "{so}", "proxies", array_val);
	send_api_response(val, sockd);
}

static void send_sublist(gdata_t *gdata, const int sockd, const char *buf)
{
	proxy_instance_t *proxy, *subproxy, *tmp;
	json_t *val = NULL, *res = NULL, *array_val;
	json_error_t err_val;
	int64_t id;

	array_val = json_array();

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	if (unlikely(!json_get_int64(&id, val, "id"))) {
		res = json_errormsg("Failed to get ID in send_sublist JSON: %s", buf);
		goto out;
	}
	proxy = proxy_by_id(gdata, id);
	if (unlikely(!proxy)) {
		res = json_errormsg("Failed to find proxy %"PRId64" in send_sublist", id);
		goto out;
	}

	mutex_lock(&gdata->lock);
	HASH_ITER(sh, proxy->subproxies, subproxy, tmp) {
		JSON_CPACK(res, "{si,ss,ss,sf,sb,sb}",
			"subid", subproxy->id,
			"auth", subproxy->auth, "pass", subproxy->pass,
			"diff", subproxy->diff,
			"disabled", subproxy->disabled, "alive", subproxy->alive);
		if (subproxy->enonce1) {
			json_set_string(res, "enonce1", subproxy->enonce1);
			json_set_int(res, "nonce1len", subproxy->nonce1len);
			json_set_int(res, "nonce2len", subproxy->nonce2len);
		}
		json_array_append_new(array_val, res);
	}
	mutex_unlock(&gdata->lock);

	JSON_CPACK(res, "{so}", "subproxies", array_val);
out:
	if (val)
		json_decref(val);
	send_api_response(res, sockd);
}

static proxy_instance_t *__add_proxy(ckpool_t *ckp, gdata_t *gdata, const int num);

static proxy_instance_t *__add_userproxy(ckpool_t *ckp, gdata_t *gdata, const int id,
					 const int userid, char *url, char *auth, char *pass)
{
	proxy_instance_t *proxy;

	gdata->proxies_generated++;
	proxy = ckzalloc(sizeof(proxy_instance_t));
	proxy->id = id;
	proxy->userid = userid;
	proxy->url = url;
	proxy->baseurl = strdup(url);
	proxy->auth = auth;
	proxy->pass = pass;
	proxy->ckp = proxy->cs.ckp = ckp;
	cksem_init(&proxy->cs.sem);
	cksem_post(&proxy->cs.sem);
	HASH_ADD_INT(gdata->proxies, id, proxy);
	return proxy;
}

static void add_userproxy(ckpool_t *ckp, gdata_t *gdata, const int userid,
			  const char *url, const char *auth, const char *pass)
{
	proxy_instance_t *proxy;
	char *newurl = strdup(url);
	char *newauth = strdup(auth);
	char *newpass = strdup(pass ? pass : "");
	int id;

	mutex_lock(&gdata->lock);
	id = ckp->proxies++;
	proxy = __add_userproxy(ckp, gdata, id, userid, newurl, newauth, newpass);
	mutex_unlock(&gdata->lock);

	LOGWARNING("Adding non global user %s, %d proxy %d:%s", auth, userid, id, url);
	prepare_proxy(proxy);
}

static void parse_addproxy(ckpool_t *ckp, gdata_t *gdata, const int sockd, const char *buf)
{
	char *url = NULL, *auth = NULL, *pass = NULL;
	json_t *val = NULL, *res = NULL;
	proxy_instance_t *proxy;
	json_error_t err_val;
	int id, userid;
	bool global;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	json_get_string(&url, val, "url");
	json_get_string(&auth, val, "auth");
	json_get_string(&pass, val, "pass");
	if (json_get_int(&userid, val, "userid"))
		global = false;
	else
		global = true;
	if (unlikely(!url || !auth || !pass)) {
		res = json_errormsg("Failed to decode url/auth/pass in addproxy %s", buf);
		goto out;
	}

	mutex_lock(&gdata->lock);
	id = ckp->proxies++;
	if (global) {
		ckp->proxyurl = realloc(ckp->proxyurl, sizeof(char **) * ckp->proxies);
		ckp->proxyauth = realloc(ckp->proxyauth, sizeof(char **) * ckp->proxies);
		ckp->proxypass = realloc(ckp->proxypass, sizeof(char **) * ckp->proxies);
		ckp->proxyurl[id] = url;
		ckp->proxyauth[id] = auth;
		ckp->proxypass[id] = pass;
		proxy = __add_proxy(ckp, gdata, id);
	} else
		proxy = __add_userproxy(ckp, gdata, id, userid, url, auth, pass);
	mutex_unlock(&gdata->lock);

	if (global)
		LOGNOTICE("Adding global proxy %d:%s", id, proxy->url);
	else
		LOGNOTICE("Adding user %d proxy %d:%s", userid, id, proxy->url);
	prepare_proxy(proxy);
	if (global) {
		JSON_CPACK(res, "{si,ss,ss,ss}",
			"id", proxy->id, "url", url, "auth", auth, "pass", pass);
	} else {
		JSON_CPACK(res, "{si,ss,ss,ss,si}",
			"id", proxy->id, "url", url, "auth", auth, "pass", pass,
			"userid", proxy->userid);
	}
out:
	if (val)
		json_decref(val);
	send_api_response(res, sockd);
}

static void delete_proxy(ckpool_t *ckp, gdata_t *gdata, proxy_instance_t *proxy)
{
	proxy_instance_t *subproxy;

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
			send_stratifier_delproxy(ckp, subproxy->id, subproxy->subid);
			if (proxy != subproxy)
				store_proxy(gdata, subproxy);
		}
	} while (subproxy);

	/* Recycle the proxy itself */
	store_proxy(gdata, proxy);
}

static void parse_delproxy(ckpool_t *ckp, gdata_t *gdata, const int sockd, const char *buf)
{
	json_t *val = NULL, *res = NULL;
	proxy_instance_t *proxy;
	json_error_t err_val;
	int id = -1;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	json_get_int(&id, val, "id");
	proxy = proxy_by_id(gdata, id);
	if (!proxy) {
		res = json_errormsg("Proxy id %d not found", id);
		goto out;
	}
	JSON_CPACK(res, "{si,ss,ss,ss,ss}", "id", proxy->id, "url", proxy->url,
		   "baseurl", proxy->baseurl,"auth", proxy->auth, "pass", proxy->pass);

	LOGNOTICE("Deleting proxy %d:%s", proxy->id, proxy->url);
	delete_proxy(ckp, gdata, proxy);
out:
	if (val)
		json_decref(val);
	send_api_response(res, sockd);
}

static void parse_ableproxy(gdata_t *gdata, const int sockd, const char *buf, bool disable)
{
	json_t *val = NULL, *res = NULL;
	proxy_instance_t *proxy;
	json_error_t err_val;
	int id = -1;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	json_get_int(&id, val, "id");
	proxy = proxy_by_id(gdata, id);
	if (!proxy) {
		res = json_errormsg("Proxy id %d not found", id);
		goto out;
	}
	JSON_CPACK(res, "{si,ss, ss,ss,ss}", "id", proxy->id, "url", proxy->url,
		   "baseurl", proxy->baseurl,"auth", proxy->auth, "pass", proxy->pass);
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
		json_decref(val);
	send_api_response(res, sockd);
}

static void send_stats(gdata_t *gdata, const int sockd)
{
	json_t *val = json_object(), *subval;
	int total_objects, objects;
	int64_t generated, memsize;
	proxy_instance_t *proxy;
	stratum_msg_t *msg;

	mutex_lock(&gdata->lock);
	objects = HASH_COUNT(gdata->proxies);
	memsize = SAFE_HASH_OVERHEAD(gdata->proxies) + sizeof(proxy_instance_t) * objects;
	generated = gdata->proxies_generated;
	JSON_CPACK(subval, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
	json_set_object(val, "proxies", subval);

	DL_COUNT(gdata->dead_proxies, proxy, objects);
	memsize = sizeof(proxy_instance_t) * objects;
	JSON_CPACK(subval, "{si,sI}", "count", objects, "memory", memsize);
	json_set_object(val, "dead_proxies", subval);

	total_objects = memsize = 0;
	for (proxy = gdata->proxies; proxy; proxy=proxy->hh.next) {
		mutex_lock(&proxy->proxy_lock);
		total_objects += objects = HASH_COUNT(proxy->subproxies);
		memsize += SAFE_HASH_OVERHEAD(proxy->subproxies) + sizeof(proxy_instance_t) * objects;
		mutex_unlock(&proxy->proxy_lock);
	}
	generated = gdata->subproxies_generated;
	mutex_unlock(&gdata->lock);

	JSON_CPACK(subval, "{si,sI,sI}", "count", total_objects, "memory", memsize, "generated", generated);
	json_set_object(val, "subproxies", subval);

	mutex_lock(&gdata->notify_lock);
	objects = HASH_COUNT(gdata->notify_instances);
	memsize = SAFE_HASH_OVERHEAD(gdata->notify_instances) + sizeof(notify_instance_t) * objects;
	generated = gdata->proxy_notify_id;
	mutex_unlock(&gdata->notify_lock);

	JSON_CPACK(subval, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
	json_set_object(val, "notifies", subval);

	mutex_lock(&gdata->share_lock);
	objects = HASH_COUNT(gdata->shares);
	memsize = SAFE_HASH_OVERHEAD(gdata->shares) + sizeof(share_msg_t) * objects;
	generated = gdata->share_id;
	mutex_unlock(&gdata->share_lock);

	JSON_CPACK(subval, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
	json_set_object(val, "shares", subval);

	mutex_lock(&gdata->psend_lock);
	DL_COUNT(gdata->psends, msg, objects);
	generated = gdata->psends_generated;
	mutex_unlock(&gdata->psend_lock);

	memsize = sizeof(stratum_msg_t) * objects;
	JSON_CPACK(subval, "{si,sI,sI}", "count", objects, "memory", memsize, "generated", generated);
	json_set_object(val, "psends", subval);

	send_api_response(val, sockd);
}

/* Entered with parent proxy locked */
static json_t *__proxystats(proxy_instance_t *proxy, proxy_instance_t *parent, bool discrete)
{
	json_t *val = json_object();

	/* Opportunity to update hashrate just before we report it without
	 * needing to check on idle proxies regularly */
	__decay_proxy(proxy, parent, 0);

	json_set_int(val, "id", proxy->id);
	json_set_int(val, "userid", proxy->userid);
	json_set_string(val, "baseurl", proxy->baseurl);
	json_set_string(val, "url", proxy->url);
	json_set_string(val, "auth", proxy->auth);
	json_set_string(val, "pass", proxy->pass);
	json_set_string(val, "enonce1", proxy->enonce1 ? proxy->enonce1 : "");
	json_set_int(val, "nonce1len", proxy->nonce1len);
	json_set_int(val, "nonce2len", proxy->nonce2len);
	json_set_double(val, "diff", proxy->diff);
	if (parent_proxy(proxy)) {
		json_set_double(val, "total_accepted", proxy->total_accepted);
		json_set_double(val, "total_rejected", proxy->total_rejected);
		json_set_int(val, "subproxies", proxy->subproxy_count);
		json_set_double(val, "tdsps1", proxy->tdsps1);
		json_set_double(val, "tdsps5", proxy->tdsps5);
		json_set_double(val, "tdsps60", proxy->tdsps60);
		json_set_double(val, "tdsps1440", proxy->tdsps1440);
	}
	if (discrete) {
		json_set_double(val, "dsps1", proxy->dsps1);
		json_set_double(val, "dsps5", proxy->dsps5);
		json_set_double(val, "dsps60", proxy->dsps60);
		json_set_double(val, "dsps1440", proxy->dsps1440);
		json_set_double(val, "accepted", proxy->diff_accepted);
		json_set_double(val, "rejected", proxy->diff_rejected);
	}
	json_set_string(val, "connect", proxy_status[parent->connect_status]);
	json_set_string(val, "subscribe", proxy_status[parent->subscribe_status]);
	json_set_string(val, "authorise", proxy_status[parent->auth_status]);
	json_set_int(val, "backoff", parent->backoff);
	json_set_int(val, "lastshare", proxy->last_share.tv_sec);
	json_set_bool(val, "global", proxy->global);
	json_set_bool(val, "disabled", proxy->disabled);
	json_set_bool(val, "alive", proxy->alive);
	json_set_int(val, "maxclients", proxy->clients_per_proxy);

	return val;
}

static json_t *proxystats(proxy_instance_t *proxy, bool discrete)
{
	proxy_instance_t *parent = proxy->parent;
	json_t *val;

	mutex_lock(&parent->proxy_lock);
	val = __proxystats(proxy, parent, discrete);
	mutex_unlock(&parent->proxy_lock);

	return val;
}

static json_t *all_proxystats(gdata_t *gdata)
{
	json_t *res, *arr_val = json_array();
	proxy_instance_t *proxy, *tmp;

	mutex_lock(&gdata->lock);
	HASH_ITER(hh, gdata->proxies, proxy, tmp) {
		mutex_unlock(&gdata->lock);
		json_array_append_new(arr_val, proxystats(proxy, false));
		mutex_lock(&gdata->lock);
	}
	mutex_unlock(&gdata->lock);

	JSON_CPACK(res, "{so}", "proxy", arr_val);
	return res;
}

static void parse_proxystats(gdata_t *gdata, const int sockd, const char *buf)
{
	json_t *val = NULL, *res = NULL;
	proxy_instance_t *proxy;
	json_error_t err_val;
	bool totals = false;
	int id, subid = 0;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = all_proxystats(gdata);
		goto out_noval;
	}
	if (!json_get_int(&id, val, "id")) {
		res = all_proxystats(gdata);
		goto out;
	}
	if (!json_get_int(&subid, val, "subid"))
		totals = true;
	proxy = proxy_by_id(gdata, id);
	if (!proxy) {
		res = json_errormsg("Proxy id %d not found", id);
		goto out;
	}
	if (!totals)
		proxy = subproxy_by_id(proxy, subid);
	if (!proxy) {
		res = json_errormsg("Proxy id %d:%d not found", id, subid);
		goto out;
	}
	res = proxystats(proxy, true);
out:
	json_decref(val);
out_noval:
	send_api_response(res, sockd);
}

static void send_subproxystats(gdata_t *gdata, const int sockd)
{
	json_t *res, *arr_val = json_array();
	proxy_instance_t *parent, *tmp;

	mutex_lock(&gdata->lock);
	HASH_ITER(hh, gdata->proxies, parent, tmp) {
		json_t *val, *subarr_val = json_array();
		proxy_instance_t *subproxy, *subtmp;

		mutex_unlock(&gdata->lock);

		mutex_lock(&parent->proxy_lock);
		HASH_ITER(sh, parent->subproxies, subproxy, subtmp) {
			val = __proxystats(subproxy, parent, true);
			json_set_int(val, "subid", subproxy->subid);
			json_array_append_new(subarr_val, val);
		}
		mutex_unlock(&parent->proxy_lock);

		JSON_CPACK(val, "{si,so}",
			   "id", parent->id,
			   "subproxy", subarr_val);
		json_array_append_new(arr_val, val);
		mutex_lock(&gdata->lock);
	}
	mutex_unlock(&gdata->lock);

	JSON_CPACK(res, "{so}", "proxy", arr_val);
	send_api_response(res, sockd);
}

static void parse_globaluser(ckpool_t *ckp, gdata_t *gdata, const char *buf)
{
	char *url, *username, *pass = strdupa(buf);
	int userid = -1, proxyid = -1;
	proxy_instance_t *proxy, *tmp;
	int64_t clientid = -1;
	bool found = false;

	sscanf(buf, "%d:%d:%"PRId64":%s", &proxyid, &userid, &clientid, pass);
	if (unlikely(clientid < 0 || userid < 0 || proxyid < 0)) {
		LOGWARNING("Failed to parse_globaluser ids from command %s", buf);
		return;
	}
	username = strsep(&pass, ",");
	if (unlikely(!username)) {
		LOGWARNING("Failed to parse_globaluser username from command %s", buf);
		return;
	}

	LOGDEBUG("Checking userproxy proxy %d user %d:%"PRId64" worker %s pass %s",
		 proxyid, userid, clientid, username, pass);

	if (unlikely(proxyid >= ckp->proxies)) {
		LOGWARNING("Trying to find non-existent proxy id %d in parse_globaluser", proxyid);
		return;
	}

	mutex_lock(&gdata->lock);
	url = ckp->proxyurl[proxyid];
	HASH_ITER(hh, gdata->proxies, proxy, tmp) {
		if (!strcmp(proxy->auth, username)) {
			found = true;
			break;
		}
	}
	mutex_unlock(&gdata->lock);

	if (found)
		return;
	add_userproxy(ckp, gdata, userid, url, username, pass);
}

static void proxy_loop(proc_instance_t *pi)
{
	proxy_instance_t *proxi = NULL, *cproxy;
	server_instance_t *si = NULL, *old_si;
	ckpool_t *ckp = pi->ckp;
	gdata_t *gdata = ckp->gdata;
	unix_msg_t *umsg = NULL;
	connsock_t *cs = NULL;
	char *buf = NULL;

reconnect:
	clear_unix_msg(&umsg);

	if (ckp->node) {
		old_si = si;
		si = live_server(ckp, gdata);
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
	cproxy = wait_best_proxy(ckp, gdata);
	if (!cproxy)
		goto out;
	if (proxi != cproxy) {
		gdata->current_proxy = proxi = cproxy;
		LOGWARNING("Successfully connected to pool %d %s as proxy%s",
			   proxi->id, proxi->url, ckp->passthrough ? " in passthrough mode" : "");
	}

	if (unlikely(!ckp->generator_ready)) {
		ckp->generator_ready = true;
		LOGWARNING("%s generator ready", ckp->name);
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
		parse_addproxy(ckp, gdata, umsg->sockd, buf + 9);
	} else if (cmdmatch(buf, "delproxy")) {
		parse_delproxy(ckp, gdata, umsg->sockd, buf + 9);
	} else if (cmdmatch(buf, "enableproxy")) {
		parse_ableproxy(gdata, umsg->sockd, buf + 12, false);
	} else if (cmdmatch(buf, "disableproxy")) {
		parse_ableproxy(gdata, umsg->sockd, buf + 13, true);
	} else if (cmdmatch(buf, "proxystats")) {
		parse_proxystats(gdata, umsg->sockd, buf + 11);
	} else if (cmdmatch(buf, "subproxystats")) {
		send_subproxystats(gdata, umsg->sockd);
	} else if (cmdmatch(buf, "globaluser")) {
		parse_globaluser(ckp, gdata, buf + 11);
	} else if (cmdmatch(buf, "reconnect")) {
		goto reconnect;
	} else if (cmdmatch(buf, "submitblock:")) {
		char blockmsg[80];
		bool ret;

		LOGNOTICE("Submitting likely block solve share from upstream pool");
		ret = submit_block(cs, buf + 12 + 64 + 1);
		memset(buf + 12 + 64, 0, 1);
		sprintf(blockmsg, "%sblock:%s", ret ? "" : "no", buf + 12);
		send_proc(ckp->stratifier, blockmsg);
	} else if (cmdmatch(buf, "submittxn:")) {
		if (unlikely(strlen(buf) < 11)) {
			LOGWARNING("Got zero length submittxn");
			goto retry;
		}
		submit_txn(cs, buf + 10);
	} else if (cmdmatch(buf, "loglevel")) {
		sscanf(buf, "loglevel=%d", &ckp->loglevel);
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
static void *server_watchdog(void *arg)
{
	ckpool_t *ckp = (ckpool_t *)arg;
	gdata_t *gdata = ckp->gdata;

	rename_proc("swatchdog");

	pthread_detach(pthread_self());

	while (42) {
		server_instance_t *best = NULL;
		ts_t timer_t;
		int i;

		cksleep_prepare_r(&timer_t);
		for (i = 0; i < ckp->btcds; i++) {
			server_instance_t *si  = ckp->servers[i];

			/* Have we reached the current server? */
			if (server_alive(ckp, si, true) && !best)
				best = si;
		}
		if (best && best != gdata->current_si)
			send_proc(ckp->generator, "reconnect");
		cksleep_ms_r(&timer_t, 5000);
	}
	return NULL;
}

static void setup_servers(ckpool_t *ckp)
{
	pthread_t pth_watchdog;
	int i;

	ckp->servers = ckalloc(sizeof(server_instance_t *) * ckp->btcds);
	for (i = 0; i < ckp->btcds; i++) {
		server_instance_t *si;
		connsock_t *cs;

		ckp->servers[i] = ckzalloc(sizeof(server_instance_t));
		si = ckp->servers[i];
		si->url = ckp->btcdurl[i];
		si->auth = ckp->btcdauth[i];
		si->pass = ckp->btcdpass[i];
		si->notify = ckp->btcdnotify[i];
		si->id = i;
		cs = &si->cs;
		cs->ckp = ckp;
		cksem_init(&cs->sem);
		cksem_post(&cs->sem);
	}

	create_pthread(&pth_watchdog, server_watchdog, ckp);
}

static void server_mode(ckpool_t *ckp, proc_instance_t *pi)
{
	int i;

	setup_servers(ckp);

	gen_loop(pi);

	for (i = 0; i < ckp->btcds; i++) {
		server_instance_t *si = ckp->servers[i];

		kill_server(si);
		dealloc(si);
	}
	dealloc(ckp->servers);
}

static proxy_instance_t *__add_proxy(ckpool_t *ckp, gdata_t *gdata, const int id)
{
	proxy_instance_t *proxy;

	gdata->proxies_generated++;
	proxy = ckzalloc(sizeof(proxy_instance_t));
	proxy->id = id;
	proxy->url = strdup(ckp->proxyurl[id]);
	proxy->baseurl = strdup(proxy->url);
	proxy->auth = strdup(ckp->proxyauth[id]);
	if (ckp->proxypass[id])
		proxy->pass = strdup(ckp->proxypass[id]);
	else
		proxy->pass = strdup("");
	proxy->ckp = proxy->cs.ckp = ckp;
	HASH_ADD_INT(gdata->proxies, id, proxy);
	proxy->global = true;
	cksem_init(&proxy->cs.sem);
	cksem_post(&proxy->cs.sem);
	return proxy;
}

static void proxy_mode(ckpool_t *ckp, proc_instance_t *pi)
{
	gdata_t *gdata = ckp->gdata;
	proxy_instance_t *proxy;
	int i;

	mutex_init(&gdata->lock);
	mutex_init(&gdata->notify_lock);
	mutex_init(&gdata->share_lock);

	if (ckp->node)
		setup_servers(ckp);

	/* Create all our proxy structures and pointers */
	for (i = 0; i < ckp->proxies; i++) {
		proxy = __add_proxy(ckp, gdata, i);
		if (ckp->passthrough) {
			create_pthread(&proxy->pth_precv, passthrough_recv, proxy);
			proxy->passsends = create_ckmsgq(ckp, "passsend", &passthrough_send);
		} else {
			mutex_init(&gdata->psend_lock);
			cond_init(&gdata->psend_cond);
			prepare_proxy(proxy);
			create_pthread(&gdata->pth_uprecv, userproxy_recv, ckp);
			create_pthread(&gdata->pth_psend, proxy_send, ckp);
		}
	}

	proxy_loop(pi);
}

void *generator(void *arg)
{
	proc_instance_t *pi = (proc_instance_t *)arg;
	ckpool_t *ckp = pi->ckp;
	gdata_t *gdata;

	rename_proc(pi->processname);
	LOGWARNING("%s generator starting", ckp->name);
	gdata = ckzalloc(sizeof(gdata_t));
	ckp->gdata = gdata;
	gdata->ckp = ckp;

	if (ckp->proxy) {
		/* Wait for the stratifier to be ready for us */
		while (!ckp->stratifier_ready)
			cksleep_ms(10);
		proxy_mode(ckp, pi);
	} else
		server_mode(ckp, pi);
	/* We should never get here unless there's a fatal error */
	LOGEMERG("Generator failure, shutting down");
	exit(1);
	return NULL;
}
