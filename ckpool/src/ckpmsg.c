/*
 * Copyright 2014-2018,2023 Con Kolivas
 * Copyright 2014-2016 Andrew Smith
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the Free
 * Software Foundation; either version 3 of the License, or (at your option)
 * any later version.  See COPYING for more details.
 */

#include "config.h"

#include <getopt.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>
#include <ctype.h>

#include "libckpool.h"
#include "utlist.h"

struct input_log {
	struct input_log *next;
	struct input_log *prev;
	char *buf;
};

struct input_log *input_log;

static int msg_loglevel = LOG_DEBUG;

void logmsg(int loglevel, const char *fmt, ...)
{
	va_list ap;
	char *buf;

	if (loglevel <= msg_loglevel) {
		va_start(ap, fmt);
		VASPRINTF(&buf, fmt, ap);
		va_end(ap);

		printf("%s\n", buf);
		free(buf);
	}
}

void mkstamp(char *stamp, size_t siz)
{
	long minoff, hroff;
	char tzinfo[24];
	time_t now_t;
	struct tm tm;
	char tzch;

	now_t = time(NULL);
	localtime_r(&now_t, &tm);
	minoff = tm.tm_gmtoff / 60;
	if (minoff < 0) {
		tzch = '-';
		minoff *= -1;
	} else
		tzch = '+';
	hroff = minoff / 60;
	if (minoff % 60) {
		snprintf(tzinfo, sizeof(tzinfo),
			 "%c%02ld:%02ld",
			 tzch, hroff, minoff % 60);
	} else {
		snprintf(tzinfo, sizeof(tzinfo),
			 "%c%02ld",
			 tzch, hroff);
	}
	snprintf(stamp, siz,
			"[%d-%02d-%02d %02d:%02d:%02d%s]",
			tm.tm_year + 1900,
			tm.tm_mon + 1,
			tm.tm_mday,
			tm.tm_hour,
			tm.tm_min,
			tm.tm_sec,
			tzinfo);
}

static struct option long_options[] = {
	{"counter",	no_argument,		0,	'c'},
	{"help",	no_argument,		0,	'h'},
	{"loglevel",	required_argument,	0,	'l'},
	{"name",	required_argument,	0,	'n'},
	{"sockname",	required_argument,	0,	'N'},
	{"proxy",	no_argument,		0,	'p'},
	{"sockdir",	required_argument,	0,	's'},
	{"timeout1",	required_argument,	0,	't'},
	{"timeout2",	required_argument,	0,	'T'},
	{0, 0, 0, 0}
};

struct termios oldctrl;

static void sighandler(const int sig)
{
	/* Return console to its previous state */
	tcsetattr(STDIN_FILENO, TCSANOW, &oldctrl);

	if (sig) {
		signal (sig, SIG_DFL);
		raise (sig);
	}
}

int get_line(char **buf)
{
	struct input_log *entry = NULL;
	int c, len = 0, ctl1, ctl2;
	struct termios ctrl;
	*buf = NULL;

	/* If we're not reading from a terminal, parse lines at a time allowing
	 * us to script usage of ckpmsg */
	if (!isatty(fileno((FILE *)stdin))) do {
		size_t n;

		dealloc(*buf);
		len = getline(buf, &n, stdin);
		if (len == -1) {
			dealloc(*buf);
			goto out;
		}
		len = strlen(*buf);
		(*buf)[--len] = '\0'; // Strip \n
		goto out;
	} while (42);

	tcgetattr(STDIN_FILENO, &ctrl);
	ctrl.c_lflag &= ~(ICANON | ECHO); // turn off canonical mode and echo
	tcsetattr(STDIN_FILENO, TCSANOW, &ctrl);

	do {
		c = getchar();
		if (c == EOF || c == '\n')
			break;
		if (c == 27) {
			ctl1 = getchar();
			ctl2 = getchar();
			if (ctl1 != '[')
				continue;
			if (ctl2 < 'A' || ctl2 > 'B')
				continue;
			if (!input_log)
				continue;
			printf("\33[2K\r");
			free(*buf);
			if (ctl2 == 'B')
				entry = entry ? entry->prev : input_log->prev;
			else
				entry = entry ? entry->next : input_log;
			*buf = strdup(entry->buf);
			len = strlen(*buf);
			printf("%s", *buf);
		}
		if (c == 127) {
			if (!len)
				continue;
			printf("\b \b");
			(*buf)[--len] = '\0';
			continue;
		}
		if (c < 32 || c > 126)
			continue;
		len++;
		realloc_strcat(buf, (char *)&c);
		putchar(c);
	} while (42);

	if (*buf)
		len = strlen(*buf);
	printf("\n");
out:
	return len;
}

