#define PLSM_INTERNAL_ABI
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

    CHECK(plsm_abi_major() == 2, "load ABI major 2");
    plsm_error_t *err = NULL;
    require_usage_error(
        plsm_client_new(argv[1], NULL, &err), &err, "null client output");
    require_usage_error(
        plsm_pool_open(NULL, "missing", 0, NULL, &err), &err, "null client handle");

    plsm_client_t *client = NULL;
    CHECK(plsm_client_new(argv[1], &client, &err) == 0, "create client");
    CHECK(client != NULL, "client output initialized");

    require_usage_error(
        plsm_pool_create(client, "null-output", 1024 * 1024, NULL, &err),
        &err, "null create output");
    plsm_pool_t *unexpected = NULL;
    CHECK(plsm_pool_open(client, "null-output", 0, &unexpected, &err) != 0,
          "invalid create leaves no pool");
    if (err != NULL) { plsm_error_free(err); err = NULL; }
    plsm_pool_free(unexpected);

    plsm_pool_t *pool = NULL;
    CHECK(plsm_pool_create(client, "abi-adversarial", 1024 * 1024, &pool, &err) == 0,
          "create pool");
    CHECK(pool != NULL, "pool output initialized");

    plsm_buf_t message = {0};
    require_usage_error(
        plsm_pool_feed_json(pool, NULL, 1, NULL, &message, &err),
        &err,
        "null JSON bytes");

    const uint8_t invalid_json[] = "{";
    require_usage_error(
        plsm_pool_feed_json(pool,
                              invalid_json,
                              sizeof(invalid_json) - 1,
                              NULL,
                              &message,
                              &err),
        &err,
        "invalid JSON bytes");

    const uint8_t json[] = "{\"kind\":\"abi-adversarial\"}";
    require_usage_error(
        plsm_pool_feed_json(pool, json, sizeof(json) - 1, NULL, NULL, &err),
        &err, "null append output");
    const plsm_feed_options_t flush = { .struct_size = sizeof(flush), .durability = 1 };
    CHECK(plsm_pool_feed_json(pool,
                                json,
                                sizeof(json) - 1,
                                &flush,
                                &message,
                                &err) == 0,
          "append flushed JSON");
    CHECK(message.data != NULL && message.len != 0, "message output populated");
    CHECK(message.len >= 9 && memcmp(message.data, "{\"seq\":1,", 9) == 0,
          "invalid append consumes no sequence");
    plsm_buf_free(&message);
    CHECK(message.data == NULL && message.len == 0, "message cleanup resets output");
    plsm_buf_free(&message);

    plsm_feed_receipt_t receipt = {0};
    const plsm_feed_options_t invalid_feed = {
        .struct_size = sizeof(invalid_feed),
        .durability = 99,
    };
    require_usage_error(
        plsm_pool_feed_lite3_receipt(pool, NULL, 1, NULL, &receipt, &err),
        &err,
        "null Lite3 bytes");
    require_usage_error(
        plsm_pool_feed_lite3_receipt(pool, json, sizeof(json) - 1, &invalid_feed, &receipt, &err),
        &err,
        "invalid durability");

    plsm_stream_t *stream = NULL;
    CHECK(plsm_pool_fetch(pool, NULL, &stream, &err) == 0, "open reader");
    CHECK(stream != NULL, "stream output initialized");
    require_usage_error(plsm_stream_next_wait(stream, 0, NULL, &err), &err, "null stream output");
    CHECK(plsm_stream_next_wait(stream, 0, &message, &err) == 1,
          "invalid stream read leaves first message available");
    plsm_buf_free(&message);
    plsm_stream_free(stream);

    plsm_lite3_stream_t *lite3_stream = NULL;
    CHECK(plsm_pool_fetch_lite3(pool, NULL, &lite3_stream, &err) == 0,
          "open Lite3 stream");
    require_usage_error(plsm_lite3_stream_next_wait(lite3_stream, 0, NULL, &err), &err,
                        "null Lite3 stream output");
    CHECK(plsm_lite3_stream_next_wait(lite3_stream, 0, &empty_frame, &err) == 1,
          "invalid Lite3 stream read leaves first message available");
    CHECK(empty_frame.seq == 1, "Lite3 stream starts at first message");
    plsm_lite3_frame_free(&empty_frame);
    plsm_lite3_stream_free(lite3_stream);

    /* The v2 feed and read helpers share the native selection contract. */
    const plsm_text_t tags[] = {
        {(const uint8_t *)"gfx", 3},
        {(const uint8_t *)"scope:sharedCanvas", 18},
    };
    plsm_read_options_t read = {0};
    read.struct_size = sizeof(read) - 1;
    stream = NULL;
    require_usage_error(
        plsm_pool_fetch(pool, &read, &stream, &err), &err,
        "undersized v2 read options");
    CHECK(stream == NULL, "rejected read leaves output untouched");

    read.struct_size = sizeof(read);
    read.tags = tags;
    read.tags_len = 2;
    CHECK(plsm_pool_follow(pool, &read, &stream, &err) == 0,
          "open v2 follow before feeding");
    plsm_feed_options_t feed = {0};
    feed.struct_size = sizeof(feed);
    feed.tags = tags;
    feed.tags_len = 2;
    const uint8_t scalar[] = "true";
    require_usage_error(
        plsm_pool_feed_json(pool, scalar, sizeof(scalar) - 1, &feed, &message, &err),
        &err, "feed rejects non-object");
    CHECK(plsm_pool_feed_json(pool, json, sizeof(json) - 1, &feed, &message, &err) == 0,
          "feed tagged message");
    plsm_buf_free(&message);
    CHECK(plsm_stream_next_wait(stream, UINT64_MAX, &message, &err) == 1,
          "follow receives message fed after open");
    CHECK(message.len >= 9 && memcmp(message.data, "{\"seq\":2,", 9) == 0,
          "follow skips old history");
    plsm_buf_free(&message);
    plsm_stream_cancel(stream);
    CHECK(plsm_stream_next_wait(stream, UINT64_MAX, &message, &err) == 3,
          "v2 follow can cancel");
    plsm_stream_free(stream);

    lite3_stream = NULL;
    CHECK(plsm_pool_fetch_lite3(pool, &read, &lite3_stream, &err) == 0,
          "open finite tagged read");
    CHECK(plsm_lite3_stream_next_wait(lite3_stream, UINT64_MAX, &empty_frame, &err) == 1,
          "v2 fetch returns tagged history");
    CHECK(empty_frame.seq == 2, "v2 fetch applies tags");
    const uint64_t last_time = empty_frame.time;
    plsm_lite3_frame_free(&empty_frame);
    CHECK(plsm_lite3_stream_next_wait(lite3_stream, 100, &empty_frame, &err) == 2,
          "v2 fetch ends at opening boundary");
    plsm_lite3_stream_free(lite3_stream);

    read.tags_len = 0;
    read.start_kind = PLSM_READ_LAST;
    read.start_value = 1;
    CHECK(plsm_pool_fetch_lite3(pool, &read, &lite3_stream, &err) == 0,
          "open at the last message");
    CHECK(plsm_lite3_stream_next_wait(lite3_stream, 0, &empty_frame, &err) == 1 &&
          empty_frame.seq == 2, "last start selects retained history");

    read.start_kind = PLSM_READ_TIME;
    read.start_value = last_time;
    CHECK(plsm_pool_fetch(pool, &read, &stream, &err) == 0, "open at a time");
    CHECK(plsm_stream_next_wait(stream, 0, &message, &err) == 1 &&
          message.len >= 9 && memcmp(message.data, "{\"seq\":2,", 9) == 0,
          "time start includes the selected message");
    plsm_buf_free(&message);
    plsm_stream_free(stream);

    feed.tags_len = 0;
    CHECK(plsm_pool_feed_lite3_receipt(pool, empty_frame.payload.data,
          empty_frame.payload.len, &feed, &receipt, &err) == 0 && receipt.seq == 3,
          "a nonnull tag pointer with zero tags allows forwarding");
    plsm_lite3_frame_free(&empty_frame);
    plsm_lite3_stream_free(lite3_stream);
    read.tags_len = 2;

    plsm_pool_t *small = NULL;
    CHECK(plsm_pool_create(client, "short-history", 16384, &small, &err) == 0,
          "create short-history pool");
    CHECK(plsm_pool_follow(small, NULL, &stream, &err) == 0, "open live JSON reader");
    CHECK(plsm_pool_follow_lite3(small, NULL, &lite3_stream, &err) == 0,
          "open live Lite3 reader");
    plsm_stream_t *views = NULL;
    CHECK(plsm_pool_follow(small, NULL, &views, &err) == 0, "open live view reader");
    char large[1024] = "{\"padding\":\"";
    memset(large + 12, 'x', sizeof(large) - 14);
    large[sizeof(large) - 2] = '"';
    large[sizeof(large) - 1] = '}';
    for (int i = 0; i < 32; i++) {
        CHECK(plsm_pool_feed_json(small, (const uint8_t *)large, sizeof(large),
                                  NULL, &message, &err) == 0,
              "feed until the first record is overwritten");
        plsm_buf_free(&message);
    }
    size_t rejected_count = 99;
    CHECK(plsm_view_stream_next(views, 0, 1, SIZE_MAX, NULL, &rejected_count, &err) == -1 &&
          err && err->kind == PLSM_ERROR_USAGE && rejected_count == 0,
          "invalid view output reports no initialized owners");
    plsm_error_free(err);
    err = NULL;
    for (int attempt = 0; attempt < 2; ++attempt) {
        const plsm_message_view_t *view = NULL;
        size_t count = 0;
        CHECK(plsm_stream_next_wait(stream, 0, &message, &err) == -1 &&
              err && err->kind == PLSM_ERROR_RETENTION_GAP && err->missing == 1 &&
              err->next_seq > 1, "JSON reader keeps reporting the gap");
        plsm_error_free(err);
        err = NULL;
        CHECK(plsm_lite3_stream_next_wait(lite3_stream, 0, &empty_frame, &err) == -1 &&
              err && err->kind == PLSM_ERROR_RETENTION_GAP && err->missing == 1 &&
              err->next_seq > 1, "Lite3 reader keeps reporting the gap");
        plsm_error_free(err);
        err = NULL;
        CHECK(plsm_view_stream_next(views, 0, 1, SIZE_MAX, &view, &count, &err) == -1 &&
              err && err->kind == PLSM_ERROR_RETENTION_GAP && err->missing == 1 &&
              err->next_seq > 1 && count == 0, "view reader keeps reporting the gap");
        plsm_error_free(err);
        err = NULL;
    }
    plsm_stream_free(stream);
    plsm_lite3_stream_free(lite3_stream);
    plsm_stream_free(views);
    read.start_kind = PLSM_READ_SEQUENCE;
    read.start_value = 1;
    stream = NULL;
    CHECK(plsm_pool_fetch(small, &read, &stream, &err) != 0,
          "filtered fetch of an expired sequence fails");
    CHECK(stream == NULL, "a failed open returns no stream");
    CHECK(err != NULL && err->kind == PLSM_ERROR_RETENTION_GAP &&
          err->missing == 1 && err->next_seq > 1, "retention gap carries both positions");
    if (err != NULL) { plsm_error_free(err); err = NULL; }
    plsm_pool_free(small);

    plsm_pool_t *created = NULL;
    require_usage_error(plsm_pool_open(client, "explicit-create", 2, &created, &err),
                        &err, "reject non-boolean create");
    CHECK(plsm_pool_open(client, "explicit-create", 0, &created, &err) != 0,
          "open without create requires an existing pool");
    if (err != NULL) { plsm_error_free(err); err = NULL; }
    CHECK(plsm_pool_open(client, "explicit-create", 1, &created, &err) == 0,
          "open can create at the default size");
    plsm_pool_free(created);

    plsm_pool_free(pool);
    plsm_client_free(client);
    if (err != NULL) {
        plsm_error_free(err);
    }
    return failures == 0 ? 0 : 1;
}
