/*
 * Copyright 2014-2020,2023 Con Kolivas
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

#include "ckpool.h"
#include "libckpool.h"
#include "bitcoin.h"
#include "sha2.h"
#include "stratifier.h"
#include "uthash.h"
#include "utlist.h"
#include "connector.h"
#include "generator.h"

/* Consistent across all pool instances */
static const char *workpadding = "000000800000000000000000000000000000000000000000000000000000000000000000000000000000000080020000";
static const char *scriptsig_header = "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff";
static uchar scriptsig_header_bin[41];
static const double nonces = 4294967296;

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

	double network_diff;
	int64_t best_diff;
};

typedef struct pool_stats pool_stats_t;

typedef struct genwork workbase_t;

struct json_params {
	json_t *method;
	json_t *params;
	json_t *id_val;
	int64_t client_id;
};

typedef struct json_params json_params_t;

/* Stratum json messages with their associated client id */
struct smsg {
	json_t *json_msg;
	int64_t client_id;
};

typedef struct smsg smsg_t;

struct userwb {
	UT_hash_handle hh;
	int64_t id;

	uchar *coinb2bin; // Coinb2 cointaining this user's address for generation
	char *coinb2;
	int coinb2len; // Length of user coinb2
};

struct user_instance;
struct worker_instance;
struct stratum_instance;

typedef struct user_instance user_instance_t;
typedef struct worker_instance worker_instance_t;
typedef struct stratum_instance stratum_instance_t;

struct user_instance {
	UT_hash_handle hh;
	char username[128];
	int id;
	char *secondaryuserid;
	bool btcaddress;
	bool script;
	bool segwit;

	/* A linked list of all connected instances of this user */
	stratum_instance_t *clients;

	/* A linked list of all connected workers of this user */
	worker_instance_t *worker_instances;

	int workers;
	int remote_workers;
	char txnbin[48];
	int txnlen;
	struct userwb *userwbs; /* Protected by instance lock */

	double best_diff; /* Best share found by this user */
	int64_t best_ever; /* Best share ever found by this user */

	int64_t shares;

	int64_t uadiff; /* Shares not yet accounted for in hashmeter */

	double dsps1; /* Diff shares per second, 1 minute rolling average */
	double dsps5; /* ... 5 minute ... */
	double dsps60;/* etc */
	double dsps1440;
	double dsps10080;
	tv_t last_share;
	tv_t last_decay;

	bool authorised; /* Has this username ever been authorised? */
	time_t auth_time;
	time_t failed_authtime; /* Last time this username failed to authorise */
	int auth_backoff; /* How long to reject any auth attempts since last failure */
	bool throttled; /* Have we begun rejecting auth attempts */
};

/* Combined data from workers with the same workername */
struct worker_instance {
	user_instance_t *user_instance;
	char *workername;

	/* Number of stratum instances attached as this one worker */
	int instance_count;

	worker_instance_t *next;
	worker_instance_t *prev;

	int64_t shares;

	int64_t uadiff; /* Shares not yet accounted for in hashmeter */

	double dsps1;
	double dsps5;
	double dsps60;
	double dsps1440;
	double dsps10080;
	tv_t last_share;
	tv_t last_decay;
	time_t start_time;

	double best_diff; /* Best share found by this worker */
	int64_t best_ever; /* Best share ever found by this worker */
	int mindiff; /* User chosen mindiff */

	bool idle;
	bool notified_idle;
};

typedef struct stratifier_data sdata_t;

typedef struct proxy_base proxy_t;

/* Per client stratum instance == workers */
struct stratum_instance {
	UT_hash_handle hh;
	int64_t id;

	/* Virtualid used as unique local id for passthrough clients */
	int64_t virtualid;

	stratum_instance_t *recycled_next;
	stratum_instance_t *recycled_prev;

	stratum_instance_t *user_next;
	stratum_instance_t *user_prev;

	stratum_instance_t *node_next;
	stratum_instance_t *node_prev;

	stratum_instance_t *remote_next;
	stratum_instance_t *remote_prev;

	/* Descriptive of ID number and passthrough if any */
	char identity[128];

	/* Reference count for when this instance is used outside of the
	 * instance_lock */
	int ref;

	char enonce1[36]; /* Fit up to 16 byte binary enonce1 */
	uchar enonce1bin[16];
	char enonce1var[20]; /* Fit up to 8 byte binary enonce1var */
	uint64_t enonce1_64;
	int session_id;

	int64_t diff; /* Current diff */
	int64_t old_diff; /* Previous diff */
	int64_t diff_change_job_id; /* Last job_id we changed diff */

	int64_t uadiff; /* Shares not yet accounted for in hashmeter */

	double dsps1; /* Diff shares per second, 1 minute rolling average */
	double dsps5; /* ... 5 minute ... */
	double dsps60;/* etc */
	double dsps1440;
	double dsps10080;
	tv_t ldc; /* Last diff change */
	int ssdc; /* Shares since diff change */
	tv_t first_share;
	tv_t last_share;
	tv_t last_decay;
	time_t first_invalid; /* Time of first invalid in run of non stale rejects */
	time_t upstream_invalid; /* As first_invalid but for upstream responses */
	time_t start_time;

	char address[INET6_ADDRSTRLEN];
	bool node; /* Is this a mining node */
	bool subscribed;
	bool authorising; /* In progress, protected by instance_lock */
	bool authorised;
	bool dropped;
	bool idle;
	int reject;	/* Indicator that this client is having a run of rejects
			 * or other problem and should be dropped lazily if
			 * this is set to 2 */

	int latency; /* Latency when on a mining node */

	bool reconnect; /* This client really needs to reconnect */
	time_t reconnect_request; /* The time we sent a reconnect message */

	user_instance_t *user_instance;
	worker_instance_t *worker_instance;

	char *useragent;
	char *workername;
	char *password;
	bool messages; /* Is this a client that understands stratum messages */
	int user_id;
	int server; /* Which server is this instance bound to */

	ckpool_t *ckp;

	time_t last_txns; /* Last time this worker requested txn hashes */
	time_t disconnected_time; /* Time this instance disconnected */

	int64_t suggest_diff; /* Stratum client suggested diff */
	double best_diff; /* Best share found by this instance */

	sdata_t *sdata; /* Which sdata this client is bound to */
	proxy_t *proxy; /* Proxy this is bound to in proxy mode */
	int proxyid; /* Which proxy id  */
	int subproxyid; /* Which subproxy */

	bool passthrough; /* Is this a passthrough */
	bool trusted; /* Is this a trusted remote server */
	bool remote; /* Is this a remote client on a trusted remote server */
};

struct share {
	UT_hash_handle hh;
	uchar hash[32];
	int64_t workbase_id;
};

typedef struct share share_t;

struct proxy_base {
	UT_hash_handle hh;
	UT_hash_handle sh; /* For subproxy hashlist */
	proxy_t *next; /* For retired subproxies */
	proxy_t *prev;
	int id;
	int subid;

	/* Priority has the user id encoded in the high bits if it's not a
	 * global proxy. */
	int64_t priority;

	bool global; /* Is this a global proxy */
	int userid; /* Userid for non global proxies */

	double diff;

	char baseurl[128];
	char url[128];
	char auth[128];
	char pass[128];
	char enonce1[32];
	uchar enonce1bin[16];
	int enonce1constlen;
	int enonce1varlen;

	int nonce2len;
	int enonce2varlen;

	bool subscribed;
	bool notified;

	int64_t clients; /* Incrementing client count */
	int64_t max_clients; /* Maximum number of clients per subproxy */
	int64_t bound_clients; /* Currently actively bound clients */
	int64_t combined_clients; /* Total clients of all subproxies of a parent proxy */
	int64_t headroom; /* Temporary variable when calculating how many more clients can bind */

	int subproxy_count; /* Number of subproxies */
	proxy_t *parent; /* Parent proxy of each subproxy */
	proxy_t *subproxies; /* Hashlist of subproxies sorted by subid */
	sdata_t *sdata; /* Unique stratifer data for each subproxy */
	bool dead;
	bool deleted;
};

typedef struct session session_t;

struct session {
	UT_hash_handle hh;
	int session_id;
	uint64_t enonce1_64;
	int64_t client_id;
	int userid;
	time_t added;
	char address[INET6_ADDRSTRLEN];
};

typedef struct txntable txntable_t;

struct txntable {
	UT_hash_handle hh;
	int id;
	char hash[68];
	char *data;
	int refcount;
	bool seen;
};

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
	ckpool_t *ckp;

	char txnbin[48];
	int txnlen;
	char dontxnbin[48];
	int dontxnlen;

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
	workbase_t *workbases;
	workbase_t *current_workbase;
	int workbases_generated;
	txntable_t *txns;
	int64_t txns_generated;

	/* Workbases from remote trusted servers */
	workbase_t *remote_workbases;

	/* Is this a node and unable to rebuild workinfos due to lack of txns */
	bool wbincomplete;

	/* Semaphore to serialise calls to add_base */
	sem_t update_sem;
	/* Time we last sent out a stratum update */
	time_t update_time;

	int64_t workbase_id;
	int64_t blockchange_id;
	int session_id;
	char lasthash[68];
	char lastswaphash[68];

	ckmsgq_t *updateq;	// Generator base work updates
	ckmsgq_t *ssends;	// Stratum sends
	ckmsgq_t *srecvs;	// Stratum receives
	ckmsgq_t *sshareq;	// Stratum share sends
	ckmsgq_t *sauthq;	// Stratum authorisations
	ckmsgq_t *stxnq;	// Transaction requests

	int user_instance_id;

	stratum_instance_t *stratum_instances;
	stratum_instance_t *recycled_instances;
	stratum_instance_t *node_instances;
	stratum_instance_t *remote_instances;

	int64_t stratum_generated;
	int64_t disconnected_generated;
	int64_t userwbs_generated;
	session_t *disconnected_sessions;

	user_instance_t *user_instances;

	/* Protects both stratum and user instances */
	cklock_t instance_lock;

	share_t *shares;
	mutex_t share_lock;

	int64_t shares_generated;

	int proxy_count; /* Total proxies generated (not necessarily still alive) */
	proxy_t *proxy; /* Current proxy in use */
	proxy_t *proxies; /* Hashlist of all proxies */
	mutex_t proxy_lock; /* Protects all proxy data */
	proxy_t *subproxy; /* Which subproxy this sdata belongs to in proxy mode */
};

typedef struct json_entry json_entry_t;

struct json_entry {
	json_entry_t *next;
	json_entry_t *prev;
	json_t *val;
};

/* Priority levels for generator messages */
#define GEN_LAX 0
#define GEN_NORMAL 1
#define GEN_PRIORITY 2

/* For storing a set of messages within another lock, allowing us to dump them
 * to the log outside of lock */
static void add_msg_entry(char_entry_t **entries, char **buf)
{
	char_entry_t *entry;

	if (!*buf)
		return;
	entry = ckalloc(sizeof(char_entry_t));
	entry->buf = *buf;
	*buf = NULL;
	DL_APPEND(*entries, entry);
}

static void notice_msg_entries(char_entry_t **entries)
{
	char_entry_t *entry, *tmpentry;

	DL_FOREACH_SAFE(*entries, entry, tmpentry) {
		DL_DELETE(*entries, entry);
		LOGNOTICE("%s", entry->buf);
		free(entry->buf);
		free(entry);
	}
}

static void info_msg_entries(char_entry_t **entries)
{
	char_entry_t *entry, *tmpentry;

	DL_FOREACH_SAFE(*entries, entry, tmpentry) {
		DL_DELETE(*entries, entry);
		LOGINFO("%s", entry->buf);
		free(entry->buf);
		free(entry);
	}
}

static const int witnessdata_size = 36; // commitment header + hash

static void generate_coinbase(ckpool_t *ckp, workbase_t *wb)
{
	uint64_t *u64, g64, d64 = 0;
	sdata_t *sdata = ckp->sdata;
	char header[272];
	int len, ofs = 0;
	ts_t now;

	/* Set fixed length coinb1 arrays to be more than enough */
	wb->coinb1 = ckzalloc(256);
	wb->coinb1bin = ckzalloc(128);

	/* Strings in wb should have been zero memset prior. Generate binary
	 * templates first, then convert to hex */
	memcpy(wb->coinb1bin, scriptsig_header_bin, 41);
	ofs += 41; // Fixed header length;

	ofs++; // Script length is filled in at the end @wb->coinb1bin[41];

	/* Put block height at start of template */
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

	wb->enonce1varlen = ckp->nonce1length;
	wb->enonce2varlen = ckp->nonce2length;
	wb->coinb1bin[ofs++] = wb->enonce1varlen + wb->enonce2varlen;

	wb->coinb1len = ofs;

	len = wb->coinb1len - 41;

	len += wb->enonce1varlen;
	len += wb->enonce2varlen;

	wb->coinb2bin = ckzalloc(512);
	memcpy(wb->coinb2bin, "\x0a\x63\x6b\x70\x6f\x6f\x6c", 7);
	wb->coinb2len = 7;
	if (ckp->btcsig) {
		int siglen = strlen(ckp->btcsig);

		LOGDEBUG("Len %d sig %s", siglen, ckp->btcsig);
		if (siglen) {
			wb->coinb2bin[wb->coinb2len++] = siglen;
			memcpy(wb->coinb2bin + wb->coinb2len, ckp->btcsig, siglen);
			wb->coinb2len += siglen;
		}
	}
	len += wb->coinb2len;

	wb->coinb1bin[41] = len - 1; /* Set the length now */
	__bin2hex(wb->coinb1, wb->coinb1bin, wb->coinb1len);
	LOGDEBUG("Coinb1: %s", wb->coinb1);
	/* Coinbase 1 complete */

	memcpy(wb->coinb2bin + wb->coinb2len, "\xff\xff\xff\xff", 4);
	wb->coinb2len += 4;

	// Generation value
	g64 = wb->coinbasevalue;
	if (ckp->donvalid && ckp->donation > 0) {
		double dbl64 = (double)g64 / 100 * ckp->donation;

		d64 = dbl64;
		g64 -= d64; // To guarantee integers add up to the original coinbasevalue
		wb->coinb2bin[wb->coinb2len++] = 2 + wb->insert_witness;
	} else
		wb->coinb2bin[wb->coinb2len++] = 1 + wb->insert_witness;

	u64 = (uint64_t *)&wb->coinb2bin[wb->coinb2len];
	*u64 = htole64(g64);
	wb->coinb2len += 8;

	/* Coinb2 address goes here, takes up 23~25 bytes + 1 byte for length */

	wb->coinb3len = 0;
	wb->coinb3bin = ckzalloc(256 + wb->insert_witness * (8 + witnessdata_size + 2));

	if (ckp->donvalid && ckp->donation > 0) {
		u64 = (uint64_t *)wb->coinb3bin;
		*u64 = htole64(d64);
		wb->coinb3len += 8;

		wb->coinb3bin[wb->coinb3len++] = sdata->dontxnlen;
		memcpy(wb->coinb3bin + wb->coinb3len, sdata->dontxnbin, sdata->dontxnlen);
		wb->coinb3len += sdata->dontxnlen;
	} else
		ckp->donation = 0;

	if (wb->insert_witness) {
		// 0 value
		wb->coinb3len += 8;

		wb->coinb3bin[wb->coinb3len++] = witnessdata_size + 2; // total scriptPubKey size
		wb->coinb3bin[wb->coinb3len++] = 0x6a; // OP_RETURN
		wb->coinb3bin[wb->coinb3len++] = witnessdata_size;

		hex2bin(&wb->coinb3bin[wb->coinb3len], wb->witnessdata, witnessdata_size);
		wb->coinb3len += witnessdata_size;
	}

	wb->coinb3len += 4; // Blank lock

	if (!ckp->btcsolo) {
		int coinbase_len, offset = 0;
		char *coinbase, *cb;
		json_t *val = NULL;

		/* Append the generation address and coinb3 in !solo mode */
		wb->coinb2bin[wb->coinb2len++] = sdata->txnlen;
		memcpy(wb->coinb2bin + wb->coinb2len, sdata->txnbin, sdata->txnlen);
		wb->coinb2len += sdata->txnlen;
		memcpy(wb->coinb2bin + wb->coinb2len, wb->coinb3bin, wb->coinb3len);
		wb->coinb2len += wb->coinb3len;
		wb->coinb3len = 0;
		dealloc(wb->coinb3bin);

		/* Set this only once */
		if (unlikely(!ckp->coinbase_valid)) {
			/* We have enough to test the validity of the coinbase here */
			coinbase_len = wb->coinb1len + ckp->nonce1length + ckp->nonce2length + wb->coinb2len;
			coinbase = ckzalloc(coinbase_len);
			memcpy(coinbase, wb->coinb1bin, wb->coinb1len);
			offset += wb->coinb1len;
			/* Space for nonce1 and 2 */
			offset += ckp->nonce1length + ckp->nonce2length;
			memcpy(coinbase + offset, wb->coinb2bin, wb->coinb2len);
			offset += wb->coinb2len;
			cb = bin2hex(coinbase, offset);
			LOGDEBUG("Coinbase txn %s", cb);
			free(coinbase);
			if (generator_checktxn(ckp, cb, &val)) {
				char *s = json_dumps(val, JSON_NO_UTF8 | JSON_COMPACT);

				json_decref(val);
				LOGNOTICE("Coinbase transaction confirmed valid");
				LOGDEBUG("%s", s);
				free(s);
			} else {
				/* This is a fatal error */
				LOGEMERG("Coinbase failed valid transaction check, aborting!");
				exit(1);
			}
			free(cb);
			ckp->coinbase_valid = true;
			LOGWARNING("Mining from any incoming username to address %s", ckp->btcaddress);
			if (ckp->donation)
				LOGWARNING("%.1f percent donation to %s", ckp->donation, ckp->donaddress);
		}
	} else if (unlikely(!ckp->coinbase_valid)) {
		/* Create a sample coinbase to test its validity in solo mode */
		int coinbase_len, offset = 0;
		char *coinbase, *cb;
		json_t *val = NULL;

		coinbase_len = wb->coinb1len + ckp->nonce1length + ckp->nonce2length + wb->coinb2len +
			       sdata->txnlen + wb->coinb3len + 1;
		coinbase = ckzalloc(coinbase_len);
		memcpy(coinbase, wb->coinb1bin, wb->coinb1len);
		offset += wb->coinb1len;
		offset += ckp->nonce1length + ckp->nonce2length;
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
		if (generator_checktxn(ckp, cb, &val)) {
			char *s = json_dumps(val, JSON_NO_UTF8 | JSON_COMPACT);

			json_decref(val);
			LOGNOTICE("Coinbase transaction confirmed valid");
			LOGDEBUG("%s", s);
			free(s);
		} else {
			/* This is a fatal error */
			LOGEMERG("Coinbase failed valid transaction check, aborting!");
			exit(1);
		}
		free(cb);
		ckp->coinbase_valid = true;
		LOGWARNING("Mining solo to any incoming valid BTC address username");
		if (ckp->donation)
			LOGWARNING("%.1f percent donation to %s", ckp->donation, ckp->donaddress);
	}

	/* Set this just for node compatibility, though it's unused */
	wb->coinb2 = bin2hex(wb->coinb2bin, wb->coinb2len);
	LOGDEBUG("Coinb2: %s", wb->coinb2);
	/* Coinbases 2 +/- 3 templates complete */

	snprintf(header, 270, "%s%s%s%s%s%s%s",
		 wb->bbversion, wb->prevhash,
		 "0000000000000000000000000000000000000000000000000000000000000000",
		 wb->ntime, wb->nbit,
		 "00000000", /* nonce */
		 workpadding);
	header[224] = 0;
	LOGDEBUG("Header: %s", header);
	hex2bin(wb->headerbin, header, 112);
}

static void stratum_broadcast_update(sdata_t *sdata, const workbase_t *wb, bool clean);
static void stratum_broadcast_updates(sdata_t *sdata, bool clean);

