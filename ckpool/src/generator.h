/*
 * Copyright 2014-2018,2023,2026 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#ifndef GENERATOR_H
#define GENERATOR_H

#include "config.h"

#define GETBEST_FAILED -1
#define GETBEST_NOTIFY 0
#define GETBEST_SUCCESS 1

void            generator_add_send(yyjson_mut_doc* doc);
struct genwork* generator_getbase(void);
int             generator_getbest(char* hash);
bool            generator_alive(void);
bool            generator_checkaddr(const char* addr, bool* script, bool* segwit);
char*           generator_checktxn(const char* txn);
char*           generator_get_txn(const char* hash);
bool            generator_submitblock(const char* buf);
void            generator_preciousblock(const char* hash);
bool            generator_get_blockhash(int height, char* hash);
void*           generator(void* arg);

#endif /* GENERATOR_H */
