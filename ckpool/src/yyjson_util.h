/*
 * Copyright Con Kolivas 2026
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

yyjson_mut_doc*
_yyjson_mut_pack(const char* file, const char* func, const int line, const char* types, const char* fmt, ...);
yyjson_mut_val* _yyjson_mut_pack_val(
    const char*     file,
    const char*     func,
    const int       line,
    const char*     types,
    yyjson_mut_doc* doc,
    const char*     fmt,
    ...);
extern const yyjson_alc ckyyalc;

/* Map a pack argument to the format specifier class it satisfies: 's' for
 * strings, 'i' for int sized integers (including bool for 'b'), 'I' for 64
 * bit integers, 'f' for floating point and 'o' for yyjson values.
 * Deliberately no default association so passing an argument of any other
 * type is a compile time error. */
/* Left unformatted: clang-format before 20 aligns these continuations to a
 * different column. */
/* clang-format off */
#define YYPACK_TYPE(x)                                \
    _Generic(                                         \
        (x),                                          \
        char*: 's',                                   \
        const char*: 's',                             \
        _Bool: 'i',                                   \
        char: 'i',                                    \
        signed char: 'i',                             \
        unsigned char: 'i',                           \
        short: 'i',                                   \
        unsigned short: 'i',                          \
        int: 'i',                                     \
        unsigned int: 'i',                            \
        long: sizeof(long) == 8 ? 'I' : 'i',          \
        unsigned long: sizeof(long) == 8 ? 'I' : 'i', \
        long long: 'I',                               \
        unsigned long long: 'I',                      \
        float: 'f',                                   \
        double: 'f',                                  \
        yyjson_mut_val*: 'o')
/* clang-format on */

/* Count the number of arguments passed, up to 64 */
#define YYPACK_NARG(...)                                                                                               \
    YYPACK_ARG_N(                                                                                                      \
        _0, ##__VA_ARGS__, 64, 63, 62, 61, 60, 59, 58, 57, 56, 55, 54, 53, 52, 51, 50, 49, 48, 47, 46, 45, 44, 43, 42, \
        41, 40, 39, 38, 37, 36, 35, 34, 33, 32, 31, 30, 29, 28, 27, 26, 25, 24, 23, 22, 21, 20, 19, 18, 17, 16, 15,    \
        14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0)
#define YYPACK_ARG_N(                                                                                                  \
    _0, _1, _2, _3, _4, _5, _6, _7, _8, _9, _10, _11, _12, _13, _14, _15, _16, _17, _18, _19, _20, _21, _22, _23, _24, \
    _25, _26, _27, _28, _29, _30, _31, _32, _33, _34, _35, _36, _37, _38, _39, _40, _41, _42, _43, _44, _45, _46, _47, \
    _48, _49, _50, _51, _52, _53, _54, _55, _56, _57, _58, _59, _60, _61, _62, _63, _64, N, ...)                       \
    N

