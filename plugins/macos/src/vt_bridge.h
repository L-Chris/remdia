#ifndef REMDIA_VT_BRIDGE_H
#define REMDIA_VT_BRIDGE_H
#include <stdint.h>
#include <stddef.h>
typedef struct RMVT RMVT;
typedef struct {
    uint32_t width, height, x, y;
    uint32_t matrix, range, primaries, transfer;
    int32_t sar_num, sar_den;
    uint64_t surface_bytes;
} RMPixelInfo;
/* The callback transfers one retained image reference, even on error. */
typedef void (*RMVTOutput)(void *, const void *, int32_t, void *, int64_t, int64_t, int32_t);
int32_t rm_vt_create(int32_t hevc, const uint8_t *const *sets, const size_t *sizes,
                    size_t count, int32_t length, int32_t hardware, RMVTOutput callback,
                    void *context, RMVT **out);
int32_t rm_vt_decode(RMVT *, const uint8_t *, size_t, int64_t pts_ns,
                    int64_t dts_ns, int64_t duration_ns, int32_t keyframe, const void *ticket);
int32_t rm_vt_drain(RMVT *);
void rm_vt_close(RMVT *);
int32_t rm_vt_hardware(RMVT *);
int32_t rm_pixel_info(RMVT *, void *, RMPixelInfo *);
int32_t rm_pixel_copy(void *, const RMPixelInfo *, uint8_t *, size_t, uint8_t *, size_t);
void rm_pixel_release(void *);
size_t rm_pixel_info_size(void);
#endif
