#ifndef REMDIA_H
#define REMDIA_H
#include <stdint.h>
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
#define REMDIA_ABI_MAJOR 1
#define REMDIA_OK 0
#define REMDIA_EOF 1
#define REMDIA_ERROR (-1)
#define REMDIA_PANIC (-2)
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
    int32_t (*open)(RemdiaView, RemdiaControl, void **, RemdiaOutput *);
    int32_t (*next)(void *, RemdiaOutput *);
    int32_t (*seek)(void *, int64_t, RemdiaOutput *);
    int32_t (*artwork)(void *, RemdiaOutput *);
    int32_t (*encode_mp3)(RemdiaView, RemdiaOutput *);
    void (*close)(void *);
} RemdiaPluginApi;
/* Locate dynamically by name; the header size is sizeof(RemdiaPluginApi). */
typedef int32_t (*RemdiaQueryV1)(RemdiaHeader, RemdiaPluginApi *);
int32_t remdia_plugin_query_v1(RemdiaHeader host, RemdiaPluginApi *out);
#ifdef __cplusplus
}
#endif
#endif
