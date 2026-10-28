#include <stddef.h>
#include <stdint.h>
typedef struct {
    uint8_t *data; size_t size;
    uint32_t stream; int32_t time_scale; int64_t pts, dts, duration;
    uint64_t trim_start, trim_end;
    int keyframe;
} RmPacket;
typedef uint8_t (*RmPoll)(void *context);
int rm_demux_open(const char *path, const char *mime, uint64_t timeout_ms, RmPoll poll, void *context, void **reader, char **metadata);
int rm_demux_read(void *reader, size_t limit, RmPacket *out);
int rm_demux_seek(void *reader, int64_t target_us);
int rm_demux_artwork(void *reader, size_t limit, uint8_t **data, size_t *size);
void rm_demux_close(void *reader);
void rm_demux_free(void *data);
const char *rm_demux_error(void);
