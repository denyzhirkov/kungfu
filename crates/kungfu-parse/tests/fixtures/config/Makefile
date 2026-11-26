# Build
CC := gcc
CFLAGS ?= -O2 -Wall
VERSION = 1.2.3
OBJS += main.o util.o

.PHONY: all clean test

all: build test

build test: $(OBJS)
	$(CC) $(CFLAGS) -o app $(OBJS)

%.o: %.c
	$(CC) -c $< -o $@

clean:
	rm -f app *.o

$(OBJS): config.h

ifeq ($(OS),Windows_NT)
EXE := .exe
endif

define HELP
usage: make [target]
bad: line
endef

deploy: build
	@echo deploying
	scp app host:/srv/app
