/*
 * WhaleRein Mosquitto broker plugin.
 *
 * The plugin keeps every MQTT decision local: connection admission is
 * validated once per CONNECT against Access and installed as a session-bound
 * policy with a revision and a bounded validity window. Publish, subscribe,
 * delivery and Will checks then read only that local policy. Access manages
 * policy, snapshots and targeted disconnects over the private $CONTROL
 * interface and receives presence notifications on a broker-internal topic.
 *
 * Credential validation is delegated to Access over a bounded private HTTP
 * request from a worker thread so the broker event loop never blocks. The
 * completion is applied from the main thread during MOSQ_EVT_TICK, as required
 * by mosquitto_complete_basic_auth().
 *
 * Control wire format v1 (UTF-8, one field per line, no embedded newlines):
 *
 *   whalerein/1
 *   id <request id>
 *   op <ping|set|remove|deny|kick|snapshot>
 *   ... operation fields, repeated lines for list values
 *
 * Replies are published to whalerein/v1/control/reply and presence
 * notifications to whalerein/v1/control/events.
 */

#include <arpa/inet.h>
#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include "mosquitto.h"
#include "mosquitto/broker.h"
#include "mosquitto/broker_plugin.h"

#define PLUGIN_NAME "whalerein-broker"
#define PLUGIN_VERSION "1.0.0"
#define PROTOCOL_VERSION 1

#define CONTROL_SUBSCRIBE_TOPIC "$CONTROL/whalerein/v1"
#define CONTROL_REQUEST_TOPIC "$CONTROL/whalerein/v1"
#define CONTROL_REPLY_TOPIC "whalerein/v1/control/reply"
#define CONTROL_EVENT_TOPIC "whalerein/v1/control/events"

#define MAX_LINE 4096
#define MAX_ACL 1024
#define MAX_CLIENTS 20000
#define MAX_INFLIGHT 256
#define AUTH_BODY_MAX 65536
#define AUTH_RESPONSE_MAX 262144
#define EVENT_BUFFER 4096
#define WILL_GRACE_MS 30000
#define DEFAULT_TIMEOUT_MS 500
#define DEFAULT_WORKERS 8
#define DEFAULT_TTL_MS 15000
#define MAX_TTL_MS 120000
#define MAX_SESSION_MS 300000
#define SESSION_ID_SIZE 97
#define SESSION_SCAN_INTERVAL_MS 100

#ifndef UNUSED
#  define UNUSED(A) (void)(A)
#endif

MOSQUITTO_PLUGIN_DECLARE_VERSION(5);

enum principal_kind {
	KIND_NONE = 0,
	KIND_ACCESS,
	KIND_HOST,
	KIND_DEVICE,
	KIND_PROBE,
};

enum acl_access {
	ACL_WRITE = 1,
	ACL_READ = 2,
	ACL_SUBSCRIBE = 4,
};

struct acl_entry {
	int access;
	uint64_t generation;
	char *host_id; /* NULL means the grant is not host-scoped. */
	char *filter;
};

struct wclient {
	struct wclient *next;
	struct mosquitto *handle; /* Current broker connection identity. */
	char *client_id;
	char *username;
	enum principal_kind kind;
	char *host_id;
	char *device_id;
	char session_id[SESSION_ID_SIZE];
	uint64_t session_revision;
	int64_t session_deadline_ms;
	uint64_t conn_id;
	uint64_t seq;
	uint64_t revision;
	bool connected;
	time_t valid_until_ms;
	uint64_t connected_at_ms;
	struct acl_entry acls[MAX_ACL];
	size_t acl_count;
};

struct fence {
	struct fence *next;
	char *device_id;
	char *host_id; /* NULL means device-wide */
	uint64_t generation;
	uint64_t order;
};

struct session_fence {
	struct session_fence *next;
	char session_id[SESSION_ID_SIZE];
	int64_t retained_until_ms;
};

struct auth_result {
	struct auth_result *next;
	char *client_id;
	int rc; /* MOSQ_ERR_SUCCESS or a denial */
	enum principal_kind kind;
	char *username;
	char *host_id;
	char *device_id;
	char session_id[SESSION_ID_SIZE];
	uint64_t session_revision;
	int64_t session_deadline_ms;
	uint64_t revision;
	bool ready;
	bool completed_auth;
	int64_t queued_ms;
	bool will_allowed;
	char *will_topic;
	struct acl_entry *acls;
	size_t acl_count;
	char error[128];
};

struct auth_job {
	struct auth_job *next;
	char *client_id;
	char *username;
	char *password;
	char *will_topic;
};

struct config {
	char *access_username;
	char *access_password;
	char *auth_url;
	char *auth_token;
	int timeout_ms;
	int workers;
	int ttl_ms;
};

static mosquitto_plugin_id_t *mosq_pid = NULL;
static struct config cfg;

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;

/* Guarded by `lock`. */
static struct wclient *clients = NULL;
static struct fence *fences = NULL;
static struct session_fence *session_fences = NULL;
static struct auth_result *completed = NULL;
static struct auth_job *jobs_head = NULL;
static struct auth_job *jobs_tail = NULL;
static size_t inflight = 0;
static uint64_t next_conn_id = 1;
static uint64_t next_seq = 1;
static uint64_t next_fence_order = 1;
static time_t policy_fresh_until_ms = 0;
/* True once Access confirmed the policy validity window in this broker
 * lifetime. After that, an expired window fails new external connections
 * closed even if the Access HTTP listener still answers, so a client cannot
 * bootstrap policy that Access never reconciled. */
static bool policy_confirmed = false;
static bool access_ready = false;
static uint64_t applied_revision = 0;

static pthread_t worker_ids[DEFAULT_WORKERS];
static pthread_cond_t jobs_cond = PTHREAD_COND_INITIALIZER;
static bool workers_started = false;
static bool stopping = false;

static int sock_timeout_ms = DEFAULT_TIMEOUT_MS;

/* ------------------------------------------------------------------ */
/* small helpers                                                      */
/* ------------------------------------------------------------------ */