/* Apply YYPACK_TYPE to every argument passed */
#define YYPACK_T_0()
#define YYPACK_T_1(x) YYPACK_TYPE(x),
#define YYPACK_T_2(x, ...) YYPACK_TYPE(x), YYPACK_T_1(__VA_ARGS__)
#define YYPACK_T_3(x, ...) YYPACK_TYPE(x), YYPACK_T_2(__VA_ARGS__)
#define YYPACK_T_4(x, ...) YYPACK_TYPE(x), YYPACK_T_3(__VA_ARGS__)
#define YYPACK_T_5(x, ...) YYPACK_TYPE(x), YYPACK_T_4(__VA_ARGS__)
#define YYPACK_T_6(x, ...) YYPACK_TYPE(x), YYPACK_T_5(__VA_ARGS__)
#define YYPACK_T_7(x, ...) YYPACK_TYPE(x), YYPACK_T_6(__VA_ARGS__)
#define YYPACK_T_8(x, ...) YYPACK_TYPE(x), YYPACK_T_7(__VA_ARGS__)
#define YYPACK_T_9(x, ...) YYPACK_TYPE(x), YYPACK_T_8(__VA_ARGS__)
#define YYPACK_T_10(x, ...) YYPACK_TYPE(x), YYPACK_T_9(__VA_ARGS__)
#define YYPACK_T_11(x, ...) YYPACK_TYPE(x), YYPACK_T_10(__VA_ARGS__)
#define YYPACK_T_12(x, ...) YYPACK_TYPE(x), YYPACK_T_11(__VA_ARGS__)
#define YYPACK_T_13(x, ...) YYPACK_TYPE(x), YYPACK_T_12(__VA_ARGS__)
#define YYPACK_T_14(x, ...) YYPACK_TYPE(x), YYPACK_T_13(__VA_ARGS__)
#define YYPACK_T_15(x, ...) YYPACK_TYPE(x), YYPACK_T_14(__VA_ARGS__)
#define YYPACK_T_16(x, ...) YYPACK_TYPE(x), YYPACK_T_15(__VA_ARGS__)
#define YYPACK_T_17(x, ...) YYPACK_TYPE(x), YYPACK_T_16(__VA_ARGS__)
#define YYPACK_T_18(x, ...) YYPACK_TYPE(x), YYPACK_T_17(__VA_ARGS__)
#define YYPACK_T_19(x, ...) YYPACK_TYPE(x), YYPACK_T_18(__VA_ARGS__)
#define YYPACK_T_20(x, ...) YYPACK_TYPE(x), YYPACK_T_19(__VA_ARGS__)
#define YYPACK_T_21(x, ...) YYPACK_TYPE(x), YYPACK_T_20(__VA_ARGS__)
#define YYPACK_T_22(x, ...) YYPACK_TYPE(x), YYPACK_T_21(__VA_ARGS__)
#define YYPACK_T_23(x, ...) YYPACK_TYPE(x), YYPACK_T_22(__VA_ARGS__)
#define YYPACK_T_24(x, ...) YYPACK_TYPE(x), YYPACK_T_23(__VA_ARGS__)
#define YYPACK_T_25(x, ...) YYPACK_TYPE(x), YYPACK_T_24(__VA_ARGS__)
#define YYPACK_T_26(x, ...) YYPACK_TYPE(x), YYPACK_T_25(__VA_ARGS__)
#define YYPACK_T_27(x, ...) YYPACK_TYPE(x), YYPACK_T_26(__VA_ARGS__)
#define YYPACK_T_28(x, ...) YYPACK_TYPE(x), YYPACK_T_27(__VA_ARGS__)
#define YYPACK_T_29(x, ...) YYPACK_TYPE(x), YYPACK_T_28(__VA_ARGS__)
#define YYPACK_T_30(x, ...) YYPACK_TYPE(x), YYPACK_T_29(__VA_ARGS__)
#define YYPACK_T_31(x, ...) YYPACK_TYPE(x), YYPACK_T_30(__VA_ARGS__)
#define YYPACK_T_32(x, ...) YYPACK_TYPE(x), YYPACK_T_31(__VA_ARGS__)
#define YYPACK_T_33(x, ...) YYPACK_TYPE(x), YYPACK_T_32(__VA_ARGS__)
#define YYPACK_T_34(x, ...) YYPACK_TYPE(x), YYPACK_T_33(__VA_ARGS__)
#define YYPACK_T_35(x, ...) YYPACK_TYPE(x), YYPACK_T_34(__VA_ARGS__)
#define YYPACK_T_36(x, ...) YYPACK_TYPE(x), YYPACK_T_35(__VA_ARGS__)
#define YYPACK_T_37(x, ...) YYPACK_TYPE(x), YYPACK_T_36(__VA_ARGS__)
#define YYPACK_T_38(x, ...) YYPACK_TYPE(x), YYPACK_T_37(__VA_ARGS__)
#define YYPACK_T_39(x, ...) YYPACK_TYPE(x), YYPACK_T_38(__VA_ARGS__)
#define YYPACK_T_40(x, ...) YYPACK_TYPE(x), YYPACK_T_39(__VA_ARGS__)
#define YYPACK_T_41(x, ...) YYPACK_TYPE(x), YYPACK_T_40(__VA_ARGS__)
#define YYPACK_T_42(x, ...) YYPACK_TYPE(x), YYPACK_T_41(__VA_ARGS__)
#define YYPACK_T_43(x, ...) YYPACK_TYPE(x), YYPACK_T_42(__VA_ARGS__)
#define YYPACK_T_44(x, ...) YYPACK_TYPE(x), YYPACK_T_43(__VA_ARGS__)
#define YYPACK_T_45(x, ...) YYPACK_TYPE(x), YYPACK_T_44(__VA_ARGS__)
#define YYPACK_T_46(x, ...) YYPACK_TYPE(x), YYPACK_T_45(__VA_ARGS__)
#define YYPACK_T_47(x, ...) YYPACK_TYPE(x), YYPACK_T_46(__VA_ARGS__)
#define YYPACK_T_48(x, ...) YYPACK_TYPE(x), YYPACK_T_47(__VA_ARGS__)
#define YYPACK_T_49(x, ...) YYPACK_TYPE(x), YYPACK_T_48(__VA_ARGS__)
#define YYPACK_T_50(x, ...) YYPACK_TYPE(x), YYPACK_T_49(__VA_ARGS__)
#define YYPACK_T_51(x, ...) YYPACK_TYPE(x), YYPACK_T_50(__VA_ARGS__)
#define YYPACK_T_52(x, ...) YYPACK_TYPE(x), YYPACK_T_51(__VA_ARGS__)
#define YYPACK_T_53(x, ...) YYPACK_TYPE(x), YYPACK_T_52(__VA_ARGS__)
#define YYPACK_T_54(x, ...) YYPACK_TYPE(x), YYPACK_T_53(__VA_ARGS__)
#define YYPACK_T_55(x, ...) YYPACK_TYPE(x), YYPACK_T_54(__VA_ARGS__)
#define YYPACK_T_56(x, ...) YYPACK_TYPE(x), YYPACK_T_55(__VA_ARGS__)
#define YYPACK_T_57(x, ...) YYPACK_TYPE(x), YYPACK_T_56(__VA_ARGS__)
#define YYPACK_T_58(x, ...) YYPACK_TYPE(x), YYPACK_T_57(__VA_ARGS__)
#define YYPACK_T_59(x, ...) YYPACK_TYPE(x), YYPACK_T_58(__VA_ARGS__)
#define YYPACK_T_60(x, ...) YYPACK_TYPE(x), YYPACK_T_59(__VA_ARGS__)
#define YYPACK_T_61(x, ...) YYPACK_TYPE(x), YYPACK_T_60(__VA_ARGS__)
#define YYPACK_T_62(x, ...) YYPACK_TYPE(x), YYPACK_T_61(__VA_ARGS__)
#define YYPACK_T_63(x, ...) YYPACK_TYPE(x), YYPACK_T_62(__VA_ARGS__)
#define YYPACK_T_64(x, ...) YYPACK_TYPE(x), YYPACK_T_63(__VA_ARGS__)

#define YYPACK_CAT_(a, b) a##b
#define YYPACK_CAT(a, b) YYPACK_CAT_(a, b)

/* Build a compile time string of the type classes of all arguments passed */
#define YYPACK_TYPES(...) ((const char[]){YYPACK_CAT(YYPACK_T_, YYPACK_NARG(__VA_ARGS__))(__VA_ARGS__) '\0'})

/* Wrappers for the yyjson pack functions that check call sites for
 * correctness in the number and type of parameters passed, mismatched types
 * being a compile time error and mismatched format strings being reported
 * with LOGEMERG at runtime, returning NULL without reading any arguments. */
#define yyjson_mut_pack(fmt, ...) \
    _yyjson_mut_pack(__FILE__, __func__, __LINE__, YYPACK_TYPES(__VA_ARGS__), fmt, ##__VA_ARGS__)

#define yyjson_mut_pack_val(doc, fmt, ...) \
    _yyjson_mut_pack_val(__FILE__, __func__, __LINE__, YYPACK_TYPES(__VA_ARGS__), doc, fmt, ##__VA_ARGS__)
