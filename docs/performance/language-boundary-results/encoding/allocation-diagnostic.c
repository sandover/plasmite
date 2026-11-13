/* macOS allocation diagnostic; not a timing benchmark. Link the pinned Lite3
 * archive and the candidate shim. Compare exact output after every strategy.
 */
#include "lite3_shim.h"
#include "yyjson/yyjson.h"
#include <assert.h>
#include <errno.h>
#include <malloc/malloc.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    assert(argc == 3);
    FILE *file = fopen(argv[1], "rb");
    assert(file);
    assert(fseek(file, 0, SEEK_END) == 0);
    size_t len = (size_t)ftell(file);
    rewind(file);
    char *json = malloc(len + 1);
    assert(json && fread(json, 1, len, file) == len);
    fclose(file);
    json[len] = 0;
    size_t cap = strtoull(argv[2], NULL, 10);
    size_t values = 0;
    yyjson_doc *probe = plasmite_lite3_json_parse(json, len, &values);
    assert(probe);
    size_t dom_bytes = malloc_size(probe) + malloc_size(probe->str_pool);
    plasmite_lite3_json_doc_free(probe);
    unsigned char *expected = NULL;
    size_t expected_len = 0;
    const char *names[] = {"legacy-2x", "legacy-4x", "parse-once-2x", "parse-once-values"};
    printf("{\"json_bytes\":%zu,\"values\":%zu,\"document_allocation_bytes\":%zu,\"cap\":%zu,\"strategies\":[", len, values, dom_bytes, cap);
    for (int mode = 0; mode < 4; mode++) {
        size_t size = mode == 3 ? len + values * 32 + 64 : len * (mode == 1 ? 4 : 2);
        if (size < 256) size = 256;
        if (size > cap) size = cap;
        void *parsed = mode >= 2 ? plasmite_lite3_json_parse(json, len, &values) : NULL;
        size_t attempts = 0, requested = 0, allocated = 0, out_len = 0;
        unsigned char *out;
        printf("%s{\"name\":\"%s\",\"attempt_sizes\":[", mode ? "," : "", names[mode]);
        for (;;) {
            out = calloc(1, size);
            assert(out);
            requested += size;
            allocated += malloc_size(out);
            printf("%s%zu", attempts++ ? "," : "", size);
            int ret = parsed ? plasmite_lite3_json_build(parsed, out, &out_len, size)
                             : plasmite_lite3_json_dec(json, len, out, &out_len, size);
            if (ret == 0) break;
            assert(errno == ENOBUFS && size < cap);
            free(out);
            size = size * 2 > cap ? cap : size * 2;
        }
        if (mode == 0) {
            expected = malloc(out_len);
            assert(expected);
            memcpy(expected, out, out_len);
            expected_len = out_len;
        } else assert(out_len == expected_len && memcmp(out, expected, out_len) == 0);
        printf("],\"parses\":%zu,\"output_requested_bytes\":%zu,\"output_allocated_bytes\":%zu,\"final_capacity\":%zu,\"final_allocation_bytes\":%zu,\"encoded_bytes\":%zu}",
               parsed ? 1 : attempts, requested, allocated, size, malloc_size(out), out_len);
        if (parsed) plasmite_lite3_json_doc_free(parsed);
        free(out);
    }
    puts("]}");
    free(expected);
    free(json);
}