static int64_t now_ms(void)
{
	struct timespec ts;

	clock_gettime(CLOCK_REALTIME, &ts);
	return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void safe_free(char **value)
{
	if (*value) {
		free(*value);
		*value = NULL;
	}
}

static char *dup_string(const char *value)
{
	char *copy;
	size_t len;

	if (!value) {
		return NULL;
	}
	len = strlen(value);
	copy = malloc(len + 1);
	if (!copy) {
		return NULL;
	}
	memcpy(copy, value, len + 1);
	return copy;
}

/* MQTT topic-filter match for a concrete topic. */
static bool topic_matches(const char *filter, const char *topic)
{
	const char *f = filter;
	const char *t = topic;

	if (!filter || !topic) {
		return false;
	}
	while (*f) {
		const char *fend = strchr(f, '/');
		const char *tend = strchr(t, '/');
		size_t flen = fend ? (size_t)(fend - f) : strlen(f);
		size_t tlen = tend ? (size_t)(tend - t) : strlen(t);

		if (flen == 1 && f[0] == '#') {
			return true;
		}
		if (flen == 1 && f[0] == '+') {
			if (tlen == 0) {
				return false;
			}
		} else if (flen != tlen || memcmp(f, t, flen) != 0) {
			return false;
		}
		if (!fend && !tend) {
			return true;
		}
		if (!fend || !tend) {
			/* A trailing "/#" also matches the parent level. */
			if (fend && !tend && strcmp(fend, "/#") == 0) {
				return true;
			}
			return false;
		}
		f = fend + 1;
		t = tend + 1;
	}
	return *t == '\0';
}

/* True when every topic matching `requested` also matches `allowed`. */
static bool filter_covers(const char *allowed, const char *requested)
{
	if (strcmp(allowed, requested) == 0) {
		return true;
	}
	if (strcmp(allowed, "#") == 0) {
		return true;
	}
	/* Prefix match for trailing # entries, e.g. "a/+/c/#" covers "a/b/c/d". */
	{
		size_t alen = strlen(allowed);
		size_t rlen = strlen(requested);

		if (alen >= 2 && strcmp(allowed + alen - 2, "/#") == 0 &&
		    rlen >= alen - 1 && strncmp(allowed, requested, alen - 1) == 0) {
			return true;
		}
		if (alen >= 2 && strcmp(allowed + alen - 2, "/+") == 0 &&
		    rlen >= alen - 1 && strncmp(allowed, requested, alen - 2) == 0) {
			return true;
		}
	}
	return false;
}

static bool filter_has_control_chars(const char *value)
{
	const unsigned char *p = (const unsigned char *)value;

	if (!value || value[0] == '\0') {
		return true;
	}
	for (; *p; p++) {
		if (*p < 0x20 || *p == 0x7f) {
			return true;
		}
	}
	return false;
}

/* ------------------------------------------------------------------ */
/* client and fence state                                             */
/* ------------------------------------------------------------------ */

static struct wclient *client_find_locked(const char *client_id)
{
	struct wclient *client;

	for (client = clients; client; client = client->next) {
		if (strcmp(client->client_id, client_id) == 0) {
			return client;
		}
	}
	return NULL;
}

static void client_clear_acls(struct wclient *client)
{
	size_t i;

	for (i = 0; i < client->acl_count; i++) {
		safe_free(&client->acls[i].filter);
	}
	client->acl_count = 0;
}

static void client_free_locked(struct wclient *target)
{
	struct wclient **link = &clients;
	struct wclient *client;

	while ((client = *link) != NULL) {
		if (client == target) {
			*link = client->next;
			client_clear_acls(client);
			safe_free(&client->client_id);
			safe_free(&client->username);
			safe_free(&client->host_id);
			safe_free(&client->device_id);
			free(client);
			return;
		}
		link = &client->next;
	}
}

static void clients_free_all_locked(void)
{
	while (clients) {
		client_free_locked(clients);
	}
}

static struct fence *fence_find_locked(const char *device_id, const char *host_id)
{
	struct fence *fence;

	for (fence = fences; fence; fence = fence->next) {
		if (strcmp(fence->device_id, device_id) != 0) {
			continue;
		}
		if (host_id && fence->host_id && strcmp(fence->host_id, host_id) == 0) {
			return fence;
		}
		if (!host_id && !fence->host_id) {
			return fence;
		}
	}
	return NULL;
}

static bool session_fenced_locked(const char *id)
{
	struct session_fence *fence;

	for (fence = session_fences; fence; fence = fence->next) {
		if (strcmp(fence->session_id, id) == 0) {
			return true;
		}
	}
	return false;
}

static bool session_valid_locked(const char *id, int64_t deadline_ms)
{
	return id[0] == '\0' || (deadline_ms > now_ms() && !session_fenced_locked(id));
}

/* Fence generations and stale policy. */
static bool acl_still_valid_locked(const struct wclient *client, const struct acl_entry *entry)
{
	struct fence *device_fence;
	struct fence *host_fence;

	if (!client->device_id) {
		return true;
	}
	device_fence = fence_find_locked(client->device_id, NULL);
	if (device_fence && entry->generation <= device_fence->generation) {
		return false;
	}
	if (entry->host_id) {
		host_fence = fence_find_locked(client->device_id, entry->host_id);
		if (host_fence && entry->generation <= host_fence->generation) {
			return false;
		}
	}
	return true;
}

static bool policy_fresh_locked(void)
{
	return policy_fresh_until_ms > now_ms();
}

static int acl_decide_locked(struct wclient *client, int access, const char *topic)
{
	size_t i;

	if (!client) {
		return MOSQ_ERR_ACL_DENIED;
	}
	if (client->kind == KIND_ACCESS) {
		return MOSQ_ERR_SUCCESS;
	}
	if (!client->connected && client->kind != KIND_DEVICE) {
		return MOSQ_ERR_ACL_DENIED;
	}
	if (!policy_fresh_locked()) {
		return MOSQ_ERR_ACL_DENIED;
	}
	if (client->valid_until_ms <= now_ms() ||
	    !session_valid_locked(client->session_id, client->session_deadline_ms)) {
		return MOSQ_ERR_ACL_DENIED;
	}
	/* Control topics are reserved for the internal Access identity. */
	if (strncmp(topic, "$CONTROL/", 9) == 0 || strncmp(topic, "whalerein/v1/control/", 21) == 0) {
		return MOSQ_ERR_ACL_DENIED;
	}
	for (i = 0; i < client->acl_count; i++) {
		const struct acl_entry *entry = &client->acls[i];
		int wanted = access == ACL_SUBSCRIBE ? (ACL_SUBSCRIBE | ACL_READ) : access;

		if (!(entry->access & wanted)) {
			continue;
		}
		if (!acl_still_valid_locked(client, entry)) {
			continue;
		}
		if (access == ACL_READ || access == ACL_WRITE) {
			if (topic_matches(entry->filter, topic)) {
				return MOSQ_ERR_SUCCESS;
			}
		} else if (access == ACL_SUBSCRIBE) {
			if (filter_covers(entry->filter, topic)) {
				return MOSQ_ERR_SUCCESS;
			}
		}
	}
	return MOSQ_ERR_ACL_DENIED;
}

/* ------------------------------------------------------------------ */
/* broker-originated publications                                     */
/* ------------------------------------------------------------------ */

static void publish_event_locked(uint64_t seq, const char *type, struct wclient *client,
				 const char *client_id, const char *kind, const char *host_id,
				 const char *device_id, uint64_t conn_id, int reason)
{
	char payload[EVENT_BUFFER];
	int len;
	int rc;

	len = snprintf(payload, sizeof(payload),
		       "whalerein/1\n"
		       "seq %llu\n"
		       "event %s\n"
		       "client %s\n"
		       "kind %s\n"
		       "host %s\n"
		       "device %s\n"
		       "conn %llu\n"
		       "at %lld\n"
		       "reason %d\n",
		       (unsigned long long)seq, type,
		       client_id ? client_id : "",
		       kind ? kind : "",
		       host_id ? host_id : "",
		       device_id ? device_id : "",
		       (unsigned long long)conn_id,
		       (long long)now_ms(), reason);
	if (len <= 0 || (size_t)len >= sizeof(payload)) {
		return;
	}
	UNUSED(client);
	rc = mosquitto_broker_publish_copy(NULL, CONTROL_EVENT_TOPIC, len, payload, 1, false, NULL);
	if (rc != MOSQ_ERR_SUCCESS) {
		mosquitto_log_printf(MOSQ_LOG_ERR, "whalerein: presence event publish failed: %d", rc);
	}
}

static void publish_reply(const char *request_id, const char *body)
{
	char *payload;
	int len;

	payload = malloc(AUTH_RESPONSE_MAX);
	if (!payload) {
		return;
	}
	len = snprintf(payload, AUTH_RESPONSE_MAX,
		       "whalerein/1\n"
		       "id %s\n"
		       "%s",
		       request_id ? request_id : "", body ? body : "ok\n");
	if (len > 0 && (size_t)len < AUTH_RESPONSE_MAX) {
		if (mosquitto_broker_publish_copy(NULL, CONTROL_REPLY_TOPIC, len, payload, 1, false, NULL) != MOSQ_ERR_SUCCESS) {
			mosquitto_log_printf(MOSQ_LOG_ERR, "whalerein: control reply publish failed");
		}
	}
	free(payload);
}

/* ------------------------------------------------------------------ */
/* control request parsing                                            */
/* ------------------------------------------------------------------ */

#define MAX_REQUEST_LINES 4096

struct request {
	char *lines[MAX_REQUEST_LINES];
	size_t line_count;
};

static void request_free(struct request *request)
{
	size_t i;

	for (i = 0; i < request->line_count; i++) {
		free(request->lines[i]);
	}
	request->line_count = 0;
}

/*
 * Parses one control payload into owned, newline-free lines. The payload must
 * start with the protocol banner. No line may exceed MAX_LINE.
 */
static bool request_init(struct request *request, const void *payload, size_t payloadlen)
{
	const char *data = payload;
	size_t offset = 0;
	bool banner = true;

	memset(request, 0, sizeof(*request));
	if (!payload || payloadlen == 0 || payloadlen > AUTH_RESPONSE_MAX) {
		return false;
	}
	while (offset < payloadlen) {
		const char *start = data + offset;
		const char *newline = memchr(start, '\n', payloadlen - offset);
		size_t len = newline ? (size_t)(newline - start) : payloadlen - offset;
		char *line;

		if (len > 0 && start[len - 1] == '\r') {
			len--;
		}
		if (len > MAX_LINE) {
			request_free(request);
			return false;
		}
		if (banner) {
			banner = false;
			if (len != strlen("whalerein/1") || memcmp(start, "whalerein/1", len) != 0) {
				request_free(request);
				return false;
			}
		} else if (len > 0) {
			if (request->line_count >= MAX_REQUEST_LINES) {
				request_free(request);
				return false;
			}
			line = malloc(len + 1);
			if (!line) {
				request_free(request);
				return false;
			}
			memcpy(line, start, len);
			line[len] = '\0';
			request->lines[request->line_count++] = line;
		}
		offset += newline ? (size_t)(newline - start) + 1 : payloadlen - offset;
	}
	return true;
}

/* Copies the value of a "<key> <value>" line into out. */
static bool line_value(const struct request *request, const char *key, char *out, size_t out_len)
{
	size_t i;
	size_t key_len = strlen(key);

	for (i = 0; i < request->line_count; i++) {
		const char *line = request->lines[i];

		if (strncmp(line, key, key_len) == 0 && line[key_len] == ' ') {
			const char *value = line + key_len + 1;
			size_t len = strcspn(value, "\n");

			if (len >= out_len) {
				return false;
			}
			memcpy(out, value, len);
			out[len] = '\0';
			return true;
		}
	}
	return false;
}

static bool line_uint(const struct request *request, const char *key, uint64_t *out, bool required)
{
	char value[64];
	char *end = NULL;
	unsigned long long parsed;

	if (!line_value(request, key, value, sizeof(value))) {
		return !required;
	}
	errno = 0;
	parsed = strtoull(value, &end, 10);
	if (errno || !end || *end != '\0') {
		return false;
	}
	*out = (uint64_t)parsed;
	return true;
}

static bool parse_acl_lines(const struct request *request, struct acl_entry *entries, size_t *count, uint64_t revision)
{
	size_t i;

	*count = 0;
	for (i = 0; i < request->line_count; i++) {
		const char *line = request->lines[i];
		const char *rest;
		const char *space;
		int access = 0;
		size_t len;

		if (strncmp(line, "acl ", 4) != 0) {
			continue;
		}
		rest = line + 4;
		space = strchr(rest, ' ');
		if (!space) {
			return false;
		}
		len = (size_t)(space - rest);
		if (len == 5 && strncmp(rest, "write", 5) == 0) {
			access = ACL_WRITE;
		} else if (len == 4 && strncmp(rest, "read", 4) == 0) {
			access = ACL_READ;
		} else if (len == 9 && strncmp(rest, "subscribe", 9) == 0) {
			access = ACL_SUBSCRIBE;
		} else {
			return false;
		}
		if (*count >= MAX_ACL) {
			return false;
		}
		{
			const char *generation_text = space + 1;
			const char *scope = strchr(generation_text, ' ');
			const char *filter;
			char *end = NULL;
			unsigned long long generation;

			if (!scope) {
				return false;
			}
			errno = 0;
			generation = strtoull(generation_text, &end, 10);
			if (errno || end != scope) {
				return false;
			}
			scope++;
			filter = strchr(scope, ' ');
			if (!filter) {
				return false;
			}
			if (filter_has_control_chars(filter + 1)) {
				return false;
			}
			entries[*count].access = access;
			entries[*count].generation = generation ? (uint64_t)generation : revision;
			entries[*count].host_id = (strncmp(scope, "- ", 2) == 0) ? NULL : dup_string(scope);
			if (entries[*count].host_id) {
				char *space_in_host = strchr(entries[*count].host_id, ' ');

				if (space_in_host) {
					*space_in_host = '\0';
				}
			}
			entries[*count].filter = dup_string(filter + 1);
			if (!entries[*count].filter) {
				safe_free(&entries[*count].host_id);
				return false;
			}
			(*count)++;
		}
	}
	return true;
}

static void clear_parsed_acls(struct acl_entry *entries, size_t count)
{
	size_t i;

	for (i = 0; i < count; i++) {
		safe_free(&entries[i].filter);
		safe_free(&entries[i].host_id);
	}
}

static void set_reply_error(char *body, size_t body_len, const char *code, const char *detail)
{
	snprintf(body, body_len, "error %s %s\n", code, detail ? detail : "");
}

/* Applies a policy.set for an existing connection. */
static void handle_set(struct request *request, char *body, size_t body_len)
{
	char client_id[MAX_LINE];
	char kind[32];
	char host_id[MAX_LINE];
	char device_id[MAX_LINE];
	uint64_t revision = 0;
	uint64_t ttl = 0;
	struct acl_entry acls[MAX_ACL];
	size_t acl_count = 0;
	bool have_client;
	bool have_kind;
	struct wclient *client;
	enum principal_kind parsed_kind = KIND_NONE;
	size_t i;

	have_client = line_value(request, "client", client_id, sizeof(client_id));
	have_kind = line_value(request, "kind", kind, sizeof(kind));
	if (!have_client || !have_kind || !line_uint(request, "revision", &revision, true) ||
	    !line_uint(request, "ttl", &ttl, true)) {
		set_reply_error(body, body_len, "invalid_argument", "missing client, kind, revision or ttl");
		return;
	}
	if (filter_has_control_chars(client_id) || filter_has_control_chars(kind)) {
		set_reply_error(body, body_len, "invalid_argument", "invalid identity");
		return;
	}
	if (strcmp(kind, "host") == 0) {
		parsed_kind = KIND_HOST;
	} else if (strcmp(kind, "device") == 0) {
		parsed_kind = KIND_DEVICE;
	} else if (strcmp(kind, "probe") == 0) {
		parsed_kind = KIND_PROBE;
	} else {
		set_reply_error(body, body_len, "invalid_argument", "unknown kind");
		return;
	}
	if (ttl < 1000 || ttl > MAX_TTL_MS) {
		set_reply_error(body, body_len, "invalid_argument", "ttl out of range");
		return;
	}
	if (!line_value(request, "host", host_id, sizeof(host_id))) {
		host_id[0] = '\0';
	}
	if (!line_value(request, "device", device_id, sizeof(device_id))) {
		device_id[0] = '\0';
	}
	if ((host_id[0] && filter_has_control_chars(host_id)) ||
	    (device_id[0] && filter_has_control_chars(device_id))) {
		set_reply_error(body, body_len, "invalid_argument", "invalid scope");
		return;
	}
	if (!parse_acl_lines(request, acls, &acl_count, revision)) {
		clear_parsed_acls(acls, acl_count);
		set_reply_error(body, body_len, "invalid_argument", "invalid acl entry");
		return;
	}

	pthread_mutex_lock(&lock);
	client = client_find_locked(client_id);
	if (!client) {
		clear_parsed_acls(acls, acl_count);
		pthread_mutex_unlock(&lock);
		set_reply_error(body, body_len, "not_found", "client not connected");
		return;
	}
	if (revision <= client->revision) {
		clear_parsed_acls(acls, acl_count);
		pthread_mutex_unlock(&lock);
		snprintf(body, body_len, "ok\nstale 1\nseq %llu\n",
			 (unsigned long long)next_seq);
		return;
	}
	client_clear_acls(client);
	for (i = 0; i < acl_count; i++) {
		if ((client->session_id[0] && !acls[i].host_id) ||
		    !acl_still_valid_locked(client, &acls[i])) {
			clear_parsed_acls(&acls[i], 1);
			continue;
		}
		if (client->acl_count < MAX_ACL) {
			client->acls[client->acl_count++] = acls[i];
		} else {
			clear_parsed_acls(&acls[i], 1);
		}
	}
	client->kind = parsed_kind;
	safe_free(&client->host_id);
	safe_free(&client->device_id);
	client->host_id = host_id[0] ? dup_string(host_id) : NULL;
	client->device_id = device_id[0] ? dup_string(device_id) : NULL;
	client->revision = revision;
	client->valid_until_ms = now_ms() + (time_t)ttl;
	if (revision > applied_revision) {
		applied_revision = revision;
	}
	snprintf(body, body_len, "ok\nrevision %llu\nseq %llu\n",
		 (unsigned long long)revision, (unsigned long long)next_seq);
	pthread_mutex_unlock(&lock);
}

static void handle_remove(struct request *request, char *body, size_t body_len)
{
	char client_id[MAX_LINE];
	char kick[16];
	struct wclient *client;
	uint64_t conn_id = 0;

	if (!line_value(request, "client", client_id, sizeof(client_id))) {
		set_reply_error(body, body_len, "invalid_argument", "missing client");
		return;
	}
	if (!line_value(request, "kick", kick, sizeof(kick))) {
		kick[0] = '\0';
	}
	pthread_mutex_lock(&lock);
	client = client_find_locked(client_id);
	if (!client) {
		pthread_mutex_unlock(&lock);
		snprintf(body, body_len, "ok\nseq %llu\n", (unsigned long long)next_seq);
		return;
	}
	conn_id = client->conn_id;
	client_clear_acls(client);
	client->revision = 0;
	client->kind = KIND_NONE;
	client->valid_until_ms = 0;
	pthread_mutex_unlock(&lock);
	if (strcmp(kick, "1") == 0) {
		mosquitto_kick_client_by_clientid(client_id, false);
	}
	snprintf(body, body_len, "ok\nconn %llu\nseq %llu\n",
		 (unsigned long long)conn_id, (unsigned long long)next_seq);
}

static void handle_deny(struct request *request, char *body, size_t body_len)
{
	char device_id[MAX_LINE];
	char host_id[MAX_LINE];
	char kick[16];
	uint64_t generation = 0;
	uint64_t order = 0;
	uint64_t matched = 0;
	bool have_host;
	struct fence *fence;
	struct wclient *client;
	char kick_ids[16][128];
	size_t kick_count = 0;
	size_t i;

	if (!line_value(request, "device", device_id, sizeof(device_id)) ||
	    !line_uint(request, "generation", &generation, true)) {
		set_reply_error(body, body_len, "invalid_argument", "missing device or generation");
		return;
	}
	have_host = line_value(request, "host", host_id, sizeof(host_id));
	if (!line_value(request, "kick", kick, sizeof(kick))) {
		kick[0] = '\0';
	}
	if (filter_has_control_chars(device_id) || (have_host && filter_has_control_chars(host_id))) {
		set_reply_error(body, body_len, "invalid_argument", "invalid scope");
		return;
	}

	pthread_mutex_lock(&lock);
	fence = fence_find_locked(device_id, have_host ? host_id : NULL);
	if (!fence) {
		fence = calloc(1, sizeof(*fence));
		if (!fence) {
			pthread_mutex_unlock(&lock);
			set_reply_error(body, body_len, "internal", "out of memory");
			return;
		}
		fence->device_id = dup_string(device_id);
		fence->host_id = have_host ? dup_string(host_id) : NULL;
		fence->generation = generation;
		fence->order = ++next_fence_order;
		fence->next = fences;
		fences = fence;
	} else if (generation > fence->generation) {
		fence->generation = generation;
	}
	order = fence->order;
	/* Remove fenced entries from live policies and count affected clients. */
	for (client = clients; client; client = client->next) {
		bool affected = false;
		bool scope_match = false;

		if (!client->device_id || strcmp(client->device_id, device_id) != 0) {
			continue;
		}
		for (i = 0; i < client->acl_count; i++) {
			if (have_host) {
				if (!client->acls[i].host_id || strcmp(client->acls[i].host_id, host_id) != 0) {
					continue;
				}
			}
			if (client->acls[i].generation <= generation) {
				affected = true;
				scope_match = true;
				break;
			}
			scope_match = true;
		}
		if (!affected) {
			/*
			 * A fenced revision with no stale entry still counts as an
			 * applied scope: the connection policy is fenced for future
			 * installs even when this connection holds newer authority.
			 */
			(void)scope_match;
			continue;
		}
		matched++;
		for (i = client->acl_count; i > 0; i--) {
			size_t index = i - 1;

			if (have_host) {
				if (!client->acls[index].host_id || strcmp(client->acls[index].host_id, host_id) != 0) {
					continue;
				}
			}
			if (client->acls[index].generation <= generation) {
				safe_free(&client->acls[index].filter);
				safe_free(&client->acls[index].host_id);
				client->acl_count--;
				if (index != client->acl_count) {
					client->acls[index] = client->acls[client->acl_count];
					memset(&client->acls[client->acl_count], 0, sizeof(struct acl_entry));
				}
			}
		}
		if (strcmp(kick, "1") == 0 && client->connected && kick_count < 16) {
			/* Kick only when no Host-scoped authority remains. One device
			 * connection can carry grants for several Hosts; revoking one Host
			 * must not disconnect a connection that still holds another Host's
			 * live authority. Device-wide entries (the owner's Will topic) do
			 * not represent Host authority. */
			size_t remaining_hosts = 0;

			for (i = 0; i < client->acl_count; i++) {
				if (client->acls[i].host_id && acl_still_valid_locked(client, &client->acls[i])) {
					remaining_hosts++;
				}
			}
			if (remaining_hosts == 0) {
				snprintf(kick_ids[kick_count], sizeof(kick_ids[0]), "%s", client->client_id);
				kick_count++;
			}
		}
	}
	pthread_mutex_unlock(&lock);

	for (i = 0; i < kick_count; i++) {
		mosquitto_kick_client_by_clientid(kick_ids[i], false);
	}
	snprintf(body, body_len, "ok\napplied_generation %llu\norder %llu\nmatched %llu\nseq %llu\n",
		 (unsigned long long)generation, (unsigned long long)order,
		 (unsigned long long)matched, (unsigned long long)next_seq);
}

static void handle_kick(struct request *request, char *body, size_t body_len)
{
	char client_id[MAX_LINE];
	char username[MAX_LINE];
	int rc = MOSQ_ERR_NOT_FOUND;

	if (line_value(request, "client", client_id, sizeof(client_id))) {
		rc = mosquitto_kick_client_by_clientid(client_id, false);
	} else if (line_value(request, "username", username, sizeof(username))) {
		rc = mosquitto_kick_client_by_username(username, false);
	} else {
		set_reply_error(body, body_len, "invalid_argument", "missing client or username");
		return;
	}
	if (rc != MOSQ_ERR_SUCCESS) {
		snprintf(body, body_len, "ok\nabsent 1\nseq %llu\n", (unsigned long long)next_seq);
		return;
	}
	snprintf(body, body_len, "ok\nseq %llu\n", (unsigned long long)next_seq);
}

/* Session controls never act on a Device username. A terminal fence is
 * retained long enough to cover any queued authentication and its permit. */
static void handle_end_session(struct request *request, char *body, size_t body_len, bool terminal)
{
	char id[SESSION_ID_SIZE];
	char username[SESSION_ID_SIZE + 8];
	struct session_fence *fence;
	int rc;

	if (!line_value(request, "session", id, sizeof(id)) || filter_has_control_chars(id)) {
		set_reply_error(body, body_len, "invalid_argument", "missing session");
		return;
	}
	pthread_mutex_lock(&lock);
	if (terminal && !session_fenced_locked(id)) {
		fence = calloc(1, sizeof(*fence));
		if (!fence) {
			pthread_mutex_unlock(&lock);
			set_reply_error(body, body_len, "internal", "out of memory");
			return;
		}
		snprintf(fence->session_id, sizeof(fence->session_id), "%s", id);
		fence->retained_until_ms = now_ms() + MAX_SESSION_MS + 30000;
		fence->next = session_fences;
		session_fences = fence;
	}
	pthread_mutex_unlock(&lock);
	snprintf(username, sizeof(username), "session:%s", id);
	rc = mosquitto_kick_client_by_username(username, false);
	if (rc != MOSQ_ERR_SUCCESS && rc != MOSQ_ERR_NOT_FOUND) {
		set_reply_error(body, body_len, "internal", "could not close managed session");
		return;
	}
	snprintf(body, body_len, "ok\n");
}

static void handle_renew_session(struct request *request, char *body, size_t body_len)
{
	char id[SESSION_ID_SIZE];
	uint64_t revision, deadline;
	struct wclient *client;
	struct auth_result *result;

	if (!line_value(request, "session", id, sizeof(id)) ||
	    !line_uint(request, "session_revision", &revision, true) ||
	    !line_uint(request, "deadline_ms", &deadline, true) ||
	    deadline > (uint64_t)(now_ms() + MAX_SESSION_MS + 30000)) {
		set_reply_error(body, body_len, "invalid_argument", "invalid session renewal");
		return;
	}
	pthread_mutex_lock(&lock);
	if (!session_fenced_locked(id)) {
		for (client = clients; client; client = client->next) {
			if (client->connected && strcmp(client->session_id, id) == 0 &&
			    revision > client->session_revision) {
				client->session_revision = revision;
				client->session_deadline_ms = (int64_t)deadline;
			}
		}
		for (result = completed; result; result = result->next) {
			if (strcmp(result->session_id, id) == 0 && revision > result->session_revision) {
				result->session_revision = revision;
				result->session_deadline_ms = (int64_t)deadline;
			}
		}
	}
	pthread_mutex_unlock(&lock);
	snprintf(body, body_len, "ok\n");
}

static size_t snapshot_line(char *out, size_t out_len, const struct wclient *client, uint64_t seq)
{
	return (size_t)snprintf(out, out_len,
				"client %s %s %s %s %llu %llu %s %llu %lld %s\n",
				client->client_id,
				client->kind == KIND_HOST ? "host" :
				client->kind == KIND_DEVICE ? "device" :
				client->kind == KIND_PROBE ? "probe" : "none",
				client->host_id ? client->host_id : "",
				client->device_id ? client->device_id : "",
				(unsigned long long)client->conn_id,
				(unsigned long long)client->revision,
				client->connected ? "connected" : "offline",
				(unsigned long long)seq,
				(long long)client->connected_at_ms,
				client->session_id[0] ? client->session_id : "-");
}

static char *handle_snapshot(char *body, size_t body_len)
{
	struct wclient *client;
	size_t used;
	uint64_t seq;

	pthread_mutex_lock(&lock);
	/* The boundary is the last sequence already assigned. Notifications that
	 * follow carry a strictly greater sequence, so Access can discard only
	 * events the snapshot supersedes. */
	seq = next_seq > 0 ? next_seq - 1 : 0;
	used = (size_t)snprintf(body, body_len, "ok\nseq %llu\nrevision %llu\nsession_capability 1\n",
				 (unsigned long long)seq, (unsigned long long)applied_revision);
	for (client = clients; client && used < body_len - 1; client = client->next) {
		size_t written = snapshot_line(body + used, body_len - used, client, seq);

		used += written;
	}
	pthread_mutex_unlock(&lock);
	return body;
}

static void handle_ping(struct request *request, char *body, size_t body_len)
{
	uint64_t revision = 0;
	char valid[32];
	struct wclient *client;
	time_t deadline;

	line_uint(request, "revision", &revision, false);
	if (!line_value(request, "ttl", valid, sizeof(valid))) {
		snprintf(valid, sizeof(valid), "%d", cfg.ttl_ms);
	}
	deadline = now_ms() + cfg.ttl_ms;
	pthread_mutex_lock(&lock);
	policy_fresh_until_ms = deadline;
	policy_confirmed = true;
	access_ready = true;
	for (client = clients; client; client = client->next) {
		if (client->connected && client->kind != KIND_ACCESS) {
			client->valid_until_ms = deadline;
		}
	}
	if (revision > applied_revision) {
		applied_revision = revision;
	}
	snprintf(body, body_len, "ok\nrevision %llu\nseq %llu\n",
		 (unsigned long long)applied_revision, (unsigned long long)next_seq);
	pthread_mutex_unlock(&lock);
}

static void handle_control(const struct mosquitto_evt_control *ed)
{
	struct request request;
	char *body;
	char id[128];
	char op[32];
	const char *username;
	struct mosquitto *client;

	if (!ed || !ed->payload || ed->payloadlen == 0) {
		return;
	}
	client = ed->client;
	username = client ? mosquitto_client_username(client) : NULL;
	if (!username || !cfg.access_username || strcmp(username, cfg.access_username) != 0) {
		mosquitto_log_printf(MOSQ_LOG_WARNING, "whalerein: control request from a non-Access principal rejected");
		return;
	}
	body = malloc(AUTH_RESPONSE_MAX);
	if (!body) {
		publish_reply("unknown", "error internal out_of_memory\n");
		return;
	}
	if (!request_init(&request, ed->payload, ed->payloadlen)) {
		publish_reply("unknown", "error invalid_argument malformed request\n");
		free(body);
		return;
	}
	if (!line_value(&request, "id", id, sizeof(id)) ||
	    !line_value(&request, "op", op, sizeof(op))) {
		publish_reply("unknown", "error invalid_argument missing id or op\n");
		request_free(&request);
		free(body);
		return;
	}
	if (strcmp(op, "snapshot") == 0) {
		handle_snapshot(body, AUTH_RESPONSE_MAX);
	} else if (strcmp(op, "ping") == 0) {
		handle_ping(&request, body, AUTH_RESPONSE_MAX);
	} else if (strcmp(op, "set") == 0) {
		handle_set(&request, body, AUTH_RESPONSE_MAX);
	} else if (strcmp(op, "remove") == 0) {
		handle_remove(&request, body, AUTH_RESPONSE_MAX);
	} else if (strcmp(op, "deny") == 0) {
		handle_deny(&request, body, AUTH_RESPONSE_MAX);
	} else if (strcmp(op, "kick") == 0) {
		handle_kick(&request, body, AUTH_RESPONSE_MAX);
	} else if (strcmp(op, "end_session") == 0) {
		handle_end_session(&request, body, AUTH_RESPONSE_MAX, true);
	} else if (strcmp(op, "session_absent") == 0) {
		handle_end_session(&request, body, AUTH_RESPONSE_MAX, false);
	} else if (strcmp(op, "renew_session") == 0) {
		handle_renew_session(&request, body, AUTH_RESPONSE_MAX);
	} else {
		set_reply_error(body, AUTH_RESPONSE_MAX, "invalid_argument", "unknown op");
	}
	publish_reply(id, body);
	request_free(&request);
	free(body);
}

/* ------------------------------------------------------------------ */
/* deferred authentication                                            */
/* ------------------------------------------------------------------ */

static int http_post(const char *url, const char *token, const char *body,
		     char *response, size_t response_len)
{
	char scheme[16] = {0};
	char host[256] = {0};
	char path[512] = {0};
	int port = 80;
	const char *rest;
	const char *slash;
	const char *colon;
	struct addrinfo hints;
	struct addrinfo *result = NULL;
	struct addrinfo *candidate;
	int fd = -1;
	int rc;
	char request[AUTH_BODY_MAX + 1024];
	int request_len;
	size_t used = 0;
	bool header_done = false;
	int content_length = -1;
	size_t body_start = 0;

	if (!url || strncmp(url, "http://", 7) != 0) {
		return -1;
	}
	rest = url + 7;
	slash = strchr(rest, '/');
	if (!slash) {
		return -1;
	}
	snprintf(host, sizeof(host), "%.*s", (int)(slash - rest), rest);
	snprintf(path, sizeof(path), "%s", slash);
	colon = strchr(host, ':');
	if (colon) {
		port = atoi(colon + 1);
		host[colon - host] = '\0';
	}
	UNUSED(scheme);

	if (!token) {
		token = "";
	}
	request_len = snprintf(request, sizeof(request),
			       "POST %s HTTP/1.1\r\n"
			       "Host: %s\r\n"
			       "Content-Type: application/x-www-form-urlencoded\r\n"
			       "X-WhaleRein-Internal-Token: %s\r\n"
			       "Content-Length: %zu\r\n"
			       "Connection: close\r\n"
			       "\r\n"
			       "%s",
			       path, host, token, strlen(body), body);
	if (request_len <= 0 || (size_t)request_len >= sizeof(request)) {
		return -1;
	}

	memset(&hints, 0, sizeof(hints));
	hints.ai_family = AF_UNSPEC;
	hints.ai_socktype = SOCK_STREAM;
	{
		char port_text[16];

		snprintf(port_text, sizeof(port_text), "%d", port);
		if (getaddrinfo(host, port_text, &hints, &result) != 0) {
			return -1;
		}
	}
	for (candidate = result; candidate; candidate = candidate->ai_next) {
		fd = socket(candidate->ai_family, candidate->ai_socktype, candidate->ai_protocol);
		if (fd < 0) {
			continue;
		}
		{
			struct timeval tv;

			tv.tv_sec = sock_timeout_ms / 1000;
			tv.tv_usec = (sock_timeout_ms % 1000) * 1000;
			setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
			setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));
		}
		if (connect(fd, candidate->ai_addr, candidate->ai_addrlen) == 0) {
			break;
		}
		close(fd);
		fd = -1;
	}
	freeaddrinfo(result);
	if (fd < 0) {
		return -1;
	}

	{
		size_t sent = 0;

		while (sent < (size_t)request_len) {
			ssize_t n = send(fd, request + sent, (size_t)request_len - sent, 0);

			if (n <= 0) {
				close(fd);
				return -1;
			}
			sent += (size_t)n;
		}
	}

	while (used < response_len - 1) {
		ssize_t n = recv(fd, response + used, response_len - 1 - used, 0);

		if (n <= 0) {
			break;
		}
		used += (size_t)n;
		response[used] = '\0';
		if (!header_done) {
			char *sep = strstr(response, "\r\n\r\n");

			if (sep) {
				const char *cursor = response;
				const char *headers_end = sep;

				header_done = true;
				body_start = (size_t)(sep - response) + 4;
				/* Status line. */
				{
					const char *line_end = strstr(cursor, "\r\n");

					if (!line_end || strncmp(cursor, "HTTP/1.", 7) != 0) {
						close(fd);
						return -1;
					}
					rc = atoi(cursor + 9);
					if (rc != 200) {
						close(fd);
						return -1;
					}
					cursor = line_end + 2;
				}
				while (cursor < headers_end) {
					const char *line_end = strstr(cursor, "\r\n");
					size_t line_len;

					if (!line_end || line_end > headers_end) {
						break;
					}
					line_len = (size_t)(line_end - cursor);
					if (line_len >= 15 && strncasecmp(cursor, "Content-Length:", 15) == 0) {
						content_length = atoi(cursor + 15);
					}
					cursor = line_end + 2;
				}
			}
		}
		if (header_done && content_length >= 0 && used - body_start >= (size_t)content_length) {
			break;
		}
	}
	close(fd);
	if (!header_done) {
		return -1;
	}
	memmove(response, response + body_start, used - body_start + 1);
	return 0;
}

