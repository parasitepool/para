#!/bin/sh
# Fetch the bundled secp256k1 submodule on fresh clones
if test -e .git && test ! -f src/secp256k1/autogen.sh; then
	git submodule update --init
fi
autoreconf --force --install -I m4
