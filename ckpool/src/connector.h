/*
 * Copyright 2014-2016,2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#ifndef CONNECTOR_H
#define CONNECTOR_H

#include "config.h"

int64_t connector_newclientid(void);
void    connector_upstream_msg(char* msg);
void    _connector_add_yymessage(yyjson_mut_doc* doc, const char* file, const char* func, const int line);
#define connector_add_yymessage(doc) _connector_add_yymessage(doc, __FILE__, __func__, __LINE__)
char* connector_stats(void* data, const int runtime);
void  connector_send_fd(const int fdno, const int sockd);
void* connector(void* arg);

#ifdef HAVE_SV2
/* Queue a plaintext SV2 frame (header+payload) to client_id. Noise encrypt
 * runs on the connector sender-shard thread only. Takes ownership of plain. */
void connector_sv2_send_plain(int64_t client_id, uint8_t* plain, size_t plainlen);
#endif

#endif /* CONNECTOR_H */