static void result_free(struct auth_result *result)
{
	safe_free(&result->client_id);
	safe_free(&result->username);
	safe_free(&result->host_id);
	safe_free(&result->device_id);
	safe_free(&result->will_topic);
	clear_parsed_acls(result->acls, result->acl_count);
	free(result->acls);
	free(result);
}

static struct auth_result *result_parse(const char *client_id, const char *body, int http_rc)
{
	struct auth_result *result;
	char *copy;
	char *line;
	char *save = NULL;
	bool first = true;
	bool ok = false;

	result = calloc(1, sizeof(*result));
	if (!result) {
		return NULL;
	}
	result->rc = MOSQ_ERR_AUTH;
	result->client_id = dup_string(client_id);
	result->acls = calloc(MAX_ACL, sizeof(*result->acls));
	if (!result->client_id || !result->acls) {
		result_free(result);
		return NULL;
	}
	if (http_rc != 0 || !body) {
		/* The policy/auth source is unreachable, which is a transport
		 * availability failure, not a credential decision. Mosquitto maps
		 * MOSQ_ERR_SERVER_UNAVAILABLE to CONNACK 0x88 (server unavailable)
		 * instead of 0x87 (not authorized) for MQTT 5 clients, so an operator
		 * does not misread a policy outage as revoked credentials. */
		result->rc = MOSQ_ERR_SERVER_UNAVAILABLE;
		snprintf(result->error, sizeof(result->error), "auth service unavailable");
		return result;
	}
	copy = dup_string(body);
	if (!copy) {
		result_free(result);
		return NULL;
	}
	for (line = strtok_r(copy, "\n", &save); line; line = strtok_r(NULL, "\n", &save)) {
		size_t len = strlen(line);

		if (len && line[len - 1] == '\r') {
			line[len - 1] = '\0';
		}
		if (first) {
			first = false;
			if (strcmp(line, "whalerein/1") == 0) {
				continue;
			}
		}
		if (strcmp(line, "ok") == 0) {
			ok = true;
		} else if (strncmp(line, "deny", 4) == 0) {
			result->rc = MOSQ_ERR_AUTH;
			snprintf(result->error, sizeof(result->error), "%.*s",
				 (int)strcspn(line + 4, " "), line + 4);
			/* Access distinguishes a policy projection outage ("unavailable")
			 * from a credential decision ("unauthorized"). Mosquitto 2.1.2's
			 * deferred completion collapses every non-success result to
			 * not-authorized, so this only takes effect if that broker path
			 * preserves the reason. The synchronous stale-policy gate in
			 * basic_auth_callback is the path that does reach CONNACK 0x88. */
			if (line[4] == ' ' && strncmp(line + 5, "unavailable", 11) == 0) {
				result->rc = MOSQ_ERR_SERVER_UNAVAILABLE;
			}
		} else if (strncmp(line, "kind ", 5) == 0) {
			const char *value = line + 5;

			if (strcmp(value, "host") == 0) {
				result->kind = KIND_HOST;
			} else if (strcmp(value, "device") == 0) {
				result->kind = KIND_DEVICE;
			} else if (strcmp(value, "probe") == 0) {
				result->kind = KIND_PROBE;
			} else if (strcmp(value, "access") == 0) {
				result->kind = KIND_ACCESS;
			}
		} else if (strncmp(line, "host ", 5) == 0) {
			safe_free(&result->host_id);
			result->host_id = dup_string(line + 5);
		} else if (strncmp(line, "device ", 7) == 0) {
			safe_free(&result->device_id);
			result->device_id = dup_string(line + 7);
		} else if (strncmp(line, "session ", 8) == 0) {
			unsigned long long revision;
			long long deadline;

			if (sscanf(line + 8, "%96s %llu %lld", result->session_id, &revision, &deadline) == 3) {
				result->session_revision = revision;
				result->session_deadline_ms = deadline;
			}
		} else if (strncmp(line, "revision ", 9) == 0) {
			result->revision = strtoull(line + 9, NULL, 10);
		} else if (strncmp(line, "ready ", 6) == 0) {
			result->ready = strcmp(line + 6, "1") == 0;
		} else if (strncmp(line, "will ", 5) == 0) {
			result->will_allowed = true;
			safe_free(&result->will_topic);
			result->will_topic = dup_string(line + 5);
		} else if (strncmp(line, "acl ", 4) == 0) {
			const char *rest = line + 4;
			const char *space = strchr(rest, ' ');
			const char *generation_text;
			const char *scope;
			const char *filter;
			char *end = NULL;
			unsigned long long generation;
			int access = 0;

			if (!space || result->acl_count >= MAX_ACL) {
				continue;
			}
			if ((size_t)(space - rest) == 5 && strncmp(rest, "write", 5) == 0) {
				access = ACL_WRITE;
			} else if ((size_t)(space - rest) == 4 && strncmp(rest, "read", 4) == 0) {
				access = ACL_READ;
			} else if ((size_t)(space - rest) == 9 && strncmp(rest, "subscribe", 9) == 0) {
				access = ACL_SUBSCRIBE;
			} else {
				continue;
			}
			generation_text = space + 1;
			scope = strchr(generation_text, ' ');
			if (!scope) {
				continue;
			}
			errno = 0;
			generation = strtoull(generation_text, &end, 10);
			if (errno || end != scope) {
				continue;
			}
			scope++;
			filter = strchr(scope, ' ');
			if (!filter) {
				continue;
			}
			result->acls[result->acl_count].access = access;
			result->acls[result->acl_count].generation = generation;
			result->acls[result->acl_count].host_id =
				(strncmp(scope, "- ", 2) == 0) ? NULL : strndup(scope, (size_t)(filter - scope));
			result->acls[result->acl_count].filter = dup_string(filter + 1);
			if (result->acls[result->acl_count].filter) {
				result->acl_count++;
			} else {
				safe_free(&result->acls[result->acl_count].host_id);
			}
		}
	}
	free(copy);
	if (!ok) {
		if (result->rc != MOSQ_ERR_SERVER_UNAVAILABLE) {
			result->rc = MOSQ_ERR_AUTH;
		}
		if (result->error[0] == '\0') {
			snprintf(result->error, sizeof(result->error), "denied by access");
		}
		return result;
	}
	result->rc = MOSQ_ERR_SUCCESS;
	return result;
}

