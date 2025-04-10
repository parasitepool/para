SUBDIRS	= jansson-2.14

ACLOCAL_AMFLAGS = -I m4
AM_CPPFLAGS =  -I$(top_srcdir)/src/jansson-2.14/src

native_objs :=

if HAVE_AVX2
native_objs += sha256_code_release/sha256_avx2_rorx2.A
endif
if HAVE_AVX1
native_objs += sha256_code_release/sha256_avx1.A
endif
if HAVE_SSE4
native_objs += sha256_code_release/sha256_sse4.A
endif

%.A: %.asm
	yasm -f x64 -f elf64 -X gnu -g dwarf2 -D LINUX -o $@ $<

noinst_LIBRARIES = libckpool.a
libckpool_a_SOURCES = libckpool.c libckpool.h sha2.c sha2.h sha256_code_release
libckpool_a_LIBADD = $(native_objs)

bin_PROGRAMS = ckpool ckpmsg notifier
ckpool_SOURCES = ckpool.c ckpool.h generator.c generator.h bitcoin.c bitcoin.h \
		 stratifier.c stratifier.h connector.c connector.h uthash.h \
		 utlist.h
ckpool_LDADD = libckpool.a @JANSSON_LIBS@ @LIBS@

ckpmsg_SOURCES = ckpmsg.c
ckpmsg_LDADD = libckpool.a @JANSSON_LIBS@

notifier_SOURCES = notifier.c
notifier_LDADD = libckpool.a @JANSSON_LIBS@

install-exec-hook:
	setcap CAP_NET_BIND_SERVICE=+eip $(bindir)/ckpool
	$(LN_S) -f ckpool $(DESTDIR)$(bindir)/ckproxy

uninstall-local:
	rm -f $(bindir)/ckproxy
