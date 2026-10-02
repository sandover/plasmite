/*
 * Purpose: Persistent local C ABI benchmark worker for access comparisons.
 * Input: `append N` plus N compact JSON lines, or `read N` plus N sequence lines.
 * Output: One JSON result per request, with raw message envelopes for verification.
 */

#include "plasmite.h"

#include <errno.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <windows.h>
#else
#include <time.h>
#endif

static int fail(const char *label, plsm_error_t *err) {
    fprintf(stderr, "%s failed", label);
    if (err != NULL && err->message != NULL) fprintf(stderr, ": %s", err->message);
    fputc('\n', stderr);
    plsm_error_free(err);
    return 1;
}

static uint64_t frequency(void) {
#ifdef _WIN32
    LARGE_INTEGER value;
    if (!QueryPerformanceFrequency(&value)) return 0;
    return (uint64_t)value.QuadPart;
#else
    return UINT64_C(1000000000);
#endif
}

static uint64_t ticks(void) {
#ifdef _WIN32
    LARGE_INTEGER value;
    QueryPerformanceCounter(&value);
    return (uint64_t)value.QuadPart;
#else
    struct timespec value;
    clock_gettime(CLOCK_MONOTONIC, &value);
    return (uint64_t)value.tv_sec * UINT64_C(1000000000) + (uint64_t)value.tv_nsec;
#endif
}

static uint64_t to_ns(uint64_t value, uint64_t rate) {
    return (value / rate) * UINT64_C(1000000000) +
           ((value % rate) * UINT64_C(1000000000)) / rate;
}

static char *read_line(void) {
    size_t capacity = 256;
    size_t length = 0;
    char *line = (char *)malloc(capacity);
    int ch;
    if (line == NULL) return NULL;
    while ((ch = fgetc(stdin)) != EOF && ch != '\n') {
        if (length + 1 >= capacity) {
            size_t next = capacity * 2;
            char *grown;
            if (next <= capacity) {
                free(line);
                return NULL;
            }
            grown = (char *)realloc(line, next);
            if (grown == NULL) {
                free(line);
                return NULL;
            }
            line = grown;
            capacity = next;
        }
        line[length++] = (char)ch;
    }
    if (ch == EOF && length == 0) {
        free(line);
        return NULL;
    }
    if (length > 0 && line[length - 1] == '\r') length--;
    line[length] = '\0';
    return line;
}

static int parse_u64(const char *text, uint64_t *value) {
    char *end;
    unsigned long long parsed;
    if (text == NULL || *text < '0' || *text > '9') return 0;
    errno = 0;
    parsed = strtoull(text, &end, 10);
    if (errno != 0 || *end != '\0') return 0;
    *value = (uint64_t)parsed;
    return 1;
}

static int parse_count(const char *text, size_t *count) {
    uint64_t value;
    if (!parse_u64(text, &value) || value > SIZE_MAX / sizeof(plsm_buf_t)) return 0;
    *count = (size_t)value;
    return 1;
}

static void free_request(char **lines, plsm_buf_t *messages, uint64_t *latencies, size_t count) {
    size_t i;
    if (lines != NULL) for (i = 0; i < count; i++) free(lines[i]);
    if (messages != NULL) for (i = 0; i < count; i++) plsm_buf_free(&messages[i]);
    free(lines);
    free(messages);
    free(latencies);
}