static char *form_encode(const char *label, const char *value, char *out, size_t out_len)
{
	size_t used = 0;
	const unsigned char *p = (const unsigned char *)(value ? value : "");

	used += (size_t)snprintf(out + used, out_len - used, "%s=", label);
	for (; *p && used + 4 < out_len; p++) {
		if (isalnum(*p) || *p == '-' || *p == '_' || *p == '.' || *p == '~') {
			out[used++] = (char)*p;
		} else {
			used += (size_t)snprintf(out + used, out_len - used, "%%%02X", *p);
		}
	}
	out[used] = '\0';
	return out;
}

static void auth_worker(struct auth_job *job, struct auth_result **result_out)
{
	char body[AUTH_BODY_MAX];
	char part[4096];
	char response[AUTH_RESPONSE_MAX];
	size_t used = 0;
	int rc;

	body[0] = '\0';
	form_encode("clientid", job->client_id, part, sizeof(part));
	used += (size_t)snprintf(body + used, sizeof(body) - used, "%s", part);
	form_encode("username", job->username, part, sizeof(part));
	used += (size_t)snprintf(body + used, sizeof(body) - used, "&%s", part);
	form_encode("password", job->password, part, sizeof(part));
	used += (size_t)snprintf(body + used, sizeof(body) - used, "&%s", part);
	if (job->will_topic) {
		form_encode("will", job->will_topic, part, sizeof(part));
		used += (size_t)snprintf(body + used, sizeof(body) - used, "&%s", part);
	}
	memset(response, 0, sizeof(response));
	rc = http_post(cfg.auth_url, cfg.auth_token, body, response, sizeof(response));
	if (rc != 0) {
		mosquitto_log_printf(MOSQ_LOG_WARNING, "whalerein: authentication request failed for %s/%s",
				     job->client_id, job->username);
	}
	*result_out = result_parse(job->client_id, rc == 0 ? response : NULL, rc);
}

