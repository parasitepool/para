AC_INIT([ckpool],[0.9.9],[kernel@kolivas.org])

AC_CANONICAL_TARGET
AC_CONFIG_MACRO_DIR([m4])
AC_CONFIG_SRCDIR([src/ckpool.c])
AC_CONFIG_HEADERS([config.h])

AM_INIT_AUTOMAKE([foreign subdir-objects])
m4_ifdef([AM_SILENT_RULES], [AM_SILENT_RULES([yes])])
AC_USE_SYSTEM_EXTENSIONS

AC_CANONICAL_BUILD
AC_CANONICAL_HOST

dnl Make sure anyone changing configure.ac/Makefile.am has a clue
AM_MAINTAINER_MODE

dnl Checks for programs
AC_PROG_CC
# gl_EARLY - maybe later
AC_PROG_GCC_TRADITIONAL
AM_PROG_CC_C_O
LT_INIT([disable-shared])

# gl_INIT - maybe later

dnl Checks for header files.

AC_FUNC_ALLOCA

PKG_PROG_PKG_CONFIG()

AC_CHECK_HEADERS(stdio.h stdlib.h fcntl.h sys/time.h unistd.h dirent.h)
AC_CHECK_HEADERS(ctype.h errno.h byteswap.h string.h time.h fenv.h)
AC_CHECK_HEADERS(endian.h sys/endian.h arpa/inet.h sys/poll.h syslog.h)
AC_CHECK_HEADERS(alloca.h pthread.h stdio.h math.h signal.h sys/prctl.h)
AC_CHECK_HEADERS(sys/types.h sys/socket.h sys/stat.h linux/un.h netdb.h)
AC_CHECK_HEADERS(stdint.h netinet/in.h netinet/tcp.h sys/ioctl.h getopt.h)
AC_CHECK_HEADERS(sys/epoll.h libpq-fe.h postgresql/libpq-fe.h grp.h)
AC_CHECK_HEADERS(gsl/gsl_math.h gsl/gsl_cdf.h)
AC_CHECK_HEADERS(openssl/x509.h openssl/hmac.h)
AC_CHECK_HEADERS(zmq.h)

AC_CHECK_PROG(YASM, yasm, yes)
AM_CONDITIONAL([HAVE_YASM], [test x$YASM = xyes])

rorx=
avx1=
sse4=
if test x$YASM = xyes; then
	rorx=`cat /proc/cpuinfo | grep -o -m 1 avx2`
	if [test x$rorx != xavx2]; then
		avx1=`cat /proc/cpuinfo | grep -o -m 1 avx`
		if [test x$avx1 != xavx]; then
			sse4=`cat /proc/cpuinfo | grep -o -m 1 sse4_1`
		fi
	fi
fi
AM_CONDITIONAL([HAVE_AVX2], [test x$rorx = xavx2])
AM_CONDITIONAL([HAVE_AVX1], [test x$avx1 = xavx])
AM_CONDITIONAL([HAVE_SSE4], [test x$sse4 = xsse4_1])
if test x$rorx = xavx2; then
	AC_DEFINE([USE_AVX2], [1], [Use avx2 assembly instructions for sha256])
fi
if test x$avx1 = xavx; then
	AC_DEFINE([USE_AVX1], [1], [Use avx1 assembly instructions for sha256])
fi
if test x$sse4 = xsse4_1; then
	AC_DEFINE([USE_SSE4], [1], [Use sse4 assembly instructions for sha256])
fi

AC_CONFIG_SUBDIRS([src/jansson-2.14])
JANSSON_LIBS="jansson-2.14/src/.libs/libjansson.a"

AC_SUBST(JANSSON_LIBS)

AC_SEARCH_LIBS(clock_nanosleep, rt, , echo "Error: Required library rt not found." && exit 1)
AC_SEARCH_LIBS(exp, m, , echo "Error: Required library math not found." && exit 1)
AC_SEARCH_LIBS(pthread_mutex_trylock, pthread, , echo "Error: Required library pthreads not found." && exit 1)
AC_SEARCH_LIBS(zmq_socket, zmq, ZMQ=yes, ZMQ=no)

AC_CONFIG_FILES([Makefile src/Makefile])
AC_OUTPUT

LDFLAGS="${LDFLAGS} -Wl,--as-needed"

echo
echo "Compilation............: make (or gmake)"
echo "  YASM (Intel ASM).....: $YASM"
echo "  ZMQ..................: $ZMQ"
echo "  CPPFLAGS.............: $CPPFLAGS"
echo "  CFLAGS...............: $CFLAGS"
echo "  LDFLAGS..............: $LDFLAGS"
echo "  LDADD................: $LIBS $JANSSON_LIBS"
echo
echo "Installation...........: make install (as root if needed, with 'su' or 'sudo')"
echo "  prefix...............: $prefix"
echo