int main(int argc, char **argv)
{
	char *name = NULL, *socket_dir = NULL, *buf = NULL, *sockname = "listener";
	bool proxy = false, counter = false;
	int tmo1 = RECV_UNIX_TIMEOUT1;
	int tmo2 = RECV_UNIX_TIMEOUT2;
	struct sigaction handler;
	int c, count, i = 0, j;
	char stamp[128];

	tcgetattr(STDIN_FILENO, &oldctrl);

	while ((c = getopt_long(argc, argv, "chl:N:n:ps:t:T:", long_options, &i)) != -1) {
		switch(c) {
			/* You'd normally disable most logmsg with -l 3 to
			 * only see the counter */
			case 'c':
				counter = true;
				break;
			case 'h':
				for (j = 0; long_options[j].val; j++) {
					struct option *jopt = &long_options[j];

					if (jopt->has_arg) {
						char *upper = alloca(strlen(jopt->name) + 1);
						int offset = 0;

						do {
							upper[offset] = toupper(jopt->name[offset]);
						} while (upper[offset++] != '\0');
						printf("-%c %s | --%s %s\n", jopt->val,
						       upper, jopt->name, upper);
					} else
						printf("-%c | --%s\n", jopt->val, jopt->name);
				}
				exit(0);
			case 'l':
				msg_loglevel = atoi(optarg);
				if (msg_loglevel < LOG_EMERG ||
				    msg_loglevel > LOG_DEBUG) {
					quit(1, "Invalid loglevel: %d (range %d"
						" - %d)",
						msg_loglevel,
						LOG_EMERG,
						LOG_DEBUG);
				}
				break;
			/* Allows us to specify which process or socket to
			 * talk to. */
			case 'N':
				sockname = strdup(optarg);
				break;
			case 'n':
				name = strdup(optarg);
				break;
			case 'p':
				proxy = true;
				break;
			case 's':
				socket_dir = strdup(optarg);
				break;
			case 't':
				tmo1 = atoi(optarg);
				break;
			case 'T':
				tmo2 = atoi(optarg);
				break;
		}
	}
	if (!socket_dir)
		socket_dir = strdup("/tmp");
	trail_slash(&socket_dir);
	if (!name) {
		if (proxy)
			name = strdup("ckproxy");
		else
			name = strdup("ckpool");
	}
	realloc_strcat(&socket_dir, name);
	dealloc(name);
	trail_slash(&socket_dir);
	realloc_strcat(&socket_dir, sockname);

	signal(SIGPIPE, SIG_IGN);
	handler.sa_handler = &sighandler;
	handler.sa_flags = 0;
	sigemptyset(&handler.sa_mask);
	sigaction(SIGTERM, &handler, NULL);
	sigaction(SIGINT, &handler, NULL);
	sigaction(SIGQUIT, &handler, NULL);
	sigaction(SIGKILL, &handler, NULL);
	sigaction(SIGHUP, &handler, NULL);

	count = 0;
	while (42) {
		struct input_log *log_entry;
		int sockd, len;
		char *buf2;

		len = get_line(&buf);
		if (len == -1)
			break;
		mkstamp(stamp, sizeof(stamp));
		if (len < 1) {
			LOGERR("%s No message", stamp);
			continue;
		}
		if (buf[0] == '#') {
			LOGDEBUG("%s Got comment: %s", stamp, buf);
			continue;
		}
		LOGDEBUG("%s Got message: %s", stamp, buf);
		log_entry = ckalloc(sizeof(struct input_log));
		log_entry->buf = buf;
		CDL_PREPEND(input_log, log_entry);

		sockd = open_unix_client(socket_dir);
		if (sockd < 0) {
			LOGERR("Failed to open socket: %s", socket_dir);
			break;
		}
		if (!send_unix_msg(sockd, buf)) {
			LOGERR("Failed to send unix msg: %s", buf);
			break;
		}
		buf2 = recv_unix_msg_tmo2(sockd, tmo1, tmo2);
		close(sockd);
		if (!buf2) {
			LOGERR("Received empty reply");
			continue;
		}
		mkstamp(stamp, sizeof(stamp));
		LOGMSGSIZ(65536, LOG_NOTICE, "%s Received response: %s", stamp, buf2);
		dealloc(buf2);

		if (counter) {
			if ((++count % 100) == 0) {
				printf("%8d\r", count);
				fflush(stdout);
			}
		}
	}

	dealloc(socket_dir);
	sighandler(0);

	return 0;
}