static void *worker_main(void *arg)
{
	UNUSED(arg);
	for (;;) {
		struct auth_job *job;
		struct auth_result *result = NULL;

		pthread_mutex_lock(&lock);
		while (!jobs_head && !stopping) {
			pthread_cond_wait(&jobs_cond, &lock);
		}
		if (stopping) {
			pthread_mutex_unlock(&lock);
			break;
		}
		job = jobs_head;
		jobs_head = job->next;
		if (!jobs_head) {
			jobs_tail = NULL;
		}
		pthread_mutex_unlock(&lock);

		auth_worker(job, &result);
		pthread_mutex_lock(&lock);
		if (result) {
			result->username = job->username;
			job->username = NULL;
			result->queued_ms = now_ms();
			result->next = completed;
			completed = result;
		}
		pthread_mutex_unlock(&lock);

		safe_free(&job->client_id);
		safe_free(&job->username);
		safe_free(&job->password);
		safe_free(&job->will_topic);
		free(job);
	}
	return NULL;
}

/* ------------------------------------------------------------------ */
/* plugin callbacks                                                   */
/* ------------------------------------------------------------------ */

static enum principal_kind kind_from_username(const char *username)
{
	if (!username) {
		return KIND_NONE;
	}
	if (strncmp(username, "host:", 5) == 0) {
		return KIND_HOST;
	}
	if (strncmp(username, "device:", 7) == 0 || strncmp(username, "session:", 8) == 0) {
		return KIND_DEVICE;
	}
	if (strncmp(username, "probe:", 6) == 0) {
		return KIND_PROBE;
	}
	return KIND_NONE;
}

