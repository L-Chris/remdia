#ifndef REMDIA_SDK_H
#define REMDIA_SDK_H
#include "remdia.h"
#ifdef __cplusplus
extern "C" {
#endif
/* Single-library Engine facade, using remdia.h's ABI 2.2 output ownership.
 * JSON config: {operation:"media",request:{...},budget_bytes:100663296,
 *               plugins:"optional trusted extension directory"}
 * operation:"image" accepts operations:[...] and either encoded input bytes
 * or source:"local path". operation:"graph" accepts graph:{nodes,edges}.
 * open/read/seek/close are serial on the creating worker. The cancellation
 * callback/context stays live until close and may be checked by any worker.
 * Every returned Output must be released exactly once when release != NULL.
 * Its JSON/planes remain valid after seek and session close until release.
 * Release may happen on any thread. SDK library stays loaded through release.
 * read returns OK (a frame), EOF, or an error. Never reuse a closed handle.
 * close consumes the handle after validation, including if destruction panics.
 * A null/wrong-worker validation error does not consume a live handle.
 */
int32_t remdia_sdk_open(RemdiaView config, RemdiaView input, RemdiaControl control, void **job, RemdiaOutput *out);
int32_t remdia_sdk_read(void *job, RemdiaOutput *out);
int32_t remdia_sdk_seek(void *job, int64_t microseconds, RemdiaOutput *out);
int32_t remdia_sdk_close(void *job, RemdiaOutput *out);
#ifdef __cplusplus
}
#endif
#endif
