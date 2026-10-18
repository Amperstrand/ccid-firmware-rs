# ifd-nucula — pcscd IFD driver for the nucula wifi CCID transport (#157)
#
#   make            build libifd-nucula.so + selftest
#   make selftest   standalone wire-level test (needs mock_board.py)
#   make install    copy the .so to /usr/lib/pcsc/drivers/ (root)

PCSC_CFLAGS := $(shell pkg-config --cflags libpcsclite 2>/dev/null || echo -I/usr/include/PCSC)
CC      ?= cc
CFLAGS  ?= -O2 -g -Wall -Wextra -Werror -fPIC -std=gnu11
ALL_CFLAGS = $(CFLAGS) $(PCSC_CFLAGS)
LDLIBS  ?= -pthread

DRIVER   = libifd-nucula.so
SELFTEST = nucula-selftest
DESTDIR ?=
PREFIX  ?= /usr

.PHONY: all clean install selftest

all: $(DRIVER) $(SELFTEST)

$(DRIVER): ifd-nucula.c
	$(CC) $(ALL_CFLAGS) -shared -o $@ $< $(LDLIBS)

$(SELFTEST): ifd-nucula.c
	$(CC) $(ALL_CFLAGS) -DNUCULA_STANDALONE -o $@ $< $(LDLIBS)

selftest: $(SELFTEST) mock_board.py
	@python3 mock_board.py 29444 & MOCK=$$!; sleep 0.4; \
	./$(SELFTEST) 127.0.0.1 29444; RC=$$?; \
	kill $$MOCK 2>/dev/null; exit $$RC

# Full pcscd-level smoke: reader enumeration, ATR, transmit, escape
# passthrough, board-death (reader stays listed, card absent) and
# recovery. Isolated in a private mount namespace — the system pcscd
# is never touched. Requires root (unshare -m + tmpfs over /run/pcscd).
#
# Card emulation: NUCULA_MOCK_CARD=<profile> puts a scripted picc
# behind the mock board (card_profiles.py; legacy|satocash-g05|iso-case2).
# satocash-g05 keeps this smoke's G-05 ATR + SELECT 9000 expectations.
# Bench host only (root + unshare -m). On namespace-blocked hosts the
# wire-level twin runs without pcscd:
#   python3 -m pytest tests/hardware/test_emulated_card_harness.py -v
smoke: $(DRIVER) smoke_test.py run-smoke.sh mock_board.py card_profiles.py
	@unshare -m bash ./run-smoke.sh

install: $(DRIVER)
	install -D -m 0755 $(DRIVER) $(DESTDIR)$(PREFIX)/lib/pcsc/drivers/$(DRIVER)
	@echo "installed; reader.conf entry (see reader.conf.nucula):"
	@echo "  DEVICENAME <board-ip>:29443  LIBPATH $(PREFIX)/lib/pcsc/drivers/$(DRIVER)"

clean:
	rm -f $(DRIVER) $(SELFTEST)
