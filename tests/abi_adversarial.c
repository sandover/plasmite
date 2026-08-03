#include "plasmite.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>

static int failures = 0;

#define CHECK(condition, label)                                               \
    do {                                                                      \
        if (!(condition)) {                                                   \
            fprintf(stderr, "abi adversarial check failed: %s\n", (label)); \
            failures += 1;                                                    \
        }                                                                     \
    } while (0)

static void require_usage_error(int rc, plsm_error_t **err, const char *label) {
    CHECK(rc != 0, label);
    CHECK(*err != NULL, "validation failure returns an error");
    if (*err != NULL) {
        CHECK((*err)->kind == PLSM_ERROR_USAGE, "validation failure is Usage");
        plsm_error_free(*err);
        *err = NULL;
    }
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s <pool_dir>\n", argv[0]);
        return 2;
    }

    /* Documented no-op cleanup inputs. */
    plsm_client_free(NULL);
    plsm_pool_free(NULL);
    plsm_stream_free(NULL);
    plsm_lite3_stream_free(NULL);
    plsm_buf_free(NULL);
    plsm_lite3_frame_free(NULL);
    plsm_error_free(NULL);

    /* Returned value structs are reset, so repeated cleanup is supported. */
    plsm_buf_t empty_buf = {0};
    plsm_buf_free(&empty_buf);
    plsm_buf_free(&empty_buf);
    plsm_lite3_frame_t empty_frame = {0};
    plsm_lite3_frame_free(&empty_frame);
    plsm_lite3_frame_free(&empty_frame);

    plsm_error_t *err = NULL;
    require_usage_error(
        plsm_client_new(argv[1], NULL, &err), &err, "null client output");
    require_usage_error(
        plsm_pool_open(NULL, "missing", NULL, &err), &err, "null client handle");

    plsm_client_t *client = NULL;
    CHECK(plsm_client_new(argv[1], &client, &err) == 0, "create client");
    CHECK(client != NULL, "client output initialized");

    plsm_pool_t *pool = NULL;
    CHECK(plsm_pool_create(client, "abi-adversarial", 1024 * 1024, &pool, &err) == 0,
          "create pool");
    CHECK(pool != NULL, "pool output initialized");

    plsm_buf_t message = {0};
    require_usage_error(
        plsm_pool_append_json(pool, NULL, 1, NULL, 0, 0, &message, &err),
        &err,
        "null JSON bytes");

    const uint8_t invalid_json[] = "{";
    require_usage_error(
        plsm_pool_append_json(pool,
                              invalid_json,
                              sizeof(invalid_json) - 1,
                              NULL,
                              0,
                              0,
                              &message,
                              &err),
        &err,
        "invalid JSON bytes");

    const uint8_t json[] = "{\"kind\":\"abi-adversarial\"}";
    CHECK(plsm_pool_append_json(pool,
                                json,
                                sizeof(json) - 1,
                                NULL,
                                0,
                                1,
                                &message,
                                &err) == 0,
          "append flushed JSON");
    CHECK(message.data != NULL && message.len != 0, "message output populated");
    plsm_buf_free(&message);
    CHECK(message.data == NULL && message.len == 0, "message cleanup resets output");
    plsm_buf_free(&message);

    uint64_t seq = 0;
    require_usage_error(
        plsm_pool_append_lite3(pool, NULL, 1, 0, &seq, &err),
        &err,
        "null Lite3 bytes");
    require_usage_error(
        plsm_pool_append_lite3(pool, json, sizeof(json) - 1, 99, &seq, &err),
        &err,
        "invalid durability");

    plsm_stream_t *stream = NULL;
    require_usage_error(
        plsm_stream_open_ex(pool, NULL, &stream, &err), &err, "null stream options");

    plsm_stream_options_t options = {0};
    options.struct_size = sizeof(options) - 1;
    require_usage_error(
        plsm_stream_open_ex(pool, &options, &stream, &err),
        &err,
        "undersized stream options");

    options = (plsm_stream_options_t){0};
    options.struct_size = sizeof(options);
    options.has_since = 1;
    options.since_seq = 1;
    options.has_max = 1;
    options.max_messages = 1;
    CHECK(plsm_stream_open_ex(pool, &options, &stream, &err) == 0, "open stream");
    CHECK(stream != NULL, "stream output initialized");
    CHECK(plsm_stream_next(stream, &message, &err) == 1, "read stream message");
    plsm_buf_free(&message);
    plsm_stream_free(stream);

    plsm_pool_free(pool);
    plsm_client_free(client);
    if (err != NULL) {
        plsm_error_free(err);
    }
    return failures == 0 ? 0 : 1;
}
