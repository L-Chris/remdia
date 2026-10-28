#ifndef REMDIA_H
#define REMDIA_H
#include <stdint.h>
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
#define REMDIA_ABI_MAJOR 2
#define REMDIA_ABI_MINOR 2
#define REMDIA_OK 0
#define REMDIA_EOF 1
#define REMDIA_NEED_INPUT 2
#define REMDIA_WOULD_BLOCK 3
#define REMDIA_RECONFIGURED 4
#define REMDIA_ERROR (-1)
#define REMDIA_PANIC (-2)
#define REMDIA_INTERRUPTED (-3)
typedef struct { uint32_t struct_size; uint16_t major, minor; } RemdiaHeader;
typedef struct { const uint8_t *ptr; size_t len; } RemdiaView;
typedef struct { void *context; uint8_t (*cancelled)(void *); } RemdiaControl;
typedef struct {
    RemdiaHeader header;
    RemdiaView json;
    RemdiaView planes[4];
    void *owner;
    void (*release)(void *);
} RemdiaOutput;
typedef struct {
    RemdiaHeader header;
    RemdiaView json;
    RemdiaView planes[4];
    const void *owner;
    void (*retain)(const void *);
    void (*release)(const void *);
} RemdiaInput;
typedef struct {
    RemdiaHeader header;
    int32_t (*describe)(RemdiaOutput *);
    int32_t (*create)(RemdiaView, RemdiaControl, void **, RemdiaOutput *);
    int32_t (*submit)(void *, const RemdiaInput *, RemdiaOutput *);
    int32_t (*receive)(void *, RemdiaOutput *);
    int32_t (*drain)(void *, RemdiaOutput *);
    int32_t (*reset)(void *, uint64_t, int64_t, RemdiaOutput *);
    int32_t (*command)(void *, RemdiaView, RemdiaOutput *);
    void (*close)(void *);
} RemdiaPluginApi;
/* Locate dynamically by name; the header size is sizeof(RemdiaPluginApi). */
typedef int32_t (*RemdiaQueryV2)(RemdiaHeader, RemdiaPluginApi *);
int32_t remdia_plugin_query_v2(RemdiaHeader host, RemdiaPluginApi *out);
#ifdef __cplusplus
}
#endif
#endif
