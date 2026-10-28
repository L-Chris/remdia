#ifndef REMDIA_IMAGE_BRIDGE_H
#define REMDIA_IMAGE_BRIDGE_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* 1 unsupported, 2 invalid input, 3 resource limit, 4 system failure. */
typedef struct {
    uint32_t width, height, frames, orientation;
    char format[64];
} RmImageProbe;
typedef struct {
    uint32_t width, height;
    size_t stride, size;
    uint8_t *data;
} RmImage;
int rm_image_probe(const uint8_t *, size_t, RmImageProbe *);
int rm_image_decode(const uint8_t *, size_t, uint32_t, uint32_t, size_t, size_t, RmImage *);
int rm_image_scale(const uint8_t *, uint32_t, uint32_t, size_t, uint32_t, uint32_t, size_t, RmImage *);
int rm_image_encode(const uint8_t *, uint32_t, uint32_t, size_t, int, double, size_t, RmImage *);
int rm_image_supports(const char *, int);
void rm_image_free(RmImage *);
#ifdef __cplusplus
}
#endif
#endif
