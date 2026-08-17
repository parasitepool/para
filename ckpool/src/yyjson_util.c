/*
 * Copyright Con Kolivas 2026
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#include "config.h"

#include <stdarg.h>
#include <string.h>
#include "libckpool.h"
#include "yyjson.h"

/* yyjson custom allocator using ckalloc */
static void* ck_yyjson_malloc(void* ctx, size_t size) {
    (void)ctx;
    return ckalloc(size);
}

static void* ck_yyjson_realloc(void* ctx, void* ptr, size_t old_size, size_t size) {
    (void)ctx;
    (void)old_size; /* ckpool's ckrealloc doesn't need the old size */
    return ckrealloc(ptr, size);
}

static void ck_yyjson_free(void* ctx, void* ptr) {
    (void)ctx;
    free(ptr);
}

const yyjson_alc ckyyalc = {
    .malloc = ck_yyjson_malloc,
    .realloc = ck_yyjson_realloc,
    .free = ck_yyjson_free,
    .ctx = NULL};

static void yyjson_mut_pack_skip(const char** pp) {
    const char* p = *pp;

    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r' || *p == ',' || *p == ':')
        p++;

    *pp = p;
}

static yyjson_mut_val* yyjson_mut_pack_value(yyjson_mut_doc* doc, const char** pp, va_list* ap) {
    yyjson_mut_pack_skip(pp);
    const char* p = *pp;

    if (*p == '\0')
        return NULL;

    char            c = *p++;
    yyjson_mut_val* val = NULL;

    switch (c) {
        case 's': {
            const char* str = va_arg(*ap, const char*);
            if (!str)
                return NULL;
            val = yyjson_mut_strcpy(doc, str);
            break;
        }
        case 'n':
            val = yyjson_mut_null(doc);
            break;
        case 'b': {
            int b = va_arg(*ap, int);
            val = yyjson_mut_bool(doc, b != 0);
            break;
        }
        case 'i': {
            int i = va_arg(*ap, int);
            val = yyjson_mut_int(doc, i);
            break;
        }
        case 'I': {
            int64_t i = va_arg(*ap, int64_t);
            val = yyjson_mut_int(doc, i);
            break;
        }
        case 'f':
        case 'F': {
            double d = va_arg(*ap, double);
            val = yyjson_mut_real(doc, d);
            break;
        }
        case 'o': {
            /* Deep copy the value into doc so callers may pass
             * values belonging to any document */
            yyjson_mut_val* obj = va_arg(*ap, yyjson_mut_val*);
            if (!obj)
                return NULL;
            val = yyjson_mut_val_mut_copy(doc, obj);
            if (!val)
                return NULL;
            break;
        }
        case '[': {
            val = yyjson_mut_arr(doc);
            if (!val)
                return NULL;

            for (;;) {
                yyjson_mut_pack_skip(&p);
                if (*p == ']') {
                    p++;
                    break;
                }
                yyjson_mut_val* item = yyjson_mut_pack_value(doc, &p, ap);
                if (!item || !yyjson_mut_arr_append(val, item))
                    return NULL;
            }
            break;
        }
        case '{': {
            val = yyjson_mut_obj(doc);
            if (!val)
                return NULL;

            for (;;) {
                yyjson_mut_pack_skip(&p);
                if (*p == '}') {
                    p++;
                    break;
                }
                if (*p != 's')
                    return NULL;
                p++; /* consume 's' */

                const char* key = va_arg(*ap, const char*);
                if (!key)
                    return NULL;

                yyjson_mut_val* key_val = yyjson_mut_strcpy(doc, key);
                if (!key_val)
                    return NULL;

                yyjson_mut_val* item_val = yyjson_mut_pack_value(doc, &p, ap);
                if (!item_val || !yyjson_mut_obj_add(val, key_val, item_val))
                    return NULL;
            }
            break;
        }
        default:
            return NULL;
    }

    *pp = p;
    return val;
}

/* Verifies the number and type of arguments at a pack call site match the
 * format string, types being the class characters built up by the
 * YYPACK_TYPES macro. Returns NULL on success or an error description. */
static const char* yyjson_pack_typecheck(const char* fmt, const char* types) {
    int n = 0;

    while (*fmt) {
        char want;

        switch (*fmt++) {
            case 's':
                want = 's';
                break;
            case 'b':
            case 'i':
                want = 'i';
                break;
            case 'I':
                want = 'I';
                break;
            case 'f':
            case 'F':
                want = 'f';
                break;
            case 'o':
                want = 'o';
                break;
            case 'n':
            case '{':
            case '}':
            case '[':
            case ']':
            case ':':
            case ',':
            case ' ':
            case '\t':
            case '\n':
            case '\r':
                continue;
            default:
                return "invalid format specifier";
        }
        if (unlikely(!types[n]))
            return "too few arguments for format";
        if (unlikely(types[n] != want))
            return "argument type mismatch";
        n++;
    }
    if (unlikely(types[n]))
        return "too many arguments for format";
    return NULL;
}

/* Emulates the simpler functionality of libjansson's json_pack. Called via
 * the yyjson_mut_pack macro wrapper which checks call site correctness. */
yyjson_mut_doc*
_yyjson_mut_pack(const char* file, const char* func, const int line, const char* types, const char* fmt, ...) {
    const char* err;

    if (unlikely(!fmt || !*fmt))
        return NULL;

    err = yyjson_pack_typecheck(fmt, types);
    if (unlikely(err)) {
        LOGEMERG("%s in yyjson_mut_pack fmt \"%s\" from %s %s:%d", err, fmt, file, func, line);
        return NULL;
    }

    yyjson_mut_doc* doc = yyjson_mut_doc_new(&ckyyalc);
    if (!doc)
        return NULL;

    va_list ap;
    va_start(ap, fmt);

    const char*     p = fmt;
    yyjson_mut_val* root = yyjson_mut_pack_value(doc, &p, &ap);

    va_end(ap);

    if (!root) {
        yyjson_mut_doc_free(doc);
        return NULL;
    }

    yyjson_mut_pack_skip(&p);
    if (*p != '\0') {
        yyjson_mut_doc_free(doc);
        return NULL;
    }

    yyjson_mut_doc_set_root(doc, root);
    return doc;
}

yyjson_mut_val* _yyjson_mut_pack_val(
    const char*     file,
    const char*     func,
    const int       line,
    const char*     types,
    yyjson_mut_doc* doc,
    const char*     fmt,
    ...) {
    const char* err;

    if (unlikely(!doc || !fmt || !*fmt))
        return NULL;

    err = yyjson_pack_typecheck(fmt, types);
    if (unlikely(err)) {
        LOGEMERG("%s in yyjson_mut_pack_val fmt \"%s\" from %s %s:%d", err, fmt, file, func, line);
        return NULL;
    }

    va_list ap;
    va_start(ap, fmt);

    const char*     p = fmt;
    yyjson_mut_val* val = yyjson_mut_pack_value(doc, &p, &ap);

    va_end(ap);

    if (!val)
        return NULL;

    yyjson_mut_pack_skip(&p);
    if (*p != '\0')
        return NULL;

    return val;
}
