AC_INIT([ckpool],[1.2.0],[kernel@kolivas.org])

dnl Must come before anything that expands AC_PROG_CC/AC_PROG_CXX, which is as
dnl early as AC_USE_SYSTEM_EXTENSIONS below. Autoconf fills in a bare "-g -O2"
dnl the moment it detects a compiler, and a later "test -z" then sees a
dnl non-empty value and never applies these.
AC_MSG_CHECKING([whether to set default CFLAGS])
if test -z "$CFLAGS"; then
    CFLAGS="-g -O2 -Wall -march=native"
    AC_MSG_RESULT([yes, setting default CFLAGS to $CFLAGS])
else
    AC_MSG_RESULT([no, using user-provided CFLAGS])
fi

AC_MSG_CHECKING([whether to set default CXXFLAGS])
if test -z "$CXXFLAGS"; then
    CXXFLAGS="-g -O2 -Wall -march=native"
    AC_MSG_RESULT([yes, setting default CXXFLAGS to $CXXFLAGS])
else
    AC_MSG_RESULT([no, using user-provided CXXFLAGS])
fi

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
dnl C++ compiler required for the optional Cap'n Proto mining IPC shim
AC_PROG_CXX
# gl_EARLY - maybe later
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

x86_shani=
rorx=
avx1=
sse4=
sha2=
if test $host_cpu = 'x86_64' && test x$YASM = xyes; then
	cpuinfo=$(cat /proc/cpuinfo)

	if [[[ $cpuinfo =~ sha_ni ]]]; then
		 CFLAGS="$CFLAGS -msse4.1 -msha"
        	x86_shani="sha_ni"
	elif [[[ $cpuinfo =~ avx2 ]]]; then
		rorx="avx2"
	elif [[[ $cpuinfo =~ avx ]]]; then
		avx1="avx"
	elif [[[ $cpuinfo =~ sse4_1 ]]]; then
		sse4="sse4_1"
	fi
fi
if test $host_cpu = 'aarch64'; then
	CFLAGS="$CFLAGS -march=armv8-a+crypto"
	sha2=`cat /proc/cpuinfo | grep -o -m 1 sha2`
fi
AM_CONDITIONAL([HAVE_X86_SHANI], [test x$x86_shani = xsha_ni])
AM_CONDITIONAL([HAVE_AVX2], [test x$rorx = xavx2])
AM_CONDITIONAL([HAVE_AVX1], [test x$avx1 = xavx])
AM_CONDITIONAL([HAVE_SSE4], [test x$sse4 = xsse4_1])
AM_CONDITIONAL([HAVE_ARM_SHA2], [test x$sha2 = xsha2])
if test x$x86_shani = xsha_ni; then
        AC_DEFINE([USE_X86_SHANI], [1], [Use x86 SHA-NI assembly instructions for sha256])
fi
if test x$rorx = xavx2; then
	AC_DEFINE([USE_AVX2], [1], [Use avx2 assembly instructions for sha256])
fi
if test x$avx1 = xavx; then
	AC_DEFINE([USE_AVX1], [1], [Use avx1 assembly instructions for sha256])
fi
if test x$sse4 = xsse4_1; then
	AC_DEFINE([USE_SSE4], [1], [Use sse4 assembly instructions for sha256])
fi
if test x$sha2 = xsha2; then
	AC_DEFINE([USE_ARM_SHA2], [1], [Use ARMv8 instructions for sha256])
fi



AC_SEARCH_LIBS(clock_nanosleep, rt, , echo "Error: Required library rt not found." && exit 1)
AC_SEARCH_LIBS(exp, m, , echo "Error: Required library math not found." && exit 1)
AC_SEARCH_LIBS(pthread_mutex_trylock, pthread, , echo "Error: Required library pthreads not found." && exit 1)
AC_SEARCH_LIBS(zmq_socket, zmq, ZMQ=yes, ZMQ=no)

dnl Optional PostgreSQL (libpq) for share/block/user logging to a database.
dnl Only the ckpool binary links it; see src/Makefile.am.
AC_ARG_WITH([libpq],
  [AS_HELP_STRING([--without-libpq], [build without PostgreSQL logging support])],
  [with_libpq=$withval], [with_libpq=check])

LIBPQ_CFLAGS=""
LIBPQ_LIBS=""
if test "x$with_libpq" = "xno"; then
  LIBPQ=no
else
  PKG_CHECK_MODULES([LIBPQ], [libpq],
    [LIBPQ=yes],
    [LIBPQ=no
     AC_CHECK_HEADER([libpq-fe.h],
       [LIBPQ=yes
        LIBPQ_LIBS="-lpq"],
       [dnl Debian/Ubuntu put the headers in a subdirectory without pkg-config.
        AC_CHECK_HEADER([postgresql/libpq-fe.h],
          [LIBPQ=yes
           LIBPQ_CFLAGS="-I/usr/include/postgresql"
           LIBPQ_LIBS="-lpq"],
          [LIBPQ=no])])])
  if test "x$with_libpq" = "xyes" && test "x$LIBPQ" = "xno"; then
    AC_MSG_ERROR([--with-libpq given but libpq was not found])
  fi
fi
if test "x$LIBPQ" = "xyes"; then
  AC_DEFINE([HAVE_LIBPQ], [1], [Have PostgreSQL libpq for database logging])