static int basic_auth_callback(int event, void *event_data, void *userdata)
{
	struct mosquitto_evt_basic_auth *ed = event_data;
	const char *client_id;
	const char *username;
	const char *password;
	struct auth_job *job;
	enum principal_kind local_kind;

	UNUSED(event);
	UNUSED(userdata);

	if (!ed || !ed->client) {
		return MOSQ_ERR_AUTH;
	}
	client_id = mosquitto_client_id(ed->client);
	username = mosquitto_client_username(ed->client);
	password = ed->password;
	if (!client_id || !username) {
		return MOSQ_ERR_AUTH;
	}

	/* The internal Access identity authenticates locally so startup and
	 * recovery never depend on the Access HTTP listener. */
	if (cfg.access_username && strcmp(username, cfg.access_username) == 0) {
		struct auth_result *result;
		struct acl_entry acls[16];
		size_t acl_count = 0;
		static const struct {
			int access;
			const char *filter;
		} access_acls[] = {
			{ACL_WRITE, "whalerein/v1/h/+/request"},
			{ACL_WRITE, "whalerein/v1/h/+/d/+/event"},
			{ACL_WRITE, "whalerein/v1/h/+/presence"},
			{ACL_WRITE, "$CONTROL/whalerein/v1"},
			{ACL_READ, "whalerein/v1/h/+/result"},
			{ACL_READ, "whalerein/v1/h/+/event"},
			{ACL_READ, "whalerein/v1/h/+/d/+/ack"},
			{ACL_READ, "whalerein/v1/presence/device/+"},
			{ACL_READ, "whalerein/v1/control/reply"},
			{ACL_READ, "whalerein/v1/control/events"},
		};
		size_t i;

		if (!cfg.access_password || !password || strcmp(password, cfg.access_password) != 0) {
			return MOSQ_ERR_AUTH;
		}
		result = calloc(1, sizeof(*result));
		if (!result) {
			return MOSQ_ERR_NOMEM;
		}
		result->client_id = dup_string(client_id);
		result->username = dup_string(username);
		result->kind = KIND_ACCESS;
		result->rc = MOSQ_ERR_SUCCESS;
		result->ready = true;
		result->queued_ms = now_ms();
		result->acls = calloc(sizeof(access_acls) / sizeof(access_acls[0]), sizeof(*result->acls));
		if (!result->acls || !result->client_id || !result->username) {
			result_free(result);
			return MOSQ_ERR_NOMEM;
		}
		for (i = 0; i < sizeof(access_acls) / sizeof(access_acls[0]); i++) {
			acls[acl_count].access = access_acls[i].access;
			acls[acl_count].generation = 0;
			acls[acl_count].host_id = NULL;
			acls[acl_count].filter = dup_string(access_acls[i].filter);
			if (!acls[acl_count].filter) {
				clear_parsed_acls(acls, acl_count);
				result_free(result);
				return MOSQ_ERR_NOMEM;
			}
			result->acls[i] = acls[acl_count];
			acl_count++;
		}
		result->acl_count = acl_count;
		pthread_mutex_lock(&lock);
		result->next = completed;
		completed = result;
		pthread_mutex_unlock(&lock);
		return MOSQ_ERR_AUTH_DELAYED;
	}

	local_kind = kind_from_username(username);
	if (local_kind == KIND_NONE) {
		return MOSQ_ERR_AUTH;
	}
	{
		bool stale;

		pthread_mutex_lock(&lock);
		stale = policy_confirmed && !policy_fresh_locked();
		pthread_mutex_unlock(&lock);
		if (stale) {
			/* Access has not confirmed policy within the validity window.
			 * Fail closed and report server-unavailable rather than
			 * not-authorized so the client does not misread this as revoked
			 * credentials. */
			mosquitto_log_printf(MOSQ_LOG_WARNING,
					     "whalerein: external CONNECT refused while policy is stale");
			return MOSQ_ERR_SERVER_UNAVAILABLE;
		}
	}
	job = calloc(1, sizeof(*job));
	if (!job) {
		return MOSQ_ERR_NOMEM;
	}
	job->client_id = dup_string(client_id);
	job->username = dup_string(username);
	job->password = dup_string(password ? password : "");
	if (!job->client_id || !job->username || !job->password) {
		safe_free(&job->client_id);
		safe_free(&job->username);
		safe_free(&job->password);
		free(job);
		return MOSQ_ERR_NOMEM;
	}
	pthread_mutex_lock(&lock);
	if (inflight >= MAX_INFLIGHT) {
		pthread_mutex_unlock(&lock);
		safe_free(&job->client_id);
		safe_free(&job->username);
		safe_free(&job->password);
		free(job);
		mosquitto_log_printf(MOSQ_LOG_WARNING, "whalerein: auth queue full");
		return MOSQ_ERR_AUTH;
	}
	inflight++;
	job->next = NULL;
	if (jobs_tail) {
		jobs_tail->next = job;
	} else {
		jobs_head = job;
	}
	jobs_tail = job;
	pthread_cond_signal(&jobs_cond);
	pthread_mutex_unlock(&lock);
	return MOSQ_ERR_AUTH_DELAYED;
}

