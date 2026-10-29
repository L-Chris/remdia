/* Exercise the complete engine in a single SDK library from an independent C host. */
#define _CRT_SECURE_NO_WARNINGS
#include "remdia_sdk.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
#define SYMBOL(name) GetProcAddress(library, name)
#else
#include <dlfcn.h>
#define SYMBOL(name) dlsym(library, name)
#endif
static uint8_t cancelled(void *context) { return *(uint8_t *)context; }
static void release(RemdiaOutput *out) {
    if (out->release) out->release(out->owner);
    memset(out, 0, sizeof(*out));
}
static RemdiaView read_file(const char *path) {
    FILE *file = fopen(path, "rb"); assert(file);
    assert(fseek(file, 0, SEEK_END) == 0); long size = ftell(file);
    assert(size > 0 && size < 1024 * 1024); rewind(file);
    uint8_t *bytes = malloc((size_t)size); assert(bytes);
    assert(fread(bytes, 1, (size_t)size, file) == (size_t)size); fclose(file);
    RemdiaView view = {bytes, (size_t)size}; return view;
}
int main(int argc, char **argv) {
    assert(argc == 5);
#ifdef _WIN32
    HMODULE library = LoadLibraryA(argv[1]);
#else
    void *library = dlopen(argv[1], RTLD_NOW);
#endif
    assert(library);
    typedef int32_t (*Open)(RemdiaView, RemdiaView, RemdiaControl, void **, RemdiaOutput *);
    typedef int32_t (*Read)(void *, RemdiaOutput *);
    typedef int32_t (*Seek)(void *, int64_t, RemdiaOutput *);
    Open open_job = (Open)SYMBOL("remdia_sdk_open");
    Read read_job = (Read)SYMBOL("remdia_sdk_read");
    Read close_job = (Read)SYMBOL("remdia_sdk_close");
    Seek seek_job = (Seek)SYMBOL("remdia_sdk_seek");
    assert(open_job && read_job && close_job && seek_job);
    RemdiaView config = read_file(argv[2]), empty = {NULL, 0};
    uint8_t stop = 0; RemdiaControl control = {&stop, cancelled};
    void *job = NULL; RemdiaOutput out = {0}, held = {0};
    assert(open_job(config, empty, control, &job, &out) == REMDIA_OK);
    assert(job && out.json.len > 0); free((void *)config.ptr); release(&out);
    assert(read_job(job, &held) == REMDIA_OK);
    assert(held.planes[0].len >= 4); uint8_t snapshot[4]; memcpy(snapshot, held.planes[0].ptr, 4);
    int frames = 1, result;
    while ((result = read_job(job, &out)) == REMDIA_OK) { frames++; release(&out); }
    assert(result == REMDIA_EOF && frames > 1); release(&out);
    assert(seek_job(job, 500000, &out) == REMDIA_OK); release(&out);
    assert(read_job(job, &out) == REMDIA_OK && out.planes[0].len > 0); release(&out);
    stop = 1; assert(read_job(job, &out) < 0); release(&out); stop = 0;
    assert(close_job(job, &out) == REMDIA_OK); release(&out);
    assert(memcmp(snapshot, held.planes[0].ptr, 4) == 0); release(&held);
    config = read_file(argv[3]); RemdiaView pixels = read_file(argv[4]); job = NULL;
    assert(open_job(config, pixels, control, &job, &out) == REMDIA_OK); release(&out);
    free((void *)config.ptr); free((void *)pixels.ptr);
    assert(read_job(job, &held) == REMDIA_OK && held.planes[0].len > 0);
    assert(close_job(job, &out) == REMDIA_OK); release(&out);
    assert(held.planes[0].ptr[0] || held.planes[0].len > 0); release(&held);
    puts("Single SDK C host: audio/EOF/seek/cancellation/held planes and image graph passed");
    return 0;
}