fi
AC_SUBST(LIBPQ_CFLAGS)
AC_SUBST(LIBPQ_LIBS)
AM_CONDITIONAL([HAVE_LIBPQ], [test x$LIBPQ = xyes])

dnl Optional Cap'n Proto (capnp-rpc) for the Bitcoin Core mining IPC interface.
dnl When present, ckpool builds the C++ IPC shim (ipc/mining) and prefers the
dnl IPC interface for block notifications, falling back to ZMQ otherwise.
CAPNP_CFLAGS=""
CAPNP_LIBS=""
CAPNP_REQ="pkg-config package capnp-rpc (e.g. libcapnp-dev), C++ compiler"
PKG_CHECK_MODULES([CAPNP], [capnp-rpc],
  [CAPNP=yes
   AC_DEFINE([HAVE_CAPNP], [1], [Have Cap'n Proto (capnp-rpc) for the mining IPC interface])],
  [CAPNP=no])
AC_SUBST(CAPNP_CFLAGS)
AC_SUBST(CAPNP_LIBS)
AM_CONDITIONAL([HAVE_CAPNP], [test x$CAPNP = xyes])

dnl Optional Stratum V2 (Mining Protocol) — requires libsodium and bundled
dnl libsecp256k1 with ellswift + schnorrsig (see src/secp256k1).
dnl Default off in this fork. Re-enabling also needs the src/secp256k1
dnl submodule, which is not vendored here.
AC_ARG_ENABLE([sv2],
  [AS_HELP_STRING([--enable-sv2], [build Stratum V2 support (default: no)])],
  [enable_sv2=$enableval], [enable_sv2=no])

SV2=no
SV2_CFLAGS=""
SV2_LIBS=""
have_sodium=no
have_secp_src=no
SV2_MISSING=""
if test "x$enable_sv2" = "xno"; then
  SV2_MISSING="disabled (--disable-sv2)"
else
  PKG_CHECK_MODULES([SODIUM], [libsodium], [have_sodium=yes], [have_sodium=no])
  if test "x$have_sodium" = "xno"; then
    AC_CHECK_HEADER([sodium.h], [have_sodium=yes], [have_sodium=no])
    if test "x$have_sodium" = "xyes"; then
      SODIUM_LIBS="-lsodium"
    fi
  fi
  if test -f "$srcdir/src/secp256k1/include/secp256k1_ellswift.h" && \
     test -f "$srcdir/src/secp256k1/include/secp256k1_schnorrsig.h"; then
    have_secp_src=yes
  else
    have_secp_src=no
  fi
  if test "x$have_sodium" = "xyes" && test "x$have_secp_src" = "xyes"; then
    SV2=yes
    AC_DEFINE([HAVE_SV2], [1], [Build Stratum V2 support])
    SV2_CFLAGS="$SODIUM_CFLAGS -I\$(top_srcdir)/src/secp256k1/include"
    SV2_LIBS="$SODIUM_LIBS \$(top_builddir)/src/secp256k1/.libs/libsecp256k1.a"
  else
    missing=""
    if test "x$have_sodium" != "xyes"; then
      missing="libsodium (pkg-config libsodium or sodium.h + -lsodium)"
    fi
    if test "x$have_secp_src" != "xyes"; then
      if test -n "$missing"; then
        missing="$missing; "
      fi
      missing="${missing}src/secp256k1 with ellswift+schnorrsig (secp256k1_ellswift.h, secp256k1_schnorrsig.h)"
    fi
    SV2_MISSING="$missing"
    if test "x$enable_sv2" = "xyes"; then
      AC_MSG_ERROR([--enable-sv2 requires: $SV2_MISSING])
    fi
    SV2=no
  fi
fi
AC_SUBST(SV2_CFLAGS)
AC_SUBST(SV2_LIBS)
AM_CONDITIONAL([HAVE_SV2], [test x$SV2 = xyes])

AC_CONFIG_FILES([Makefile src/Makefile test/Makefile])

AC_OUTPUT

LDFLAGS="${LDFLAGS} -Wl,--as-needed"

echo
echo "Compilation............: make (or gmake)"
echo "  YASM (Intel ASM).....: $YASM"
echo "  ZMQ..................: $ZMQ"
echo "  PostgreSQL (libpq)...: $LIBPQ"
if test "x$CAPNP" = "xyes"; then
  echo "  IPC (mining Cap'n)...: yes"
else
  echo "  IPC (mining Cap'n)...: no"
  echo "    requires...........: $CAPNP_REQ"
fi
if test "x$SV2" = "xyes"; then
  echo "  SV2 (Stratum V2).....: yes"
else
  echo "  SV2 (Stratum V2).....: no"
  if test -n "$SV2_MISSING"; then
    echo "    requires...........: $SV2_MISSING"
  else
    echo "    requires...........: libsodium; src/secp256k1 with ellswift+schnorrsig"
  fi
fi
echo "  CPPFLAGS.............: $CPPFLAGS"
echo "  CFLAGS...............: $CFLAGS"
if test "x$CAPNP" = "xyes"; then
  echo "  CXXFLAGS.............: $CXXFLAGS"
fi
echo "  LDFLAGS..............: $LDFLAGS"
echo "  LDADD................: $LIBS"
echo "  ARCHITECTURE.........: $host_cpu $x86_shani$rorx$avx1$sse4$sha2"
echo
echo "Installation...........: make install (as root if needed, with 'su' or 'sudo')"
echo "  prefix...............: $prefix"
echo