static void install_policy_locked(struct wclient *client, struct auth_result *result)
{
	size_t i;

	client->kind = result->kind;
	snprintf(client->session_id, sizeof(client->session_id), "%s", result->session_id);
	client->session_revision = result->session_revision;
	client->session_deadline_ms = result->session_deadline_ms;
	safe_free(&client->host_id);
	safe_free(&client->device_id);
	client->host_id = dup_string(result->host_id);
	client->device_id = dup_string(result->device_id);
	client_clear_acls(client);
	for (i = 0; i < result->acl_count && i < MAX_ACL; i++) {
		if (!acl_still_valid_locked(client, &result->acls[i])) {
			continue;
		}
		client->acls[client->acl_count++] = result->acls[i];
		result->acls[i].filter = NULL;
		result->acls[i].host_id = NULL;
	}
	result->acl_count = 0;
	client->revision = result->revision;
	client->valid_until_ms = now_ms() + cfg.ttl_ms;
	if (result->revision > applied_revision) {
		applied_revision = result->revision;
	}
}

static int connect_callback(int event, void *event_data, void *userdata)
{
	struct mosquitto_evt_connect *ed = event_data;
	struct auth_result *result = NULL;
	struct auth_result **link;
	const char *client_id;
	struct wclient *client;
	uint64_t seq;

	UNUSED(event);
	UNUSED(userdata);

	if (!ed || !ed->client) {
		return MOSQ_ERR_SUCCESS;
	}
	client_id = mosquitto_client_id(ed->client);
	if (!client_id) {
		return MOSQ_ERR_SUCCESS;
	}
	pthread_mutex_lock(&lock);
	for (link = &completed; *link; link = &(*link)->next) {
		if ((*link)->completed_auth && strcmp((*link)->client_id, client_id) == 0) {
			result = *link;
			*link = result->next;
			break;
		}
	}
	if (!result) {
		mosquitto_log_printf(MOSQ_LOG_DEBUG, "whalerein: connect without completed auth for %s", client_id);
		pthread_mutex_unlock(&lock);
		return MOSQ_ERR_SUCCESS;
	}
	client = client_find_locked(client_id);
	if (!client) {
		client = calloc(1, sizeof(*client));
		if (!client) {
			pthread_mutex_unlock(&lock);
			result_free(result);
			return MOSQ_ERR_NOMEM;
		}
		client->client_id = dup_string(client_id);
		if (!client->client_id) {
			free(client);
			pthread_mutex_unlock(&lock);
			result_free(result);
			return MOSQ_ERR_NOMEM;
		}
		client->next = clients;
		clients = client;
	}
	safe_free(&client->username);
	client->username = dup_string(result->username ? result->username : mosquitto_client_username(ed->client));
	client->handle = ed->client;
	client->conn_id = next_conn_id++;
	client->seq = next_seq++;
	client->connected = true;
	client->connected_at_ms = now_ms();
	install_policy_locked(client, result);
	if (client->kind != KIND_ACCESS) {
		time_t deadline = now_ms() + cfg.ttl_ms;

		if (deadline > policy_fresh_until_ms) {
			policy_fresh_until_ms = deadline;
		}
	}
	seq = client->seq;
	publish_event_locked(seq, "connected", client, client->client_id,
			     client->kind == KIND_HOST ? "host" :
			     client->session_id[0] ? "session" :
			     client->kind == KIND_DEVICE ? "device" :
			     client->kind == KIND_PROBE ? "probe" : "access",
			     client->host_id, client->device_id, client->conn_id, 0);
	pthread_mutex_unlock(&lock);
	result_free(result);
	return MOSQ_ERR_SUCCESS;
}

static int disconnect_callback(int event, void *event_data, void *userdata)
{
	struct mosquitto_evt_disconnect *ed = event_data;
	const char *client_id;
	struct wclient *client;
	uint64_t seq;
	char kind[16];

	UNUSED(event);
	UNUSED(userdata);

	if (!ed || !ed->client) {
		return MOSQ_ERR_SUCCESS;
	}
	client_id = mosquitto_client_id(ed->client);
	if (!client_id) {
		return MOSQ_ERR_SUCCESS;
	}
	pthread_mutex_lock(&lock);
	client = client_find_locked(client_id);
	if (!client || !client->connected) {
		pthread_mutex_unlock(&lock);
		return MOSQ_ERR_SUCCESS;
	}
	/* A client id can be reconnected before the previous connection's
	 * disconnect callback runs. Only the current connection may change
	 * presence; the superseded one is already accounted for. */
	if (client->handle != ed->client) {
		pthread_mutex_unlock(&lock);
		return MOSQ_ERR_SUCCESS;
	}
	client->connected = false;
	client->seq = next_seq++;
	seq = client->seq;
	snprintf(kind, sizeof(kind), "%s",
		 client->kind == KIND_HOST ? "host" :
		 client->session_id[0] ? "session" :
		 client->kind == KIND_DEVICE ? "device" :
		 client->kind == KIND_PROBE ? "probe" : "access");
	publish_event_locked(seq, "disconnected", client, client_id, kind,
			     client->host_id, client->device_id, client->conn_id, ed->reason);
	pthread_mutex_unlock(&lock);
	return MOSQ_ERR_SUCCESS;
}

static int acl_check_callback(int event, void *event_data, void *userdata)
{
	struct mosquitto_evt_acl_check *ed = event_data;
	const char *client_id;
	const char *username;
	struct wclient *client;
	int access;
	int rc;

	UNUSED(event);
	UNUSED(userdata);

	if (!ed || !ed->client || !ed->topic) {
		return MOSQ_ERR_ACL_DENIED;
	}
	client_id = mosquitto_client_id(ed->client);
	username = mosquitto_client_username(ed->client);
	if (!client_id) {
		return MOSQ_ERR_ACL_DENIED;
	}
	switch (ed->access) {
	case MOSQ_ACL_WRITE:
		access = ACL_WRITE;
		break;
	case MOSQ_ACL_READ:
		access = ACL_READ;
		break;
	case MOSQ_ACL_SUBSCRIBE:
		access = ACL_SUBSCRIBE;
		break;
	case MOSQ_ACL_UNSUBSCRIBE:
		return MOSQ_ERR_SUCCESS;
	default:
		return MOSQ_ERR_ACL_DENIED;
	}
	pthread_mutex_lock(&lock);
	client = client_find_locked(client_id);
	if (!client && username && cfg.access_username && strcmp(username, cfg.access_username) == 0) {
		/* Access identity policy is fixed and does not need CONNECT state. */
		static const struct {
			int access;
			const char *filter;
		} access_acls[] = {
			{ACL_WRITE, "whalerein/v1/h/+/request"},
			{ACL_WRITE, "whalerein/v1/h/+/d/+/event"},
			{ACL_WRITE, "whalerein/v1/h/+/presence"},
			{ACL_WRITE, "$CONTROL/whalerein/v1"},
			{ACL_READ, "whalerein/v1/h/+/result"},
			{ACL_READ, "whalerein/v1/h/+/event"},
			{ACL_READ, "whalerein/v1/h/+/d/+/ack"},
			{ACL_READ, "whalerein/v1/presence/device/+"},
			{ACL_READ, "whalerein/v1/control/reply"},
			{ACL_READ, "whalerein/v1/control/events"},
		};
		size_t i;

		rc = MOSQ_ERR_ACL_DENIED;
		for (i = 0; i < sizeof(access_acls) / sizeof(access_acls[0]); i++) {
			int wanted = access == ACL_SUBSCRIBE ? (ACL_SUBSCRIBE | ACL_READ) : access;

			if (!(access_acls[i].access & wanted)) {
				continue;
			}
			if (access == ACL_SUBSCRIBE ? filter_covers(access_acls[i].filter, ed->topic)
			    : topic_matches(access_acls[i].filter, ed->topic)) {
				rc = MOSQ_ERR_SUCCESS;
				break;
			}
		}
		pthread_mutex_unlock(&lock);
		return rc;
	}
	rc = acl_decide_locked(client, access, ed->topic);
	pthread_mutex_unlock(&lock);
	return rc;
}

static int control_callback(int event, void *event_data, void *userdata)
{
	UNUSED(event);
	UNUSED(userdata);
	handle_control(event_data);
	return MOSQ_ERR_SUCCESS;
}

static void close_expired_session_clients(int64_t now)
{
	char clients_to_close[256][256];
	size_t count = 0;
	size_t index;
	struct wclient *client;
	struct session_fence **link;
	struct session_fence *fence;

	pthread_mutex_lock(&lock);
	for (link = &session_fences; (fence = *link) != NULL;) {
		if (fence->retained_until_ms <= now) {
			*link = fence->next;
			free(fence);
		} else {
			link = &fence->next;
		}
	}
	for (client = clients; client && count < 256; client = client->next) {
		if (client->connected && client->session_id[0] &&
		    !session_valid_locked(client->session_id, client->session_deadline_ms)) {
			snprintf(clients_to_close[count++], sizeof(clients_to_close[0]), "%s", client->client_id);
		}
	}
	pthread_mutex_unlock(&lock);
	for (index = 0; index < count; index++) {
		mosquitto_kick_client_by_clientid(clients_to_close[index], false);
	}
}