static int run_request(const char *operation, size_t count, plsm_pool_t *pool, uint64_t rate) {
    char **lines = NULL;
    plsm_buf_t *messages = NULL;
    uint64_t *latencies = NULL;
    uint64_t started;
    size_t i;
    int result = 1;

    if (count != 0) {
        lines = (char **)calloc(count, sizeof(char *));
        messages = (plsm_buf_t *)calloc(count, sizeof(plsm_buf_t));
        latencies = (uint64_t *)calloc(count, sizeof(uint64_t));
        if (lines == NULL || messages == NULL || latencies == NULL) {
            fprintf(stderr, "out of memory for request\n");
            goto done;
        }
        for (i = 0; i < count; i++) {
            lines[i] = read_line();
            if (lines[i] == NULL) {
                fprintf(stderr, "unexpected end of input in %s request\n", operation);
                goto done;
            }
            if (strcmp(operation, "read") == 0) {
                uint64_t ignored;
                if (!parse_u64(lines[i], &ignored)) {
                    fprintf(stderr, "read input must be a sequence number\n");
                    goto done;
                }
            }
        }
    }

    started = ticks();
    for (i = 0; i < count; i++) {
        uint64_t call_started = ticks();
        plsm_error_t *err = NULL;
        int rc;
        if (strcmp(operation, "append") == 0) {
            rc = plsm_pool_append_json(pool, (const uint8_t *)lines[i], strlen(lines[i]),
                                       NULL, 0, 0, &messages[i], &err);
            if (rc != 0) {
                result = fail("plsm_pool_append_json", err);
                goto done;
            }
        } else {
            uint64_t seq;
            (void)parse_u64(lines[i], &seq);
            rc = plsm_pool_get_json(pool, seq, &messages[i], &err);
            if (rc != 0) {
                result = fail("plsm_pool_get_json", err);
                goto done;
            }
        }
        latencies[i] = to_ns(ticks() - call_started, rate);
    }
    {
        uint64_t elapsed_ns = to_ns(ticks() - started, rate);
        printf("{\"elapsed_ns\":%" PRIu64 ",\"latencies_ns\":[", elapsed_ns);
        for (i = 0; i < count; i++) printf("%s%" PRIu64, i == 0 ? "" : ",", latencies[i]);
        printf("],\"result\":{\"messages\":[");
        for (i = 0; i < count; i++) {
            if (i != 0) fputc(',', stdout);
            if (messages[i].data == NULL || messages[i].len == 0 ||
                fwrite(messages[i].data, 1, messages[i].len, stdout) != messages[i].len) {
                fprintf(stderr, "failed to write message envelope\n");
                goto done;
            }
        }
        printf("]}}\n");
        if (fflush(stdout) != 0 || ferror(stdout)) {
            fprintf(stderr, "failed to write or flush result\n");
            goto done;
        }
    }
    result = 0;

done:
    free_request(lines, messages, latencies, count);
    return result;
}

int main(int argc, char **argv) {
    plsm_client_t *client = NULL;
    plsm_pool_t *pool = NULL;
    plsm_error_t *err = NULL;
    uint64_t rate = frequency();
    int result = 0;

    if (argc != 3) {
        fprintf(stderr, "usage: %s POOL_DIR POOL\n", argv[0]);
        return 2;
    }
    if (rate == 0) {
        fprintf(stderr, "high-resolution clock unavailable\n");
        return 1;
    }
    if (plsm_client_new(argv[1], &client, &err) != 0) {
        plsm_client_free(client);
        return fail("plsm_client_new", err);
    }
    if (plsm_pool_open(client, argv[2], &pool, &err) != 0) {
        plsm_client_free(client);
        return fail("plsm_pool_open", err);
    }

    for (;;) {
        char *request = read_line();
        char *space;
        size_t count;
        if (request == NULL) break;
        space = strchr(request, ' ');
        if (space == NULL) {
            fprintf(stderr, "request must be `append N` or `read N`\n");
            free(request);
            result = 1;
            break;
        }
        *space++ = '\0';
        if ((strcmp(request, "append") != 0 && strcmp(request, "read") != 0) ||
            !parse_count(space, &count)) {
            fprintf(stderr, "request must be `append N` or `read N` with a valid count\n");
            free(request);
            result = 1;
            break;
        }
        result = run_request(request, count, pool, rate);
        free(request);
        if (result != 0) break;
    }

    plsm_pool_free(pool);
    plsm_client_free(client);
    return result;
}
