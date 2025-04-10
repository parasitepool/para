/*
 * Copyright 2014-2018,2023 Con Kolivas
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#ifndef BITCOIN_H
#define BITCOIN_H

typedef struct genwork gbtbase_t;

bool validate_address(connsock_t *cs, const char *address, bool *script, bool *segwit);
json_t *validate_txn(connsock_t *cs, const char *txn);
bool gen_gbtbase(connsock_t *cs, gbtbase_t *gbt);
void clear_gbtbase(gbtbase_t *gbt);
int get_blockcount(connsock_t *cs);
bool get_blockhash(connsock_t *cs, int height, char *hash);
bool get_bestblockhash(connsock_t *cs, char *hash);
bool submit_block(connsock_t *cs, const char *params);
void precious_block(connsock_t *cs, const char *params);
void submit_txn(connsock_t *cs, const char *params);
char *get_txn(connsock_t *cs, const char *hash);

#endif /* BITCOIN_H */
