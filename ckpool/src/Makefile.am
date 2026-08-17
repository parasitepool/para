
ACLOCAL_AMFLAGS = -I m4
AM_CPPFLAGS =

if HAVE_CAPNP
AM_CPPFLAGS += @CAPNP_CFLAGS@ -I$(top_srcdir)/ipc/mining
endif

if HAVE_SV2
AM_CPPFLAGS += @SV2_CFLAGS@
endif

if HAVE_LIBPQ
AM_CPPFLAGS += @LIBPQ_CFLAGS@
endif

native_objs :=

if HAVE_X86_SHANI
native_objs += sha256_x86_shani.o
endif
if HAVE_AVX2
native_objs += sha256_code_release/sha256_avx2_rorx2.A
endif
if HAVE_AVX1
native_objs += sha256_code_release/sha256_avx1.A
endif
if HAVE_SSE4
native_objs += sha256_code_release/sha256_sse4.A
endif
if HAVE_ARM_SHA2
native_objs += sha256_arm_shani.o
endif

%.A: %.asm
	yasm -f x64 -f elf64 -X gnu -g dwarf2 -D LINUX -o $@ $<

noinst_LIBRARIES = libckpool.a
libckpool_a_SOURCES = libckpool.c sha2.c sha256_arm_shani.c sha256_code_release
libckpool_a_LIBADD = $(native_objs)

bin_PROGRAMS = ckpool ckpmsg notifier
ckpool_SOURCES = ckpool.c generator.c bitcoin.c stratifier.c connector.c \
		 yyjson.c yyjson_util.c
ckpool_LDADD = libckpool.a @LIBS@ @CAPNP_LIBS@ @SV2_LIBS@ @LIBPQ_LIBS@

if HAVE_CAPNP
ckpool_SOURCES += $(top_srcdir)/ipc/mining/mining_ipc.cpp \
	$(top_srcdir)/ipc/mining/init.capnp.c++ \
	$(top_srcdir)/ipc/mining/mining.capnp.c++ \
	$(top_srcdir)/ipc/mining/common.capnp.c++ \
	$(top_srcdir)/ipc/mining/echo.capnp.c++ \
	$(top_srcdir)/ipc/mining/rpc.capnp.c++ \
	$(top_srcdir)/ipc/mining/mp/proxy.capnp.c++
endif

if HAVE_SV2
# Codec in libckpool.a so tests link it without recompiling src/sv2_codec.c
# under a second "sv2_codec.o" target (test + src .Po collision forced rebuild).
libckpool_a_SOURCES += sv2_bridge.c sv2_cb.c sv2_codec.c sv2_tx.c
ckpool_SOURCES += sv2_noise.c sv2_conn.c sv2_strat.c sv2_jd.c sv2_jdc.c
# Bundled libsecp256k1 must be built before linking ckpool. Bootstrap the
# submodule on first build: fresh checkouts have no configure or Makefile.
SECP_CONFIG_FLAGS = --enable-module-ellswift --enable-module-schnorrsig \
	--enable-module-extrakeys --disable-shared --enable-static --with-pic \
	--disable-benchmark --disable-tests --disable-exhaustive-tests \
	--disable-ctime-tests --disable-examples
$(top_builddir)/src/secp256k1/.libs/libsecp256k1.a:
	cd $(top_builddir)/src/secp256k1 && \
	if test ! -f Makefile; then \
		test -x ./configure || ./autogen.sh; \
		./configure $(SECP_CONFIG_FLAGS); \
	fi && \
	$(MAKE) $(AM_MAKEFLAGS)
# Ensure secp is built first without overriding automake's program deps
$(ckpool_OBJECTS) $(ckpool_LINK): $(top_builddir)/src/secp256k1/.libs/libsecp256k1.a
endif
ckpmsg_SOURCES = ckpmsg.c
ckpmsg_LDADD = libckpool.a

notifier_SOURCES = notifier.c
notifier_LDADD = libckpool.a

install-exec-hook:
	setcap CAP_NET_BIND_SERVICE=+eip $(bindir)/ckpool
	$(LN_S) -f ckpool $(DESTDIR)$(bindir)/ckproxy

uninstall-local:
	rm -f $(bindir)/ckproxy
