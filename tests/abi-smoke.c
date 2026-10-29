/* An independently compiled C host verifies the public header against the DLL. */
#define _CRT_SECURE_NO_WARNINGS
#include "remdia.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
#else
#include <dlfcn.h>
#endif
static uint8_t cancelled(void *context) { (void)context; return 0; }
int main(int argc, char **argv) {
    assert(argc == 3);
#ifdef _WIN32
    HMODULE library = LoadLibraryA(argv[1]);
    assert(library);
    RemdiaQueryV2 query = (RemdiaQueryV2)GetProcAddress(library, "remdia_plugin_query_v2");
#else
    void *library = dlopen(argv[1], RTLD_NOW);
    if (!library) { fprintf(stderr, "%s\n", dlerror()); return 1; }
    RemdiaQueryV2 query = (RemdiaQueryV2)dlsym(library, "remdia_plugin_query_v2");
#endif
    assert(query);
    RemdiaPluginApi api;
    memset(&api, 0, sizeof(api));
    RemdiaHeader header = {sizeof(api), 99, 0};
    assert(query(header, &api) == REMDIA_ERROR);
    assert(api.header.struct_size == 0);
    header.major = REMDIA_ABI_MAJOR;
    header.struct_size = 8;
    assert(query(header, &api) == REMDIA_ERROR);
    header.struct_size = sizeof(api);
    assert(query(header, &api) == REMDIA_ERROR); /* old host minor */
    header.minor = REMDIA_ABI_MINOR - 1;
    assert(query(header, &api) == REMDIA_ERROR); /* pre-image schema host */
    header.minor = REMDIA_ABI_MINOR;
    assert(query(header, &api) == REMDIA_OK);
    assert(api.header.major == REMDIA_ABI_MAJOR);
    assert(api.header.struct_size == sizeof(api));
    FILE *file = fopen(argv[2], "rb");
    assert(file);
    uint8_t request[8192];
    size_t length = fread(request, 1, sizeof(request), file);
    fclose(file);
    assert(length > 0 && length < sizeof(request));
    RemdiaOutput output;
    memset(&output, 0, sizeof(output));
    void *session = NULL;
    RemdiaView view = {request, length};
    RemdiaControl control = {NULL, cancelled};
    assert(api.create(view, control, &session, &output) == REMDIA_OK);
    assert(session && output.json.ptr && output.json.len > 0 && output.release);
    api.close(session);
    /* Even control metadata is owned by the output, independent of session. */
    assert(output.json.ptr[0] == '{');
    output.release(output.owner);
    puts("Independent C host: ABI rejection, open, ownership and close passed");
    return 0;
}
