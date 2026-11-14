# Builds the WhaleRein Mosquitto broker plugin.
#
#   make                      # builds whalerein.so against the local broker headers
#   make MOSQUITTO_INCLUDE=... # uses a pinned Mosquitto source/include tree
#
# The deployment image builds this file inside a stage with the pinned
# Mosquitto 2.1.2 headers and copies only the shared object into the runtime.

CC ?= cc
MOSQUITTO_INCLUDE ?= /usr/include
CFLAGS ?= -O2 -Wall -Wextra -Werror -fPIC -std=c11 -D_POSIX_C_SOURCE=200809L -D_DEFAULT_SOURCE
UNAME_S := $(shell uname -s)

ifeq ($(UNAME_S),Darwin)
LDFLAGS ?= -shared -pthread -undefined dynamic_lookup
else
LDFLAGS ?= -shared -pthread
endif

.PHONY: all clean

all: whalerein.so

whalerein.so: whalerein.c
	$(CC) $(CFLAGS) -I$(MOSQUITTO_INCLUDE) $(LDFLAGS) -o $@ $<

clean:
	rm -f whalerein.so
