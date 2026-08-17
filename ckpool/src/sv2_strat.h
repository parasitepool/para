/*
 * Copyright 2026 Con Kolivas
 *
 * Stratifier-side SV2 mining protocol handling (Phase 1: pool work).
 */

#ifndef SV2_STRAT_H
#define SV2_STRAT_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

/* Process one plaintext SV2 frame for client_id. Returns a heap-allocated
 * plaintext reply frame (header+payload) or NULL. Caller frees with dealloc.
 * Additional frames may be queued via connector_sv2_send_plain. */
uint8_t* sv2_strat_handle_frame(int64_t client_id, const uint8_t* frame, size_t framelen, size_t* replylen);

/* Drop all SV2 channel state for a connector client id. */
void sv2_strat_drop_client(int64_t client_id);

/* Drop every SV2 mining client/channel and close virtual sessions
 * (stratifier dropall / bulk teardown). */
void sv2_strat_drop_all(void);

/* Re-push pool jobs to all SV2 channels after workbase update. */
void sv2_strat_on_work_update(bool clean);

/* Record connector address/server for a new SV2 TCP client. */
void sv2_strat_note_client(int64_t client_id, const char* address, int server);

/* Pool auto-vardiff / UpdateChannel: update channel target and send SetTarget
 * for every SV2 channel bound to this stratum instance_id. */
void sv2_strat_set_instance_diff(int64_t instance_id, double diff);

/* Clear instance binding when stratifier drops a virtual SV2 session so we
 * stop pushing jobs / validating shares for a dead instance. */
void sv2_strat_clear_instance(int64_t instance_id);

/* Connected SV2 mining clients / channels / JD clients as a JSON object
 * line for pool.status. NULL when SV2 is not configured. Caller frees. */
char* sv2_strat_stats_json(void);

/* sshareq worker: hash/account a queued SV2 share and send Success/Error.
 * Takes ownership of job (void * is struct sv2_share_job *). */
void sv2_strat_process_share_job(void* job);

#endif /* SV2_STRAT_H */