static void clear_userwb(sdata_t *sdata, int64_t id)
{
	user_instance_t *instance, *tmp;

	ck_wlock(&sdata->instance_lock);
	HASH_ITER(hh, sdata->user_instances, instance, tmp) {
		struct userwb *userwb;

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

static void clear_workbase(ckpool_t *ckp, workbase_t *wb)
{
	if (ckp->btcsolo)
		clear_userwb(ckp->sdata, wb->id);
	free(wb->flags);
	free(wb->txn_data);
	free(wb->txn_hashes);
	free(wb->logdir);
	free(wb->coinb1bin);
	free(wb->coinb1);
	free(wb->coinb2bin);
	free(wb->coinb2);
	free(wb->coinb3bin);
	json_decref(wb->merkle_array);
	if (wb->json)
		json_decref(wb->json);
	free(wb);
}

/* Remove all shares with a workbase id less than wb_id for block changes */
static void purge_share_hashtable(sdata_t *sdata, const int64_t wb_id)
{
	share_t *share, *tmp;
	int purged = 0;

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
static void age_share_hashtable(sdata_t *sdata, const int64_t wb_id)
{
	share_t *share, *tmp;
	int aged = 0;

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
static void ssend_bulk_append(sdata_t *sdata, ckmsg_t *bulk_send, const int messages)
{
	ckmsgq_t *ssends = sdata->ssends;

	mutex_lock(ssends->lock);
	ssends->messages += messages;
	DL_CONCAT(ssends->msgs, bulk_send);
	pthread_cond_signal(ssends->cond);
	mutex_unlock(ssends->lock);
}

/* As ssend_bulk_append but for high priority messages to be put at the front
 * of the list. */
static void ssend_bulk_prepend(sdata_t *sdata, ckmsg_t *bulk_send, const int messages)
{
	ckmsgq_t *ssends = sdata->ssends;
	ckmsg_t *tmp;

	mutex_lock(ssends->lock);
	tmp = ssends->msgs;
	ssends->msgs = bulk_send;
	ssends->messages += messages;
	DL_CONCAT(ssends->msgs, tmp);
	pthread_cond_signal(ssends->cond);
	mutex_unlock(ssends->lock);
}

/* Send a json msg to an upstream trusted remote server */
static void upstream_json(ckpool_t *ckp, json_t *val)
{
	char *msg;

	msg = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER | JSON_COMPACT | JSON_EOL);
	/* Connector absorbs and frees msg */
	connector_upstream_msg(ckp, msg);
}

/* Upstream a json msgtype */
static void upstream_json_msgtype(ckpool_t *ckp, json_t *val, const int msg_type)
{
	json_set_string(val, "method", stratum_msgs[msg_type]);
	upstream_json(ckp, val);
}

/* Upstream a json msgtype, duplicating the json */
static void upstream_msgtype(ckpool_t *ckp, const json_t *val, const int msg_type)
{
	json_t *json_msg = json_deep_copy(val);

	json_set_string(json_msg, "method", stratum_msgs[msg_type]);
	upstream_json(ckp, json_msg);
	json_decref(json_msg);
}

static void send_node_workinfo(ckpool_t *ckp, sdata_t *sdata, const workbase_t *wb)
{
	stratum_instance_t *client;
	ckmsg_t *bulk_send = NULL;
	int messages = 0;
	json_t *wb_val;

	wb_val = json_object();

	json_set_int(wb_val, "jobid", wb->mapped_id);
	json_set_string(wb_val, "target", wb->target);
	json_set_double(wb_val, "diff", wb->diff);
	json_set_int(wb_val, "version", wb->version);
	json_set_int(wb_val, "curtime", wb->curtime);
	json_set_string(wb_val, "prevhash", wb->prevhash);
	json_set_string(wb_val, "ntime", wb->ntime);
	json_set_string(wb_val, "bbversion", wb->bbversion);
	json_set_string(wb_val, "nbit", wb->nbit);
	json_set_int(wb_val, "coinbasevalue", wb->coinbasevalue);
	json_set_int(wb_val, "height", wb->height);
	json_set_string(wb_val, "flags", wb->flags);
	json_set_int(wb_val, "txns", wb->txns);
	json_set_string(wb_val, "txn_hashes", wb->txn_hashes);
	json_set_int(wb_val, "merkles", wb->merkles);
	json_object_set_new_nocheck(wb_val, "merklehash", json_deep_copy(wb->merkle_array));
	json_set_string(wb_val, "coinb1", wb->coinb1);
	json_set_int(wb_val, "enonce1varlen", wb->enonce1varlen);
	json_set_int(wb_val, "enonce2varlen", wb->enonce2varlen);
	json_set_int(wb_val, "coinb1len", wb->coinb1len);
	json_set_int(wb_val, "coinb2len", wb->coinb2len);
	json_set_string(wb_val, "coinb2", wb->coinb2);

	ck_rlock(&sdata->instance_lock);
	DL_FOREACH2(sdata->node_instances, client, node_next) {
		ckmsg_t *client_msg;
		smsg_t *msg;
		json_t *json_msg = json_deep_copy(wb_val);

		json_set_string(json_msg, "node.method", stratum_msgs[SM_WORKINFO]);
		client_msg = ckalloc(sizeof(ckmsg_t));
		msg = ckzalloc(sizeof(smsg_t));
		msg->json_msg = json_msg;
		msg->client_id = client->id;
		client_msg->data = msg;
		DL_APPEND(bulk_send, client_msg);
		messages++;
	}
	DL_FOREACH2(sdata->remote_instances, client, remote_next) {
		ckmsg_t *client_msg;
		smsg_t *msg;
		json_t *json_msg = json_deep_copy(wb_val);

		json_set_string(json_msg, "method", stratum_msgs[SM_WORKINFO]);
		client_msg = ckalloc(sizeof(ckmsg_t));
		msg = ckzalloc(sizeof(smsg_t));
		msg->json_msg = json_msg;
		msg->client_id = client->id;
		client_msg->data = msg;
		DL_APPEND(bulk_send, client_msg);
		messages++;
	}
	ck_runlock(&sdata->instance_lock);

	if (ckp->remote)
		upstream_msgtype(ckp, wb_val, SM_WORKINFO);

	json_decref(wb_val);

	if (bulk_send) {
		LOGINFO("Sending workinfo to mining nodes");
		ssend_bulk_append(sdata, bulk_send, messages);
	}
}

static json_t *generate_workinfo(ckpool_t *ckp, const workbase_t *wb, const char *func)
{
	char cdfield[64];
	json_t *val;

	sprintf(cdfield, "%lu,%lu", wb->gentime.tv_sec, wb->gentime.tv_nsec);

	JSON_CPACK(val, "{sI,ss,ss,ss,ss,ss,ss,ss,ss,sI,so,ss,ss,ss,ss}",
			"workinfoid", wb->id,
			"poolinstance", ckp->name,
			"transactiontree", wb->txn_hashes,
			"prevhash", wb->prevhash,
			"coinbase1", wb->coinb1,
			"coinbase2", wb->coinb2,
			"version", wb->bbversion,
			"ntime", wb->ntime,
			"bits", wb->nbit,
			"reward", wb->coinbasevalue,
			"merklehash", json_deep_copy(wb->merkle_array),
			"createdate", cdfield,
			"createby", "code",
			"createcode", func,
			"createinet", ckp->serverurl[0]);
	return val;
}

static void send_workinfo(ckpool_t *ckp, sdata_t *sdata, const workbase_t *wb)
{
	if (!ckp->proxy)
		send_node_workinfo(ckp, sdata, wb);
}

/* Entered with instance_lock held, make sure wb can't be pulled from us */
static void __generate_userwb(sdata_t *sdata, workbase_t *wb, user_instance_t *user)
{
	struct userwb *userwb;
	int64_t id = wb->id;

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

static void generate_userwbs(sdata_t *sdata, workbase_t *wb)
{
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
static void add_base(ckpool_t *ckp, sdata_t *sdata, workbase_t *wb, bool *new_block)
{
	sdata_t *ckp_sdata = ckp->sdata;
	pool_stats_t *stats = &sdata->stats;
	double old_diff = stats->network_diff;
	workbase_t *tmp, *tmpa;
	int len, ret;

	ts_realtime(&wb->gentime);
	/* Stats network_diff is not protected by lock but is not a critical
	 * value */
	wb->network_diff = diff_from_nbits(wb->headerbin + 72);
	if (wb->network_diff < 1)
		wb->network_diff = 1;
	stats->network_diff = wb->network_diff;
	if (stats->network_diff != old_diff)
		LOGWARNING("Network diff set to %.1f", stats->network_diff);
	len = strlen(ckp->logdir) + 8 + 1 + 16 + 1;
	wb->logdir = ckzalloc(len);

	/* In proxy mode, the wb->id is received in the notify update and
	 * we set workbase_id from it. In server mode the stratifier is
	 * setting the workbase_id */
	ck_wlock(&sdata->workbase_lock);
	ckp_sdata->workbases_generated++;
	if (!ckp->proxy)
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
	if (*new_block && ckp->logshares) {
		sprintf(wb->logdir, "%s%08x/", ckp->logdir, wb->height);
		ret = mkdir(wb->logdir, 0750);
		if (unlikely(ret && errno != EEXIST))
			LOGERR("Failed to create log directory %s", wb->logdir);
	}
	sprintf(wb->idstring, "%016lx", wb->id);
	if (ckp->logshares)
		sprintf(wb->logdir, "%s%08x/%s", ckp->logdir, wb->height, wb->idstring);

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
			clear_workbase(ckp, tmp);

			ck_wlock(&sdata->workbase_lock);
		}
	}
	ck_wunlock(&sdata->workbase_lock);

	/* This wb can't be pulled out from under us so no workbase lock is
	 * required to generate_userwbs */
	if (ckp->btcsolo)
		generate_userwbs(sdata, wb);

	if (*new_block)
		purge_share_hashtable(sdata, wb->id);

	if (!ckp->passthrough)
		send_workinfo(ckp, sdata, wb);
}

static void broadcast_ping(sdata_t *sdata);

#define REFCOUNT_REMOTE		20
#define REFCOUNT_LOCAL		10
#define REFCOUNT_RETURNED	5

/* Submit the transactions in node/remote mode so the local btcd has all the
 * transactions that will go into the next blocksolve. */
static void submit_transaction(ckpool_t *ckp, const char *hash)
{
	char *buf;

	if (unlikely(!ckp->generator_ready))
		return;
	ASPRINTF(&buf, "submittxn:%s", hash);
	send_proc(ckp->generator,buf);
	free(buf);
}

/* Build a hashlist of all transactions, allowing us to compare with the list of
 * existing transactions to determine which need to be propagated */
static bool add_txn(ckpool_t *ckp, sdata_t *sdata, txntable_t **txns, const char *hash,
		    const char *data, bool local)
{
	bool found = false;
	txntable_t *txn;

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
		txn->data = generator_get_txn(ckp, hash);
		if (!txn->data) {
			/* If our local bitcoind hasn't seen this transaction,
			 * submit it for mempools to be ~synchronised */
			submit_transaction(ckp, data);
			txn->data = strdup(data);
		}
	}

	txn->seen = true;
	if (!local || ckp->node)
		txn->refcount = REFCOUNT_REMOTE;
	else
		txn->refcount = REFCOUNT_LOCAL;
	HASH_ADD_STR(*txns, hash, txn);

	return true;
}

static void send_node_transactions(ckpool_t *ckp, sdata_t *sdata, const json_t *txn_val)
{
	stratum_instance_t *client;
	ckmsg_t *bulk_send = NULL;
	ckmsg_t *client_msg;
	int messages = 0;
	json_t *json_msg;
	smsg_t *msg;

	ck_rlock(&sdata->instance_lock);
	DL_FOREACH2(sdata->node_instances, client, node_next) {
		json_msg = json_deep_copy(txn_val);
		json_set_string(json_msg, "node.method", stratum_msgs[SM_TRANSACTIONS]);
		client_msg = ckalloc(sizeof(ckmsg_t));
		msg = ckzalloc(sizeof(smsg_t));
		msg->json_msg = json_msg;
		msg->client_id = client->id;
		client_msg->data = msg;
		DL_APPEND(bulk_send, client_msg);
		messages++;
	}
	DL_FOREACH2(sdata->remote_instances, client, remote_next) {
		json_msg = json_deep_copy(txn_val);
		json_set_string(json_msg, "method", stratum_msgs[SM_TRANSACTIONS]);
		client_msg = ckalloc(sizeof(ckmsg_t));
		msg = ckzalloc(sizeof(smsg_t));
		msg->json_msg = json_msg;
		msg->client_id = client->id;
		client_msg->data = msg;
		DL_APPEND(bulk_send, client_msg);
		messages++;
	}
	ck_runlock(&sdata->instance_lock);

	if (ckp->remote)
		upstream_msgtype(ckp, txn_val, SM_TRANSACTIONS);

	if (bulk_send) {
		LOGINFO("Sending transactions to mining nodes");
		ssend_bulk_append(sdata, bulk_send, messages);
	}
}

static void submit_transaction_array(ckpool_t *ckp, const json_t *arr)
{
	json_t *arr_val;
	size_t index;

	json_array_foreach(arr, index, arr_val) {
		submit_transaction(ckp, json_string_value(arr_val));
	}
}

static void clear_txn(txntable_t *txn)
{
	free(txn->data);
	free(txn);
}

static void update_txns(ckpool_t *ckp, sdata_t *sdata, txntable_t *txns, bool local)
{
	json_t *val, *txn_array = json_array(), *purged_txns = json_array();
	int added = 0, purged = 0;
	txntable_t *tmp, *tmpa;

	/* Find which transactions have their refcount decremented to zero
	 * and remove them. */
	ck_wlock(&sdata->txn_lock);
	HASH_ITER(hh, sdata->txns, tmp, tmpa) {
		json_t *txn_val;

		if (tmp->seen) {
			tmp->seen = false;
			continue;
		}
		if (tmp->refcount-- > 0)
			continue;
		HASH_DEL(sdata->txns, tmp);
		txn_val = json_string(tmp->data);
		json_array_append_new(purged_txns, txn_val);
		clear_txn(tmp);
		purged++;
	}
	/* Add the new transactions to the transaction table */
	HASH_ITER(hh, txns, tmp, tmpa) {
		txntable_t *found;
		json_t *txn_val;

		HASH_DEL(txns, tmp);
		/* Propagate transaction here */
		JSON_CPACK(txn_val, "{ss,ss}", "hash", tmp->hash, "data", tmp->data);
		json_array_append_new(txn_array, txn_val);

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
		JSON_CPACK(val, "{so}", "transaction", txn_array);
		send_node_transactions(ckp, sdata, val);
		json_decref(val);
	} else
		json_decref(txn_array);

	/* Submit transactions to bitcoind again when we're purging them in
	 * case they've been removed from its mempool as well and we need them
	 * again in the future for a remote workinfo that hasn't forgotten
	 * about them. */
	if (purged && ckp->nodeservers)
		submit_transaction_array(ckp, purged_txns);
	json_decref(purged_txns);

	if (added || purged) {
		LOGINFO("Stratifier added %d %stransactions and purged %d", added,
			local ? "" : "remote ", purged);
	}
}

/* Distill down a set of transactions into an efficient tree arrangement for
 * stratum messages and fast work assembly. */
static txntable_t *wb_merkle_bin_txns(ckpool_t *ckp, sdata_t *sdata, workbase_t *wb,
				      json_t *txn_array, bool local)
{
	int i, j, binleft, binlen;
	txntable_t *txns = NULL;
	json_t *arr_val;
	uchar *hashbin;

	wb->txns = json_array_size(txn_array);
	wb->merkles = 0;
	binlen = wb->txns * 32 + 32;
	hashbin = alloca(binlen + 32);
	memset(hashbin, 0, 32);
	binleft = binlen / 32;
	if (wb->txns) {
		int len = 1, ofs = 0;
		const char *txn;

		for (i = 0; i < wb->txns; i++) {
			arr_val = json_array_get(txn_array, i);
			txn = json_string_value(json_object_get(arr_val, "data"));
			if (!txn) {
				LOGWARNING("json_string_value fail - cannot find transaction data");
				goto out;
			}
			len += strlen(txn);
		}

		wb->txn_data = ckzalloc(len + 1);
		wb->txn_hashes = ckzalloc(wb->txns * 65 + 1);
		memset(wb->txn_hashes, 0x20, wb->txns * 65); // Spaces

		for (i = 0; i < wb->txns; i++) {
			const char *txid, *hash;
			char binswap[32];

			arr_val = json_array_get(txn_array, i);

			// Post-segwit, txid returns the tx hash without witness data
			txid = json_string_value(json_object_get(arr_val, "txid"));
			hash = json_string_value(json_object_get(arr_val, "hash"));
			if (!txid)
				txid = hash;
			if (unlikely(!txid)) {
				LOGERR("Missing txid for transaction in wb_merkle_bins");
				goto out;
			}
			txn = json_string_value(json_object_get(arr_val, "data"));
			add_txn(ckp, sdata, &txns, hash, txn, local);
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
	wb->merkle_array = json_array();
	if (binleft > 1) {
		while (42) {
			if (binleft == 1)
				break;
			memcpy(&wb->merklebin[wb->merkles][0], hashbin + 32, 32);
			__bin2hex(&wb->merklehash[wb->merkles][0], &wb->merklebin[wb->merkles][0], 32);
			json_array_append_new(wb->merkle_array, json_string(&wb->merklehash[wb->merkles][0]));
			LOGDEBUG("MerkleHash %d %s",wb->merkles, &wb->merklehash[wb->merkles][0]);
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
	LOGNOTICE("Stored %s workbase with %d transactions", local ? "local" : "remote",
		  wb->txns);
out:
	return txns;
}

static const unsigned char witness_nonce[32] = {0};
static const int witness_nonce_size = sizeof(witness_nonce);
static const unsigned char witness_header[] = {0xaa, 0x21, 0xa9, 0xed};
static const int witness_header_size = sizeof(witness_header);

static void gbt_witness_data(workbase_t *wb, json_t *txn_array)
{
	int i, binlen, txncount = json_array_size(txn_array);
	const char* hash;
	json_t *arr_val;
	uchar *hashbin;

	binlen = txncount * 32 + 32;
	hashbin = alloca(binlen + 32);
	memset(hashbin, 0, 32);

	for (i = 0; i < txncount; i++) {
		char binswap[32];

		arr_val = json_array_get(txn_array, i);
		hash = json_string_value(json_object_get(arr_val, "hash"));
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
	for (txncount++ ; txncount > 1 ; txncount /= 2) {
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

/* This function assumes it will only receive a valid json gbt base template
 * since checking should have been done earlier, and creates the base template
 * for generating work templates. This is a ckmsgq so all uses of this function
 * are serialised. */
static void block_update(ckpool_t *ckp, int *prio)
{
	bool new_block = false, ret = false;
	const char *witnessdata_check;
	sdata_t *sdata = ckp->sdata;
	json_t *txn_array;
	txntable_t *txns;
	int retries = 0;
	workbase_t *wb;

retry:
	wb = generator_getbase(ckp);
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

	wb->ckp = ckp;

	txn_array = json_object_get(wb->json, "transactions");
	txns = wb_merkle_bin_txns(ckp, sdata, wb, txn_array, true);

	wb->insert_witness = false;

	witnessdata_check = json_string_value(json_object_get(wb->json, "default_witness_commitment"));
	if (likely(witnessdata_check)) {
		LOGDEBUG("Default witness commitment present, adding witness data");
		gbt_witness_data(wb, txn_array);
		// Verify against the pre-calculated value if it exists. Skip the size/OP_RETURN bytes.
		if (wb->insert_witness && safecmp(witnessdata_check + 4, wb->witnessdata) != 0)
			LOGERR("Witness from btcd: %s. Calculated Witness: %s", witnessdata_check + 4, wb->witnessdata);
	}

	generate_coinbase(ckp, wb);

	add_base(ckp, sdata, wb, &new_block);

	if (new_block)
		LOGNOTICE("Block hash changed to %s", sdata->lastswaphash);
	if (ckp->btcsolo)
		stratum_broadcast_updates(sdata, new_block);
	else
		stratum_broadcast_update(sdata, wb, new_block);
	ret = true;
	LOGINFO("Broadcast updated stratum base");
	/* Update transactions after stratum broadcast to not delay
	 * propagation. */
	if (likely(txns))
		update_txns(ckp, sdata, txns, true);
	/* Reset the update time to avoid stacked low priority notifies. Bring
	 * forward the next notify in case of a new block. */
	sdata->update_time = time(NULL);
	if (new_block)
		sdata->update_time -= ckp->update_interval / 2;
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

#define SSEND_PREPEND	0
#define SSEND_APPEND	1

/* Downstream a json message to all remote servers except for the one matching
 * client_id */
static void downstream_json(sdata_t *sdata, const json_t *val, const int64_t client_id,
			    const int prio)
{
	stratum_instance_t *client;
	ckmsg_t *bulk_send = NULL;
	int messages = 0;

	ck_rlock(&sdata->instance_lock);
	DL_FOREACH2(sdata->remote_instances, client, remote_next) {
		ckmsg_t *client_msg;
		json_t *json_msg;
		smsg_t *msg;

		/* Don't send remote workinfo back to same remote */
		if (client->id == client_id)
			continue;
		json_msg = json_deep_copy(val);
		client_msg = ckalloc(sizeof(ckmsg_t));
		msg = ckzalloc(sizeof(smsg_t));
		msg->json_msg = json_msg;
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
static void request_txns(ckpool_t *ckp, sdata_t *sdata, json_t *txns)
{
	json_t *val;

	JSON_CPACK(val, "{so}", "hash", txns);
	if (ckp->remote)
		upstream_msgtype(ckp, val, SM_REQTXNS);
	else if (ckp->node) {
		/* Nodes have no way to signal upstream pool yet */
	} else {
		/* We don't know which remote sent the transaction hash so ask
		 * all of them for it */
		json_set_string(val, "method", stratum_msgs[SM_REQTXNS]);
		downstream_json(sdata, val, 0, SSEND_APPEND);
	}
}

/* Rebuilds transactions from txnhashes to be able to construct wb_merkle_bins
 * on remote workbases */
static bool rebuild_txns(ckpool_t *ckp, sdata_t *sdata, workbase_t *wb)
{
	const char *hashes = wb->txn_hashes;
	json_t *txn_array, *missing_txns;
	char hash[68] = {};
	bool ret = false;
	txntable_t *txns;
	int i, len = 0;

	/* We'll only see this on testnet now */
	if (unlikely(!wb->txns)) {
		ret = true;
		goto out;
	}
	if (likely(hashes))
		len = strlen(hashes);
	if (!hashes || !len)
		goto out;

	if (unlikely(len < wb->txns * 65)) {
		LOGERR("Truncated transactions in rebuild_txns only %d long", len);
		goto out;
	}
	ret = true;
	txn_array = json_array();
	missing_txns = json_array();

	for (i = 0; i < wb->txns; i++) {
		json_t *txn_val = NULL;
		txntable_t *txn;
		char *data;

		memcpy(hash, hashes + i * 65, 64);

		ck_wlock(&sdata->txn_lock);
		HASH_FIND_STR(sdata->txns, hash, txn);
		if (likely(txn)) {
			txn->refcount = REFCOUNT_REMOTE;
			txn->seen = true;
			JSON_CPACK(txn_val, "{ss,ss}",
				   "hash", hash, "data", txn->data);
			json_array_append_new(txn_array, txn_val);
		}
		ck_wunlock(&sdata->txn_lock);

		if (likely(txn_val))
			continue;
		/* See if we can find it in our local bitcoind */
		data = generator_get_txn(ckp, hash);
		if (!data) {
			txn_val = json_string(hash);
			json_array_append_new(missing_txns, txn_val);
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
		JSON_CPACK(txn_val, "{ss,ss}",
			   "hash", hash, "data", txn->data);
		json_array_append_new(txn_array, txn_val);
		ck_wunlock(&sdata->txn_lock);
	}

	if (ret) {
		wb->incomplete = false;
		LOGINFO("Rebuilt txns into workbase with %d transactions", i);
		/* These two structures are regenerated so free their ram */
		json_decref(wb->merkle_array);
		dealloc(wb->txn_hashes);
		txns = wb_merkle_bin_txns(ckp, sdata, wb, txn_array, false);
		if (likely(txns))
			update_txns(ckp, sdata, txns, false);
	} else {
		if (!sdata->wbincomplete) {
			sdata->wbincomplete = true;
			if (ckp->proxy)
				LOGWARNING("Unable to rebuild transactions to create workinfo, ignore displayed hashrate");
		}
		LOGINFO("Failed to find all txns in rebuild_txns");
		request_txns(ckp, sdata, missing_txns);
	}

	json_decref(txn_array);
	json_decref(missing_txns);
out:
	return ret;
}

/* Remote workbases are keyed by the combined values of wb->id and
 * wb->client_id to prevent collisions in the unlikely event two remote
 * servers are generating the same workbase ids. */
static void __add_to_remote_workbases(sdata_t *sdata, workbase_t *wb)
{
	HASH_ADD(hh, sdata->remote_workbases, id, sizeof(int64_t) * 2, wb);
}

static void add_remote_base(ckpool_t *ckp, sdata_t *sdata, workbase_t *wb)
{
	stratum_instance_t *client;
	ckmsg_t *bulk_send = NULL;
	workbase_t *tmp, *tmpa;
	int messages = 0;
	int64_t skip;
	json_t *val;

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

			clear_workbase(ckp, tmp);

			ck_wlock(&sdata->workbase_lock);
		}
	}
	__add_to_remote_workbases(sdata, wb);
	ck_wunlock(&sdata->workbase_lock);

	val = generate_workinfo(ckp, wb, __func__);

	/* Set jobid with mapped id for other nodes and remotes */
	json_set_int64(val, "jobid", wb->mapped_id);

	/* Replace workinfoid to mapped id */
	json_set_int64(val, "workinfoid", wb->mapped_id);

	/* Strip unnecessary fields and add extra fields needed */
	json_set_int(val, "txns", wb->txns);
	json_set_string(val, "txn_hashes", wb->txn_hashes);
	json_set_int(val, "merkles", wb->merkles);

	skip = subclient(wb->client_id);

	/* Send a copy of this to all OTHER remote trusted servers as well */
	ck_rlock(&sdata->instance_lock);
	DL_FOREACH2(sdata->remote_instances, client, remote_next) {
		ckmsg_t *client_msg;
		json_t *json_msg;
		smsg_t *msg;

		/* Don't send remote workinfo back to the source remote */
		if (client->id == wb->client_id)
			continue;
		json_msg = json_deep_copy(val);
		json_set_string(json_msg, "method", stratum_msgs[SM_WORKINFO]);
		client_msg = ckalloc(sizeof(ckmsg_t));
		msg = ckzalloc(sizeof(smsg_t));
		msg->json_msg = json_msg;
		msg->client_id = client->id;
		client_msg->data = msg;
		DL_APPEND(bulk_send, client_msg);
		messages++;
	}
	DL_FOREACH2(sdata->node_instances, client, node_next) {
		ckmsg_t *client_msg;
		json_t *json_msg;
		smsg_t *msg;

		/* Don't send node workinfo back to the source node */
		if (client->id == skip)
			continue;
		json_msg = json_deep_copy(val);
		json_set_string(json_msg, "node.method", stratum_msgs[SM_WORKINFO]);
		client_msg = ckalloc(sizeof(ckmsg_t));
		msg = ckzalloc(sizeof(smsg_t));
		msg->json_msg = json_msg;
		msg->client_id = client->id;
		client_msg->data = msg;
		DL_APPEND(bulk_send, client_msg);
		messages++;
	}
	ck_runlock(&sdata->instance_lock);

	json_decref(val);

	if (bulk_send) {
		LOGINFO("Sending remote workinfo to %d other remote servers", messages);
		ssend_bulk_append(sdata, bulk_send, messages);
	}
}

static void add_node_base(ckpool_t *ckp, json_t *val, bool trusted, int64_t client_id)
{
	workbase_t *wb = ckzalloc(sizeof(workbase_t));
	sdata_t *sdata = ckp->sdata;
	bool new_block = false;
	char header[272];

	wb->ckp = ckp;
	/* This is the client id if this workbase came from a remote trusted
	 * server. */
	wb->client_id = client_id;

	/* Some of these fields are empty when running as a remote trusted
	 * server receiving other workinfos from the upstream pool */
	json_int64cpy(&wb->id, val, "jobid");
	json_strcpy(wb->target, val, "target");
	json_dblcpy(&wb->diff, val, "diff");
	json_uintcpy(&wb->version, val, "version");
	json_uintcpy(&wb->curtime, val, "curtime");
	json_strcpy(wb->prevhash, val, "prevhash");
	json_strcpy(wb->ntime, val, "ntime");
	sscanf(wb->ntime, "%x", &wb->ntime32);
	json_strcpy(wb->bbversion, val, "bbversion");
	json_strcpy(wb->nbit, val, "nbit");
	json_uint64cpy(&wb->coinbasevalue, val, "coinbasevalue");
	json_intcpy(&wb->height, val, "height");
	json_strdup(&wb->flags, val, "flags");

	json_intcpy(&wb->txns, val, "txns");
	json_strdup(&wb->txn_hashes, val, "txn_hashes");
	if (!ckp->proxy) {
		/* This is a workbase from a trusted remote */
		wb->merkle_array = json_object_dup(val, "merklehash");
		json_intcpy(&wb->merkles, val, "merkles");
		if (!rebuild_txns(ckp, sdata, wb))
			wb->incomplete = true;
	} else {
		if (!rebuild_txns(ckp, sdata, wb)) {
			clear_workbase(ckp, wb);
			return;
		}
	}
	json_strdup(&wb->coinb1, val, "coinb1");
	json_intcpy(&wb->coinb1len, val, "coinb1len");
	wb->coinb1bin = ckzalloc(wb->coinb1len);
	hex2bin(wb->coinb1bin, wb->coinb1, wb->coinb1len);
	json_strdup(&wb->coinb2, val, "coinb2");
	json_intcpy(&wb->coinb2len, val, "coinb2len");
	wb->coinb2bin = ckzalloc(wb->coinb2len);
	hex2bin(wb->coinb2bin, wb->coinb2, wb->coinb2len);
	json_intcpy(&wb->enonce1varlen, val, "enonce1varlen");
	json_intcpy(&wb->enonce2varlen, val, "enonce2varlen");
	ts_realtime(&wb->gentime);

	snprintf(header, 270, "%s%s%s%s%s%s%s",
		 wb->bbversion, wb->prevhash,
		 "0000000000000000000000000000000000000000000000000000000000000000",
		 wb->ntime, wb->nbit,
		 "00000000", /* nonce */
		 workpadding);
	header[224] = 0;
	LOGDEBUG("Header: %s", header);
	hex2bin(wb->headerbin, header, 112);

	/* If this is from a remote trusted server or an upstream server, add
	 * it to the remote_workbases hashtable */
	if (trusted)
		add_remote_base(ckp, sdata, wb);
	else
		add_base(ckp, sdata, wb, &new_block);

	if (new_block)
		LOGNOTICE("Block hash changed to %s", sdata->lastswaphash);
}

/* Calculate share diff and fill in hash and swap. Need to hold workbase read count */
static double
share_diff(char *coinbase, const uchar *enonce1bin, const workbase_t *wb, const char *nonce2,
	   const uint32_t ntime32, uint32_t version_mask, const char *nonce,
	   uchar *hash, uchar *swap, int *cblen)
{
	unsigned char merkle_root[32], merkle_sha[64];
	uint32_t *data32, *swap32, benonce32;
	uchar hash1[32];
	char data[80];
	int i;

	memcpy(coinbase, wb->coinb1bin, wb->coinb1len);
	*cblen = wb->coinb1len;
	memcpy(coinbase + *cblen, enonce1bin, wb->enonce1constlen + wb->enonce1varlen);
	*cblen += wb->enonce1constlen + wb->enonce1varlen;
	hex2bin(coinbase + *cblen, nonce2, wb->enonce2varlen);
	*cblen += wb->enonce2varlen;
	memcpy(coinbase + *cblen, wb->coinb2bin, wb->coinb2len);
	*cblen += wb->coinb2len;

	gen_hash((uchar *)coinbase, merkle_root, *cblen);
	memcpy(merkle_sha, merkle_root, 32);
	for (i = 0; i < wb->merkles; i++) {
		memcpy(merkle_sha + 32, &wb->merklebin[i], 32);
		gen_hash(merkle_sha, merkle_root, 64);
		memcpy(merkle_sha, merkle_root, 32);
	}
	data32 = (uint32_t *)merkle_sha;
	swap32 = (uint32_t *)merkle_root;
	flip_32(swap32, data32);

	/* Copy the cached header binary and insert the merkle root */
	memcpy(data, wb->headerbin, 80);
	memcpy(data + 36, merkle_root, 32);

	/* Update nVersion when version_mask is in use */
	if (version_mask) {
		version_mask = htobe32(version_mask);
		data32 = (uint32_t *)data;
		*data32 |= version_mask;
	}

	/* Insert the nonce value into the data */
	hex2bin(&benonce32, nonce, 4);
	data32 = (uint32_t *)(data + 64 + 12);
	*data32 = benonce32;

	/* Insert the ntime value into the data */
	data32 = (uint32_t *)(data + 68);
	*data32 = htobe32(ntime32);

	/* Hash the share */
	data32 = (uint32_t *)data;
	swap32 = (uint32_t *)swap;
	flip_80(swap32, data32);
	sha256(swap, 80, hash1);
	sha256(hash1, 32, hash);

	/* Calculate the diff of the share here */
	return diff_from_target(hash);
}

static void add_remote_blockdata(ckpool_t *ckp, json_t *val, const int cblen, const char *coinbase,
				 const uchar *data)
{
	char *buf;

	json_set_string(val, "name", ckp->name);
	json_set_int(val, "cblen", cblen);
	buf = bin2hex(coinbase, cblen);
	json_set_string(val, "coinbasehex", buf);
	free(buf);
	buf = bin2hex(data, 80);
	json_set_string(val, "swaphex", buf);
	free(buf);
}

/* Entered with workbase readcount, grabs instance_lock. client_id is where the
 * block originated. */
static void send_nodes_block(sdata_t *sdata, const json_t *block_val, const int64_t client_id)
{
	stratum_instance_t *client;
	ckmsg_t *bulk_send = NULL;
	int messages = 0;
	int64_t skip;

	/* Don't send the block back to a remote node if that's where it was
	 * found. */
	skip = subclient(client_id);

	ck_rlock(&sdata->instance_lock);
	DL_FOREACH2(sdata->node_instances, client, node_next) {
		ckmsg_t *client_msg;
		json_t *json_msg;
		smsg_t *msg;

		if (client->id == skip)
			continue;
		json_msg = json_deep_copy(block_val);
		json_set_string(json_msg, "node.method", stratum_msgs[SM_BLOCK]);
		client_msg = ckalloc(sizeof(ckmsg_t));
		msg = ckzalloc(sizeof(smsg_t));
		msg->json_msg = json_msg;
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
static void send_node_block(ckpool_t *ckp, sdata_t *sdata, const char *enonce1, const char *nonce,
			    const char *nonce2, const uint32_t ntime32, const uint32_t version_mask,
			    const int64_t jobid, const double diff, const int64_t client_id,
			    const char *coinbase, const int cblen, const uchar *data)
{
	if (sdata->node_instances) {
		json_t *val = json_object();

		json_set_string(val, "enonce1", enonce1);
		json_set_string(val, "nonce", nonce);
		json_set_string(val, "nonce2", nonce2);
		json_set_uint32(val, "ntime32", ntime32);
		json_set_uint32(val, "version_mask", version_mask);
		json_set_int64(val, "jobid", jobid);
		json_set_double(val, "diff", diff);
		add_remote_blockdata(ckp, val, cblen, coinbase, data);
		send_nodes_block(sdata, val, client_id);
		json_decref(val);
	}
}

/* Process a block into a message for the generator to submit. Must hold
 * workbase readcount */
static char *
process_block(const workbase_t *wb, const char *coinbase, const int cblen,
	      const uchar *data, const uchar *hash, uchar *flip32, char *blockhash)
{
	char *gbt_block, varint[12];
	int txns = wb->txns + 1;
	char hexcoinbase[1024];

	flip_32(flip32, hash);
	__bin2hex(blockhash, flip32, 32);

	/* Message format: "data" */
	gbt_block = ckzalloc(1024);
	__bin2hex(gbt_block, data, 80);
	if (txns < 0xfd) {
		uint8_t val8 = txns;

		__bin2hex(varint, (const unsigned char *)&val8, 1);
	} else if (txns <= 0xffff) {
		uint16_t val16 = htole16(txns);

		strcat(gbt_block, "fd");
		__bin2hex(varint, (const unsigned char *)&val16, 2);
	} else {
		uint32_t val32 = htole32(txns);

		strcat(gbt_block, "fe");
		__bin2hex(varint, (const unsigned char *)&val32, 4);
	}
	strcat(gbt_block, varint);
	__bin2hex(hexcoinbase, coinbase, cblen);
	strcat(gbt_block, hexcoinbase);
	if (wb->txns)
		realloc_strcat(&gbt_block, wb->txn_data);
	return gbt_block;
}

/* Submit block data locally, absorbing and freeing gbt_block */
static bool local_block_submit(ckpool_t *ckp, char *gbt_block, const uchar *flip32, int height)
{
	bool ret = generator_submitblock(ckp, gbt_block);
	char heighthash[68] = {}, rhash[68] = {};
	uchar swap256[32];

	free(gbt_block);
	swap_256(swap256, flip32);
	__bin2hex(rhash, swap256, 32);
	generator_preciousblock(ckp, rhash);

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
		if (generator_get_blockhash(ckp, height, heighthash)) {
			ret = !strncmp(rhash, heighthash, 64);
			LOGWARNING("Hash for forced possibly stale block, height %d confirms block was %s",
				   height, ret ? "ACCEPTED" : "REJECTED");
		}
	}
	return ret;
}

static workbase_t *get_workbase(sdata_t *sdata, const int64_t id)
{
	workbase_t *wb;

	ck_wlock(&sdata->workbase_lock);
	HASH_FIND_I64(sdata->workbases, &id, wb);
	if (wb)
		wb->readcount++;
	ck_wunlock(&sdata->workbase_lock);

	return wb;
}

static workbase_t *__find_remote_workbase(sdata_t *sdata, const int64_t id, const int64_t client_id)
{
	int64_t lookup[2] = {id, client_id};
	workbase_t *wb;

	HASH_FIND(hh, sdata->remote_workbases, lookup, sizeof(int64_t) * 2, wb);
	return wb;
}

static workbase_t *get_remote_workbase(sdata_t *sdata, const int64_t id, const int64_t client_id)
{
	workbase_t *wb;

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

static void put_workbase(sdata_t *sdata, workbase_t *wb)
{
	ck_wlock(&sdata->workbase_lock);
	wb->readcount--;
	ck_wunlock(&sdata->workbase_lock);
}

#define put_remote_workbase(sdata, wb) put_workbase(sdata, wb)

static void block_solve(ckpool_t *ckp, json_t *val);
static void block_reject(json_t *val);

static void submit_node_block(ckpool_t *ckp, sdata_t *sdata, json_t *val)
{
	char *coinbase = NULL, *enonce1 = NULL, *nonce = NULL, *nonce2 = NULL, *gbt_block,
		*coinbasehex, *swaphex;
	uchar *enonce1bin = NULL, hash[32], swap[80], flip32[32];
	uint32_t ntime32, version_mask = 0;
	char blockhash[68], cdfield[64];
	int enonce1len, cblen;
	workbase_t *wb = NULL;
	json_t *bval;
	double diff;
	ts_t ts_now;
	int64_t id;
	bool ret;

	if (unlikely(!json_get_string(&enonce1, val, "enonce1"))) {
		LOGWARNING("Failed to get enonce1 from node method block");
		goto out;
	}
	if (unlikely(!json_get_string(&nonce, val, "nonce"))) {
		LOGWARNING("Failed to get nonce from node method block");
		goto out;
	}
	if (unlikely(!json_get_string(&nonce2, val, "nonce2"))) {
		LOGWARNING("Failed to get nonce2 from node method block");
		goto out;
	}
	if (unlikely(!json_get_uint32(&ntime32, val, "ntime32"))) {
		LOGWARNING("Failed to get ntime32 from node method block");
		goto out;
	}
	if (unlikely(!json_get_int64(&id, val, "jobid"))) {
		LOGWARNING("Failed to get jobid from node method block");
		goto out;
	}
	if (unlikely(!json_get_double(&diff, val, "diff"))) {
		LOGWARNING("Failed to get diff from node method block");
		goto out;
	}

	if (!json_get_uint32(&version_mask, val, "version_mask")) {
		/* No version mask is not fatal, assume it to be zero */
		LOGINFO("No version mask in node method block");
	}

	LOGWARNING("Possible upstream block solve diff %lf !", diff);

	ts_realtime(&ts_now);
	sprintf(cdfield, "%lu,%lu", ts_now.tv_sec, ts_now.tv_nsec);

	wb = get_workbase(sdata, id);
	if (unlikely(!wb)) {
		LOGWARNING("Failed to find workbase with jobid %"PRId64" in node method block", id);
		goto out;
	}

	/* Get parameters if upstream pool supports them with new format */
	json_get_string(&coinbasehex, val, "coinbasehex");
	json_get_int(&cblen, val, "cblen");
	json_get_string(&swaphex, val, "swaphex");
	if (coinbasehex && cblen && swaphex) {
		uchar hash1[32];

		coinbase = alloca(cblen);
		hex2bin(coinbase, coinbasehex, cblen);
		hex2bin(swap, swaphex, 80);
		sha256(swap, 80, hash1);
		sha256(hash1, 32, hash);
	} else {
		/* Rebuild the old way if we can if the upstream pool is using
		 * the old format only */
		enonce1len = wb->enonce1constlen + wb->enonce1varlen;
		enonce1bin = alloca(enonce1len);
		hex2bin(enonce1bin, enonce1, enonce1len);
		coinbase = alloca(wb->coinb1len + wb->enonce1constlen + wb->enonce1varlen + wb->enonce2varlen + wb->coinb2len);
		/* Fill in the hashes */
		share_diff(coinbase, enonce1bin, wb, nonce2, ntime32, version_mask, nonce, hash, swap, &cblen);
	}

	/* Now we have enough to assemble a block */
	gbt_block = process_block(wb, coinbase, cblen, swap, hash, flip32, blockhash);
	ret = local_block_submit(ckp, gbt_block, flip32, wb->height);

	JSON_CPACK(bval, "{si,ss,ss,sI,ss,ss,si,ss,sI,sf,ss,ss,ss,ss}",
			 "height", wb->height,
			 "blockhash", blockhash,
			 "confirmed", "n",
			 "workinfoid", wb->id,
			 "enonce1", enonce1,
			 "nonce2", nonce2,
		         "version_mask", version_mask,
			 "nonce", nonce,
			 "reward", wb->coinbasevalue,
			 "diff", diff,
			 "createdate", cdfield,
			 "createby", "code",
			 "createcode", __func__,
			 "createinet", ckp->serverurl[0]);
	put_workbase(sdata, wb);

	if (ret)
		block_solve(ckp, bval);
	else
		block_reject(bval);

	json_decref(bval);
out:
	free(nonce2);
	free(nonce);
	free(enonce1);
}

static void update_base(sdata_t *sdata, const int prio)
{
	int *uprio;

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
static void __kill_instance(sdata_t *sdata, stratum_instance_t *client)
{
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
static void __inc_worker(sdata_t *sdata, user_instance_t *user, worker_instance_t *worker)
{
	sdata->stats.workers++;
	if (!user->workers++)
		sdata->stats.users++;
	worker->instance_count++;
}

static void __dec_worker(sdata_t *sdata, user_instance_t *user, worker_instance_t *worker)
{
	sdata->stats.workers--;
	if (!--user->workers)
		sdata->stats.users--;
	worker->instance_count--;
}

static void __disconnect_session(sdata_t *sdata, const stratum_instance_t *client)
{
	time_t now_t = time(NULL);
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
static void __del_client(sdata_t *sdata, stratum_instance_t *client)
{
	user_instance_t *user = client->user_instance;

	HASH_DEL(sdata->stratum_instances, client);
	if (user) {
		DL_DELETE2(user->clients, client, user_prev, user_next );
		__dec_worker(sdata, user, client->worker_instance);
	}
}

static void connector_drop_client(ckpool_t *ckp, const int64_t id)
{
	char buf[256];

	LOGDEBUG("Stratifier requesting connector drop client %"PRId64, id);
	snprintf(buf, 255, "dropclient=%"PRId64, id);
	send_proc(ckp->connector, buf);
}

static void drop_allclients(ckpool_t *ckp)
{
	stratum_instance_t *client, *tmp;
	sdata_t *sdata = ckp->sdata;
	int kills = 0;

	ck_wlock(&sdata->instance_lock);
	HASH_ITER(hh, sdata->stratum_instances, client, tmp) {
		int64_t client_id = client->id;

		if (!client->ref) {
			__del_client(sdata, client);
			__kill_instance(sdata, client);
		} else
			client->dropped = true;
		kills++;
		connector_drop_client(ckp, client_id);
	}
	sdata->stats.users = sdata->stats.workers = 0;
	ck_wunlock(&sdata->instance_lock);

	if (kills)
		LOGNOTICE("Dropped %d instances for dropall request", kills);
}

/* Copy only the relevant parts of the master sdata for each subproxy */
static sdata_t *duplicate_sdata(const sdata_t *sdata)
{
	sdata_t *dsdata = ckzalloc(sizeof(sdata_t));

	dsdata->ckp = sdata->ckp;

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

static int64_t prio_sort(proxy_t *a, proxy_t *b)
{
	return (a->priority - b->priority);
}

/* Masked increment */
static int64_t masked_inc(int64_t value, int64_t mask)
{
	value &= ~mask;
	value++;
	value |= mask;
	return value;
}

/* Priority values can be sparse, they do not need to be sequential */
static void __set_proxy_prio(sdata_t *sdata, proxy_t *proxy, int64_t priority)
{
	proxy_t *tmpa, *tmpb, *exists = NULL;
	int64_t mask, next_prio = 0;

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

static proxy_t *__generate_proxy(sdata_t *sdata, const int id)
{
	proxy_t *proxy = ckzalloc(sizeof(proxy_t));

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

static proxy_t *__generate_subproxy(sdata_t *sdata, proxy_t *proxy, const int subid)
{
	proxy_t *subproxy = ckzalloc(sizeof(proxy_t));

	subproxy->parent = proxy;
	subproxy->id = proxy->id;
	subproxy->subid = subid;
	HASH_ADD(sh, proxy->subproxies, subid, sizeof(int), subproxy);
	proxy->subproxy_count++;
	subproxy->sdata = duplicate_sdata(sdata);
	subproxy->sdata->subproxy = subproxy;
	return subproxy;
}

static proxy_t *__existing_proxy(const sdata_t *sdata, const int id)
{
	proxy_t *proxy;

	HASH_FIND_INT(sdata->proxies, &id, proxy);
	return proxy;
}

static proxy_t *existing_proxy(sdata_t *sdata, const int id)
{
	proxy_t *proxy;

	mutex_lock(&sdata->proxy_lock);
	proxy = __existing_proxy(sdata, id);
	mutex_unlock(&sdata->proxy_lock);

	return proxy;
}

/* Find proxy by id number, generate one if none exist yet by that id */
static proxy_t *__proxy_by_id(sdata_t *sdata, const int id)
{
	proxy_t *proxy = __existing_proxy(sdata, id);

	if (unlikely(!proxy)) {
		proxy = __generate_proxy(sdata, id);
		LOGNOTICE("Stratifier added new proxy %d", id);
	}

	return proxy;
}

static proxy_t *__existing_subproxy(proxy_t *proxy, const int subid)
{
	proxy_t *subproxy;

	HASH_FIND(sh, proxy->subproxies, &subid, sizeof(int), subproxy);
	return subproxy;
}

static proxy_t *__subproxy_by_id(sdata_t *sdata, proxy_t *proxy, const int subid)
{
	proxy_t *subproxy = __existing_subproxy(proxy, subid);

	if (!subproxy) {
		subproxy = __generate_subproxy(sdata, proxy, subid);
		LOGINFO("Stratifier added new subproxy %d:%d", proxy->id, subid);
	}
	return subproxy;
}

static proxy_t *subproxy_by_id(sdata_t *sdata, const int id, const int subid)
{
	proxy_t *proxy, *subproxy;

	mutex_lock(&sdata->proxy_lock);
	proxy = __proxy_by_id(sdata, id);
	subproxy = __subproxy_by_id(sdata, proxy, subid);
	mutex_unlock(&sdata->proxy_lock);

	return subproxy;
}

static proxy_t *existing_subproxy(sdata_t *sdata, const int id, const int subid)
{
	proxy_t *proxy, *subproxy = NULL;

	mutex_lock(&sdata->proxy_lock);
	proxy = __existing_proxy(sdata, id);
	if (proxy)
		subproxy = __existing_subproxy(proxy, subid);
	mutex_unlock(&sdata->proxy_lock);

	return subproxy;
}

static void check_userproxies(sdata_t *sdata, proxy_t *proxy, const int userid);

static void set_proxy_prio(sdata_t *sdata, proxy_t *proxy, const int priority)
{
	mutex_lock(&sdata->proxy_lock);
	__set_proxy_prio(sdata, proxy, priority);
	mutex_unlock(&sdata->proxy_lock);

	if (!proxy->global)
		check_userproxies(sdata, proxy, proxy->userid);
}

/* Set proxy to the current proxy and calculate how much headroom it has */
static int64_t current_headroom(sdata_t *sdata, proxy_t **proxy)
{
	proxy_t *subproxy, *tmp;
	int64_t headroom = 0;

	mutex_lock(&sdata->proxy_lock);
	*proxy = sdata->proxy;
	if (!*proxy)
		goto out_unlock;
	HASH_ITER(sh, (*proxy)->subproxies, subproxy, tmp) {
		if (subproxy->dead)
			continue;
		headroom += subproxy->max_clients - subproxy->clients;
	}
out_unlock:
	mutex_unlock(&sdata->proxy_lock);

	return headroom;
}

/* Returns the headroom available for more clients of the best alive user proxy
 * for userid. */
static int64_t best_userproxy_headroom(sdata_t *sdata, const int userid)
{
	proxy_t *proxy, *subproxy, *tmp, *subtmp;
	int64_t headroom = 0;

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
			headroom += subproxy->max_clients - subproxy->clients;
		}
		/* Proxies are ordered by priority so first available will be
		 * the best priority */
		if (alive)
			break;
	}
	mutex_unlock(&sdata->proxy_lock);

	return headroom;
}

static void reconnect_client(sdata_t *sdata, stratum_instance_t *client);

static void generator_recruit(ckpool_t *ckp, const int proxyid, const int recruits)
{
	char buf[256];

	sprintf(buf, "recruit=%d:%d", proxyid, recruits);
	LOGINFO("Stratifer requesting %d more subproxies of proxy %d from generator",
		recruits, proxyid);
	send_proc(ckp->generator,buf);
}

/* Find how much headroom we have and connect up to that many clients that are
 * not currently on this pool, recruiting more slots to switch more clients
 * later on lazily. Only reconnect clients bound to global proxies. */
static void reconnect_global_clients(sdata_t *sdata)
{
	stratum_instance_t *client, *tmpclient;
	int reconnects = 0;
	int64_t headroom;
	proxy_t *proxy;

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
		LOGINFO("%d clients flagged for reconnect to global proxy %d",
			reconnects, proxy->id);
	}
	if (headroom < 0)
		generator_recruit(sdata->ckp, proxy->id, -headroom);
}

static bool __subproxies_alive(proxy_t *proxy)
{
	proxy_t *subproxy, *tmp;
	bool alive = false;

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
static void check_bestproxy(sdata_t *sdata)
{
	proxy_t *proxy, *tmp;
	int changed_id = -1;

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

static proxy_t *best_proxy(sdata_t *sdata)
{
	proxy_t *proxy;

	mutex_lock(&sdata->proxy_lock);
	proxy = sdata->proxy;
	mutex_unlock(&sdata->proxy_lock);

	return proxy;
}

static void check_globalproxies(sdata_t *sdata, proxy_t *proxy)
{
	check_bestproxy(sdata);
	if (proxy->parent == best_proxy(sdata)->parent)
		reconnect_global_clients(sdata);
}

static void check_proxy(sdata_t *sdata, proxy_t *proxy)
{
	if (proxy->global)
		check_globalproxies(sdata, proxy);
	else
		check_userproxies(sdata, proxy, proxy->userid);
}

static void dead_proxyid(sdata_t *sdata, const int id, const int subid, const bool replaced, const bool deleted)
{
	stratum_instance_t *client, *tmp;
	int reconnects = 0, proxyid = 0;
	int64_t headroom;
	proxy_t *proxy;

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
		LOGINFO("%d clients flagged to reconnect from dead proxy %d:%d", reconnects,
			id, subid);
	}
	/* When a proxy dies, recruit more of the global proxies for them to
	 * fail over to in case user proxies are unavailable. */
	if (headroom < 0)
		generator_recruit(sdata->ckp, proxyid, -headroom);
}

static void update_subscribe(ckpool_t *ckp, const char *cmd)
{
	sdata_t *sdata = ckp->sdata, *dsdata;
	int id = 0, subid = 0, userid = 0;
	proxy_t *proxy, *old = NULL;
	const char *buf;
	bool global;
	json_t *val;

	if (unlikely(strlen(cmd) < 11)) {
		LOGWARNING("Received zero length string for subscribe in update_subscribe");
		return;
	}
	buf = cmd + 10;
	LOGDEBUG("Update subscribe: %s", buf);
	val = json_loads(buf, 0, NULL);
	if (unlikely(!val)) {
		LOGWARNING("Failed to json decode subscribe response in update_subscribe %s", buf);
		return;
	}
	if (unlikely(!json_get_int(&id, val, "proxy"))) {
		LOGWARNING("Failed to json decode proxy value in update_subscribe %s", buf);
		return;
	}
	if (unlikely(!json_get_int(&subid, val, "subproxy"))) {
		LOGWARNING("Failed to json decode subproxy value in update_subscribe %s", buf);
		return;
	}
	if (unlikely(!json_get_bool(&global, val, "global"))) {
		LOGWARNING("Failed to json decode global value in update_subscribe %s", buf);
		return;
	}
	if (!global) {
		if (unlikely(!json_get_int(&userid, val, "userid"))) {
			LOGWARNING("Failed to json decode userid value in update_subscribe %s", buf);
			return;
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
	proxy->diff = ckp->startdiff;
	memset(proxy->baseurl, 0, 128);
	memset(proxy->url, 0, 128);
	memset(proxy->auth, 0, 128);
	memset(proxy->pass, 0, 128);
	strncpy(proxy->baseurl, json_string_value(json_object_get(val, "baseurl")), 127);
	strncpy(proxy->url, json_string_value(json_object_get(val, "url")), 127);
	strncpy(proxy->auth, json_string_value(json_object_get(val, "auth")), 127);
	strncpy(proxy->pass, json_string_value(json_object_get(val, "pass")), 127);

	dsdata = proxy->sdata;

	ck_wlock(&dsdata->workbase_lock);
	/* Length is checked by generator */
	strcpy(proxy->enonce1, json_string_value(json_object_get(val, "enonce1")));
	proxy->enonce1constlen = strlen(proxy->enonce1) / 2;
	hex2bin(proxy->enonce1bin, proxy->enonce1, proxy->enonce1constlen);
	proxy->nonce2len = json_integer_value(json_object_get(val, "nonce2len"));
	if (ckp->nonce2length) {
		proxy->enonce1varlen = proxy->nonce2len - ckp->nonce2length;
		if (proxy->enonce1varlen < 0)
			proxy->enonce1varlen = 0;
	} else if (proxy->nonce2len > 7)
		proxy->enonce1varlen = 4;
	else if (proxy->nonce2len > 5)
		proxy->enonce1varlen = 2;
	else if (proxy->nonce2len > 3)
		proxy->enonce1varlen = 1;
	else
		proxy->enonce1varlen = 0;
	proxy->enonce2varlen = proxy->nonce2len - proxy->enonce1varlen;
	proxy->max_clients = 1ll << (proxy->enonce1varlen * 8);
	proxy->clients = 0;
	ck_wunlock(&dsdata->workbase_lock);

	if (subid) {
		LOGINFO("Upstream pool %s %d:%d extranonce2 length %d, max proxy clients %"PRId64,
			proxy->url, id, subid, proxy->nonce2len, proxy->max_clients);
	} else {
		LOGNOTICE("Upstream pool %s %d extranonce2 length %d, max proxy clients %"PRId64,
			  proxy->url, id, proxy->nonce2len, proxy->max_clients);
	}
	if (ckp->nonce2length && proxy->enonce2varlen != ckp->nonce2length)
		LOGWARNING("Only able to set nonce2len %d of requested %d on proxy %d:%d",
			   proxy->enonce2varlen, ckp->nonce2length, id, subid);
	json_decref(val);

	/* Set the priority on a new proxy now that we have all the fields
	 * filled in to push it to its correct priority position in the
	 * hashlist. */
	if (!old)
		set_proxy_prio(sdata, proxy, id);

	check_proxy(sdata, proxy);
}

/* Find the highest priority alive proxy belonging to userid and recruit extra
 * subproxies. */
static void recruit_best_userproxy(sdata_t *sdata, const int userid, const int recruits)
{
	proxy_t *proxy, *subproxy, *tmp, *subtmp;
	int id = -1;

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
		generator_recruit(sdata->ckp, id, recruits);
}

/* Check how much headroom the userid proxies have and reconnect any clients
 * that are not bound to it that should be */
static void check_userproxies(sdata_t *sdata, proxy_t *proxy, const int userid)
{
	int64_t headroom = best_userproxy_headroom(sdata, userid);
	stratum_instance_t *client, *tmpclient;
	int reconnects = 0;

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
		if (client->proxy->userid == userid &&
		    client->proxy->parent->priority <= proxy->parent->priority)
			continue;
		if (headroom-- < 1)
			continue;
		reconnects++;
		reconnect_client(sdata, client);
	}
	ck_runlock(&sdata->instance_lock);

	if (reconnects) {
		LOGINFO("%d clients flagged for reconnect to user %d proxies",
			reconnects, userid);
	}
	if (headroom < 0)
		recruit_best_userproxy(sdata, userid, -headroom);
}

static void update_notify(ckpool_t *ckp, const char *cmd)
{
	sdata_t *sdata = ckp->sdata, *dsdata;
	bool new_block = false, clean;
	int i, id = 0, subid = 0;
	char header[272];
	const char *buf;
	proxy_t *proxy;
	workbase_t *wb;
	json_t *val;

	if (unlikely(strlen(cmd) < 8)) {
		LOGWARNING("Zero length string passed to update_notify");
		return;
	}
	buf = cmd + 7; /* "notify=" */
	LOGDEBUG("Update notify: %s", buf);

	val = json_loads(buf, 0, NULL);
	if (unlikely(!val)) {
		LOGWARNING("Failed to json decode in update_notify");
		return;
	}
	json_get_int(&id, val, "proxy");
	json_get_int(&subid, val, "subproxy");
	proxy = existing_subproxy(sdata, id, subid);
	if (unlikely(!proxy || !proxy->subscribed)) {
		LOGINFO("No valid proxy %d:%d subscription to update notify yet", id, subid);
		goto out;
	}
	LOGINFO("Got updated notify for proxy %d:%d", id, subid);

	wb = ckzalloc(sizeof(workbase_t));
	wb->ckp = ckp;
	wb->proxy = true;

	json_get_int64(&wb->id, val, "jobid");
	json_strcpy(wb->prevhash, val, "prevhash");
	json_intcpy(&wb->coinb1len, val, "coinb1len");
	wb->coinb1bin = ckalloc(wb->coinb1len);
	wb->coinb1 = ckalloc(wb->coinb1len * 2 + 1);
	json_strcpy(wb->coinb1, val, "coinbase1");
	hex2bin(wb->coinb1bin, wb->coinb1, wb->coinb1len);
	wb->height = get_sernumber(wb->coinb1bin + 42);
	json_strdup(&wb->coinb2, val, "coinbase2");
	wb->coinb2len = strlen(wb->coinb2) / 2;
	wb->coinb2bin = ckalloc(wb->coinb2len);
	hex2bin(wb->coinb2bin, wb->coinb2, wb->coinb2len);
	wb->merkle_array = json_object_dup(val, "merklehash");
	wb->merkles = json_array_size(wb->merkle_array);
	for (i = 0; i < wb->merkles; i++) {
		strcpy(&wb->merklehash[i][0], json_string_value(json_array_get(wb->merkle_array, i)));
		hex2bin(&wb->merklebin[i][0], &wb->merklehash[i][0], 32);
	}
	json_strcpy(wb->bbversion, val, "bbversion");
	json_strcpy(wb->nbit, val, "nbit");
	json_strcpy(wb->ntime, val, "ntime");
	sscanf(wb->ntime, "%x", &wb->ntime32);
	clean = json_is_true(json_object_get(val, "clean"));
	ts_realtime(&wb->gentime);
	snprintf(header, 270, "%s%s%s%s%s%s%s",
		 wb->bbversion, wb->prevhash,
		 "0000000000000000000000000000000000000000000000000000000000000000",
		 wb->ntime, wb->nbit,
		 "00000000", /* nonce */
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

	add_base(ckp, dsdata, wb, &new_block);
	if (new_block) {
		if (subid)
			LOGINFO("Block hash on proxy %d:%d changed to %s", id, subid, dsdata->lastswaphash);
		else
			LOGNOTICE("Block hash on proxy %d changed to %s", id, dsdata->lastswaphash);
	}

	check_proxy(sdata, proxy);
	clean |= new_block;
	LOGINFO("Proxy %d:%d broadcast updated stratum notify with%s clean", id,
		subid, clean ? "" : "out");
	stratum_broadcast_update(dsdata, wb, clean);
out:
	json_decref(val);
}

static void stratum_send_diff(sdata_t *sdata, const stratum_instance_t *client);

static void update_diff(ckpool_t *ckp, const char *cmd)
{
	sdata_t *sdata = ckp->sdata, *dsdata;
	stratum_instance_t *client, *tmp;
	double old_diff, diff;
	int id = 0, subid = 0;
	const char *buf;
	proxy_t *proxy;
	json_t *val;

	if (unlikely(strlen(cmd) < 6)) {
		LOGWARNING("Zero length string passed to update_diff");
		return;
	}
	buf = cmd + 5; /* "diff=" */
	LOGDEBUG("Update diff: %s", buf);

	val = json_loads(buf, 0, NULL);
	if (unlikely(!val)) {
		LOGWARNING("Failed to json decode in update_diff");
		return;
	}
	json_get_int(&id, val, "proxy");
	json_get_int(&subid, val, "subproxy");
	json_dblcpy(&diff, val, "diff");
	json_decref(val);

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
static void generator_drop_proxy(ckpool_t *ckp, const int64_t id, const int subid)
{
	char msg[256];

	sprintf(msg, "dropproxy=%ld:%d", id, subid);
	send_proc(ckp->generator,msg);
}
#endif

static void free_proxy(ckpool_t *ckp, proxy_t *proxy)
{
	sdata_t *dsdata = proxy->sdata;

	/* Delete any shares in the proxy's hashtable. */
	if (dsdata) {
		share_t *share, *tmpshare;
		workbase_t *wb, *tmpwb;

		mutex_lock(&dsdata->share_lock);
		HASH_ITER(hh, dsdata->shares, share, tmpshare) {
			HASH_DEL(dsdata->shares, share);
			dealloc(share);
		}
		mutex_unlock(&dsdata->share_lock);

		/* Do we need to check readcount here if freeing the proxy? */
		ck_wlock(&dsdata->workbase_lock);
		HASH_ITER(hh, dsdata->workbases, wb, tmpwb) {
			HASH_DEL(dsdata->workbases, wb);
			clear_workbase(ckp, wb);
		}
		ck_wunlock(&dsdata->workbase_lock);
	}

	free(proxy->sdata);
	free(proxy);
}

/* Remove subproxies that are flagged dead. Then see if there
 * are any retired proxies that no longer have any other subproxies and reap
 * those. */
static void reap_proxies(ckpool_t *ckp, sdata_t *sdata)
{
	proxy_t *proxy, *proxytmp, *subproxy, *subtmp;
	int dead = 0;

	if (!ckp->proxy)
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
				LOGWARNING("Unexepectedly found proxy %d:%d as subproxy of %d:%d",
					   subproxy->id, subproxy->subid, proxy->id, proxy->subid);
				continue;
			}
			if (unlikely(subproxy == sdata->proxy)) {
				LOGWARNING("Unexepectedly found proxy %d:%d as current",
					   subproxy->id, subproxy->subid);
				continue;
			}
			dead++;
			HASH_DELETE(sh, proxy->subproxies, subproxy);
			proxy->subproxy_count--;
			free_proxy(ckp, subproxy);
		}
		/* Should we reap the parent proxy too?*/
		if (!proxy->deleted || proxy->subproxy_count > 1 || proxy->bound_clients)
			continue;
		HASH_DELETE(sh, proxy->subproxies, proxy);
		HASH_DELETE(hh, sdata->proxies, proxy);
		free_proxy(ckp, proxy);
	}
	mutex_unlock(&sdata->proxy_lock);

	if (dead)
		LOGINFO("Stratifier discarded %d dead proxies", dead);
}

/* Enter with instance_lock held */
static stratum_instance_t *__instance_by_id(sdata_t *sdata, const int64_t id)
{
	stratum_instance_t *client;

	HASH_FIND_I64(sdata->stratum_instances, &id, client);
	return client;
}

/* Increase the reference count of instance */
static void __inc_instance_ref(stratum_instance_t *client)
{
	client->ref++;
}

/* Find an __instance_by_id and increase its reference count allowing us to
 * use this instance outside of instance_lock without fear of it being
 * dereferenced. Does not return dropped clients still on the list. */
static inline stratum_instance_t *ref_instance_by_id(sdata_t *sdata, const int64_t id)
{
	stratum_instance_t *client;

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

static void __drop_client(sdata_t *sdata, stratum_instance_t *client, bool lazily, char **msg)
{
	user_instance_t *user = client->user_instance;

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
				ASPRINTF(msg, "Dropped client %s %s user %s worker %s %s",
					 client->identity, client->address,
					 user->username, client->workername, lazily ? "lazily" : "");
			}
		} else {
			ASPRINTF(msg, "Dropped client %s %s no user worker %s %s",
				 client->identity, client->address, client->workername,
				 lazily ? "lazily" : "");
		}
	} else {
		/* Workerless client. Too noisy to log them all */
	}
	__del_client(sdata, client);
	__kill_instance(sdata, client);
}

static int __dec_instance_ref(stratum_instance_t *client)
{
	return --client->ref;
}

/* Decrease the reference count of instance. */
static void _dec_instance_ref(sdata_t *sdata, stratum_instance_t *client, const char *file,
			      const char *func, const int line)
{
	char_entry_t *entries = NULL;
	bool dropped = false;
	char *msg = NULL;
	int ref;

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
		reap_proxies(sdata->ckp, sdata);
}

#define dec_instance_ref(sdata, instance) _dec_instance_ref(sdata, instance, __FILE__, __func__, __LINE__)

/* If we have a no longer used stratum instance in the recycled linked list,
 * use that, otherwise calloc a fresh one. */
static stratum_instance_t *__recruit_stratum_instance(sdata_t *sdata)
{
	stratum_instance_t *client = sdata->recycled_instances;

	if (client)
		DL_DELETE2(sdata->recycled_instances, client, recycled_prev, recycled_next);
	else {
		client = ckzalloc(sizeof(stratum_instance_t));
		sdata->stratum_generated++;
	}
	return client;
}

/* Enter with write instance_lock held, drops and grabs it again */
static stratum_instance_t *__stratum_add_instance(ckpool_t *ckp, int64_t id, const char *address,
						  int server)
{
	sdata_t *sdata = ckp->sdata;
	stratum_instance_t *client;
	int64_t pass_id;

	client = __recruit_stratum_instance(sdata);
	ck_wunlock(&sdata->instance_lock);

	client->start_time = time(NULL);
	client->id = id;
	client->session_id = ++sdata->session_id;
	strcpy(client->address, address);
	/* Sanity check to not overflow lookup in ckp->serverurl[] */
	if (server >= ckp->serverurls)
		server = 0;
	client->server = server;
	client->diff = client->old_diff = ckp->startdiff;
	if (ckp->server_highdiff && ckp->server_highdiff[server]) {
		client->suggest_diff = ckp->highdiff;
		if (client->suggest_diff > client->diff)
			client->diff = client->old_diff = client->suggest_diff;
	}
	client->ckp = ckp;
	tv_time(&client->ldc);
	/* Points to ckp sdata in ckpool mode, but is changed later in proxy
	 * mode . */
	client->sdata = sdata;
	if ((pass_id = subclient(id))) {
		stratum_instance_t *remote = __instance_by_id(sdata, pass_id);

		id &= 0xffffffffll;
		if (remote && remote->node) {
			client->latency = remote->latency;
			LOGINFO("Client %s inherited node latency of %d",
				client->identity, client->latency);
			sprintf(client->identity, "node:%"PRId64" subclient:%"PRId64,
				pass_id, id);
		} else if (remote && remote->trusted) {
			sprintf(client->identity, "remote:%"PRId64" subclient:%"PRId64,
				pass_id, id);
		} else { /* remote->passthrough remaining */
			sprintf(client->identity, "passthrough:%"PRId64" subclient:%"PRId64,
				pass_id, id);
		}
		client->virtualid = connector_newclientid(ckp);
	} else {
		sprintf(client->identity, "%"PRId64, id);
		client->virtualid = id;
	}

	ck_wlock(&sdata->instance_lock);
	HASH_ADD_I64(sdata->stratum_instances, id, client);
	return client;
}

static uint64_t disconnected_sessionid_exists(sdata_t *sdata, const int session_id,
					      const int64_t id)
{
	session_t *session;
	int64_t old_id = 0;
	uint64_t ret = 0;

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
		LOGINFO("Reconnecting old instance %"PRId64" to instance %"PRId64, old_id, id);
	return ret;
}

static inline bool client_active(stratum_instance_t *client)
{
	return (client->authorised && !client->dropped);
}

static inline bool remote_server(stratum_instance_t *client)
{
	return (client->node || client->passthrough || client->trusted);
}

/* Ask the connector asynchronously to send us dropclient commands if this
 * client no longer exists. */
static void connector_test_client(ckpool_t *ckp, const int64_t id)
{
	char buf[256];

	LOGDEBUG("Stratifier requesting connector test client %"PRId64, id);
	snprintf(buf, 255, "testclient=%"PRId64, id);
	send_proc(ckp->connector, buf);
}

/* For creating a list of sends without locking that can then be concatenated
 * to the stratum_sends list. Minimises locking and avoids taking recursive
 * locks. Sends only to sdata bound clients (everyone in ckpool) */
static void stratum_broadcast(sdata_t *sdata, json_t *val, const int msg_type)
{
	ckpool_t *ckp = sdata->ckp;
	sdata_t *ckp_sdata = ckp->sdata;
	stratum_instance_t *client, *tmp;
	ckmsg_t *bulk_send = NULL;
	int messages = 0;

	if (unlikely(!val)) {
		LOGERR("Sent null json to stratum_broadcast");
		return;
	}

	if (ckp->node) {
		json_decref(val);
		return;
	}

	ck_rlock(&ckp_sdata->instance_lock);
	HASH_ITER(hh, ckp_sdata->stratum_instances, client, tmp) {
		ckmsg_t *client_msg;
		smsg_t *msg;

		if (sdata != ckp_sdata && client->sdata != sdata)
			continue;

		if (!client_active(client) || remote_server(client))
			continue;

		/* Only send messages to whitelisted clients */
		if (msg_type == SM_MSG && !client->messages)
			continue;

		client_msg = ckalloc(sizeof(ckmsg_t));
		msg = ckzalloc(sizeof(smsg_t));
		if (subclient(client->id))
			json_set_string(val, "node.method", stratum_msgs[msg_type]);
		msg->json_msg = json_deep_copy(val);
		msg->client_id = client->id;
		client_msg->data = msg;
		DL_APPEND(bulk_send, client_msg);
		messages++;
	}
	ck_runlock(&ckp_sdata->instance_lock);

	json_decref(val);

	if (likely(bulk_send))
		ssend_bulk_append(sdata, bulk_send, messages);
}

static void stratum_add_send(sdata_t *sdata, json_t *val, const int64_t client_id,
			     const int msg_type)
{
	ckpool_t *ckp = sdata->ckp;
	int64_t remote_id;
	smsg_t *msg;

	if (ckp->node) {
		/* Node shouldn't be sending any messages as it only uses the
		 * stratifier for monitoring activity. */
		json_decref(val);
		return;
	}

	if ((remote_id = subclient(client_id))) {
		stratum_instance_t *remote = ref_instance_by_id(sdata, remote_id);

		if (unlikely(!remote)) {
			json_decref(val);
			return;
		}
		if (remote->trusted)
			json_set_string(val, "method", stratum_msgs[msg_type]);
		else /* Both remote->node and remote->passthrough */
			json_set_string(val, "node.method", stratum_msgs[msg_type]);
		dec_instance_ref(sdata, remote);
	}
	LOGDEBUG("Sending stratum message %s", stratum_msgs[msg_type]);
	msg = ckzalloc(sizeof(smsg_t));
	msg->json_msg = val;
	msg->client_id = client_id;
	if (likely(ckmsgq_add(sdata->ssends, msg)))
		return;
	json_decref(msg->json_msg);
	free(msg);
}

static void drop_client(ckpool_t *ckp, sdata_t *sdata, const int64_t id)
{
	char_entry_t *entries = NULL;
	stratum_instance_t *client;
	char *msg = NULL;

	LOGINFO("Stratifier asked to drop client %"PRId64, id);

	ck_wlock(&sdata->instance_lock);
	client = __instance_by_id(sdata, id);
	if (client && !client->dropped) {
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

	if (entries)
		notice_msg_entries(&entries);
	reap_proxies(ckp, sdata);
}

static void stratum_broadcast_message(sdata_t *sdata, const char *msg)
{
	json_t *json_msg;

	JSON_CPACK(json_msg, "{sosss[s]}", "id", json_null(), "method", "client.show_message",
			     "params", msg);
	stratum_broadcast(sdata, json_msg, SM_MSG);
}

/* Send a generic reconnect to all clients without parameters to make them
 * reconnect to the same server. */
static void request_reconnect(sdata_t *sdata, const char *cmd)
{
	char *port = strdupa(cmd), *url = NULL;
	stratum_instance_t *client, *tmp;
	json_t *json_msg;

	strsep(&port, ":");
	if (port)
		url = strsep(&port, ",");
	if (url && port) {
		JSON_CPACK(json_msg, "{sosss[ssi]}", "id", json_null(), "method", "client.reconnect",
			"params", url, port, 0);
	} else
		JSON_CPACK(json_msg, "{sosss[]}", "id", json_null(), "method", "client.reconnect",
		   "params");
	stratum_broadcast(sdata, json_msg, SM_RECONNECT);

	/* Tag all existing clients as dropped now so they can be removed
	 * lazily */
	ck_wlock(&sdata->instance_lock);
	HASH_ITER(hh, sdata->stratum_instances, client, tmp) {
		client->dropped = true;
	}
	ck_wunlock(&sdata->instance_lock);
}

static void reset_bestshares(sdata_t *sdata)
{
	user_instance_t *user, *tmpuser;
	stratum_instance_t *client, *tmp;

	/* Can do this unlocked since it's just zeroing the values */
	sdata->stats.accounted_diff_shares =
	sdata->stats.accounted_shares =
	sdata->stats.accounted_rejects = 0;
	sdata->stats.best_diff = 0;

	ck_rlock(&sdata->instance_lock);
	HASH_ITER(hh, sdata->stratum_instances, client, tmp) {
		client->best_diff = 0;
	}
	HASH_ITER(hh, sdata->user_instances, user, tmpuser) {
		worker_instance_t *worker;

		user->best_diff = 0;
		DL_FOREACH(user->worker_instances, worker) {
			worker->best_diff = 0;
		}
	}
	ck_runlock(&sdata->instance_lock);
}

static user_instance_t *get_user(sdata_t *sdata, const char *username);

static user_instance_t *user_by_workername(sdata_t *sdata, const char *workername)
{
	char *username = strdupa(workername), *ignore;
	user_instance_t *user;

	ignore = username;
	strsep(&ignore, "._");

	/* Find the user first */
	user = get_user(sdata, username);
	return user;
}

static worker_instance_t *get_worker(sdata_t *sdata, user_instance_t *user, const char *workername);

static json_t *worker_stats(const worker_instance_t *worker)
{
	char suffix1[16], suffix5[16], suffix60[16], suffix1440[16], suffix10080[16];
	json_t *val;
	double ghs;

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

	JSON_CPACK(val, "{ss,ss,ss,ss,ss}",
			"hashrate1m", suffix1,
			"hashrate5m", suffix5,
			"hashrate1hr", suffix60,
			"hashrate1d", suffix1440,
			"hashrate7d", suffix10080);
	return val;
}

static json_t *user_stats(const user_instance_t *user)
{
	char suffix1[16], suffix5[16], suffix60[16], suffix1440[16], suffix10080[16];
	json_t *val;
	double ghs;

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

	JSON_CPACK(val, "{ss,ss,ss,ss,ss,sI,sI}",
			"hashrate1m", suffix1,
			"hashrate5m", suffix5,
			"hashrate1hr", suffix60,
			"hashrate1d", suffix1440,
			"hashrate7d", suffix10080,
			"shares", user->shares,
			"authorised", user->auth_time);
	return val;
}

/* Adjust workinfo id to virtual value for remote trusted workinfos */
static void remap_workinfo_id(sdata_t *sdata, json_t *val, const int64_t client_id)
{
	int64_t mapped_id, id;
	workbase_t *wb;

	json_get_int64(&id, val, "workinfoid");

	ck_rlock(&sdata->workbase_lock);
	wb = __find_remote_workbase(sdata, id, client_id);
	if (likely(wb))
		mapped_id = wb->mapped_id;
	else
		mapped_id = id;
	ck_runlock(&sdata->workbase_lock);

	/* Replace value with mapped id */
	json_set_int64(val, "workinfoid", mapped_id);
}

static void block_share_summary(sdata_t *sdata)
{
	double bdiff, sdiff;

	if (unlikely(!sdata->current_workbase || !sdata->current_workbase->network_diff))
		return;

	sdiff = sdata->stats.accounted_diff_shares;
	bdiff = sdiff / sdata->current_workbase->network_diff * 100;
	LOGWARNING("Block solved after %.0lf shares at %.1f%% diff",
		   sdiff, bdiff);
}

static void block_solve(ckpool_t *ckp, json_t *val)
{
	char *msg, *workername = NULL;
	sdata_t *sdata = ckp->sdata;
	char cdfield[64];
	double diff = 0;
	int height = 0;
	ts_t ts_now;

	ts_realtime(&ts_now);
	sprintf(cdfield, "%lu,%lu", ts_now.tv_sec, ts_now.tv_nsec);

	json_set_string(val, "confirmed", "1");
	json_set_string(val, "createdate", cdfield);
	json_set_string(val, "createcode", __func__);
	json_get_int(&height, val, "height");
	json_get_double(&diff, val, "diff");
	json_get_string(&workername, val, "workername");

	if (!workername) {
		ASPRINTF(&msg, "Block solved by %s!", ckp->name);
		LOGWARNING("Solved and confirmed block!");
	} else {
		json_t *user_val, *worker_val;
		worker_instance_t *worker;
		user_instance_t *user;
		char *s;

		ASPRINTF(&msg, "Block %d solved by %s @ %s!", height, workername, ckp->name);
		LOGWARNING("Solved and confirmed block %d by %s", height, workername);
		user = user_by_workername(sdata, workername);
		worker = get_worker(sdata, user, workername);

		ck_rlock(&sdata->instance_lock);
		user_val = user_stats(user);
		worker_val = worker_stats(worker);
		ck_runlock(&sdata->instance_lock);

		s = json_dumps(user_val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER);
		json_decref(user_val);
		LOGWARNING("User %s:%s", user->username, s);
		dealloc(s);
		s = json_dumps(worker_val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER);
		json_decref(worker_val);
		LOGWARNING("Worker %s:%s", workername, s);
		dealloc(s);
	}
	stratum_broadcast_message(sdata, msg);
	free(msg);

	free(workername);

	block_share_summary(sdata);
	reset_bestshares(sdata);
}

static void block_reject(json_t *val)
{
	int height = 0;

	json_get_int(&height, val, "height");

	LOGWARNING("Submitted, but had block %d rejected", height);
}

/* Some upstream pools (like p2pool) don't update stratum often enough and
 * miners disconnect if they don't receive regular communication so send them
 * a ping at regular intervals */
static void broadcast_ping(sdata_t *sdata)
{
	json_t *json_msg;

	JSON_CPACK(json_msg, "{s:[],s:i,s:s}",
		   "params",
		   "id", 42,
		   "method", "mining.ping");

	stratum_broadcast(sdata, json_msg, SM_PING);
}

static void ckmsgq_stats(ckmsgq_t *ckmsgq, const int size, json_t **val)
{
	int64_t memsize, generated;
	ckmsg_t *msg;
	int objects;

	mutex_lock(ckmsgq->lock);
	DL_COUNT(ckmsgq->msgs, msg, objects);
	generated = ckmsgq->messages;
	mutex_unlock(ckmsgq->lock);

	memsize = (sizeof(ckmsg_t) + size) * objects;
	JSON_CPACK(*val, "{si,si,sI}", "count", objects, "memory", memsize, "generated", generated);
}

char *stratifier_stats(ckpool_t *ckp, void *data)
{
	json_t *val = json_object(), *subval;
	int64_t memsize, generated;
	sdata_t *sdata = data;
	int objects;
	char *buf;

	ck_rlock(&sdata->workbase_lock);
	objects = HASH_COUNT(sdata->workbases);
	memsize = SAFE_HASH_OVERHEAD(sdata->workbases) + sizeof(workbase_t) * objects;
	generated = sdata->workbases_generated;
	JSON_CPACK(subval, "{si,si,sI}", "count", objects, "memory", memsize, "generated", generated);
	json_set_object(val, "workbases", subval);
	objects = HASH_COUNT(sdata->remote_workbases);
	memsize = SAFE_HASH_OVERHEAD(sdata->remote_workbases) + sizeof(workbase_t) * objects;
	ck_runlock(&sdata->workbase_lock);

	JSON_CPACK(subval, "{si,si}", "count", objects, "memory", memsize);
	json_set_object(val, "remote_workbases", subval);

	ck_rlock(&sdata->instance_lock);
	if (ckp->btcsolo) {
		user_instance_t *user, *tmpuser;
		int subobjects;

		objects = 0;
		memsize = 0;
		HASH_ITER(hh, sdata->user_instances, user, tmpuser) {
			subobjects = HASH_COUNT(user->userwbs);
			objects += subobjects;
			memsize += SAFE_HASH_OVERHEAD(user->userwbs) + sizeof(struct userwb) * subobjects;
		}
		generated = sdata->userwbs_generated;
		JSON_CPACK(subval, "{si,si,sI}", "count", objects, "memory", memsize, "generated", generated);
		json_set_object(val, "userwbs", subval);
	}

	objects = HASH_COUNT(sdata->user_instances);
	memsize = SAFE_HASH_OVERHEAD(sdata->user_instances) + sizeof(stratum_instance_t) * objects;
	JSON_CPACK(subval, "{si,si}", "count", objects, "memory", memsize);
	json_set_object(val, "users", subval);

	objects = HASH_COUNT(sdata->stratum_instances);
	memsize = SAFE_HASH_OVERHEAD(sdata->stratum_instances);
	generated = sdata->stratum_generated;
	JSON_CPACK(subval, "{si,si,sI}", "count", objects, "memory", memsize, "generated", generated);
	json_set_object(val, "clients", subval);

	objects = sdata->stats.disconnected;
	generated = sdata->disconnected_generated;
	memsize = SAFE_HASH_OVERHEAD(sdata->disconnected_sessions);
	memsize += sizeof(session_t) * sdata->stats.disconnected;
	JSON_CPACK(subval, "{si,si,sI}", "count", objects, "memory", memsize, "generated", generated);
	json_set_object(val, "disconnected", subval);
	ck_runlock(&sdata->instance_lock);

	mutex_lock(&sdata->share_lock);
	generated = sdata->shares_generated;
	objects = HASH_COUNT(sdata->shares);
	memsize = SAFE_HASH_OVERHEAD(sdata->shares) + sizeof(share_t) * objects;
	mutex_unlock(&sdata->share_lock);

	JSON_CPACK(subval, "{si,si,sI}", "count", objects, "memory", memsize, "generated", generated);
	json_set_object(val, "shares", subval);

	ck_rlock(&sdata->txn_lock);
	objects = HASH_COUNT(sdata->txns);
	memsize = SAFE_HASH_OVERHEAD(sdata->txns) + sizeof(txntable_t) * objects;
	generated = sdata->txns_generated;
	JSON_CPACK(subval, "{si,si,sI}", "count", objects, "memory", memsize, "generated", generated);
	json_set_object(val, "transactions", subval);
	ck_runlock(&sdata->txn_lock);

	ckmsgq_stats(sdata->ssends, sizeof(smsg_t), &subval);
	json_set_object(val, "ssends", subval);
	/* Don't know exactly how big the string is so just count the pointer for now */
	ckmsgq_stats(sdata->srecvs, sizeof(char *), &subval);
	json_set_object(val, "srecvs", subval);
	ckmsgq_stats(sdata->stxnq, sizeof(json_params_t), &subval);
	json_set_object(val, "stxnq", subval);

	buf = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER);
	json_decref(val);
	LOGNOTICE("Stratifier stats: %s", buf);
	return buf;
}

/* Send a single client a reconnect request, setting the time we sent the
 * request so we can drop the client lazily if it hasn't reconnected on its
 * own more than one minute later if we call reconnect again */
static void reconnect_client(sdata_t *sdata, stratum_instance_t *client)
{
	json_t *json_msg;

	/* Already requested? */
	if (client->reconnect_request) {
		if (time(NULL) - client->reconnect_request >= 60)
			connector_drop_client(sdata->ckp, client->id);
		return;
	}
	client->reconnect_request = time(NULL);
	JSON_CPACK(json_msg, "{sosss[]}", "id", json_null(), "method", "client.reconnect",
		   "params");
	stratum_add_send(sdata, json_msg, client->id, SM_RECONNECT);
}

static void dead_proxy(ckpool_t *ckp, sdata_t *sdata, const char *buf)
{
	int id = 0, subid = 0;

	sscanf(buf, "deadproxy=%d:%d", &id, &subid);
	dead_proxyid(sdata, id, subid, false, false);
	reap_proxies(ckp, sdata);
}

static void del_proxy(ckpool_t *ckp, sdata_t *sdata, const char *buf)
{
	int id = 0, subid = 0;

	sscanf(buf, "delproxy=%d:%d", &id, &subid);
	dead_proxyid(sdata, id, subid, false, true);
	reap_proxies(ckp, sdata);
}

static void reconnect_client_id(sdata_t *sdata, const int64_t client_id)
{
	stratum_instance_t *client;

	client = ref_instance_by_id(sdata, client_id);
	if (!client) {
		LOGINFO("reconnect_client_id failed to find client %"PRId64, client_id);
		return;
	}
	client->reconnect = true;
	reconnect_client(sdata, client);
	dec_instance_ref(sdata, client);
}

/* API commands */

static json_t *userinfo(const user_instance_t *user)
{
	json_t *val;

	JSON_CPACK(val, "{ss,si,si,sf,sf,sf,sf,sf,sf,si}",
		   "user", user->username, "id", user->id, "workers", user->workers,
	    "bestdiff", user->best_diff, "dsps1", user->dsps1, "dsps5", user->dsps5,
	    "dsps60", user->dsps60, "dsps1440", user->dsps1440, "dsps10080", user->dsps10080,
	    "lastshare", user->last_share.tv_sec);
	return val;
}

static void getuser(sdata_t *sdata, const char *buf, int *sockd)
{
	json_t *val = NULL, *res = NULL;
	char *username = NULL;
	user_instance_t *user;
	json_error_t err_val;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	if (!json_get_string(&username, val, "user")) {
		res = json_errormsg("Failed to find user key");
		goto out;
	}
	if (!strlen(username)) {
		res = json_errormsg("Zero length user key");
		goto out;
	}
	user = get_user(sdata, username);
	res = userinfo(user);
out:
	if (val)
		json_decref(val);
	free(username);
	send_api_response(res, *sockd);
}

static void userclients(sdata_t *sdata, const char *buf, int *sockd)
{
	json_t *val = NULL, *res = NULL, *client_arr;
	stratum_instance_t *client;
	char *username = NULL;
	user_instance_t *user;
	json_error_t err_val;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	if (!json_get_string(&username, val, "user")) {
		res = json_errormsg("Failed to find user key");
		goto out;
	}
	if (!strlen(username)) {
		res = json_errormsg("Zero length user key");
		goto out;
	}
	user = get_user(sdata, username);
	client_arr = json_array();

	ck_rlock(&sdata->instance_lock);
	DL_FOREACH2(user->clients, client, user_next) {
		json_array_append_new(client_arr, json_integer(client->id));
	}
	ck_runlock(&sdata->instance_lock);

	JSON_CPACK(res, "{ss,so}", "user", username, "clients", client_arr);
out:
	if (val)
		json_decref(val);
	free(username);
	send_api_response(res, *sockd);
}

static void workerclients(sdata_t *sdata, const char *buf, int *sockd)
{
	json_t *val = NULL, *res = NULL, *client_arr;
	char *tmp, *username, *workername = NULL;
	stratum_instance_t *client;
	user_instance_t *user;
	json_error_t err_val;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	if (!json_get_string(&workername, val, "worker")) {
		res = json_errormsg("Failed to find worker key");
		goto out;
	}
	if (!strlen(workername)) {
		res = json_errormsg("Zero length worker key");
		goto out;
	}
	tmp = strdupa(workername);
	username = strsep(&tmp, "._");
	user = get_user(sdata, username);
	client_arr = json_array();

	ck_rlock(&sdata->instance_lock);
	DL_FOREACH2(user->clients, client, user_next) {
		if (strcmp(client->workername, workername))
			continue;
		json_array_append_new(client_arr, json_integer(client->id));
	}
	ck_runlock(&sdata->instance_lock);

	JSON_CPACK(res, "{ss,so}", "worker", workername, "clients", client_arr);
out:
	if (val)
		json_decref(val);
	free(workername);
	send_api_response(res, *sockd);
}

static json_t *workerinfo(const user_instance_t *user, const worker_instance_t *worker)
{
	json_t *val;

	JSON_CPACK(val, "{ss,ss,si,sf,sf,sf,sf,si,sf,si,sb}",
		   "user", user->username, "worker", worker->workername, "id", user->id,
	    "dsps1", worker->dsps1, "dsps5", worker->dsps5, "dsps60", worker->dsps60,
	    "dsps1440", worker->dsps1440, "lastshare", worker->last_share.tv_sec,
	    "bestdiff", worker->best_diff, "mindiff", worker->mindiff, "idle", worker->idle);
	return val;
}

static void getworker(sdata_t *sdata, const char *buf, int *sockd)
{
	char *tmp, *username, *workername = NULL;
	json_t *val = NULL, *res = NULL;
	worker_instance_t *worker;
	user_instance_t *user;
	json_error_t err_val;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	if (!json_get_string(&workername, val, "worker")) {
		res = json_errormsg("Failed to find worker key");
		goto out;
	}
	if (!strlen(workername)) {
		res = json_errormsg("Zero length worker key");
		goto out;
	}
	tmp = strdupa(workername);
	username = strsep(&tmp, "._");
	user = get_user(sdata, username);
	worker = get_worker(sdata, user, workername);
	res = workerinfo(user, worker);
out:
	if (val)
		json_decref(val);
	free(workername);
	send_api_response(res, *sockd);
}

static void getworkers(sdata_t *sdata, int *sockd)
{
	json_t *val = NULL, *worker_arr;
	worker_instance_t *worker;
	user_instance_t *user;

	worker_arr = json_array();

	ck_rlock(&sdata->instance_lock);
	for (user = sdata->user_instances; user; user = user->hh.next) {
		DL_FOREACH(user->worker_instances, worker) {
			json_array_append_new(worker_arr, workerinfo(user, worker));
		}
	}
	ck_runlock(&sdata->instance_lock);

	JSON_CPACK(val, "{so}", "workers", worker_arr);
	send_api_response(val, *sockd);
}

static void getusers(sdata_t *sdata, int *sockd)
{
	json_t *val = NULL, *user_array;
	user_instance_t *user;

	user_array = json_array();

	ck_rlock(&sdata->instance_lock);
	for (user = sdata->user_instances; user; user = user->hh.next) {
		json_array_append_new(user_array, userinfo(user));
	}
	ck_runlock(&sdata->instance_lock);

	JSON_CPACK(val, "{so}", "users", user_array);
	send_api_response(val, *sockd);
}

static json_t *clientinfo(const stratum_instance_t *client)
{
	json_t *val = json_object();

	/* Too many fields for a pack object, do each discretely to keep track */
	json_set_int(val, "id", client->id);
	json_set_string(val, "enonce1", client->enonce1);
	json_set_string(val, "enonce1var", client->enonce1var);
	json_set_int(val, "enonce1_64", client->enonce1_64);
	json_set_double(val, "diff", client->diff);
	json_set_double(val, "dsps1", client->dsps1);
	json_set_double(val, "dsps5", client->dsps5);
	json_set_double(val, "dsps60", client->dsps60);
	json_set_double(val, "dsps1440", client->dsps1440);
	json_set_double(val, "dsps10080", client->dsps10080);
	json_set_int(val, "lastshare", client->last_share.tv_sec);
	json_set_int(val, "starttime", client->start_time);
	json_set_string(val, "address", client->address);
	json_set_bool(val, "subscribed", client->subscribed);
	json_set_bool(val, "authorised", client->authorised);
	json_set_bool(val, "idle", client->idle);
	json_set_string(val, "useragent", client->useragent ? client->useragent : "");
	json_set_string(val, "workername", client->workername ? client->workername : "");
	json_set_int(val, "userid", client->user_id);
	json_set_int(val, "server", client->server);
	json_set_double(val, "bestdiff", client->best_diff);
	json_set_int(val, "proxyid", client->proxyid);
	json_set_int(val, "subproxyid", client->subproxyid);

	return val;
}

static void getclient(sdata_t *sdata, const char *buf, int *sockd)
{
	json_t *val = NULL, *res = NULL;
	stratum_instance_t *client;
	json_error_t err_val;
	int64_t client_id;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	if (!json_get_int64(&client_id, val, "id")) {
		res = json_errormsg("Failed to find id key");
		goto out;
	}
	client = ref_instance_by_id(sdata, client_id);
	if (!client) {
		res = json_errormsg("Failed to find client %"PRId64, client_id);
		goto out;
	}
	res = clientinfo(client);

	dec_instance_ref(sdata, client);
out:
	if (val)
		json_decref(val);
	send_api_response(res, *sockd);
}

static void getclients(sdata_t *sdata, int *sockd)
{
	json_t *val = NULL, *client_arr;
	stratum_instance_t *client;

	client_arr = json_array();

	ck_rlock(&sdata->instance_lock);
	for (client = sdata->stratum_instances; client; client = client->hh.next) {
		json_array_append_new(client_arr, clientinfo(client));
	}
	ck_runlock(&sdata->instance_lock);

	JSON_CPACK(val, "{so}", "clients", client_arr);
	send_api_response(val, *sockd);
}

static void user_clientinfo(sdata_t *sdata, const char *buf, int *sockd)
{
	json_t *val = NULL, *res = NULL, *client_arr;
	stratum_instance_t *client;
	char *username = NULL;
	user_instance_t *user;
	json_error_t err_val;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	if (!json_get_string(&username, val, "user")) {
		res = json_errormsg("Failed to find user key");
		goto out;
	}
	if (!strlen(username)) {
		res = json_errormsg("Zero length user key");
		goto out;
	}
	user = get_user(sdata, username);
	client_arr = json_array();

	ck_rlock(&sdata->instance_lock);
	DL_FOREACH2(user->clients, client, user_next) {
		json_array_append_new(client_arr, clientinfo(client));
	}
	ck_runlock(&sdata->instance_lock);

	JSON_CPACK(res, "{ss,so}", "user", username, "clients", client_arr);
out:
	if (val)
		json_decref(val);
	free(username);
	send_api_response(res, *sockd);
}

static void worker_clientinfo(sdata_t *sdata, const char *buf, int *sockd)
{
	json_t *val = NULL, *res = NULL, *client_arr;
	char *tmp, *username, *workername = NULL;
	stratum_instance_t *client;
	user_instance_t *user;
	json_error_t err_val;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	if (!json_get_string(&workername, val, "worker")) {
		res = json_errormsg("Failed to find worker key");
		goto out;
	}
	if (!strlen(workername)) {
		res = json_errormsg("Zero length worker key");
		goto out;
	}
	tmp = strdupa(workername);
	username = strsep(&tmp, "._");
	user = get_user(sdata, username);
	client_arr = json_array();

	ck_rlock(&sdata->instance_lock);
	DL_FOREACH2(user->clients, client, user_next) {
		if (strcmp(client->workername, workername))
			continue;
		json_array_append_new(client_arr, clientinfo(client));
	}
	ck_runlock(&sdata->instance_lock);

	JSON_CPACK(res, "{ss,so}", "worker", workername, "clients", client_arr);
out:
	if (val)
		json_decref(val);
	free(workername);
	send_api_response(res, *sockd);
}

/* Return the user masked priority value of the proxy */
static int proxy_prio(const proxy_t *proxy)
{
	int prio = proxy->priority & 0x00000000ffffffff;

	return prio;
}

static json_t *json_proxyinfo(const proxy_t *proxy)
{
	const proxy_t *parent = proxy->parent;
	json_t *val;

	JSON_CPACK(val, "{si,si,si,sf,ss,ss,ss,ss,ss,si,si,si,si,sb,sb,sI,sI,sI,sI,sI,si,sb,sb,si}",
	    "id", proxy->id, "subid", proxy->subid, "priority", proxy_prio(parent),
	    "diff", proxy->diff, "baseurl", proxy->baseurl, "url", proxy->url,
	    "auth", proxy->auth, "pass", proxy->pass,
	    "enonce1", proxy->enonce1, "enonce1constlen", proxy->enonce1constlen,
	    "enonce1varlen", proxy->enonce1varlen, "nonce2len", proxy->nonce2len,
	    "enonce2varlen", proxy->enonce2varlen, "subscribed", proxy->subscribed,
	    "notified", proxy->notified, "clients", proxy->clients, "maxclients", proxy->max_clients,
	    "bound_clients", proxy->bound_clients, "combined_clients", parent->combined_clients,
	    "headroom", proxy->headroom, "subproxy_count", parent->subproxy_count,
	    "dead", proxy->dead, "global", proxy->global, "userid", proxy->userid);
	return val;
}

static void getproxy(sdata_t *sdata, const char *buf, int *sockd)
{
	json_t *val = NULL, *res = NULL;
	json_error_t err_val;
	int id, subid = 0;
	proxy_t *proxy;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	if (!json_get_int(&id, val, "id")) {
		res = json_errormsg("Failed to find id key");
		goto out;
	}
	json_get_int(&subid, val, "subid");
	if (!subid)
		proxy = existing_proxy(sdata, id);
	else
		proxy = existing_subproxy(sdata, id, subid);
	if (!proxy) {
		res = json_errormsg("Failed to find proxy %d:%d", id, subid);
		goto out;
	}
	res = json_proxyinfo(proxy);
out:
	if (val)
		json_decref(val);
	send_api_response(res, *sockd);
}

static void proxyinfo(sdata_t *sdata, const char *buf, int *sockd)
{
	json_t *val = NULL, *res = NULL, *arr_val = json_array();
	proxy_t *proxy, *subproxy;
	bool all = true;
	int userid = 0;

	if (buf) {
		/* See if there's a userid specified */
		val = json_loads(buf, 0, NULL);
		if (json_get_int(&userid, val, "userid"))
			all = false;
	}

	mutex_lock(&sdata->proxy_lock);
	for (proxy = sdata->proxies; proxy; proxy = proxy->hh.next) {
		if (!all && proxy->userid != userid)
			continue;
		for (subproxy = proxy->subproxies; subproxy; subproxy = subproxy->sh.next)
			json_array_append_new(arr_val, json_proxyinfo(subproxy));
	}
	mutex_unlock(&sdata->proxy_lock);

	if (val)
		json_decref(val);
	JSON_CPACK(res, "{so}", "proxies", arr_val);
	send_api_response(res, *sockd);
}

static void setproxy(sdata_t *sdata, const char *buf, int *sockd)
{
	json_t *val = NULL, *res = NULL;
	json_error_t err_val;
	int id, priority;
	proxy_t *proxy;

	val = json_loads(buf, 0, &err_val);
	if (unlikely(!val)) {
		res = json_encode_errormsg(&err_val);
		goto out;
	}
	if (!json_get_int(&id, val, "id")) {
		res = json_errormsg("Failed to find id key");
		goto out;
	}
	if (!json_get_int(&priority, val, "priority")) {
		res = json_errormsg("Failed to find priority key");
		goto out;
	}
	proxy = existing_proxy(sdata, id);
	if (!proxy) {
		res = json_errormsg("Failed to find proxy %d", id);
		goto out;
	}
	if (priority != proxy_prio(proxy))
		set_proxy_prio(sdata, proxy, priority);
	res = json_proxyinfo(proxy);
out:
	if (val)
		json_decref(val);
	send_api_response(res, *sockd);
}

static void get_poolstats(sdata_t *sdata, int *sockd)
{
	pool_stats_t *stats = &sdata->stats;
	json_t *val;

	mutex_lock(&sdata->stats_lock);
	JSON_CPACK(val, "{si,si,si,si,si,sI,sf,sf,sf,sf,sI,sI,sf,sf,sf,sf,sf,sf,sf}",
		   "start", stats->start_time.tv_sec, "update", stats->last_update.tv_sec,
	    "workers", stats->workers + stats->remote_workers, "users", stats->users + stats->remote_users,
	    "disconnected", stats->disconnected,
	    "shares", stats->accounted_shares, "sps1", stats->sps1, "sps5", stats->sps5,
	    "sps15", stats->sps15, "sps60", stats->sps60, "accepted", stats->accounted_diff_shares,
	    "rejected", stats->accounted_rejects, "dsps1", stats->dsps1, "dsps5", stats->dsps5,
	    "dsps15", stats->dsps15, "dsps60", stats->dsps60, "dsps360", stats->dsps360,
	    "dsps1440", stats->dsps1440, "dsps10080", stats->dsps10080);
	mutex_unlock(&sdata->stats_lock);

	send_api_response(val, *sockd);
}

static void get_uptime(sdata_t *sdata, int *sockd)
{
	int uptime = time(NULL) - sdata->stats.start_time.tv_sec;
	json_t *val;

	JSON_CPACK(val, "{si}", "uptime", uptime);
	send_api_response(val, *sockd);
}

static void stratum_loop(ckpool_t *ckp, proc_instance_t *pi)
{
	sdata_t *sdata = ckp->sdata;
	unix_msg_t *umsg = NULL;
	int ret = 0;
	char *buf;

retry:
	if (umsg) {
		Close(umsg->sockd);
		free(umsg->buf);
		dealloc(umsg);
	}

	do {
		time_t end_t;

		end_t = time(NULL);
		if (end_t - sdata->update_time >= ckp->update_interval) {
			sdata->update_time = end_t;
			if (!ckp->proxy) {
				LOGDEBUG("%ds elapsed in strat_loop, updating gbt base",
					 ckp->update_interval);
				update_base(sdata, GEN_NORMAL);
			} else if (!ckp->passthrough) {
				LOGDEBUG("%ds elapsed in strat_loop, pinging miners",
					 ckp->update_interval);
				broadcast_ping(sdata);
			}
		}

		umsg = get_unix_msg(pi);
	} while (!umsg);

	buf = umsg->buf;
	if (buf[0] == '{') {
		json_t *val = json_loads(buf, JSON_DISABLE_EOF_CHECK, NULL);

		/* This is a message for a node */
		if (likely(val))
			ckmsgq_add(sdata->srecvs, val);
		goto retry;
	}
	if (cmdmatch(buf, "ping")) {
		LOGDEBUG("Stratifier received ping request");
		send_unix_msg(umsg->sockd, "pong");
		goto retry;
	}
	if (cmdmatch(buf, "stats")) {
		char *msg;

		LOGDEBUG("Stratifier received stats request");
		msg = stratifier_stats(ckp, sdata);
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
	if (cmdmatch(buf,"uptime")) {
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
		update_subscribe(ckp, buf);
	} else if (cmdmatch(buf, "notify")) {
		/* Proxifier has a new notify ready */
		update_notify(ckp, buf);
	} else if (cmdmatch(buf, "diff")) {
		update_diff(ckp, buf);
	} else if (cmdmatch(buf, "dropclient")) {
		int64_t client_id;

		ret = sscanf(buf, "dropclient=%"PRId64, &client_id);
		if (ret < 0)
			LOGDEBUG("Stratifier failed to parse dropclient command: %s", buf);
		else
			drop_client(ckp, sdata, client_id);
	} else if (cmdmatch(buf, "reconnclient")) {
		int64_t client_id;

		ret = sscanf(buf, "reconnclient=%"PRId64, &client_id);
		if (ret < 0)
			LOGWARNING("Stratifier failed to parse reconnclient command: %s", buf);
		else
			reconnect_client_id(sdata, client_id);
	} else if (cmdmatch(buf, "dropall")) {
		drop_allclients(ckp);
	} else if (cmdmatch(buf, "reconnect")) {
		request_reconnect(sdata, buf);
	} else if (cmdmatch(buf, "deadproxy")) {
		dead_proxy(ckp, sdata, buf);
	} else if (cmdmatch(buf, "delproxy")) {
		del_proxy(ckp, sdata, buf);
	} else if (cmdmatch(buf, "loglevel")) {
		sscanf(buf, "loglevel=%d", &ckp->loglevel);
	} else if (cmdmatch(buf, "resetshares")) {
		reset_bestshares(sdata);
	} else
		LOGWARNING("Unhandled stratifier message: %s", buf);
	goto retry;
}

static void *blockupdate(void *arg)
{
	ckpool_t *ckp = (ckpool_t *)arg;
	sdata_t *sdata = ckp->sdata;
	char hash[68];

	pthread_detach(pthread_self());
	rename_proc("blockupdate");

	while (42) {
		int ret;

		ret = generator_getbest(ckp, hash);
		switch (ret) {
			case GETBEST_NOTIFY:
				cksleep_ms(5000);
				break;
			case GETBEST_SUCCESS:
				if (strcmp(hash, sdata->lastswaphash)) {
					update_base(sdata, GEN_PRIORITY);
					break;
				}
				[[fallthrough]];
			case GETBEST_FAILED:
			default:
				cksleep_ms(ckp->blockpoll);
		}
	}
	return NULL;
}

/* Enter holding workbase_lock and client a ref count. */
static void __fill_enonce1data(const workbase_t *wb, stratum_instance_t *client)
{
	if (wb->enonce1constlen)
		memcpy(client->enonce1bin, wb->enonce1constbin, wb->enonce1constlen);
	if (wb->enonce1varlen) {
		memcpy(client->enonce1bin + wb->enonce1constlen, &client->enonce1_64, wb->enonce1varlen);
		__bin2hex(client->enonce1var, &client->enonce1_64, wb->enonce1varlen);
	}
	__bin2hex(client->enonce1, client->enonce1bin, wb->enonce1constlen + wb->enonce1varlen);
}

/* Create a new enonce1 from the 64 bit enonce1_64 value, using only the number
 * of bytes we have to work with when we are proxying with a split nonce2.
 * When the proxy space is less than 32 bits to work with, we look for an
 * unused enonce1 value and reject clients instead if there is no space left.
 * Needs to be entered with client holding a ref count. */
static bool new_enonce1(ckpool_t *ckp, sdata_t *ckp_sdata, sdata_t *sdata, stratum_instance_t *client)
{
	proxy_t *proxy = NULL;
	uint64_t enonce1;

	if (ckp->proxy) {
		if (!ckp_sdata->proxy)
			return false;

		mutex_lock(&ckp_sdata->proxy_lock);
		proxy = sdata->subproxy;
		client->proxyid = proxy->id;
		client->subproxyid = proxy->subid;
		mutex_unlock(&ckp_sdata->proxy_lock);

		if (proxy->clients >= proxy->max_clients) {
			LOGWARNING("Proxy reached max clients %"PRId64, proxy->max_clients);
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

static void stratum_send_message(sdata_t *sdata, const stratum_instance_t *client, const char *msg);

/* Need to hold sdata->proxy_lock */
static proxy_t *__best_subproxy(proxy_t *proxy)
{
	proxy_t *subproxy, *best = NULL, *tmp;
	int64_t max_headroom;

	proxy->headroom = max_headroom = 0;
	HASH_ITER(sh, proxy->subproxies, subproxy, tmp) {
		int64_t subproxy_headroom;

		if (subproxy->dead)
			continue;
		if (!subproxy->sdata->current_workbase)
			continue;
		subproxy_headroom = subproxy->max_clients - subproxy->clients;

		proxy->headroom += subproxy_headroom;
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
static sdata_t *select_sdata(ckpool_t *ckp, sdata_t *ckp_sdata, const int userid)
{
	proxy_t *global, *proxy, *tmp, *best = NULL;

	if (!ckp->proxy || ckp->passthrough)
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
			generator_recruit(ckp, global->id, 1);
	} else {
		if (best_userproxy_headroom(ckp_sdata, userid) < 2)
			generator_recruit(ckp, best->id, 1);
	}
	return best->sdata;
}

static int int_from_sessionid(const char *sessionid)
{
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

static int userid_from_sessionid(sdata_t *sdata, const int session_id)
{
	session_t *session;
	int ret = -1;

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

static int userid_from_sessionip(sdata_t *sdata, const char *address)
{
	session_t *session, *tmp;
	int ret = -1;

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

/* Extranonce1 must be set here. Needs to be entered with client holding a ref
 * count. */
static json_t *parse_subscribe(stratum_instance_t *client, const int64_t client_id, const json_t *params_val)
{
	ckpool_t *ckp = client->ckp;
	sdata_t *sdata, *ckp_sdata = ckp->sdata;
	int session_id = 0, userid = -1;
	bool old_match = false;
	char sessionid[12];
	int arr_size;
	json_t *ret;
	int n2len;

	if (unlikely(!json_is_array(params_val))) {
		stratum_send_message(ckp_sdata, client, "Invalid json: params not an array");
		return json_string("params not an array");
	}

	sdata = select_sdata(ckp, ckp_sdata, 0);
	if (unlikely(!ckp->node && (!sdata || !sdata->current_workbase))) {
		LOGWARNING("Failed to provide subscription due to no %s", sdata ? "current workbase" : "sdata");
		stratum_send_message(ckp_sdata, client, "Pool Initialising");
		return json_string("Initialising");
	}

	arr_size = json_array_size(params_val);
	/* NOTE useragent is NULL prior to this so should not be used in code
	 * till after this point */
	if (arr_size > 0) {
		const char *buf;

		buf = json_string_value(json_array_get(params_val, 0));
		if (buf && strlen(buf))
			client->useragent = strdup(buf);
		else
			client->useragent = ckzalloc(1); // Set to ""
		if (arr_size > 1) {
			/* This would be the session id for reconnect, it will
			 * not work for clients on a proxied connection. */
			buf = json_string_value(json_array_get(params_val, 1));
			session_id = int_from_sessionid(buf);
			LOGDEBUG("Found old session id %d", session_id);
		}
		if (!ckp->proxy && session_id && !subclient(client_id)) {
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
	if (ckp->node)
		return NULL;

	if (ckp->proxy) {
		/* Use the session_id to tell us which user this was.
			* If it's not available, see if there's an IP address
			* which matches a recently disconnected session. */
		if (session_id)
			userid = userid_from_sessionid(ckp_sdata, session_id);
		if (userid == -1)
			userid = userid_from_sessionip(ckp_sdata, client->address);
		if (userid != -1) {
			sdata_t *user_sdata = select_sdata(ckp, ckp_sdata, userid);

			if (user_sdata)
				sdata = user_sdata;
		}
	}

	client->sdata = sdata;
	if (ckp->proxy) {
		LOGINFO("Current %d, selecting proxy %d:%d for client %s", ckp_sdata->proxy->id,
			sdata->subproxy->id, sdata->subproxy->subid, client->identity);
	}

	if (!old_match) {
		/* Create a new extranonce1 based on a uint64_t pointer */
		if (!new_enonce1(ckp, ckp_sdata, sdata, client)) {
			stratum_send_message(sdata, client, "Pool full of clients");
			client->reject = 3;
			return json_string("proxy full");
		}
		LOGINFO("Set new subscription %s to new enonce1 %lx string %s", client->identity,
			client->enonce1_64, client->enonce1);
	} else {
		LOGINFO("Set new subscription %s to old matched enonce1 %lx string %s",
			client->identity, client->enonce1_64, client->enonce1);
	}

	/* Workbases will exist if sdata->current_workbase is not NULL */
	ck_rlock(&sdata->workbase_lock);
	n2len = sdata->workbases->enonce2varlen;
	sprintf(sessionid, "%08x", client->session_id);
	JSON_CPACK(ret, "[[[s,s]],s,i]", "mining.notify", sessionid, client->enonce1,
			n2len);
	ck_runlock(&sdata->workbase_lock);

	client->subscribed = true;

	return ret;
}

static double dsps_from_key(json_t *val, const char *key)
{
	char *string, *endptr;
	double ret = 0;

	json_get_string(&string, val, key);
	if (!string)
		return ret;
	ret = strtod(string, &endptr) / nonces;
	if (endptr) {
		switch (endptr[0]) {
			case 'E':
				ret *= (double)1000;
				[[fallthrough]];
			case 'P':
				ret *= (double)1000;
				[[fallthrough]];
			case 'T':
				ret *= (double)1000;
				[[fallthrough]];
			case 'G':
				ret *= (double)1000;
				[[fallthrough]];
			case 'M':
				ret *= (double)1000;
				[[fallthrough]];
			case 'K':
				ret *= (double)1000;
				[[fallthrough]];
			default:
				break;
		}
	}
	free(string);
	return ret;
}

static void decay_client(stratum_instance_t *client, double diff, tv_t *now_t)
{
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

static void decay_worker(worker_instance_t *worker, double diff, tv_t *now_t)
{
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

static void decay_user(user_instance_t *user, double diff, tv_t *now_t)
{
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

static user_instance_t *get_create_user(sdata_t *sdata, const char *username, bool *new_user);
static worker_instance_t *get_create_worker(sdata_t *sdata, user_instance_t *user,
					    const char *workername, bool *new_worker);

/* Load the statistics of and create all known users at startup */
static void read_userstats(ckpool_t *ckp, sdata_t *sdata, int tvsec_diff)
{
	char dnam[256], s[4096], *username, *buf;
	int ret, users = 0, workers = 0;
	user_instance_t *user;
	struct dirent *dir;
	struct stat fdbuf;
	bool new_user;
	json_t *val;
	FILE *fp;
	tv_t now;
	DIR *d;
	int fd;

	snprintf(dnam, 255, "%susers", ckp->logdir);
	d = opendir(dnam);
	if (!d) {
		LOGNOTICE("No user directory found");
		return;
	}

	tv_time(&now);

	while ((dir = readdir(d)) != NULL) {
		json_t *worker_array, *arr_val;
		int64_t authorised;
		int lastshare;
		size_t index;

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
		val = json_loads(buf, 0, NULL);
		if (!val) {
			LOGNOTICE("Failed to json decode user %s logfile: %s", username, buf);
			dealloc(buf);
			continue;
		}
		dealloc(buf);

		copy_tv(&user->last_share, &now);
		copy_tv(&user->last_decay, &now);
		user->dsps1 = dsps_from_key(val, "hashrate1m");
		user->dsps5 = dsps_from_key(val, "hashrate5m");
		user->dsps60 = dsps_from_key(val, "hashrate1hr");
		user->dsps1440 = dsps_from_key(val, "hashrate1d");
		user->dsps10080 = dsps_from_key(val, "hashrate7d");
		json_get_int(&lastshare, val, "lastshare");
		user->last_share.tv_sec = lastshare;
		json_get_int64(&user->shares, val, "shares");
		json_get_double(&user->best_diff, val, "bestshare");
		json_get_int64(&user->best_ever, val, "bestever");
		json_get_int64(&authorised, val, "authorised");
		user->auth_time = authorised;
		if (user->best_diff > user->best_ever)
			user->best_ever = user->best_diff;
		LOGINFO("Successfully read user %s stats %f %f %f %f %f %f %ld %ld", user->username,
			user->dsps1, user->dsps5, user->dsps60, user->dsps1440,
			user->dsps10080, user->best_diff, user->best_ever, user->auth_time);
		if (tvsec_diff > 60)
			decay_user(user, 0, &now);

		worker_array = json_object_get(val, "worker");
		json_array_foreach(worker_array, index, arr_val) {
			const char *workername = json_string_value(json_object_get(arr_val, "workername"));
			worker_instance_t *worker;
			bool new_worker = false;

			if (unlikely(!workername || !strlen(workername)) ||
			    !strstr(workername, username)) {
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
			json_get_int(&lastshare, arr_val, "lastshare");
			worker->last_share.tv_sec = lastshare;
			json_get_double(&worker->best_diff, arr_val, "bestshare");
			json_get_int64(&worker->best_ever, arr_val, "bestever");
			if (worker->best_diff > worker->best_ever)
				worker->best_ever = worker->best_diff;
			json_get_int64(&worker->shares, arr_val, "shares");
			LOGINFO("Successfully read worker %s stats %f %f %f %f %f %ld", worker->workername,
				worker->dsps1, worker->dsps5, worker->dsps60, worker->dsps1440, worker->best_diff, worker->best_ever);
			if (tvsec_diff > 60)
				decay_worker(worker, 0, &now);
		}
		json_decref(val);
	}
	closedir(d);

	if (likely(users))
		LOGWARNING("Loaded %d users and %d workers", users, workers);
}

#define DEFAULT_AUTH_BACKOFF	(3)  /* Set initial backoff to 3 seconds */

static user_instance_t *__create_user(sdata_t *sdata, const char *username)
{
	user_instance_t *user = ckzalloc(sizeof(user_instance_t));

	user->auth_backoff = DEFAULT_AUTH_BACKOFF;
	strcpy(user->username, username);
	user->id = ++sdata->user_instance_id;
	HASH_ADD_STR(sdata->user_instances, username, user);
	return user;
}


/* Find user by username or create one if it doesn't already exist */
static user_instance_t *get_create_user(sdata_t *sdata, const char *username, bool *new_user)
{
	user_instance_t *user;

	ck_wlock(&sdata->instance_lock);
	HASH_FIND_STR(sdata->user_instances, username, user);
	if (unlikely(!user)) {
		user = __create_user(sdata, username);
		*new_user = true;
	}
	ck_wunlock(&sdata->instance_lock);

	return user;
}

static user_instance_t *get_user(sdata_t *sdata, const char *username)
{
	bool dummy = false;

	return get_create_user(sdata, username, &dummy);
}

static worker_instance_t *__create_worker(user_instance_t *user, const char *workername)
{
	worker_instance_t *worker = ckzalloc(sizeof(worker_instance_t));

	worker->workername = strdup(workername);
	worker->user_instance = user;
	DL_APPEND(user->worker_instances, worker);
	worker->start_time = time(NULL);
	return worker;
}

static worker_instance_t *__get_worker(user_instance_t *user, const char *workername)
{
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
static worker_instance_t *get_create_worker(sdata_t *sdata, user_instance_t *user,
					    const char *workername, bool *new_worker)
{
	worker_instance_t *worker;

	ck_wlock(&sdata->instance_lock);
	worker = __get_worker(user, workername);
	if (!worker) {
		worker = __create_worker(user, workername);
		*new_worker = true;
	}
	ck_wunlock(&sdata->instance_lock);

	return worker;
}

static worker_instance_t *get_worker(sdata_t *sdata, user_instance_t *user, const char *workername)
{
	bool dummy = false;

	return get_create_worker(sdata, user, workername, &dummy);
}

/* This simply strips off the first part of the workername and matches it to a
 * user or creates a new one. Needs to be entered with client holding a ref
 * count. */
static user_instance_t *generate_user(ckpool_t *ckp, stratum_instance_t *client,
				      const char *workername)
{
	char *base_username = strdupa(workername), *username;
	bool new_user = false, new_worker = false;
	sdata_t *sdata = ckp->sdata;
	worker_instance_t *worker;
	user_instance_t *user;
	int len;

	username = strsep(&base_username, "._");
	if (!username || !strlen(username))
		username = base_username;
	len = strlen(username);
	if (unlikely(len > 127))
		username[127] = '\0';

	user = get_create_user(sdata, username, &new_user);
	worker = get_create_worker(sdata, user, workername, &new_worker);

	/* Create one worker instance for combined data from workers of the
	 * same name */
	ck_wlock(&sdata->instance_lock);
	client->user_instance = user;
	client->worker_instance = worker;
	DL_APPEND2(user->clients, client, user_prev, user_next);
	__inc_worker(sdata,user, worker);
	ck_wunlock(&sdata->instance_lock);

	if (!ckp->proxy && (new_user || !user->btcaddress)) {
		/* Is this a btc address based username? */
		if (generator_checkaddr(ckp, username, &user->script, &user->segwit)) {
			user->btcaddress = true;
			user->txnlen = address_to_txn(user->txnbin, username, user->script, user->segwit);
		}
	}
	if (new_user) {
		LOGNOTICE("Added new user %s%s", username, user->btcaddress ?
			  " as address based registration" : "");
	}

	return user;
}

static void check_global_user(ckpool_t *ckp, user_instance_t *user, stratum_instance_t *client)
{
	sdata_t *sdata = ckp->sdata;
	proxy_t *proxy = best_proxy(sdata);
	int proxyid = proxy->id;
	char buf[256];

	sprintf(buf, "globaluser=%d:%d:%"PRId64":%s,%s", proxyid, user->id, client->id,
		user->username, client->password);
	send_proc(ckp->generator,buf);
}

/* Manage the response to auth, client must hold ref */
static void client_auth(ckpool_t *ckp, stratum_instance_t *client, user_instance_t *user,
			const bool ret)
{
	if (ret) {
		client->authorised = ret;
		user->authorised = ret;
		if (ckp->proxy) {
			LOGNOTICE("Authorised client %s to proxy %d:%d, worker %s as user %s",
				  client->identity, client->proxyid, client->subproxyid,
			          client->workername, user->username);
			if (ckp->userproxy)
				check_global_user(ckp, user, client);
		} else {
			LOGNOTICE("Authorised client %s %s worker %s as user %s",
				  client->identity, client->address, client->workername,
				  user->username);
		}
		user->failed_authtime = 0;
		user->auth_backoff = DEFAULT_AUTH_BACKOFF; /* Reset auth backoff time */
		user->throttled = false;
		if (!user->auth_time)
			user->auth_time = time(NULL);
	} else {
		if (user->throttled) {
			LOGINFO("Client %s %s worker %s failed to authorise as throttled user %s",
				client->identity, client->address, client->workername,
			        user->username);
		} else {
			LOGNOTICE("Client %s %s worker %s failed to authorise as user %s",
				  client->identity, client->address, client->workername,
			          user->username);
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

static json_t *__user_notify(const workbase_t *wb, const user_instance_t *user, const bool clean);

static void update_solo_client(sdata_t *sdata, workbase_t *wb, const int64_t client_id,
				 user_instance_t *user_instance)
{
	json_t *json_msg = __user_notify(wb, user_instance, true);

	stratum_add_send(sdata, json_msg, client_id, SM_UPDATE);
}

/* Needs to be entered with client holding a ref count. */
static json_t *parse_authorise(stratum_instance_t *client, const json_t *params_val,
			       json_t **err_val)
{
	user_instance_t *user;
	ckpool_t *ckp = client->ckp;
	const char *buf, *pass;
	bool ret = false;
	int arr_size;
	ts_t now;

	if (unlikely(!json_is_array(params_val))) {
		*err_val = json_string("params not an array");
		goto out;
	}
	arr_size = json_array_size(params_val);
	if (unlikely(arr_size < 1)) {
		*err_val = json_string("params missing array entries");
		goto out;
	}
	if (unlikely(!client->useragent)) {
		*err_val = json_string("Failed subscription");
		goto out;
	}
	buf = json_string_value(json_array_get(params_val, 0));
	if (!buf) {
		*err_val = json_string("Invalid workername parameter");
		goto out;
	}
	if (!strlen(buf)) {
		*err_val = json_string("Empty workername parameter");
		goto out;
	}
	if (!memcmp(buf, ".", 1) || !memcmp(buf, "_", 1)) {
		*err_val = json_string("Empty username parameter");
		goto out;
	}
	if (strchr(buf, '/')) {
		*err_val = json_string("Invalid character in username");
		goto out;
	}
	pass = json_string_value(json_array_get(params_val, 1));
	user = generate_user(ckp, client, buf);
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
				LOGNOTICE("Client %s %s worker %s rate limited due to failed auth attempts",
					  client->identity, client->address, buf);
			} else{
				LOGINFO("Client %s %s worker %s rate limited due to failed auth attempts",
					client->identity, client->address, buf);
			}
			client->dropped = true;
			goto out;
		}
	}
	if (!ckp->btcsolo || client->user_instance->btcaddress)
		ret = true;

	/* We do the preauth etc. in remote mode, and leave final auth to
	 * upstream pool to complete. */
	if (!ckp->remote || ckp->btcsolo)
		client_auth(ckp, client, user, ret);
out:
	if (ckp->btcsolo && ret && !client->remote) {
		sdata_t *sdata = ckp->sdata;
		workbase_t *wb;

		/* To avoid grabbing recursive lock */
		ck_wlock(&sdata->workbase_lock);
		wb = sdata->current_workbase;
		wb->readcount++;
		ck_wunlock(&sdata->workbase_lock);

		ck_wlock(&sdata->instance_lock);
		__generate_userwb(sdata, wb, user);
		ck_wunlock(&sdata->instance_lock);

		update_solo_client(sdata, wb, client->id, user);

		ck_wlock(&sdata->workbase_lock);
		wb->readcount--;
		ck_wunlock(&sdata->workbase_lock);

		stratum_send_diff(sdata, client);
	}
	return json_boolean(ret);
}

/* Needs to be entered with client holding a ref count. */
static void stratum_send_diff(sdata_t *sdata, const stratum_instance_t *client)
{
	json_t *json_msg;

	JSON_CPACK(json_msg, "{s[I]soss}", "params", client->diff, "id", json_null(),
			     "method", "mining.set_difficulty");
	stratum_add_send(sdata, json_msg, client->id, SM_DIFF);
}

/* Needs to be entered with client holding a ref count. */
static void stratum_send_message(sdata_t *sdata, const stratum_instance_t *client, const char *msg)
{
	json_t *json_msg;

	/* Only send messages to whitelisted clients */
	if (!client->messages)
		return;
	JSON_CPACK(json_msg, "{sosss[s]}", "id", json_null(), "method", "client.show_message",
			     "params", msg);
	stratum_add_send(sdata, json_msg, client->id, SM_MSG);
}

static double time_bias(const double tdiff, const double period)
{
	double dexp = tdiff / period;

	/* Sanity check to prevent silly numbers for double accuracy **/
	if (unlikely(dexp > 36))
		dexp = 36;
	return 1.0 - 1.0 / exp(dexp);
}

/* Needs to be entered with client holding a ref count. */
static void add_submit(ckpool_t *ckp, stratum_instance_t *client, const double diff, const bool valid,
		       const bool submit)
{
	sdata_t *ckp_sdata = ckp->sdata, *sdata = client->sdata;
	worker_instance_t *worker = client->worker_instance;
	double tdiff, bdiff, dsps, drr, network_diff, bias;
	user_instance_t *user = client->user_instance;
	int64_t next_blockid, optimal, mindiff;
	tv_t now_t;

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
	next_blockid = sdata->workbase_id + 1;
	if (ckp->proxy)
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
	if (ckp->node)
		return;

	client->ssdc++;
	bdiff = sane_tdiff(&now_t, &client->first_share);
	bias = time_bias(bdiff, 300);
	tdiff = sane_tdiff(&now_t, &client->ldc);

	/* Check the difficulty every 240 seconds or as many shares as we
	 * should have had in that time, whichever comes first. */
	if (client->ssdc < 72 && tdiff < 240)
		return;

	if (diff != client->diff) {
		client->ssdc = 0;
		return;
	}

	/* Diff rate ratio */
	dsps = client->dsps5 / bias;
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
	optimal = MAX(optimal, ckp->mindiff);

	/* Set to higher of optimal and user chosen diff */
	optimal = MAX(optimal, mindiff);

	/* Set to lower of optimal and pool maxdiff */
	if (ckp->maxdiff)
		optimal = MIN(optimal, ckp->maxdiff);

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

	LOGINFO("Client %s biased dsps %.2f dsps %.2f drr %.2f adjust diff from %"PRId64" to: %"PRId64" ",
		client->identity, dsps, client->dsps5, drr, client->diff, optimal);

	copy_tv(&client->ldc, &now_t);
	client->diff_change_job_id = next_blockid;
	client->old_diff = client->diff;
	client->diff = optimal;
	stratum_send_diff(sdata, client);
}

static void
downstream_block(ckpool_t *ckp, sdata_t *sdata, const json_t *val, const int cblen,
		 const char *coinbase, const uchar *data)
{
	json_t *block_val = json_deep_copy(val);

	/* Strip unnecessary fields and add extra fields needed */
	json_set_string(block_val, "method", stratum_msgs[SM_BLOCK]);
	add_remote_blockdata(ckp, block_val, cblen, coinbase, data);
	downstream_json(sdata, block_val, 0, SSEND_PREPEND);
	json_decref(block_val);
}

/* We should already be holding a wb readcount. Needs to be entered with
 * client holding a ref count. */
static void
test_blocksolve(const stratum_instance_t *client, const workbase_t *wb, const uchar *data,
		const uchar *hash, const double diff, const char *coinbase, int cblen,
		const char *nonce2, const char *nonce, const uint32_t ntime32, const uint32_t version_mask,
		const bool stale)
{
	char blockhash[68], cdfield[64], *gbt_block;
	sdata_t *sdata = client->sdata;
	ckpool_t *ckp = wb->ckp;
	double network_diff;
	json_t *val = NULL;
	uchar flip32[32];
	ts_t ts_now;
	bool ret;

	/* Submit anything over 99.9% of the diff in case of rounding errors */
	network_diff = sdata->current_workbase->network_diff * 0.999;
	if (likely(diff < network_diff))
		return;

	LOGWARNING("Possible %sblock solve diff %lf !", stale ? "stale share " : "", diff);
	/* Can't submit a block in proxy mode without the transactions */
	if (!ckp->node && wb->proxy)
		return;

	ts_realtime(&ts_now);
	sprintf(cdfield, "%lu,%lu", ts_now.tv_sec, ts_now.tv_nsec);

	gbt_block = process_block(wb, coinbase, cblen, data, hash, flip32, blockhash);
	send_node_block(ckp, sdata, client->enonce1, nonce, nonce2, ntime32, version_mask,
			wb->id, diff, client->id, coinbase, cblen, data);

	val = json_object();
	json_set_int(val, "height", wb->height);
	json_set_string(val,"blockhash", blockhash);
	json_set_string(val,"confirmed", "n");
	json_set_int64(val, "workinfoid", wb->id);
	json_set_string(val, "username", client->user_instance->username);
	json_set_string(val, "workername", client->workername);
	if (ckp->remote)
		json_set_int64(val, "clientid", client->virtualid);
	else
		json_set_int64(val, "clientid", client->id);
	json_set_string(val, "enonce1", client->enonce1);
	json_set_string(val, "nonce2", nonce2);
	json_set_string(val, "nonce", nonce);
	json_set_uint32(val, "ntime32", ntime32);
	json_set_uint32(val, "version_mask", version_mask);
	json_set_int64(val, "reward", wb->coinbasevalue);
	json_set_double(val, "diff", diff);
	json_set_string(val, "createdate", cdfield);
	json_set_string(val, "createby", "code");
	json_set_string(val, "createcode", __func__);
	json_set_string(val, "createinet", ckp->serverurl[client->server]);

	if (ckp->remote) {
		add_remote_blockdata(ckp, val, cblen, coinbase, data);
		upstream_json_msgtype(ckp, val, SM_BLOCK);
	} else {
		downstream_block(ckp, sdata, val, cblen, coinbase, data);
	}

	/* Submit block locally after sending it to remote locations avoiding
	 * the delay of local verification */
	ret = local_block_submit(ckp, gbt_block, flip32, wb->height);
	if (ret)
		block_solve(ckp, val);
	else
		block_reject(val);

	json_decref(val);
}

/* Entered with instance_lock held */
static inline uchar *__user_coinb2(const stratum_instance_t *client, const workbase_t *wb, int *cb2len)
{
	struct userwb *userwb;
	int64_t id;

	if (!client->ckp->btcsolo)
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

/* Needs to be entered with workbase readcount and client holding a ref count. */
static double submission_diff(sdata_t *sdata, const stratum_instance_t *client, const workbase_t *wb,
			      const char *nonce2, const uint32_t ntime32, uint32_t version_mask,
			      const char *nonce, uchar *hash, const bool stale)
{
	unsigned char merkle_root[32], merkle_sha[64];
	uint32_t *data32, *swap32, benonce32;
	char *coinbase, data[80];
	uchar swap[80], hash1[32];
	int cblen, i, cb2len;
	uchar *coinb2bin;
	double ret;

	/* Leave ample enough room for donation generation address (~25) + length counter + user generation
	 * wb->coinb1len + wb->enonce1constlen + wb->enonce1varlen + wb->enonce2varlen + wb->coinb2len + 25 + cb2len */

	coinbase = alloca(1024);
	memcpy(coinbase, wb->coinb1bin, wb->coinb1len);
	cblen = wb->coinb1len;
	memcpy(coinbase + cblen, &client->enonce1bin, wb->enonce1constlen + wb->enonce1varlen);
	cblen += wb->enonce1constlen + wb->enonce1varlen;
	hex2bin(coinbase + cblen, nonce2, wb->enonce2varlen);
	cblen += wb->enonce2varlen;

	ck_rlock(&sdata->instance_lock);
	coinb2bin = __user_coinb2(client, wb, &cb2len);
	memcpy(coinbase + cblen, coinb2bin, cb2len);
	ck_runlock(&sdata->instance_lock);

	cblen += cb2len;

	gen_hash((uchar *)coinbase, merkle_root, cblen);
	memcpy(merkle_sha, merkle_root, 32);
	for (i = 0; i < wb->merkles; i++) {
		memcpy(merkle_sha + 32, &wb->merklebin[i], 32);
		gen_hash(merkle_sha, merkle_root, 64);
		memcpy(merkle_sha, merkle_root, 32);
	}
	data32 = (uint32_t *)merkle_sha;
	swap32 = (uint32_t *)merkle_root;
	flip_32(swap32, data32);

	/* Copy the cached header binary and insert the merkle root */
	memcpy(data, wb->headerbin, 80);
	memcpy(data + 36, merkle_root, 32);

	/* Update nVersion when version_mask is in use */
	if (version_mask) {
		version_mask = htobe32(version_mask);
		data32 = (uint32_t *)data;
		*data32 |= version_mask;
	}

	/* Insert the nonce value into the data */
	hex2bin(&benonce32, nonce, 4);
	data32 = (uint32_t *)(data + 64 + 12);
	*data32 = benonce32;

	/* Insert the ntime value into the data */
	data32 = (uint32_t *)(data + 68);
	*data32 = htobe32(ntime32);

	/* Hash the share */
	data32 = (uint32_t *)data;
	swap32 = (uint32_t *)swap;
	flip_80(swap32, data32);
	sha256(swap, 80, hash1);
	sha256(hash1, 32, hash);

	/* Calculate the diff of the share here */
	ret = diff_from_target(hash);

	/* Test we haven't solved a block regardless of share status */
	test_blocksolve(client, wb, swap, hash, ret, coinbase, cblen, nonce2, nonce, ntime32, version_mask, stale);

	return ret;
}

/* Optimised for the common case where shares are new */
static bool new_share(sdata_t *sdata, const uchar *hash, const int64_t wb_id)
{
	share_t *share = ckzalloc(sizeof(share_t)), *match = NULL;
	bool ret = true;

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

static void update_client(const stratum_instance_t *client, const int64_t client_id);

/* Submit a share in proxy mode to the parent pool. workbase_lock is held.
 * Needs to be entered with client holding a ref count. */
static void submit_share(stratum_instance_t *client, const int64_t jobid, const char *nonce2,
			 const char *ntime, const char *nonce)
{
	ckpool_t *ckp = client->ckp;
	json_t *json_msg;
	char enonce2[32];

	sprintf(enonce2, "%s%s", client->enonce1var, nonce2);
	JSON_CPACK(json_msg, "{sIsssssssIsIsi}", "jobid", jobid, "nonce2", enonce2,
			     "ntime", ntime, "nonce", nonce, "client_id", client->id,
			     "proxy", client->proxyid, "subproxy", client->subproxyid);
	generator_add_send(ckp, json_msg);
}

static void check_best_diff(sdata_t *sdata, user_instance_t *user,worker_instance_t *worker,
			    const double sdiff, stratum_instance_t *client)
{
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
	 * the mutex is locked. */
	if (best_user && sdiff > sdata->stats.best_diff) {
		/* Don't set pool best diff if it's a block since we will have
		 * reset it to zero. */
		mutex_lock(&sdata->stats_lock);
		if (unlikely(sdiff > sdata->stats.best_diff && sdiff < sdata->current_workbase->network_diff))
			sdata->stats.best_diff = sdiff;
		mutex_unlock(&sdata->stats_lock);
	}
	if (likely((!best_user && !best_worker) || !client))
		return;
	snprintf(buf, 511, "New best %sshare for %s: %lf", best_ever ? "ever " : "",
		 best_user ? "user" : "worker", sdiff);
	stratum_send_message(sdata, client, buf);
}

#define JSON_ERR(err) json_string(SHARE_ERR(err))

/* Needs to be entered with client holding a ref count. */
static json_t *parse_submit(stratum_instance_t *client, json_t *json_msg,
			    const json_t *params_val, json_t **err_val)
{
	bool share = false, result = false, invalid = true, submit = false, stale = false;
	const char *workername, *job_id, *ntime, *version_mask;
	double diff = client->diff, wdiff = 0, sdiff = -1;
	char hexhash[68] = {}, sharehash[32], cdfield[64];
	user_instance_t *user = client->user_instance;
	char *fname = NULL, *s, *nonce, *nonce2;
	uint32_t ntime32, version_mask32 = 0;
	sdata_t *sdata = client->sdata;
	enum share_err err = SE_NONE;
	ckpool_t *ckp = client->ckp;
	char idstring[24] = {};
	workbase_t *wb = NULL;
	uchar hash[32];
	int nlen, len;
	time_t now_t;
	json_t *val;
	int64_t id;
	ts_t now;
	FILE *fp;

	ts_realtime(&now);
	now_t = now.tv_sec;
	sprintf(cdfield, "%lu,%lu", now.tv_sec, now.tv_nsec);

	if (unlikely(!json_is_array(params_val))) {
		err = SE_NOT_ARRAY;
		*err_val = JSON_ERR(err);
		goto out;
	}
	if (unlikely(json_array_size(params_val) < 5)) {
		err = SE_INVALID_SIZE;
		*err_val = JSON_ERR(err);
		goto out;
	}
	workername = json_string_value(json_array_get(params_val, 0));
	if (unlikely(!workername || !strlen(workername))) {
		err = SE_NO_USERNAME;
		*err_val = JSON_ERR(err);
		goto out;
	}
	job_id = json_string_value(json_array_get(params_val, 1));
	if (unlikely(!job_id || !strlen(job_id))) {
		err = SE_NO_JOBID;
		*err_val = JSON_ERR(err);
		goto out;
	}
	nonce2 = (char *)json_string_value(json_array_get(params_val, 2));
	if (unlikely(!nonce2 || !strlen(nonce2) || !validhex(nonce2))) {
		err = SE_NO_NONCE2;
		*err_val = JSON_ERR(err);
		goto out;
	}
	ntime = json_string_value(json_array_get(params_val, 3));
	if (unlikely(!ntime || !strlen(ntime) || !validhex(ntime))) {
		err = SE_NO_NTIME;
		*err_val = JSON_ERR(err);
		goto out;
	}
	nonce = (char *)json_string_value(json_array_get(params_val, 4));
	if (unlikely(!nonce || strlen(nonce) < 8 || !validhex(nonce))) {
		err = SE_NO_NONCE;
		*err_val = JSON_ERR(err);
		goto out;
	}

	version_mask = json_string_value(json_array_get(params_val, 5));
	if (version_mask && strlen(version_mask) && validhex(version_mask)) {
		sscanf(version_mask, "%x", &version_mask32);
		// check version mask
		if (version_mask32 && ((~ckp->version_mask) & version_mask32) != 0) {
			// means client changed some bits which server doesn't allow to change
			err = SE_INVALID_VERSION_MASK;
			*err_val = JSON_ERR(err);
			goto out;
		}
	}
	if (safecmp(workername, client->workername)) {
		err = SE_WORKER_MISMATCH;
		*err_val = JSON_ERR(err);
		goto out;
	}
	sscanf(job_id, "%lx", &id);
	sscanf(ntime, "%x", &ntime32);

	share = true;

	if (unlikely(!sdata->current_workbase))
		return json_boolean(false);

	wb = get_workbase(sdata, id);
	if (unlikely(!wb)) {
		id = sdata->current_workbase->id;
		err = SE_INVALID_JOBID;
		json_set_string(json_msg, "reject-reason", SHARE_ERR(err));
		strncpy(idstring, job_id, 19);
		ASPRINTF(&fname, "%s.sharelog", sdata->current_workbase->logdir);
		goto out_nowb;
	}
	wdiff = wb->diff;
	strncpy(idstring, wb->idstring, 20);
	ASPRINTF(&fname, "%s.sharelog", wb->logdir);
	/* Fix broken clients sending too many chars. Nonce2 is part of the
	 * read only json so use a temporary variable and modify it. */
	len = wb->enonce2varlen * 2;
	nlen = strlen(nonce2);
	if (unlikely(nlen != len)) {
		if (nlen > len) {
			nonce2 = strdupa(nonce2);
			nonce2[len] = '\0';
		} else if (nlen < len) {
			char *tmp = nonce2;

			nonce2 = strdupa("0000000000000000");
			memcpy(nonce2, tmp, nlen);
			nonce2[len] = '\0';
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
	sdiff = submission_diff(sdata, client, wb, nonce2, ntime32, version_mask32, nonce, hash, stale);
	if (sdiff > client->best_diff) {
		worker_instance_t *worker = client->worker_instance;

		client->best_diff = sdiff;
		LOGINFO("User %s worker %s client %s new best diff %lf", user->username,
			worker->workername, client->identity, sdiff);
		check_best_diff(sdata, user, worker, sdiff, client);
	}
	bswap_256(sharehash, hash);
	__bin2hex(hexhash, sharehash, 32);

	if (stale) {
		/* Accept shares if they're received on remote nodes before the
		 * workbase was retired. */
		if (client->latency) {
			int latency;
			tv_t now_tv;

			ts_to_tv(&now_tv, &now);
			latency = ms_tvdiff(&now_tv, &wb->retired);
			if (latency < client->latency) {
				LOGDEBUG("Accepting %dms late share from client %s",
					 latency, client->identity);
				goto no_stale;
			}
		}
		err = SE_STALE;
		json_set_string(json_msg, "reject-reason", SHARE_ERR(err));
		goto out_submit;
	}
no_stale:
	/* Ntime cannot be less, but allow forward ntime rolling up to max */
	if (ntime32 < wb->ntime32 || ntime32 > wb->ntime32 + 7000) {
		err = SE_NTIME_INVALID;
		json_set_string(json_msg, "reject-reason", SHARE_ERR(err));
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

	/* Accept shares of the old diff until the next update */
	if (id < client->diff_change_job_id)
		diff = client->old_diff;
	if (!invalid) {
		char wdiffsuffix[16];

		suffix_string(wdiff, wdiffsuffix, 16, 0);
		if (sdiff >= diff) {
			if (new_share(sdata, hash, id)) {
				LOGINFO("Accepted client %s share diff %.1f/%.0f/%s: %s",
					client->identity, sdiff, diff, wdiffsuffix, hexhash);
				result = true;
			} else {
				err = SE_DUPE;
				json_set_string(json_msg, "reject-reason", SHARE_ERR(err));
				LOGINFO("Rejected client %s dupe diff %.1f/%.0f/%s: %s",
					client->identity, sdiff, diff, wdiffsuffix, hexhash);
				submit = false;
			}
		} else {
			err = SE_HIGH_DIFF;
			LOGINFO("Rejected client %s high diff %.1f/%.0f/%s: %s",
				client->identity, sdiff, diff, wdiffsuffix, hexhash);
			json_set_string(json_msg, "reject-reason", SHARE_ERR(err));
			submit = false;
		}
	}  else
		LOGINFO("Rejected client %s invalid share %s", client->identity, SHARE_ERR(err));

	/* Submit share to upstream pool in proxy mode. We submit valid and
	 * stale shares and filter out the rest. */
	if (wb && wb->proxy && submit) {
		LOGINFO("Submitting share upstream: %s", hexhash);
		submit_share(client, id, nonce2, ntime, nonce);
	}

	add_submit(ckp, client, diff, result, submit);

	/* Now write to the pool's sharelog. */
	val = json_object();
	json_set_int(val, "workinfoid", id);
	if (ckp->remote)
		json_set_int64(val, "clientid", client->virtualid);
	else
		json_set_int64(val, "clientid", client->id);
	json_set_string(val, "enonce1", client->enonce1);
	json_set_string(val, "nonce2", nonce2);
	json_set_string(val, "nonce", nonce);
	json_set_string(val, "ntime", ntime);
	json_set_double(val, "diff", diff);
	json_set_double(val, "sdiff", sdiff);
	json_set_string(val, "hash", hexhash);
	json_set_bool(val, "result", result);
	json_object_set(val, "reject-reason", json_object_get(json_msg, "reject-reason"));
	json_object_set(val, "error", *err_val);
	json_set_int(val, "errn", err);
	json_set_string(val, "createdate", cdfield);
	json_set_string(val, "createby", "code");
	json_set_string(val, "createcode", __func__);
	json_set_string(val, "createinet", ckp->serverurl[client->server]);
	json_set_string(val, "workername", client->workername);
	json_set_string(val, "username", user->username);
        json_set_string(val, "address", client->address);
        json_set_string(val, "agent", client->useragent);

	if (ckp->logshares) {
		fp = fopen(fname, "ae");
		if (likely(fp)) {
			s = json_dumps(val, JSON_EOL);
			len = strlen(s);
			len = fprintf(fp, "%s", s);
			free(s);
			fclose(fp);
			if (unlikely(len < 0))
				LOGERR("Failed to fwrite to %s", fname);
		} else
			LOGERR("Failed to fopen %s", fname);
	}
	if (ckp->remote)
		upstream_json_msgtype(ckp, val, SM_SHARE);
	json_decref(val);
out:
	if (!sdata->wbincomplete && ((!result && !submit) || !share)) {
		/* Is this the first in a run of invalids? */
		if (client->first_invalid < client->last_share.tv_sec || !client->first_invalid)
			client->first_invalid = now_t;
		else if (client->first_invalid && client->first_invalid < now_t - 180 && client->reject < 3) {
			LOGNOTICE("Client %s rejecting for 180s, disconnecting", client->identity);
			if (ckp->node)
				connector_drop_client(ckp, client->id);
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

	if (!share) {
		if (ckp->remote) {
			val = json_object();
			if (ckp->remote)
				json_set_int64(val, "clientid", client->virtualid);
			else
				json_set_int64(val, "clientid", client->id);
			if (user->secondaryuserid)
				json_set_string(val, "secondaryuserid", user->secondaryuserid);
			json_set_string(val, "enonce1", client->enonce1);
			json_set_int(val, "workinfoid", sdata->current_workbase->id);
			json_set_string(val, "workername", client->workername);
			json_set_string(val, "username", user->username);
			json_object_set(val, "error", *err_val);
			json_set_int(val, "errn", err);
			json_set_string(val, "createdate", cdfield);
			json_set_string(val, "createby", "code");
			json_set_string(val, "createcode", __func__);
			json_set_string(val, "createinet", ckp->serverurl[client->server]);
			json_decref(val);
		}
		LOGINFO("Invalid share from client %s: %s", client->identity, client->workername);
	}
	free(fname);
	return json_boolean(result);
}

/* Must enter with workbase_lock held */
static json_t *__stratum_notify(const workbase_t *wb, const bool clean)
{
	json_t *val;

	JSON_CPACK(val, "{s:[ssssosssb],s:o,s:s}",
			"params",
			wb->idstring,
			wb->prevhash,
			wb->coinb1,
			wb->coinb2,
			json_deep_copy(wb->merkle_array),
			wb->bbversion,
			wb->nbit,
			wb->ntime,
			clean,
			"id", json_null(),
			"method", "mining.notify");
	return val;
}

static void stratum_broadcast_update(sdata_t *sdata, const workbase_t *wb, const bool clean)
{
	json_t *json_msg;

	ck_rlock(&sdata->workbase_lock);
	json_msg = __stratum_notify(wb, clean);
	ck_runlock(&sdata->workbase_lock);

	stratum_broadcast(sdata, json_msg, SM_UPDATE);
}

/* For sending a single stratum template update */
static void stratum_send_update(sdata_t *sdata, const int64_t client_id, const bool clean)
{
	ckpool_t *ckp = sdata->ckp;
	json_t *json_msg;

	if (unlikely(!sdata->current_workbase)) {
		if (!ckp->proxy)
			LOGWARNING("No current workbase to send stratum update");
		else
			LOGDEBUG("No current workbase to send stratum update for client %"PRId64, client_id);
		return;
	}

	ck_rlock(&sdata->workbase_lock);
	json_msg = __stratum_notify(sdata->current_workbase, clean);
	ck_runlock(&sdata->workbase_lock);

	stratum_add_send(sdata, json_msg, client_id, SM_UPDATE);
}

/* Hold instance and workbase lock */
static json_t *__user_notify(const workbase_t *wb, const user_instance_t *user, const bool clean)
{
	int64_t id = wb->id;
	struct userwb *userwb;
	json_t *val;

	HASH_FIND_I64(user->userwbs, &id, userwb);
	if (unlikely(!userwb)) {
		LOGINFO("Failed to find userwb in __user_notify!");
		return NULL;
	}

	JSON_CPACK(val, "{s:[ssssosssb],s:o,s:s}",
			"params",
			wb->idstring,
			wb->prevhash,
			wb->coinb1,
			userwb->coinb2,
			json_deep_copy(wb->merkle_array),
			wb->bbversion,
			wb->nbit,
			wb->ntime,
			clean,
			"id", json_null(),
			"method", "mining.notify");
	return val;
}

/* Sends a stratum update with a unique coinb2 for every client. Avoid
 * recursive locking. */
static void stratum_broadcast_updates(sdata_t *sdata, bool clean)
{
	stratum_instance_t *client, *tmp;
	json_t *json_msg;

	ck_wlock(&sdata->instance_lock);
	HASH_ITER(hh, sdata->stratum_instances, client, tmp) {
		if (!client->user_instance)
			continue;
		__inc_instance_ref(client);
		ck_wunlock(&sdata->instance_lock);

		ck_rlock(&sdata->workbase_lock);
		json_msg = __user_notify(sdata->current_workbase, client->user_instance, clean);
		ck_runlock(&sdata->workbase_lock);

		if (likely(json_msg))
			stratum_add_send(sdata, json_msg, client->id, SM_UPDATE);

		ck_wlock(&sdata->instance_lock);
		__dec_instance_ref(client);
	}
	ck_wunlock(&sdata->instance_lock);
}

static void send_json_err(sdata_t *sdata, const int64_t client_id, json_t *id_val, const char *err_msg)
{
	json_t *val;

	/* Some clients have no id_val so pass back an empty string. */
	if (unlikely(!id_val))
		JSON_CPACK(val, "{ssss}", "id", "", "error", err_msg);
	else
		JSON_CPACK(val, "{soss}", "id", json_deep_copy(id_val), "error", err_msg);
	stratum_add_send(sdata, val, client_id, SM_ERROR);
}

/* Needs to be entered with client holding a ref count. */
static void update_client(const stratum_instance_t *client, const int64_t client_id)
{
	sdata_t *sdata = client->sdata;

	if (!client->ckp->btcsolo)
		stratum_send_update(sdata, client_id, true);
	stratum_send_diff(sdata, client);
}

static json_params_t
*create_json_params(const int64_t client_id, const json_t *method, const json_t *params,
		    const json_t *id_val)
{
	json_params_t *jp = ckalloc(sizeof(json_params_t));

	jp->method = json_deep_copy(method);
	jp->params = json_deep_copy(params);
	jp->id_val = json_deep_copy(id_val);
	jp->client_id = client_id;
	return jp;
}

/* Implement support for the diff in the params as well as the originally
 * documented form of placing diff within the method. Needs to be entered with
 * client holding a ref count. */
static void suggest_diff(ckpool_t *ckp, stratum_instance_t *client, const char *method,
			 const json_t *params_val)
{
	json_t *arr_val = json_array_get(params_val, 0);
	int64_t sdiff;

	if (unlikely(!client_active(client))) {
		LOGNOTICE("Attempted to suggest diff on unauthorised client %s", client->identity);
		return;
	}
	if (arr_val && json_is_integer(arr_val))
		sdiff = json_integer_value(arr_val);
	else if (sscanf(method, "mining.suggest_difficulty(%"PRId64, &sdiff) != 1) {
		LOGINFO("Failed to parse suggest_difficulty for client %s", client->identity);
		return;
	}
	/* Clamp suggest diff to global pool mindiff */
	if (sdiff < ckp->mindiff)
		sdiff = ckp->mindiff;
	if (sdiff == client->suggest_diff)
		return;
	client->suggest_diff = sdiff;
	if (client->diff == sdiff)
		return;
	client->diff_change_job_id = client->sdata->workbase_id + 1;
	client->old_diff = client->diff;
	client->diff = sdiff;
	stratum_send_diff(ckp->sdata, client);
}

/* Send diff first when sending the first stratum template after subscribing */
static void init_client(const stratum_instance_t *client, const int64_t client_id)
{
	sdata_t *sdata = client->sdata;

	stratum_send_diff(sdata, client);
	if (!client->ckp->btcsolo)
		stratum_send_update(sdata, client_id, true);
}

/* When a node first connects it has no transactions so we have to send all
 * current ones to it. */
static void send_node_all_txns(sdata_t *sdata, const stratum_instance_t *client)
{
	json_t *txn_array, *val, *txn_val;
	txntable_t *txn, *tmp;
	smsg_t *msg;

	txn_array = json_array();

	ck_rlock(&sdata->txn_lock);
	HASH_ITER(hh, sdata->txns, txn, tmp) {
		JSON_CPACK(txn_val, "{ss,ss}", "hash", txn->hash, "data", txn->data);
		json_array_append_new(txn_array, txn_val);
	}
	ck_runlock(&sdata->txn_lock);

	if (client->trusted) {
		JSON_CPACK(val, "{ss,so}", "method", stratum_msgs[SM_TRANSACTIONS],
			   "transaction", txn_array);
	} else {
		JSON_CPACK(val, "{ss,so}", "node.method", stratum_msgs[SM_TRANSACTIONS],
			   "transaction", txn_array);
	}
	msg = ckzalloc(sizeof(smsg_t));
	msg->json_msg = val;
	msg->client_id = client->id;
	ckmsgq_add(sdata->ssends, msg);
	LOGNOTICE("Sending new node client %s all transactions", client->identity);
}

static void *setup_node(void *arg)
{
	stratum_instance_t *client = (stratum_instance_t *)arg;

	pthread_detach(pthread_self());

	client->latency = round_trip(client->address) / 2;
	LOGNOTICE("Node client %s %s latency set to %dms", client->identity,
		  client->address, client->latency);
	send_node_all_txns(client->sdata, client);
	dec_instance_ref(client->sdata, client);
	return NULL;
}

/* Create a thread to asynchronously set latency to the node to not
 * block. Increment the ref count to prevent the client pointer
 * dereferencing under us, allowing the thread to decrement it again when
 * finished. */
static void add_mining_node(ckpool_t *ckp, sdata_t *sdata, stratum_instance_t *client)
{
	pthread_t pth;

	ck_wlock(&sdata->instance_lock);
	client->node = true;
	DL_APPEND2(sdata->node_instances, client, node_prev, node_next);
	__inc_instance_ref(client);
	ck_wunlock(&sdata->instance_lock);

	LOGWARNING("Added client %s %s as mining node on server %d:%s", client->identity,
		   client->address, client->server, ckp->serverurl[client->server]);

	create_pthread(&pth, setup_node, client);
}

static void add_remote_server(sdata_t *sdata, stratum_instance_t *client)
{
	ck_wlock(&sdata->instance_lock);
	client->trusted = true;
	DL_APPEND2(sdata->remote_instances, client, remote_prev, remote_next);
	__inc_instance_ref(client);
	ck_wunlock(&sdata->instance_lock);

	send_node_all_txns(sdata, client);
	dec_instance_ref(sdata, client);
}

/* Enter with client holding ref count */
static void parse_method(ckpool_t *ckp, sdata_t *sdata, stratum_instance_t *client,
			 const int64_t client_id, json_t *id_val, json_t *method_val,
			 json_t *params_val)
{
	const char *method;

	/* Random broken clients send something not an integer as the id so we
	 * copy the json item for id_val as is for the response. By far the
	 * most common messages will be shares so look for those first */
	method = json_string_value(method_val);
	if (likely(cmdmatch(method, "mining.submit") && client->authorised)) {
		json_params_t *jp = create_json_params(client_id, method_val, params_val, id_val);

		ckmsgq_add(sdata->sshareq, jp);
		return;
	}

	if (cmdmatch(method, "mining.term")) {
		LOGDEBUG("Mining terminate requested from %s %s", client->identity, client->address);
		drop_client(ckp, sdata, client_id);
		return;
	}

	if (cmdmatch(method, "mining.subscribe")) {
		json_t *val, *result_val;

		if (unlikely(client->subscribed)) {
			LOGNOTICE("Client %s %s trying to subscribe twice",
				  client->identity, client->address);
			return;
		}
		result_val = parse_subscribe(client, client_id, params_val);
		/* Shouldn't happen, sanity check */
		if (unlikely(!result_val)) {
			LOGWARNING("parse_subscribe returned NULL result_val");
			return;
		}
		val = json_object();
		json_object_set_new_nocheck(val, "result", result_val);
		json_object_set_nocheck(val, "id", id_val);
		json_object_set_new_nocheck(val, "error", json_null());
		stratum_add_send(sdata, val, client_id, SM_SUBSCRIBERESULT);
		if (likely(client->subscribed))
			init_client(client, client_id);
		return;
	}

	if (unlikely(cmdmatch(method, "mining.remote"))) {
		char buf[256];

		/* Add this client as a trusted remote node in the connector and
		 * drop the client in the stratifier */
		if (!ckp->trusted[client->server] || ckp->proxy) {
			LOGNOTICE("Dropping client %s %s trying to authorise as remote node on non trusted server %d",
				  client->identity, client->address, client->server);
			connector_drop_client(ckp, client_id);
		} else {
			snprintf(buf, 255, "remote=%"PRId64, client_id);
			send_proc(ckp->connector, buf);
			add_remote_server(sdata, client);
		}
		sprintf(client->identity, "remote:%"PRId64, client_id);
		return;
	}

	if (unlikely(cmdmatch(method, "mining.node"))) {
		char buf[256];

		/* Add this client as a passthrough in the connector and
		 * add it to the list of mining nodes in the stratifier */
		if (!ckp->nodeserver[client->server] || ckp->proxy) {
			LOGNOTICE("Dropping client %s %s trying to authorise as node on non node server %d",
				  client->identity, client->address, client->server);
			connector_drop_client(ckp, client_id);
			drop_client(ckp, sdata, client_id);
		} else {
			snprintf(buf, 255, "passthrough=%"PRId64, client_id);
			send_proc(ckp->connector, buf);
			add_mining_node(ckp, sdata, client);
			sprintf(client->identity, "node:%"PRId64, client_id);
		}
		return;
	}

	if (unlikely(cmdmatch(method, "mining.passthrough"))) {
		char buf[256];

		if (ckp->proxy || ckp->node ) {
			LOGNOTICE("Dropping client %s %s trying to connect as passthrough on unsupported server %d",
				  client->identity, client->address, client->server);
			connector_drop_client(ckp, client_id);
			drop_client(ckp, sdata, client_id);
		} else {
			/*Flag this as a passthrough and manage its messages
			 * accordingly. No data from this client id should ever
			 * come directly back to this stratifier. */
			LOGNOTICE("Adding passthrough client %s %s", client->identity, client->address);
			client->passthrough = true;
			snprintf(buf, 255, "passthrough=%"PRId64, client_id);
			send_proc(ckp->connector, buf);
			sprintf(client->identity, "passthrough:%"PRId64, client_id);
		}
		return;
	}

	/* We shouldn't really allow unsubscribed users to authorise first but
	 * some broken stratum implementations do that and we can handle it. */
	if (cmdmatch(method, "mining.auth")) {
		json_params_t *jp;

		if (unlikely(client->authorised)) {
			LOGINFO("Client %s %s trying to authorise twice",
				client->identity, client->address);
			return;
		}
		jp = create_json_params(client_id, method_val, params_val, id_val);
		ckmsgq_add(sdata->sauthq, jp);
		return;
	}

        if (cmdmatch(method, "mining.configure")) {
		json_t *val, *result_val;
		char version_str[12];

		LOGINFO("Mining configure requested from %s %s", client->identity,
			client->address);
		sprintf(version_str, "%08x", ckp->version_mask);
		val = json_object();
		JSON_CPACK(result_val, "{sbss}", "version-rolling", json_true(),
			   "version-rolling.mask", version_str);
		json_object_set_new_nocheck(val, "result", result_val);
		json_object_set_nocheck(val, "id", id_val);
		json_object_set_new_nocheck(val, "error", json_null());
		stratum_add_send(sdata, val, client_id, SM_CONFIGURE);
		return;
	}

	/* We should only accept requests from subscribed and authed users here
	 * on */
	if (!client->subscribed) {
		LOGINFO("Dropping %s from unsubscribed client %s %s", method,
			client->identity, client->address);
		connector_drop_client(ckp, client_id);
		return;
	}

	/* We should only accept authorised requests from here on */
	if (!client->authorised) {
		LOGINFO("Dropping %s from unauthorised client %s %s", method,
			client->identity, client->address);
		return;
	}

	if (cmdmatch(method, "mining.suggest")) {
		suggest_diff(ckp, client, method, params_val);
		return;
	}

	/* Covers both get_transactions and get_txnhashes */
	if (cmdmatch(method, "mining.get")) {
		json_params_t *jp = create_json_params(client_id, method_val, params_val, id_val);

		ckmsgq_add(sdata->stxnq, jp);
		return;
	}

	/* Unhandled message here */
	LOGINFO("Unhandled client %s %s method %s", client->identity, client->address, method);
	return;
}

static void free_smsg(smsg_t *msg)
{
	json_decref(msg->json_msg);
	free(msg);
}

/* Even though we check the results locally in node mode, check the upstream
 * results in case of runs of invalids. */
static void parse_share_result(ckpool_t *ckp, stratum_instance_t *client, json_t *val)
{
	time_t now_t;
	ts_t now;

	if (likely(json_is_true(val))) {
		client->upstream_invalid = 0;
		return;
	}
	ts_realtime(&now);
	now_t = now.tv_sec;
	if (client->upstream_invalid < client->last_share.tv_sec || !client->upstream_invalid)
		client->upstream_invalid = now_t;
	else if (client->upstream_invalid && client->upstream_invalid < now_t - 150) {
		LOGNOTICE("Client %s upstream rejects for 150s, disconnecting", client->identity);
		connector_drop_client(ckp, client->id);
		client->reject = 3;
	}
}

static void parse_diff(stratum_instance_t *client, json_t *val)
{
	double diff = json_number_value(json_array_get(val, 0));

	LOGINFO("Set client %s to diff %lf", client->identity, diff);
	client->diff = diff;
}

static void parse_subscribe_result(stratum_instance_t *client, json_t *val)
{
	int len;

	strncpy(client->enonce1, json_string_value(json_array_get(val, 1)), 16);
	len = strlen(client->enonce1) / 2;
	hex2bin(client->enonce1bin, client->enonce1, len);
	memcpy(&client->enonce1_64, client->enonce1bin, 8);
	LOGINFO("Client %s got enonce1 %lx string %s", client->identity, client->enonce1_64, client->enonce1);
}

static void parse_authorise_result(ckpool_t *ckp, sdata_t *sdata, stratum_instance_t *client,
				   json_t *val)
{
	if (!json_is_true(val)) {
		LOGNOTICE("Client %s was not authorised upstream, dropping", client->identity);
		client->authorised = false;
		connector_drop_client(ckp, client->id);
		drop_client(ckp, sdata, client->id);
	} else
		LOGINFO("Client %s was authorised upstream", client->identity);
}

static int node_msg_type(json_t *val)
{
	const char *method;
	int i, ret = -1;

	if (!val)
		goto out;
	method = json_string_value(json_object_get(val, "node.method"));
	if (method) {
		for (i = 0; i < SM_NONE; i++) {
			if (!strcmp(method, stratum_msgs[i])) {
				ret = i;
				break;
			}
		}
		json_object_del(val, "node.method");
	} else
		method = json_string_value(json_object_get(val, "method"));

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

static user_instance_t *generate_remote_user(ckpool_t *ckp, const char *workername)
{
	char *base_username = strdupa(workername), *username;
	sdata_t *sdata = ckp->sdata;
	bool new_user = false;
	user_instance_t *user;
	int len;

	username = strsep(&base_username, "._");
	if (!username || !strlen(username))
		username = base_username;
	len = strlen(username);
	if (unlikely(len > 127))
		username[127] = '\0';

	user = get_create_user(sdata, username, &new_user);

	if (!ckp->proxy && (new_user || !user->btcaddress)) {
		/* Is this a btc address based username? */
		if (generator_checkaddr(ckp, username, &user->script, &user->segwit)) {
			user->btcaddress = true;
			user->txnlen = address_to_txn(user->txnbin, username, user->script, user->segwit);
		}
	}
	if (new_user) {
		LOGNOTICE("Added new remote user %s%s", username, user->btcaddress ?
			  " as address based registration" : "");
	}

	return user;
}

static void parse_remote_share(ckpool_t *ckp, sdata_t *sdata, json_t *val, const char *buf)
{
	json_t *workername_val = json_object_get(val, "workername");
	worker_instance_t *worker;
	const char *workername;
	double diff, sdiff = 0;
	user_instance_t *user;
	tv_t now_t;

	workername = json_string_value(workername_val);
	if (unlikely(!workername_val || !workername)) {
		LOGWARNING("Failed to get workername from remote message %s", buf);
		return;
	}
	if (unlikely(!json_get_double(&diff, val, "diff") || diff < 1)) {
		LOGWARNING("Unable to parse valid diff from remote message %s", buf);
		return;
	}
	json_get_double(&sdiff, val, "sdiff");
	user = generate_remote_user(ckp, workername);
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

static void parse_remote_shareerr(ckpool_t *ckp, json_t *val, const char *buf)
{
	const char *workername;

	workername = json_string_value(json_object_get(val, "workername"));
	if (unlikely(!workername)) {
		LOGWARNING("Failed to find workername in parse_remote_shareerr %s", buf);
		return;
	}
	/* Return value ignored */
	generate_remote_user(ckp, workername);
}

static void send_auth_response(sdata_t *sdata, const int64_t client_id, const bool ret,
			       json_t *id_val, json_t *err_val)
{
	json_t *json_msg = json_object();

	json_object_set_new_nocheck(json_msg, "result", json_boolean(ret));
	json_object_set_new_nocheck(json_msg, "error", err_val ? err_val : json_null());
	json_object_set(json_msg, "id", id_val);
	stratum_add_send(sdata, json_msg, client_id, SM_AUTHRESULT);
}

static void send_auth_success(ckpool_t *ckp, sdata_t *sdata, stratum_instance_t *client)
{
	char *buf;

	ASPRINTF(&buf, "Authorised, welcome to %s %s!", ckp->name,
		 client->user_instance->username);
	stratum_send_message(sdata, client, buf);
	free(buf);
}

static void send_auth_failure(sdata_t *sdata, stratum_instance_t *client)
{
	stratum_send_message(sdata, client, "Failed authorisation :(");
}

/* For finding a client by its virtualid instead of client->id. This is an
 * inefficient lookup but only occurs once on parsing a remote auth from the
 * upstream pool on passthrough subclients. */
static stratum_instance_t *ref_instance_by_virtualid(sdata_t *sdata, int64_t *client_id)
{
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

void parse_upstream_auth(ckpool_t *ckp, json_t *val)
{
	json_t *id_val = NULL, *err_val = NULL;
	sdata_t *sdata = ckp->sdata;
	stratum_instance_t *client;
	bool ret, warn = false;
	int64_t client_id;

	id_val = json_object_get(val, "id");
	if (unlikely(!id_val))
		goto out;
	if (unlikely(!json_get_int64(&client_id, val, "client_id")))
		goto out;
	if (unlikely(!json_get_bool(&ret, val, "result")))
		goto out;
	err_val = json_object_get(val, "error");
	client = ref_instance_by_id(sdata, client_id);
	/* Is this client_id a virtualid from a passthrough subclient */
	if (!client)
		client = ref_instance_by_virtualid(sdata, &client_id);
	if (!client) {
		LOGINFO("Failed to find client id %"PRId64" in parse_upstream_auth",
		        client_id);
		goto out;
	}
	if (ret)
		send_auth_success(ckp, sdata, client);
	else
		send_auth_failure(sdata, client);
	send_auth_response(sdata, client_id, ret, id_val, err_val);
	client_auth(ckp, client, client->user_instance, ret);
	dec_instance_ref(sdata, client);
out:
	if (unlikely(warn)) {
		char *s = json_dumps(val, 0);

		LOGWARNING("Failed to get valid upstream result in parse_upstream_auth %s", s);
		free(s);
	}
}

void parse_upstream_workinfo(ckpool_t *ckp, json_t *val)
{
	add_node_base(ckp, val, true, 0);
}

#define parse_remote_workinfo(ckp, val, client_id) add_node_base(ckp, val, true, client_id)

static void parse_remote_auth(ckpool_t *ckp, sdata_t *sdata, json_t *val, stratum_instance_t *remote,
			      const int64_t remote_id)
{
	json_t *params, *method, *id_val;
	stratum_instance_t *client;
	json_params_t *jp;
	int64_t client_id;

	if (ckp->btcsolo) {
		LOGWARNING("Got remote auth request in btcsolo mode, ignoring!");
		return;
	}
	json_get_int64(&client_id, val, "clientid");
	/* Encode remote server client_id into remote client's id */
	client_id = (remote_id << 32) | (client_id & 0xffffffffll);
	id_val = json_object_get(val, "id");
	method = json_object_get(val, "method");
	params = json_object_get(val, "params");
	jp = create_json_params(client_id, method, params, id_val);

	/* This is almost certainly the first time we'll see this client_id so
	 * create a new stratum instance temporarily just for auth with a plan
	 * to drop the client id locally once we finish with it */
	ck_wlock(&sdata->instance_lock);
	client = __instance_by_id(sdata, client_id);
	if (likely(!client))
		client = __stratum_add_instance(ckp, client_id, remote->address, remote->server);
	client->remote = true;
	json_strdup(&client->useragent, val, "useragent");
	json_strcpy(client->enonce1, val, "enonce1");
	json_strcpy(client->address, val, "address");
	ck_wunlock(&sdata->instance_lock);

	ckmsgq_add(sdata->sauthq, jp);
}

/* Get the remote worker count once per minute from all the remote servers */
static void parse_remote_workers(sdata_t *sdata, const json_t *val, const char *buf)
{
	json_t *username_val = json_object_get(val, "username");
	user_instance_t *user;
	const char *username;
	int workers;

	username = json_string_value(username_val);
	if (unlikely(!username_val || !username)) {
		LOGWARNING("Failed to get username from remote message %s", buf);
		return;
	}
	user = get_user(sdata, username);
	if (unlikely(!json_get_int(&workers, val, "workers"))) {
		LOGWARNING("Failed to get workers from remote message %s", buf);
		return;
	}
	user->remote_workers += workers;
	LOGDEBUG("Adding %d remote workers to user %s", workers, username);
}

/* Attempt to submit a remote block locally by recreating it from its workinfo */
static void parse_remote_block(ckpool_t *ckp, sdata_t *sdata, json_t *val, const char *buf,
			       const int64_t client_id)
{
	json_t *workername_val = json_object_get(val, "workername"),
		*name_val = json_object_get(val, "name"), *res;
	const char *workername, *name, *coinbasehex, *swaphex, *cnfrm;
	workbase_t *wb = NULL;
	double diff = 0;
	int height = 0;
	int64_t id = 0;
	char *msg;
	int cblen;

	name = json_string_value(name_val);
	if (!name_val || !name)
		goto out_add;

	/* If this is the confirm block message don't try to resubmit it */
	cnfrm = json_string_value(json_object_get(val, "confirmed"));
	if (cnfrm && cnfrm[0] == '1')
		goto out_add;

	json_get_int64(&id, val, "workinfoid");
	coinbasehex = json_string_value(json_object_get(val, "coinbasehex"));
	swaphex = json_string_value(json_object_get(val, "swaphex"));
	json_get_int(&cblen, val, "cblen");
	json_get_double(&diff, val, "diff");

	if (likely(id && coinbasehex && swaphex && cblen))
		wb = get_remote_workbase(sdata, id, client_id);

	if (unlikely(!wb))
		LOGWARNING("Inadequate data locally to attempt submit of remote block");
	else {
		uchar swap[80], hash[32], hash1[32], flip32[32];
		char *coinbase = alloca(cblen), *gbt_block;
		char blockhash[68];

		LOGWARNING("Possible remote block solve diff %lf !", diff);
		hex2bin(coinbase, coinbasehex, cblen);
		hex2bin(swap, swaphex, 80);
		sha256(swap, 80, hash1);
		sha256(hash1, 32, hash);
		gbt_block = process_block(wb, coinbase, cblen, swap, hash, flip32, blockhash);
		/* Note nodes use jobid of the mapped_id instead of workinfoid */
		json_set_int64(val, "jobid", wb->mapped_id);
		send_nodes_block(sdata, val, client_id);
		/* We rely on the remote server to give us the ID_BLOCK
		 * responses, so only use this response to determine if we
		 * should reset the best shares. */
		if (local_block_submit(ckp, gbt_block, flip32, wb->height)) {
			block_share_summary(sdata);
			reset_bestshares(sdata);
		}
		put_remote_workbase(sdata, wb);
	}

	workername = json_string_value(workername_val);
	if (unlikely(!workername_val || !workername)) {
		LOGWARNING("Failed to get workername from remote message %s", buf);
		workername = "";
	}
	if (unlikely(!json_get_int(&height, val, "height")))
		LOGWARNING("Failed to get height from remote message %s", buf);
	ASPRINTF(&msg, "Block %d solved by %s @ %s!", height, workername, name);
	LOGWARNING("%s", msg);
	stratum_broadcast_message(sdata, msg);
	free(msg);
out_add:
	/* Make a duplicate for use downstream */
	res = json_deep_copy(val);
	remap_workinfo_id(sdata, res, client_id);
	if (!ckp->remote)
		downstream_json(sdata, res, client_id, SSEND_PREPEND);

	json_decref(res);
}

void parse_upstream_block(ckpool_t *ckp, json_t *val)
{
	char *buf;
	sdata_t *sdata = ckp->sdata;

	buf = json_dumps(val, 0);
	parse_remote_block(ckp, sdata, val, buf, 0);
	free(buf);
}

static void send_remote_pong(sdata_t *sdata, stratum_instance_t *client)
{
	json_t *json_msg;

	JSON_CPACK(json_msg, "{ss}", "method", "pong");
	stratum_add_send(sdata, json_msg, client->id, SM_PONG);
}

static void add_node_txns(ckpool_t *ckp, sdata_t *sdata, const json_t *val)
{
	json_t *txn_array, *txn_val, *data_val, *hash_val;
	txntable_t *txns = NULL;
	int i, arr_size;
	int added = 0;

	txn_array = json_object_get(val, "transaction");
	arr_size = json_array_size(txn_array);

	for (i = 0; i < arr_size; i++) {
		const char *hash, *data;

		txn_val = json_array_get(txn_array, i);
		data_val = json_object_get(txn_val, "data");
		hash_val = json_object_get(txn_val, "hash");
		data = json_string_value(data_val);
		hash = json_string_value(hash_val);
		if (unlikely(!data || !hash)) {
			LOGERR("Failed to get hash/data in add_node_txns");
			continue;
		}

		if (add_txn(ckp, sdata, &txns, hash, data, false))
			added++;
	}

	if (added)
		update_txns(ckp, sdata, txns, false);
}

void parse_remote_txns(ckpool_t *ckp, const json_t *val)
{
	add_node_txns(ckp, ckp->sdata, val);
}

static json_t *get_hash_transactions(sdata_t *sdata, const json_t *hashes)
{
	json_t *txn_array = json_array(), *arr_val;
	int found = 0;
	size_t index;

	ck_rlock(&sdata->txn_lock);
	json_array_foreach(hashes, index, arr_val) {
		const char *hash = json_string_value(arr_val);
		json_t *txn_val;
		txntable_t *txn;

		HASH_FIND_STR(sdata->txns, hash, txn);
		if (!txn)
			continue;
		JSON_CPACK(txn_val, "{ss,ss}",
			   "hash", hash, "data", txn->data);
		json_array_append_new(txn_array, txn_val);
		found++;
	}
	ck_runlock(&sdata->txn_lock);

	return txn_array;
}

static json_t *get_reqtxns(sdata_t *sdata, const json_t *val, bool downstream)
{
	json_t *hashes = json_object_get(val, "hash");
	json_t *txns, *ret = NULL;
	int requested, found;

	if (unlikely(!hashes) || !json_is_array(hashes))
		goto out;
	requested = json_array_size(hashes);
	if (unlikely(!requested))
		goto out;

	txns = get_hash_transactions(sdata, hashes);
	found = json_array_size(txns);
	if (found) {
		JSON_CPACK(ret, "{ssso}", "method", stratum_msgs[SM_TRANSACTIONS], "transaction", txns);
		LOGINFO("Sending %d found of %d requested txns %s", found, requested,
			downstream ? "downstream" : "upstream");
	} else
		json_decref(txns);
out:
	return ret;
}

static void parse_remote_reqtxns(sdata_t *sdata, const json_t *val, const int64_t client_id)
{
	json_t *ret = get_reqtxns(sdata, val, true);

	if (!ret)
		return;
	stratum_add_send(sdata, ret, client_id, SM_TRANSACTIONS);
}

void parse_upstream_reqtxns(ckpool_t *ckp, json_t *val)
{
	json_t *ret = get_reqtxns(ckp->sdata, val, false);
	char *msg;

	if (!ret)
		return;
	msg = json_dumps(ret, JSON_NO_UTF8 | JSON_PRESERVE_ORDER | JSON_COMPACT | JSON_EOL);
	json_decref(ret);
	connector_upstream_msg(ckp, msg);
}

static void parse_trusted_msg(ckpool_t *ckp, sdata_t *sdata, json_t *val, stratum_instance_t *client)
{
	json_t *method_val = json_object_get(val, "method");
	char *buf = json_dumps(val, 0);
	const char *method;

	LOGDEBUG("Got remote message %s", buf);
	method = json_string_value(method_val);
	if (unlikely(!method_val || !method)) {
		LOGWARNING("Failed to get method from remote message %s", buf);
		goto out;
	}

	if (likely(!safecmp(method, stratum_msgs[SM_SHARE])))
		parse_remote_share(ckp, sdata, val, buf);
	else if (!safecmp(method, stratum_msgs[SM_TRANSACTIONS]))
		add_node_txns(ckp, sdata, val);
	else if (!safecmp(method, stratum_msgs[SM_WORKINFO]))
		parse_remote_workinfo(ckp, val, client->id);
	else if (!safecmp(method, stratum_msgs[SM_AUTH]))
		parse_remote_auth(ckp, sdata, val, client, client->id);
	else if (!safecmp(method, stratum_msgs[SM_SHAREERR]))
		parse_remote_shareerr(ckp, val, buf);
	else if (!safecmp(method, stratum_msgs[SM_BLOCK]))
		parse_remote_block(ckp, sdata, val, buf, client->id);
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
static void node_client_msg(ckpool_t *ckp, json_t *val, stratum_instance_t *client)
{
	json_t *params, *method, *res_val, *id_val, *err_val = NULL;
	int msg_type = node_msg_type(val);
	sdata_t *sdata = ckp->sdata;
	json_params_t *jp;
	char *buf = NULL;

	if (msg_type < 0) {
		buf = json_dumps(val, 0);
		LOGERR("Missing client %s node method from %s", client->identity, buf);
		goto out;
	}
	LOGDEBUG("Got client %s node method %d:%s", client->identity, msg_type, stratum_msgs[msg_type]);
	id_val = json_object_get(val, "id");
	method = json_object_get(val, "method");
	params = json_object_get(val, "params");
	res_val = json_object_get(val, "result");
	switch (msg_type) {
		case SM_SHARE:
			jp = create_json_params(client->id, method, params, id_val);
			ckmsgq_add(sdata->sshareq, jp);
			break;
		case SM_SHARERESULT:
			parse_share_result(ckp, client, res_val);
			break;
		case SM_DIFF:
			parse_diff(client, params);
			break;
		case SM_SUBSCRIBE:
			parse_subscribe(client, client->id, params);
			break;
		case SM_SUBSCRIBERESULT:
			parse_subscribe_result(client, res_val);
			break;
		case SM_AUTH:
			parse_authorise(client, params, &err_val);
			break;
		case SM_AUTHRESULT:
			parse_authorise_result(ckp, sdata, client, res_val);
			break;
		case SM_NONE:
			buf = json_dumps(val, 0);
			LOGNOTICE("Unrecognised method from client %s :%s",
				  client->identity, buf);
			break;
		default:
			break;
	}
out:
	free(buf);
}

static void parse_node_msg(ckpool_t *ckp, sdata_t *sdata, json_t *val)
{
	int msg_type = node_msg_type(val);

	if (msg_type < 0) {
		char *buf = json_dumps(val, 0);

		LOGERR("Missing node method from %s", buf);
		free(buf);
		return;
	}
	LOGDEBUG("Got node method %d:%s", msg_type, stratum_msgs[msg_type]);
	switch (msg_type) {
		case SM_TRANSACTIONS:
			add_node_txns(ckp, sdata, val);
			break;
		case SM_WORKINFO:
			add_node_base(ckp, val, false, 0);
			break;
		case SM_BLOCK:
			submit_node_block(ckp, sdata, val);
			break;
		default:
			break;
	}
}

/* Entered with client holding ref count */
static void parse_instance_msg(ckpool_t *ckp, sdata_t *sdata, smsg_t *msg, stratum_instance_t *client)
{
	json_t *val = msg->json_msg, *id_val, *method, *params;
	int64_t client_id = msg->client_id;
	int delays = 0;

	if (client->reject == 3) {
		LOGINFO("Dropping client %s %s tagged for lazy invalidation",
			client->identity, client->address);
		connector_drop_client(ckp, client_id);
		return;
	}

	/* Return back the same id_val even if it's null or not existent. */
	id_val = json_object_get(val, "id");

	method = json_object_get(val, "method");
	if (unlikely(!method)) {
		json_t *res_val = json_object_get(val, "result");

		/* Is this a spurious result or ping response? */
		if (res_val) {
			const char *result = json_string_value(res_val);

			if (!safecmp(result, "pong"))
				LOGDEBUG("Received pong from client %s", client->identity);
			else
				LOGDEBUG("Received spurious response %s from client %s",
					 result ? result : "", client->identity);
			return;
		}
		send_json_err(sdata, client_id, id_val, "-3:method not found");
		return;
	}
	if (unlikely(!json_is_string(method))) {
		send_json_err(sdata, client_id, id_val, "-1:method is not string");
		return;
	}
	params = json_object_get(val, "params");
	if (unlikely(!params)) {
		send_json_err(sdata, client_id, id_val, "-1:params not found");
		return;
	}
	/* At startup we block until there's a current workbase otherwise we
	 * will reject miners with the initialising message. A slightly delayed
	 * response to subscribe is better tolerated. */
	while (unlikely(!ckp->proxy && !sdata->current_workbase)) {
		cksleep_ms(100);
		if (!(++delays % 50))
			LOGWARNING("%d Second delay waiting for bitcoind at startup", delays / 10);
	}
	parse_method(ckp, sdata, client, client_id, id_val, method, params);
}

static void srecv_process(ckpool_t *ckp, json_t *val)
{
	char address[INET6_ADDRSTRLEN], *buf = NULL;
	bool noid = false, dropped = false;
	sdata_t *sdata = ckp->sdata;
	stratum_instance_t *client;
	smsg_t *msg;
	int server;

	if (unlikely(!val)) {
		LOGWARNING("srecv_process received NULL val!");
		return;
	}

	msg = ckzalloc(sizeof(smsg_t));
	msg->json_msg = val;
	val = json_object_get(msg->json_msg, "client_id");
	if (unlikely(!val)) {
		if (ckp->node)
			parse_node_msg(ckp, sdata, msg->json_msg);
		else {
			buf = json_dumps(val, JSON_COMPACT);
			LOGWARNING("Failed to extract client_id from connector json smsg %s", buf);
		}
		goto out;
	}

	msg->client_id = json_integer_value(val);
	json_object_clear(val);

	val = json_object_get(msg->json_msg, "address");
	if (unlikely(!val)) {
		buf = json_dumps(val, JSON_COMPACT);
		LOGWARNING("Failed to extract address from connector json smsg %s", buf);
		goto out;
	}
	strcpy(address, json_string_value(val));
	json_object_clear(val);

	val = json_object_get(msg->json_msg, "server");
	if (unlikely(!val)) {
		buf = json_dumps(val, JSON_COMPACT);
		LOGWARNING("Failed to extract server from connector json smsg %s", buf);
		goto out;
	}
	server = json_integer_value(val);
	json_object_clear(val);

	/* Parse the message here */
	ck_wlock(&sdata->instance_lock);
	client = __instance_by_id(sdata, msg->client_id);
	/* If client_id instance doesn't exist yet, create one */
	if (unlikely(!client)) {
		noid = true;
		client = __stratum_add_instance(ckp, msg->client_id, address, server);
	} else if (unlikely(client->dropped))
		dropped = true;
	if (likely(!dropped))
		__inc_instance_ref(client);
	ck_wunlock(&sdata->instance_lock);

	if (unlikely(dropped)) {
		/* Client may be NULL here */
		LOGNOTICE("Stratifier skipped dropped instance %"PRId64" message from server %d",
			  msg->client_id, server);
		connector_drop_client(ckp, msg->client_id);
		goto out;
	}
	if (unlikely(noid))
		LOGINFO("Stratifier added instance %s server %d", client->identity, server);

	if (client->trusted)
		parse_trusted_msg(ckp, sdata, msg->json_msg, client);
	else if (ckp->node)
		node_client_msg(ckp, msg->json_msg, client);
	else
		parse_instance_msg(ckp, sdata, msg, client);
	dec_instance_ref(sdata, client);
out:
	free_smsg(msg);
	free(buf);
}

void _stratifier_add_recv(ckpool_t *ckp, json_t *val, const char *file, const char *func, const int line)
{
	sdata_t *sdata;

	if (unlikely(!val)) {
		LOGWARNING("_stratifier_add_recv received NULL val from %s %s:%d", file, func, line);
		return;
	}
	sdata = ckp->sdata;
	ckmsgq_add(sdata->srecvs, val);
}

static void ssend_process(ckpool_t *ckp, smsg_t *msg)
{
	if (unlikely(!msg->json_msg)) {
		LOGERR("Sent null json msg to stratum_sender");
		free(msg);
		return;
	}

	/* Add client_id to the json message and send it to the
	 * connector process to be delivered */
	json_object_set_new_nocheck(msg->json_msg, "client_id", json_integer(msg->client_id));
	connector_add_message(ckp, msg->json_msg);
	/* The connector will free msg->json_msg */
	free(msg);
}

static void discard_json_params(json_params_t *jp)
{
	json_decref(jp->method);
	json_decref(jp->params);
	if (jp->id_val)
		json_decref(jp->id_val);
	free(jp);
}

static void steal_json_id(json_t *val, json_params_t *jp)
{
	/* Steal the id_val as is to avoid a copy */
	json_object_set_new_nocheck(val, "id", jp->id_val);
	jp->id_val = NULL;
}

static void sshare_process(ckpool_t *ckp, json_params_t *jp)
{
	json_t *result_val, *json_msg, *err_val = NULL;
	stratum_instance_t *client;
	sdata_t *sdata = ckp->sdata;
	int64_t client_id;

	client_id = jp->client_id;

	client = ref_instance_by_id(sdata, client_id);
	if (unlikely(!client)) {
		LOGINFO("Share processor failed to find client id %"PRId64" in hashtable!", client_id);
		goto out;
	}
	if (unlikely(!client->authorised)) {
		LOGDEBUG("Client %s no longer authorised to submit shares", client->identity);
		goto out_decref;
	}
	json_msg = json_object();
	result_val = parse_submit(client, json_msg, jp->params, &err_val);
	json_object_set_new_nocheck(json_msg, "result", result_val);
	json_object_set_new_nocheck(json_msg, "error", err_val ? err_val : json_null());
	steal_json_id(json_msg, jp);
	stratum_add_send(sdata, json_msg, client_id, SM_SHARERESULT);
out_decref:
	dec_instance_ref(sdata, client);
out:
	discard_json_params(jp);
}

/* As ref_instance_by_id but only returns clients not authorising or authorised,
 * and sets the authorising flag */
static stratum_instance_t *preauth_ref_instance_by_id(sdata_t *sdata, const int64_t id)
{
	stratum_instance_t *client;

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
static void upstream_auth(ckpool_t *ckp, stratum_instance_t *client, json_params_t *jp)
{
	json_t *val = json_object();
	char cdfield[64];
	char *msg;
	ts_t now;

	ts_realtime(&now);
	sprintf(cdfield, "%lu,%lu", now.tv_sec, now.tv_nsec);

	json_set_object(val, "params", jp->params);
	json_set_object(val, "id", jp->id_val);
	json_set_object(val, "method", jp->method);
	json_set_string(val, "method", stratum_msgs[SM_AUTH]);

	json_set_string(val, "useragent", client->useragent ? : "");
	json_set_string(val, "enonce1", client->enonce1 ? : "");
	json_set_string(val, "address", client->address);
	json_set_int64(val, "clientid", client->virtualid);
	msg = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER | JSON_COMPACT | JSON_EOL);
	json_decref(val);
	connector_upstream_msg(ckp, msg);
}

static void sauth_process(ckpool_t *ckp, json_params_t *jp)
{
	json_t *result_val, *err_val = NULL;
	sdata_t *sdata = ckp->sdata;
	stratum_instance_t *client;
	int64_t mindiff, client_id;
	bool ret;

	client_id = jp->client_id;

	client = preauth_ref_instance_by_id(sdata, client_id);
	if (unlikely(!client)) {
		LOGINFO("Authoriser failed to find client id %"PRId64" in hashtable!", client_id);
		goto out_noclient;
	}

	result_val = parse_authorise(client, jp->params, &err_val);
	ret = json_is_true(result_val);
	if (ret) {
		/* So far okay in remote mode, remainder to be done by upstream
		 * pool */
		if (ckp->remote && !ckp->btcsolo) {
			upstream_auth(ckp, client, jp);
			goto out;
		}
		send_auth_success(ckp, sdata, client);
	} else
		send_auth_failure(sdata, client);
	send_auth_response(sdata, client_id, ret, jp->id_val, err_val);
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
		mindiff = MAX(ckp->mindiff, mindiff);
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

static int transactions_by_jobid(sdata_t *sdata, const int64_t id)
{
	workbase_t *wb;
	int ret = -1;

	ck_rlock(&sdata->workbase_lock);
	HASH_FIND_I64(sdata->workbases, &id, wb);
	if (wb)
		ret = wb->txns;
	ck_runlock(&sdata->workbase_lock);

	return ret;
}

static json_t *txnhashes_by_jobid(sdata_t *sdata, const int64_t id)
{
	json_t *ret = NULL;
	workbase_t *wb;

	ck_rlock(&sdata->workbase_lock);
	HASH_FIND_I64(sdata->workbases, &id, wb);
	if (wb)
		ret = json_string(wb->txn_hashes);
	ck_runlock(&sdata->workbase_lock);

	return ret;
}

static void send_transactions(ckpool_t *ckp, json_params_t *jp)
{
	const char *msg = json_string_value(jp->method),
		*params = json_string_value(json_array_get(jp->params, 0));
	stratum_instance_t *client = NULL;
	sdata_t *sdata = ckp->sdata;
	json_t *val, *hashes;
	int64_t job_id = 0;
	time_t now_t;

	if (unlikely(!msg || !strlen(msg))) {
		LOGWARNING("send_transactions received null method");
		goto out;
	}
	val = json_object();
	steal_json_id(val, jp);
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
			json_set_int(val, "result", txns);
			json_object_set_new_nocheck(val, "error", json_null());
		} else
			json_set_string(val, "error", "Invalid job_id");
		goto out_send;
	}
	if (!cmdmatch(msg, "mining.get_txnhashes")) {
		LOGDEBUG("Unhandled mining get request: %s", msg);
		json_set_string(val, "error", "Unhandled");
		goto out_send;
	}

	client = ref_instance_by_id(sdata, jp->client_id);
	if (unlikely(!client)) {
		LOGINFO("send_transactions failed to find client id %"PRId64" in hashtable!",
			jp->client_id);
		goto out;
	}

	now_t = time(NULL);
	if (now_t - client->last_txns < ckp->update_interval) {
		LOGNOTICE("Rate limiting get_txnhashes on client %"PRId64"!", jp->client_id);
		json_set_string(val, "error", "Ratelimit");
		goto out_send;
	}
	client->last_txns = now_t;
	if (!params || !strlen(params)) {
		json_set_string(val, "error", "Invalid params");
		goto out_send;
	}
	sscanf(params, "%lx", &job_id);
	hashes = txnhashes_by_jobid(sdata, job_id);
	if (hashes) {
		json_object_set_new_nocheck(val, "result", hashes);
		json_object_set_new_nocheck(val, "error", json_null());
	} else
		json_set_string(val, "error", "Invalid job_id");
out_send:
	stratum_add_send(sdata, val, jp->client_id, SM_TXNSRESULT);
out:
	if (client)
		dec_instance_ref(sdata, client);
	discard_json_params(jp);
}

static void add_log_entry(log_entry_t **entries, char **fname, char **buf)
{
	log_entry_t *entry = ckalloc(sizeof(log_entry_t));

	entry->fname = *fname;
	*fname = NULL;
	entry->buf = *buf;
	*buf = NULL;
	DL_APPEND(*entries, entry);
}

static void dump_log_entries(log_entry_t **entries)
{
	log_entry_t *entry, *tmpentry;
	FILE *fp;

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

static void upstream_workers(ckpool_t *ckp, user_instance_t *user)
{
	char *msg;

	ASPRINTF(&msg, "{\"method\":\"workers\",\"username\":\"%s\",\"workers\":%d}\n",
		 user->username, user->workers);
	connector_upstream_msg(ckp, msg);
}


/* To iterate over all users, if user is initially NULL, this will return the first entry,
 * otherwise it will return the entry after user, and NULL if there are no more entries.
 * Allows us to grab and drop the lock on each iteration. */
static user_instance_t *next_user(sdata_t *sdata, user_instance_t *user)
{
	ck_rlock(&sdata->instance_lock);
	if (unlikely(!user))
		user = sdata->user_instances;
	else
		user = user->hh.next;
	ck_runlock(&sdata->instance_lock);

	return user;
}

/* Ditto for worker */
static worker_instance_t *next_worker(sdata_t *sdata, user_instance_t *user, worker_instance_t *worker)
{
	ck_rlock(&sdata->instance_lock);
	if (!worker)
		worker = user->worker_instances;
	else
		worker = worker->next;
	ck_runlock(&sdata->instance_lock);

	return worker;
}

static void *statsupdate(void *arg)
{
	ckpool_t *ckp = (ckpool_t *)arg;
	sdata_t *sdata = ckp->sdata;
	pool_stats_t *stats = &sdata->stats;

	pthread_detach(pthread_self());
	rename_proc("statsupdate");

	tv_time(&stats->start_time);
	cksleep_prepare_r(&stats->last_update);
	sleep(1);

	while (42) {
		double ghs, ghs1, ghs5, ghs15, ghs60, ghs360, ghs1440, ghs10080,
			per_tdiff, percent;
		char suffix1[16], suffix5[16], suffix15[16], suffix60[16], cdfield[64];
		char suffix360[16], suffix1440[16], suffix10080[16];
		int remote_users = 0, remote_workers = 0, idle_workers = 0;
		log_entry_t *log_entries = NULL;
		char_entry_t *char_list = NULL;
		stratum_instance_t *client;
		user_instance_t *user;
		char *fname, *s, *sp;
		tv_t now, diff;
		ts_t ts_now;
		json_t *val;
		FILE *fp;
		int i;

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
			/* Look for clients that may have been dropped which the
			 * stratifier has not been informed about and ask the
			 * connector if they still exist */
			if (client->dropped)
				connector_test_client(ckp, client->id);
			else if (remote_server(client)) {
				/* Do nothing to these */
			} else if (!client->authorised) {
				/* Test for clients that haven't authed in over a minute
				 * and drop them lazily */
				if (now.tv_sec > client->start_time + 60) {
					client->dropped = true;
					connector_drop_client(ckp, client->id);
				}
			} else {
				per_tdiff = tvdiff(&now, &client->last_share);
				/* Decay times per connected instance */
				if (per_tdiff > 60) {
					/* No shares for over a minute, decay to 0 */
					decay_client(client, 0, &now);
					idle_workers++;
					if (per_tdiff > 600)
						client->idle = true;
					/* Test idle clients are still connected */
					connector_test_client(ckp, client->id);
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
			worker_instance_t *worker;
			json_t *user_array;
			bool idle = false;

			if (!user->authorised)
				continue;

			tv_time(&now);

			/* Decay times per user */
			per_tdiff = tvdiff(&now, &user->last_share);
			if (per_tdiff > 60) {
				/* Drop storage of users idle for 1 week */
				if (per_tdiff > 600000) {
					LOGDEBUG("Skipping user %s", user->username);
					continue;
				}
				decay_user(user, 0, &now);
				idle = true;
			}

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

			JSON_CPACK(val, "{ss,ss,ss,ss,ss,si,si,sI,sf,sI, sI}",
					"hashrate1m", suffix1,
					"hashrate5m", suffix5,
					"hashrate1hr", suffix60,
					"hashrate1d", suffix1440,
					"hashrate7d", suffix10080,
				        "lastshare", user->last_share.tv_sec,
					"workers", user->workers + user->remote_workers,
					"shares", user->shares,
					"bestshare", user->best_diff,
					"bestever", user->best_ever,
					"authorised", user->auth_time);

			if (user->remote_workers) {
				remote_workers += user->remote_workers;
				/* Reset the remote_workers count once per minute */
				user->remote_workers = 0;
				/* We check this unlocked but transiently
				 * wrong is harmless */
				if (!user->workers)
					remote_users++;
			}

			if (!idle) {
				s = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER | JSON_COMPACT);
				ASPRINTF(&sp, "User %s:%s", user->username, s);
				dealloc(s);
				add_msg_entry(&char_list, &sp);
			}
			user_array = json_array();
			worker = NULL;

			/* Decay times per worker */
			while ((worker = next_worker(sdata, user, worker)) != NULL) {
				json_t *wval;

				per_tdiff = tvdiff(&now, &worker->last_share);
				if (per_tdiff > 60) {
					/* Drop storage of workers idle for 1 week */
					if (per_tdiff > 600000) {
						LOGDEBUG("Skipping worker %s", worker->workername);
						continue;
					}
					decay_worker(worker, 0, &now);
					worker->idle = true;
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

				JSON_CPACK(wval, "{ss,ss,ss,ss,ss,ss,si,sI,sf,sI}",
						"workername", worker->workername,
						"hashrate1m", suffix1,
						"hashrate5m", suffix5,
						"hashrate1hr", suffix60,
						"hashrate1d", suffix1440,
						"hashrate7d", suffix10080,
					        "lastshare", worker->last_share.tv_sec,
						"shares", worker->shares,
						"bestshare", worker->best_diff,
						"bestever", worker->best_ever);
				json_array_append_new(user_array, wval);
			}

			json_object_set_new_nocheck(val, "worker", user_array);
			ASPRINTF(&fname, "%s/users/%s", ckp->logdir, user->username);
			s = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER | JSON_EOL |
				JSON_REAL_PRECISION(16) | JSON_INDENT(1));
			add_log_entry(&log_entries, &fname, &s);
			json_decref(val);
			if (ckp->remote)
				upstream_workers(ckp, user);
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

		ASPRINTF(&fname, "%s/pool/pool.status", ckp->logdir);
		fp = fopen(fname, "we");
		if (unlikely(!fp))
			LOGERR("Failed to fopen %s", fname);
		dealloc(fname);

		JSON_CPACK(val, "{si,si,si,si,si,si}",
				"runtime", diff.tv_sec,
				"lastupdate", now.tv_sec,
				"Users", stats->users + stats->remote_users,
				"Workers", stats->workers + stats->remote_workers,
				"Idle", idle_workers,
				"Disconnected", stats->disconnected);
		s = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER);
		json_decref(val);
		LOGNOTICE("Pool:%s", s);
		fprintf(fp, "%s\n", s);
		dealloc(s);

		JSON_CPACK(val, "{ss,ss,ss,ss,ss,ss,ss}",
				"hashrate1m", suffix1,
				"hashrate5m", suffix5,
				"hashrate15m", suffix15,
				"hashrate1hr", suffix60,
				"hashrate6hr", suffix360,
				"hashrate1d", suffix1440,
				"hashrate7d", suffix10080);
		s = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER);
		json_decref(val);
		LOGNOTICE("Pool:%s", s);
		fprintf(fp, "%s\n", s);
		dealloc(s);

		/* Round to 4 significant digits */
		percent = round(stats->accounted_diff_shares * 10000 / stats->network_diff) / 100;
		JSON_CPACK(val, "{sf,sI,sI,sI,sf,sf,sf,sf}",
			        "diff", percent,
				"accepted", stats->accounted_diff_shares,
				"rejected", stats->accounted_rejects,
				"bestshare", stats->best_diff,
				"SPS1m", stats->sps1,
				"SPS5m", stats->sps5,
				"SPS15m", stats->sps15,
				"SPS1h", stats->sps60);
		s = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER | JSON_REAL_PRECISION(3));
		json_decref(val);
		LOGNOTICE("Pool:%s", s);
		fprintf(fp, "%s\n", s);
		dealloc(s);
		fclose(fp);

		if (ckp->proxy && sdata->proxy) {
			proxy_t *proxy, *proxytmp, *subproxy, *subtmp;

			mutex_lock(&sdata->proxy_lock);
			JSON_CPACK(val, "{sI,si,si}",
				   "current", sdata->proxy->id,
				   "active", HASH_COUNT(sdata->proxies),
				   "total", sdata->proxy_count);
			mutex_unlock(&sdata->proxy_lock);

			s = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER);
			json_decref(val);
			LOGNOTICE("Proxy:%s", s);
			dealloc(s);

			mutex_lock(&sdata->proxy_lock);
			HASH_ITER(hh, sdata->proxies, proxy, proxytmp) {
				JSON_CPACK(val, "{sI,si,sI,sb}",
					   "id", proxy->id,
					   "subproxies", proxy->subproxy_count,
					   "clients", proxy->combined_clients,
					   "alive", !proxy->dead);
				s = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER);
				json_decref(val);
				ASPRINTF(&sp, "Proxies:%s", s);
				dealloc(s);
				add_msg_entry(&char_list, &sp);
				HASH_ITER(sh, proxy->subproxies, subproxy, subtmp) {
					JSON_CPACK(val, "{sI,si,si,sI,sI,sf,sb}",
						   "id", subproxy->id,
						   "subid", subproxy->subid,
						   "nonce2len", subproxy->nonce2len,
						   "clients", subproxy->bound_clients,
						   "maxclients", subproxy->max_clients,
						   "diff", subproxy->diff,
						   "alive", !subproxy->dead);
					s = json_dumps(val, JSON_NO_UTF8 | JSON_PRESERVE_ORDER);
					json_decref(val);
					ASPRINTF(&sp, "Subproxies:%s", s);
					dealloc(s);
					add_msg_entry(&char_list, &sp);
				}
			}
			mutex_unlock(&sdata->proxy_lock);
			info_msg_entries(&char_list);
		}

		ts_realtime(&ts_now);
		sprintf(cdfield, "%lu,%lu", ts_now.tv_sec, ts_now.tv_nsec);
		JSON_CPACK(val, "{ss,si,si,si,sf,sf,sf,sf,ss,ss,ss,ss}",
				"poolinstance", ckp->name,
				"elapsed", diff.tv_sec,
				"users", stats->users + stats->remote_users,
				"workers", stats->workers + stats->remote_workers,
				"hashrate", ghs1,
				"hashrate5m", ghs5,
				"hashrate1hr", ghs60,
				"hashrate24hr", ghs1440,
				"createdate", cdfield,
				"createby", "code",
				"createcode", __func__,
				"createinet", ckp->serverurl[0]);
		json_decref(val);

		/* Update stats 32 times per minute to divide up userstats,
		 * displaying status every minute. */
		for (i = 0; i < 32; i++) {
			int64_t unaccounted_shares,
				unaccounted_diff_shares,
				unaccounted_rejects;

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
			stats->unaccounted_shares =
			stats->unaccounted_diff_shares =
			stats->unaccounted_rejects = 0;
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

static void read_poolstats(ckpool_t *ckp, int *tvsec_diff)
{
	char *s = alloca(4096), *pstats, *dsps, *sps;
	sdata_t *sdata = ckp->sdata;
	pool_stats_t *stats = &sdata->stats;
	tv_t now, last;
	json_t *val;
	FILE *fp;
	int ret;

	snprintf(s, 4095, "%s/pool/pool.status", ckp->logdir);
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
	val = json_loads(pstats, 0, NULL);
	if (!val) {
		LOGINFO("Failed to json decode pstats line from pool logfile: %s", pstats);
		return;
	}
	tv_time(&now);
	last.tv_sec = 0;
	json_get_int64(&last.tv_sec, val, "lastupdate");
	json_decref(val);
	LOGINFO("Successfully read pool pstats: %s", pstats);

	val = json_loads(dsps, 0, NULL);
	if (!val) {
		LOGINFO("Failed to json decode dsps line from pool logfile: %s", sps);
		return;
	}
	stats->dsps1 = dsps_from_key(val, "hashrate1m");
	stats->dsps5 = dsps_from_key(val, "hashrate5m");
	stats->dsps15 = dsps_from_key(val, "hashrate15m");
	stats->dsps60 = dsps_from_key(val, "hashrate1hr");
	stats->dsps360 = dsps_from_key(val, "hashrate6hr");
	stats->dsps1440 = dsps_from_key(val, "hashrate1d");
	stats->dsps10080 = dsps_from_key(val, "hashrate7d");
	json_decref(val);
	LOGINFO("Successfully read pool dsps: %s", dsps);

	val = json_loads(sps, 0, NULL);
	if (!val) {
		LOGINFO("Failed to json decode sps line from pool logfile: %s", dsps);
		return;
	}
	json_get_double(&stats->sps1, val, "SPS1m");
	json_get_double(&stats->sps5, val, "SPS5m");
	json_get_double(&stats->sps15, val, "SPS15m");
	json_get_double(&stats->sps60, val, "SPS1h");
	json_get_int64(&stats->accounted_diff_shares, val, "accepted");
	json_get_int64(&stats->accounted_rejects, val, "rejected");
	json_get_int64(&stats->best_diff, val, "bestshare");
	json_decref(val);

	LOGINFO("Successfully read pool sps: %s", sps);
	if (last.tv_sec)
		*tvsec_diff = now.tv_sec - last.tv_sec - 60;
	if (*tvsec_diff > 60) {
		LOGNOTICE("Old pool stats indicate pool down for %d seconds, decaying stats",
			  *tvsec_diff);
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

static char *status_chars = "|/-\\";

void *throbber(void *arg)
{
	ckpool_t *ckp = arg;
	sdata_t *sdata = ckp->sdata;
	int counter = 0;

	rename_proc("throbber");

	while (42) {
		double sdiff;
		pool_stats_t *stats;
		char stamp[128], hashrate[16], ch;

		sleep(1);
		if (ckp->quiet)
			continue;
		sdiff = sdata->stats.accounted_diff_shares;
		stats = &sdata->stats;
		suffix_string(stats->dsps1 * nonces, hashrate, 16, 3);
		ch = status_chars[(counter++) & 0x3];
		get_timestamp(stamp);
		if (likely(sdata->current_workbase)) {
			double bdiff = sdiff / sdata->current_workbase->network_diff * 100;

			fprintf(stdout, "\33[2K\r%s %c %sH/s  %.1f SPS  %d users  %d workers  %.0f shares  %.1f%% diff",
				stamp, ch, hashrate, stats->sps1, stats->users + stats->remote_users,
				stats->workers + stats->remote_workers, sdiff, bdiff);
		} else {
			fprintf(stdout, "\33[2K\r%s %c %sH/s  %.1f SPS  %d users  %d workers  %.0f shares",
				stamp, ch, hashrate, stats->sps1, stats->users + stats->remote_users,
				stats->workers + stats->remote_workers, sdiff);
		}
		fflush(stdout);
	}

	return NULL;
}

static void *zmqnotify(void *arg)
{
#ifdef HAVE_ZMQ_H
	ckpool_t *ckp = arg;
	sdata_t *sdata = ckp->sdata;
	void *context, *notify;
	int rc;

	rename_proc("zmqnotify");

	context = zmq_ctx_new();
	notify = zmq_socket(context, ZMQ_SUB);
	if (!notify)
		quit(1, "zmq_socket failed with errno %d", errno);
	rc = zmq_setsockopt(notify, ZMQ_SUBSCRIBE, "hashblock", 0);
	if (rc < 0)
		quit(1, "zmq_setsockopt failed with errno %d", errno);
	rc = zmq_connect(notify, ckp->zmqblock);
	if (rc < 0)
		quit(1, "zmq_connect failed with errno %d", errno);
	LOGNOTICE("ZMQ connected to %s", ckp->zmqblock);

	while (42) {
		zmq_msg_t message;

		do {
			char hexhash[68] = {};
			int size;

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
	zmq_ctx_destroy (context);
#endif
	pthread_detach(pthread_self());

	return NULL;
}

void *stratifier(void *arg)
{
	pthread_t pth_blockupdate, pth_statsupdate, pth_throbber, pth_zmqnotify;
	proc_instance_t *pi = (proc_instance_t *)arg;
	int threads, tvsec_diff = 0;
	ckpool_t *ckp = pi->ckp;
	int64_t randomiser;
	sdata_t *sdata;

	rename_proc(pi->processname);
	LOGWARNING("%s stratifier starting", ckp->name);
	sdata = ckzalloc(sizeof(sdata_t));
	ckp->sdata = sdata;
	sdata->ckp = ckp;
	sdata->verbose = true;

	/* Wait for the generator to have something for us */
	while (!ckp->proxy && !ckp->generator_ready)
		cksleep_ms(10);
	while (ckp->remote && !ckp->connector_ready)
		cksleep_ms(10);

	if (!ckp->proxy) {
		if (!generator_checkaddr(ckp, ckp->btcaddress, &ckp->script, &ckp->segwit)) {
			LOGEMERG("Fatal: btcaddress invalid according to bitcoind");
			goto out;
		}

		/* Store this for use elsewhere */
		hex2bin(scriptsig_header_bin, scriptsig_header, 41);
		sdata->txnlen = address_to_txn(sdata->txnbin, ckp->btcaddress, ckp->script, ckp->segwit);

		/* Find a valid donation address if possible */
		if (generator_checkaddr(ckp, ckp->donaddress, &ckp->donscript, &ckp->donsegwit)) {
			ckp->donvalid = true;
			sdata->dontxnlen = address_to_txn(sdata->dontxnbin, ckp->donaddress, ckp->donscript, ckp->donsegwit);
			LOGNOTICE("BTC donation address valid %s", ckp->donaddress);
		} else if (generator_checkaddr(ckp, ckp->tndonaddress, &ckp->donscript, &ckp->donsegwit)) {
			ckp->donaddress = ckp->tndonaddress;
			ckp->donvalid = true;
			sdata->dontxnlen = address_to_txn(sdata->dontxnbin, ckp->donaddress, ckp->donscript, ckp->donsegwit);
			LOGNOTICE("BTC testnet donation address valid %s", ckp->donaddress);
		} else if (generator_checkaddr(ckp, ckp->rtdonaddress, &ckp->donscript, &ckp->donsegwit)) {
			ckp->donaddress = ckp->rtdonaddress;
			ckp->donvalid = true;
			sdata->dontxnlen = address_to_txn(sdata->dontxnbin, ckp->donaddress, ckp->donscript, ckp->donsegwit);
			LOGNOTICE("BTC regtest donation address valid %s", ckp->donaddress);
		} else
			LOGNOTICE("No valid donation address found");
	}

	randomiser = time(NULL);
	sdata->enonce1_64 = htole64(randomiser);
	sdata->session_id = randomiser;
	/* Set the initial id to time as high bits so as to not send the same
	 * id on restarts */
	randomiser <<= 32;
	if (!ckp->proxy)
		sdata->blockchange_id = sdata->workbase_id = randomiser;

	cklock_init(&sdata->instance_lock);
	cksem_init(&sdata->update_sem);
	cksem_post(&sdata->update_sem);

	/* Create half as many share processing and receiving threads as there
	 * are CPUs */
	threads = sysconf(_SC_NPROCESSORS_ONLN) / 2 ? : 1;
	sdata->updateq = create_ckmsgq(ckp, "updater", &block_update);
	sdata->sshareq = create_ckmsgqs(ckp, "sprocessor", &sshare_process, threads);
	sdata->ssends = create_ckmsgqs(ckp, "ssender", &ssend_process, threads);
	sdata->sauthq = create_ckmsgq(ckp, "authoriser", &sauth_process);
	sdata->stxnq = create_ckmsgq(ckp, "stxnq", &send_transactions);
	sdata->srecvs = create_ckmsgqs(ckp, "sreceiver", &srecv_process, threads);
	create_pthread(&pth_throbber, throbber, ckp);
	read_poolstats(ckp, &tvsec_diff);
	read_userstats(ckp, sdata, tvsec_diff);

	/* Set diff impossibly large until we know the network diff */
	sdata->stats.network_diff = ~0ULL;

	cklock_init(&sdata->txn_lock);
	cklock_init(&sdata->workbase_lock);
	if (!ckp->proxy)
		create_pthread(&pth_blockupdate, blockupdate, ckp);
	else {
		mutex_init(&sdata->proxy_lock);
	}

	mutex_init(&sdata->stats_lock);
	mutex_init(&sdata->uastats_lock);
	if (!ckp->passthrough || ckp->node)
		create_pthread(&pth_statsupdate, statsupdate, ckp);

	mutex_init(&sdata->share_lock);
	if (!ckp->proxy)
		create_pthread(&pth_zmqnotify, zmqnotify, ckp);

	ckp->stratifier_ready = true;
	LOGWARNING("%s stratifier ready", ckp->name);

	stratum_loop(ckp, pi);
out:
	/* We should never get here unless there's a fatal error */
	LOGEMERG("Stratifier failure, shutting down");
	exit(1);
	return NULL;
}