static int tick_callback(int event, void *event_data, void *userdata)
{
	struct mosquitto_evt_tick *ed = event_data;
	struct auth_result *result;
	struct wclient *client;
	struct wclient **link;
	time_t now;
	static int64_t next_session_scan_ms;

	UNUSED(event);
	UNUSED(userdata);

	now = now_ms();
	if (now >= next_session_scan_ms) {
		close_expired_session_clients(now);
		next_session_scan_ms = now + SESSION_SCAN_INTERVAL_MS;
	}
	{
		struct {
			char client_id[256];
			int rc;
		} batch[256];
		size_t batch_count = 0;
		size_t i;

		pthread_mutex_lock(&lock);
		for (result = completed; result && batch_count < 256; result = result->next) {
			int rc = result->rc;

			if (result->completed_auth) {
				continue;
			}
			if (rc == MOSQ_ERR_SUCCESS && (!result->ready || result->kind == KIND_NONE ||
			    !session_valid_locked(result->session_id, result->session_deadline_ms) ||
			    (result->username && strncmp(result->username, "session:", 8) == 0 &&
			     strcmp(result->session_id, result->username + 8) != 0))) {
				rc = MOSQ_ERR_AUTH;
			}
			if (inflight > 0) {
				inflight--;
			}
			result->completed_auth = true;
			snprintf(batch[batch_count].client_id, sizeof(batch[batch_count].client_id), "%s", result->client_id);
			batch[batch_count].rc = rc;
			batch_count++;
		}
		pthread_mutex_unlock(&lock);

		/* Completion runs the CONNECT callback synchronously, so it must not
		 * hold the plugin lock. The result stays in the list until that
		 * callback consumes it. */
		for (i = 0; i < batch_count; i++) {
			mosquitto_log_printf(MOSQ_LOG_DEBUG, "whalerein: completing auth for %s rc=%d",
					     batch[i].client_id, batch[i].rc);
			mosquitto_complete_basic_auth(batch[i].client_id, batch[i].rc);
		}
	}

	pthread_mutex_lock(&lock);
	/* Drop completions that never produced a CONNECT event. */
	{
		struct auth_result **result_link = &completed;

		while ((result = *result_link) != NULL) {
			if (result->completed_auth && now - result->queued_ms > 30000) {
				*result_link = result->next;
				result_free(result);
				continue;
			}
			result_link = &result->next;
		}
	}
	link = &clients;
	while ((client = *link) != NULL) {
		if (!client->connected && client->kind != KIND_ACCESS &&
		    now - client->connected_at_ms > WILL_GRACE_MS) {
			*link = client->next;
			client_clear_acls(client);
			safe_free(&client->client_id);
			safe_free(&client->username);
			safe_free(&client->host_id);
			safe_free(&client->device_id);
			free(client);
			continue;
		}
		link = &client->next;
	}
	if (ed) {
		ed->next_ms = 5;
	}
	pthread_mutex_unlock(&lock);
	return MOSQ_ERR_SUCCESS;
}

/* ------------------------------------------------------------------ */
/* plugin lifecycle                                                   */
/* ------------------------------------------------------------------ */

static char *read_secret_file(const char *path)
{
	FILE *file;
	char buffer[1024];
	size_t len;

	if (!path) {
		return NULL;
	}
	file = fopen(path, "r");
	if (!file) {
		mosquitto_log_printf(MOSQ_LOG_ERR, "whalerein: cannot open %s", path);
		return NULL;
	}
	if (!fgets(buffer, sizeof(buffer), file)) {
		fclose(file);
		return NULL;
	}
	fclose(file);
	len = strlen(buffer);
	while (len > 0 && (buffer[len - 1] == '\n' || buffer[len - 1] == '\r')) {
		buffer[--len] = '\0';
	}
	if (len == 0) {
		return NULL;
	}
	return dup_string(buffer);
}

static bool option_value(struct mosquitto_opt *opts, int count, const char *key, char **out)
{
	int i;

	for (i = 0; i < count; i++) {
		if (strcmp(opts[i].key, key) == 0) {
			*out = dup_string(opts[i].value);
			return *out != NULL;
		}
	}
	return false;
}

int mosquitto_plugin_init(mosquitto_plugin_id_t *identifier, void **user_data,
			  struct mosquitto_opt *opts, int opt_count)
{
	char *value = NULL;
	int rc;
	int i;

	UNUSED(user_data);

	mosq_pid = identifier;
	mosquitto_plugin_set_info(identifier, PLUGIN_NAME, PLUGIN_VERSION);

	memset(&cfg, 0, sizeof(cfg));
	cfg.timeout_ms = DEFAULT_TIMEOUT_MS;
	cfg.workers = DEFAULT_WORKERS;
	cfg.ttl_ms = DEFAULT_TTL_MS;
	if (!option_value(opts, opt_count, "access_username", &cfg.access_username)) {
		cfg.access_username = dup_string("access");
	}
	if (!option_value(opts, opt_count, "access_password_file", &value)) {
		mosquitto_log_printf(MOSQ_LOG_ERR, "whalerein: access_password_file is required");
		return MOSQ_ERR_INVAL;
	}
	cfg.access_password = read_secret_file(value);
	safe_free(&value);
	if (!cfg.access_password) {
		return MOSQ_ERR_INVAL;
	}
	if (!option_value(opts, opt_count, "access_auth_url", &cfg.auth_url)) {
		mosquitto_log_printf(MOSQ_LOG_ERR, "whalerein: access_auth_url is required");
		return MOSQ_ERR_INVAL;
	}
	if (option_value(opts, opt_count, "access_auth_token_file", &value)) {
		cfg.auth_token = read_secret_file(value);
		safe_free(&value);
		if (!cfg.auth_token) {
			return MOSQ_ERR_INVAL;
		}
	}
	if (option_value(opts, opt_count, "auth_timeout_ms", &value)) {
		cfg.timeout_ms = atoi(value);
		safe_free(&value);
	}
	if (option_value(opts, opt_count, "auth_workers", &value)) {
		cfg.workers = atoi(value);
		safe_free(&value);
	}
	if (option_value(opts, opt_count, "policy_ttl_ms", &value)) {
		cfg.ttl_ms = atoi(value);
		safe_free(&value);
	}
	if (cfg.timeout_ms < 50 || cfg.timeout_ms > 10000) {
		mosquitto_log_printf(MOSQ_LOG_ERR, "whalerein: auth_timeout_ms out of range");
		return MOSQ_ERR_INVAL;
	}
	if (cfg.workers < 1 || cfg.workers > DEFAULT_WORKERS) {
		cfg.workers = DEFAULT_WORKERS;
	}
	if (cfg.ttl_ms < 1000 || cfg.ttl_ms > MAX_TTL_MS) {
		mosquitto_log_printf(MOSQ_LOG_ERR, "whalerein: policy_ttl_ms out of range");
		return MOSQ_ERR_INVAL;
	}
	sock_timeout_ms = cfg.timeout_ms;
	policy_fresh_until_ms = now_ms() + cfg.ttl_ms;

	{
		pthread_attr_t attr;

		/* Authentication workers build bounded request/response buffers on
		 * their stack; musl's default thread stack is smaller than glibc's. */
		if (pthread_attr_init(&attr) != 0 ||
		    pthread_attr_setstacksize(&attr, 1024 * 1024) != 0) {
			mosquitto_log_printf(MOSQ_LOG_ERR, "whalerein: cannot size auth worker stack");
			return MOSQ_ERR_UNKNOWN;
		}
		for (i = 0; i < cfg.workers; i++) {
			if (pthread_create(&worker_ids[i], &attr, worker_main, NULL) != 0) {
				pthread_attr_destroy(&attr);
				mosquitto_log_printf(MOSQ_LOG_ERR, "whalerein: cannot start auth worker");
				return MOSQ_ERR_UNKNOWN;
			}
		}
		pthread_attr_destroy(&attr);
	}
	workers_started = true;

	rc = mosquitto_callback_register(mosq_pid, MOSQ_EVT_BASIC_AUTH, basic_auth_callback, NULL, NULL);
	if (rc) {
		return rc;
	}
	rc = mosquitto_callback_register(mosq_pid, MOSQ_EVT_ACL_CHECK, acl_check_callback, NULL, NULL);
	if (rc) {
		return rc;
	}
	rc = mosquitto_callback_register(mosq_pid, MOSQ_EVT_CONNECT, connect_callback, NULL, NULL);
	if (rc) {
		return rc;
	}
	rc = mosquitto_callback_register(mosq_pid, MOSQ_EVT_DISCONNECT, disconnect_callback, NULL, NULL);
	if (rc) {
		return rc;
	}
	rc = mosquitto_callback_register(mosq_pid, MOSQ_EVT_TICK, tick_callback, NULL, NULL);
	if (rc) {
		return rc;
	}
	rc = mosquitto_callback_register(mosq_pid, MOSQ_EVT_CONTROL, control_callback,
					 CONTROL_SUBSCRIBE_TOPIC, NULL);
	return rc;
}

int mosquitto_plugin_cleanup(void *user_data, struct mosquitto_opt *opts, int opt_count)
{
	UNUSED(user_data);
	UNUSED(opts);
	UNUSED(opt_count);

	if (workers_started) {
		int i;

		pthread_mutex_lock(&lock);
		stopping = true;
		pthread_cond_broadcast(&jobs_cond);
		pthread_mutex_unlock(&lock);
		for (i = 0; i < cfg.workers; i++) {
			pthread_join(worker_ids[i], NULL);
		}
		workers_started = false;
	}
	pthread_mutex_lock(&lock);
	while (jobs_head) {
		struct auth_job *job = jobs_head;

		jobs_head = job->next;
		safe_free(&job->client_id);
		safe_free(&job->username);
		safe_free(&job->password);
		safe_free(&job->will_topic);
		free(job);
	}
	while (completed) {
		struct auth_result *result = completed;

		completed = result->next;
		result_free(result);
	}
	while (fences) {
		struct fence *fence = fences;

		fences = fence->next;
		safe_free(&fence->device_id);
		safe_free(&fence->host_id);
		free(fence);
	}
	while (session_fences) {
		struct session_fence *fence = session_fences;

		session_fences = fence->next;
		free(fence);
	}
	clients_free_all_locked();
	pthread_mutex_unlock(&lock);

	safe_free(&cfg.access_username);
	safe_free(&cfg.access_password);
	safe_free(&cfg.auth_url);
	safe_free(&cfg.auth_token);
	return MOSQ_ERR_SUCCESS;
}
