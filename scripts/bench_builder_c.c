/* Matched native-C values -> committed view, using direct builder or JSON text.
 * Build against a fresh release library; run through bench_builder.py for repeats.
 */
#define _POSIX_C_SOURCE 200809L
#define PLSM_INTERNAL_ABI
#include "plasmite.h"
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
static plsm_error_t *error;
static void check(int code) { if (code) { fprintf(stderr, "native benchmark call failed\n"); if(error) plsm_error_free(error); exit(1); } }
static uint64_t now(void) { struct timespec t; if(clock_gettime(CLOCK_MONOTONIC,&t)) abort(); return (uint64_t)t.tv_sec*1000000000ULL+t.tv_nsec; }
static size_t put(plsm_builder_t *b,size_t p,const char *key,plsm_view_value_t value) {
    size_t child=0;check(plsm_builder_put(b,p,(const uint8_t *)key,key?strlen(key):0,&value,&child,&error));return child;
}
static plsm_view_value_t read_field(plsm_value_ref_t data,const char *key) {
    plsm_value_ref_t ref;plsm_view_value_t value;
    check(plsm_value_get(data,(const uint8_t *)key,strlen(key),&ref,&error));
    check(plsm_value_read(ref,&value,&error));return value;
}
int main(int argc,char **argv) {
    if(argc!=5) return 2;
    int native=!strcmp(argv[2],"native"),wide=!strcmp(argv[3],"wide"),large=!strcmp(argv[3],"large");
    size_t n=strtoull(argv[4],NULL,10);if(!n) return 2;
    size_t length=large?65536:16;
    char *note=malloc(length+1),*json=malloc(length+16384);if(!note||!json) abort();memset(note,'x',length);note[length]=0;
    char keys[200][16];for(size_t j=0;j<200;j++) snprintf(keys[j],sizeof(keys[j]),"field%zu",j);
    plsm_client_t *client=NULL;plsm_pool_t *pool=NULL;check(plsm_client_new(argv[1],&client,&error));check(plsm_pool_create(client,"events",8*1024*1024,&pool,&error));
    plsm_text_t text={(const uint8_t *)"sensor",6};
    plsm_feed_options_t options={sizeof(options),0,&text,1};
    uint64_t start=0,producer_ns=0; clock_t cpu_start=0; size_t initial_node=0;
    for(size_t i=0;i<n+100;i++) {
        if(i==100) { start=now(); cpu_start=clock(); }
        const plsm_message_view_t *view=NULL;
        uint64_t operation_start=now();
        if(native) {
            plsm_builder_t *builder=NULL;size_t root=0;check(plsm_builder_new(&text,1,&builder,&root,&error));if(i==0)initial_node=root;
            put(builder,root,"count",(plsm_view_value_t){.kind=3,.integer=(int64_t)i});
            put(builder,root,"power",(plsm_view_value_t){.kind=4,.real=1.25});
            put(builder,root,"ok",(plsm_view_value_t){.kind=2,.integer=1});
            put(builder,root,"extra",(plsm_view_value_t){.kind=1});
            size_t array=put(builder,root,"samples",(plsm_view_value_t){.kind=8});
            for(int64_t j=1;j<=3;j++) put(builder,array,NULL,(plsm_view_value_t){.kind=3,.integer=j});
            put(builder,root,"note",(plsm_view_value_t){.kind=5,.bytes=(const uint8_t *)note,.len=length});
            if(wide) for(size_t j=0;j<200;j++) put(builder,root,keys[j],(plsm_view_value_t){.kind=3,.integer=(int64_t)j});
            check(plsm_pool_feed_builder_view(pool,builder,NULL,&view,&error));plsm_builder_free(builder);
        } else {
            size_t used=(size_t)snprintf(json,length+16384,"{\"count\":%zu,\"power\":1.25,\"ok\":true,\"extra\":null,\"samples\":[1,2,3],\"note\":\"%s\"",i,note);
            if(wide) for(size_t j=0;j<200;j++) used+=(size_t)snprintf(json+used,length+16384-used,",\"%s\":%zu",keys[j],j);
            json[used++]='}';
            check(plsm_pool_feed_json_view(pool,(const uint8_t *)json,used,&options,&view,&error));
        }
        uint64_t operation_elapsed=now()-operation_start;
        if(i>=100) producer_ns+=operation_elapsed;
        /* The same complete consumer checks run outside producer timing. */
        if(plsm_view_seq(view)!=i+1) abort();
        plsm_value_ref_t data=plsm_view_data(view),field;plsm_view_value_t value;
        check(plsm_value_get(data,(const uint8_t *)"count",5,&field,&error));check(plsm_value_read(field,&value,&error));if(value.kind!=3||value.integer!=(int64_t)i) abort();
        check(plsm_value_get(data,(const uint8_t *)"note",4,&field,&error));check(plsm_value_read(field,&value,&error));if(value.kind!=5||value.len!=length||memcmp(value.bytes,note,length)) abort();
        value=read_field(data,"power");if(value.kind!=4||value.real!=1.25) abort();
        value=read_field(data,"ok");if(value.kind!=2||value.integer!=1) abort();
        value=read_field(data,"extra");if(value.kind!=1) abort();
        plsm_value_ref_t samples;check(plsm_value_get(data,(const uint8_t *)"samples",7,&samples,&error));
        check(plsm_value_read(samples,&value,&error));if(value.kind!=8||value.len!=3) abort();
        for(uint32_t j=0;j<3;j++) {plsm_value_ref_t item;check(plsm_value_index(samples,j,&item,&error));check(plsm_value_read(item,&value,&error));if(value.kind!=3||value.integer!=(int64_t)j+1)abort();}
        if(wide) for(size_t j=0;j<200;j++){value=read_field(data,keys[j]);if(value.kind!=3||value.integer!=(int64_t)j)abort();}
        plsm_view_release(view);
    }
    uint64_t elapsed=now()-start; double cpu_ns=(double)(clock()-cpu_start)*1000000000.0/CLOCKS_PER_SEC;
    printf("{\"mode\":\"%s\",\"shape\":\"%s\",\"iterations\":%zu,\"elapsed_ns\":%"PRIu64",\"producer_ns\":%"PRIu64",\"ns_per_op\":%.3f,\"complete_ns_per_op\":%.3f,\"complete_cpu_ns_per_op\":%.3f,\"initial_opaque_node\":%zu}\n",argv[2],argv[3],n,elapsed,producer_ns,(double)producer_ns/n,(double)elapsed/n,cpu_ns/n,initial_node);
    plsm_pool_free(pool);plsm_client_free(client);free(json);free(note);return 0;
}
