#include "vt_bridge.h"
#include <CoreFoundation/CoreFoundation.h>
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <math.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

struct RMVT {
    VTDecompressionSessionRef session;
    CMVideoFormatDescriptionRef format;
    RMVTOutput callback;
    void *context;
    _Atomic(const void *) current_ticket;
    _Atomic(int32_t) callback_error;
};

static int64_t micros(CMTime t) {
    return CMTIME_IS_NUMERIC(t) ? CMTimeConvertScale(t, 1000000, kCMTimeRoundingMethod_RoundTowardZero).value : 0;
}
static void output(void *context, void *ticket, OSStatus status, VTDecodeInfoFlags flags,
                   CVImageBufferRef image, CMTime pts, CMTime duration) {
    (void)flags;
    RMVT *v = context;
    /* No stack frame reference may escape a synchronous DecodeFrame invocation. */
    if (!ticket || ticket != atomic_load_explicit(&v->current_ticket, memory_order_acquire)) {
        atomic_store_explicit(&v->callback_error, -100001, memory_order_release);
        return;
    }
    if (image) CVPixelBufferRetain(image);
    v->callback(v->context, ticket, status, image, micros(pts), micros(duration), CMTIME_IS_NUMERIC(pts));
}
int32_t rm_vt_create(int32_t hevc, const uint8_t *const *sets, const size_t *sizes,
                    size_t count, int32_t length, int32_t hardware, RMVTOutput callback,
                    void *context, RMVT **out) {
    *out = NULL;
    RMVT *v = calloc(1, sizeof(*v));
    if (!v) return -100002;
    atomic_init(&v->current_ticket, NULL);
    atomic_init(&v->callback_error, 0);
    v->callback = callback;
    v->context = context;
    OSStatus status;
    if (hevc)
        status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(NULL, count, sets, sizes, length, NULL, &v->format);
    else
        status = CMVideoFormatDescriptionCreateFromH264ParameterSets(NULL, count, sets, sizes, length, &v->format);
    if (status) { free(v); return status; }
    CFMutableDictionaryRef spec = CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFMutableDictionaryRef attrs = CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFDictionarySetValue(spec, kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder,
                         hardware ? kCFBooleanTrue : kCFBooleanFalse);
    if (hardware == 2) CFDictionarySetValue(spec, kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder, kCFBooleanTrue);
    int32_t pixel = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
    CFNumberRef number = CFNumberCreate(NULL, kCFNumberSInt32Type, &pixel);
    CFDictionarySetValue(attrs, kCVPixelBufferPixelFormatTypeKey, number);
    CFRelease(number);
    VTDecompressionOutputCallbackRecord record = { output, v };
    status = VTDecompressionSessionCreate(NULL, v->format, spec, attrs, &record, &v->session);
    CFRelease(attrs);
    CFRelease(spec);
    if (status) { CFRelease(v->format); free(v); return status; }
    *out = v;
    return 0;
}
int32_t rm_vt_decode(RMVT *v, const uint8_t *data, size_t size, int64_t pts_ns,
                    int64_t dts_ns, int64_t duration_ns, int32_t keyframe, const void *ticket) {
    CMBlockBufferRef block = NULL;
    CMSampleBufferRef sample = NULL;
    OSStatus status = CMBlockBufferCreateWithMemoryBlock(NULL, NULL, size, NULL, NULL, 0, size, 0, &block);
    if (status) return status;
    status = CMBlockBufferReplaceDataBytes(data, block, 0, size);
    if (!status) {
        CMSampleTimingInfo timing = { CMTimeMake(duration_ns, 1000000000),
                                      CMTimeMake(pts_ns, 1000000000), CMTimeMake(dts_ns, 1000000000) };
        status = CMSampleBufferCreateReady(NULL, block, v->format, 1, 1, &timing, 1, &size, &sample);
    }
    if (!status) {
        CFArrayRef array = CMSampleBufferGetSampleAttachmentsArray(sample, true);
        if (array && CFArrayGetCount(array)) {
            CFMutableDictionaryRef attachments = (CFMutableDictionaryRef)CFArrayGetValueAtIndex(array, 0);
            CFDictionarySetValue(attachments, kCMSampleAttachmentKey_NotSync, keyframe ? kCFBooleanFalse : kCFBooleanTrue);
        }
        atomic_store_explicit(&v->current_ticket, ticket, memory_order_release);
        /* Both async and temporal-processing flags remain clear. */
        status = VTDecompressionSessionDecodeFrame(v->session, sample, 0, (void *)ticket, NULL);
        atomic_store_explicit(&v->current_ticket, NULL, memory_order_release);
    }
    if (sample) CFRelease(sample);
    CFRelease(block);
    if (!status) status = atomic_load_explicit(&v->callback_error, memory_order_acquire);
    return status;
}
int32_t rm_vt_drain(RMVT *v) {
    OSStatus status = VTDecompressionSessionWaitForAsynchronousFrames(v->session);
    return status ? status : atomic_load_explicit(&v->callback_error, memory_order_acquire);
}
void rm_vt_close(RMVT *v) {
    if (!v) return;
    if (v->session) {
        VTDecompressionSessionWaitForAsynchronousFrames(v->session);
        VTDecompressionSessionInvalidate(v->session);
        CFRelease(v->session);
    }
    if (v->format) CFRelease(v->format);
    free(v);
}
int32_t rm_vt_hardware(RMVT *v) {
    CFTypeRef value = NULL;
    OSStatus status = VTSessionCopyProperty(v->session, kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder, NULL, &value);
    int result = !status && value && CFGetTypeID(value) == CFBooleanGetTypeID() && CFBooleanGetValue(value);
    if (value) CFRelease(value);
    return result;
}
static CFTypeRef attachment(RMVT *v, CVPixelBufferRef pixel, CFStringRef key) {
    CFTypeRef value = CVBufferCopyAttachment(pixel, key, NULL);
    if (!value) {
        value = CMFormatDescriptionGetExtension(v->format, key);
        if (value) CFRetain(value);
    }
    return value;
}
static int equal(CFTypeRef value, CFStringRef expected) { return value && CFEqual(value, expected); }
int32_t rm_pixel_info(RMVT *v, void *image, RMPixelInfo *info) {
    CVPixelBufferRef pixel = image;
    memset(info, 0, sizeof(*info));
    OSType type = CVPixelBufferGetPixelFormatType(pixel);
    if (type != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange && type != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange) return -100003;
    if (CVPixelBufferGetPlaneCount(pixel) != 2) return -100003;
    size_t width = CVPixelBufferGetWidth(pixel), height = CVPixelBufferGetHeight(pixel);
    info->surface_bytes = CVPixelBufferGetDataSize(pixel);
    if (!info->surface_bytes) info->surface_bytes = CVPixelBufferGetBytesPerRowOfPlane(pixel,0) * CVPixelBufferGetHeightOfPlane(pixel,0) + CVPixelBufferGetBytesPerRowOfPlane(pixel,1) * CVPixelBufferGetHeightOfPlane(pixel,1);
    CMVideoFormatDescriptionRef image_format = NULL;
    OSStatus status = CMVideoFormatDescriptionCreateForImageBuffer(NULL, pixel, &image_format);
    if (status) return status;
    CGRect rect = CMVideoFormatDescriptionGetCleanAperture(image_format, true);
    CFRelease(image_format);
    CFTypeRef clean = CVBufferCopyAttachment(pixel, kCVImageBufferCleanApertureKey, NULL);
    if (!clean) {
        CGRect original = CMVideoFormatDescriptionGetCleanAperture(v->format, true);
        if (original.size.width <= width && original.size.height <= height) rect = original;
    }
    if (clean) CFRelease(clean);
    double values[] = {rect.origin.x, rect.origin.y, rect.size.width, rect.size.height};
    for (size_t i = 0; i < 4; i++)
        if (!isfinite(values[i]) || values[i] < 0 || values[i] > UINT32_MAX || fabs(values[i] - round(values[i])) > 0.001) return -100004;
    info->x = (uint32_t)round(rect.origin.x); info->y = (uint32_t)round(rect.origin.y);
    info->width = (uint32_t)round(rect.size.width); info->height = (uint32_t)round(rect.size.height);
    if (!info->width || !info->height || info->x % 2 || info->y % 2 ||
        (uint64_t)info->x + info->width > width || (uint64_t)info->y + info->height > height) return -100004;
    info->range = type == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
    CFTypeRef value = attachment(v, pixel, kCVImageBufferYCbCrMatrixKey);
    info->matrix = equal(value,kCVImageBufferYCbCrMatrix_ITU_R_709_2) ? 1 :
                   equal(value,kCVImageBufferYCbCrMatrix_ITU_R_601_4) ? 2 :
                   equal(value,kCVImageBufferYCbCrMatrix_ITU_R_2020) ? 9 : 0;
    if (value) CFRelease(value);
    value = attachment(v, pixel, kCVImageBufferColorPrimariesKey);
    info->primaries = equal(value,kCVImageBufferColorPrimaries_ITU_R_709_2) ? 1 :
                      equal(value,kCVImageBufferColorPrimaries_ITU_R_2020) ? 9 :
                      equal(value,kCVImageBufferColorPrimaries_P3_D65) ? 12 : 0;
    if (value) CFRelease(value);
    value = attachment(v, pixel, kCVImageBufferTransferFunctionKey);
    info->transfer = equal(value,kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ) ? 16 :
                     equal(value,kCVImageBufferTransferFunction_ITU_R_2100_HLG) ? 18 :
                     equal(value,kCVImageBufferTransferFunction_ITU_R_709_2) ? 1 :
                     equal(value,kCVImageBufferTransferFunction_sRGB) ? 13 : 0;
    if (value) CFRelease(value);
    if (info->transfer == 16 || info->transfer == 18) return -100005;
    info->sar_num = info->sar_den = 1;
    value = attachment(v,pixel,kCVImageBufferPixelAspectRatioKey);
    if (value && CFGetTypeID(value) == CFDictionaryGetTypeID()) {
        CFNumberRef n = CFDictionaryGetValue(value,kCVImageBufferPixelAspectRatioHorizontalSpacingKey);
        CFNumberRef d = CFDictionaryGetValue(value,kCVImageBufferPixelAspectRatioVerticalSpacingKey);
        int32_t num = 0, den = 0;
        if (n && d && CFGetTypeID(n) == CFNumberGetTypeID() && CFGetTypeID(d) == CFNumberGetTypeID() &&
            CFNumberGetValue(n,kCFNumberSInt32Type,&num) && CFNumberGetValue(d,kCFNumberSInt32Type,&den) && num > 0 && den > 0) {
            info->sar_num = num; info->sar_den = den;
        }
    }
    if (value) CFRelease(value);
    return 0;
}
int32_t rm_pixel_copy(void *image, const RMPixelInfo *info, uint8_t *y, size_t ysize, uint8_t *uv, size_t uvsize) {
    CVPixelBufferRef pixel = image;
    size_t yw = info->width, uvw = ((yw + 1) / 2) * 2, h = info->height, uvh = (h + 1) / 2;
    if (ysize != yw*h || uvsize != uvw*uvh) return -100004;
    CVReturn status = CVPixelBufferLockBaseAddress(pixel,kCVPixelBufferLock_ReadOnly);
    if (status) return status;
    size_t ys = CVPixelBufferGetBytesPerRowOfPlane(pixel,0), us = CVPixelBufferGetBytesPerRowOfPlane(pixel,1);
    const uint8_t *yp = CVPixelBufferGetBaseAddressOfPlane(pixel,0), *up = CVPixelBufferGetBaseAddressOfPlane(pixel,1);
    if (!yp || !up || (size_t)info->x + yw > ys || (size_t)info->x + uvw > us ||
        (size_t)info->y + h > CVPixelBufferGetHeightOfPlane(pixel,0) ||
        (size_t)info->y/2 + uvh > CVPixelBufferGetHeightOfPlane(pixel,1)) status = -100004;
    if (!status) {
        for (size_t row=0;row<h;row++) memcpy(y+row*yw,yp+((size_t)info->y+row)*ys+info->x,yw);
        for (size_t row=0;row<uvh;row++) memcpy(uv+row*uvw,up+((size_t)info->y/2+row)*us+info->x,uvw);
    }
    CVPixelBufferUnlockBaseAddress(pixel,kCVPixelBufferLock_ReadOnly);
    return status;
}
void rm_pixel_release(void *image) { if (image) CVPixelBufferRelease(image); }
size_t rm_pixel_info_size(void) { return sizeof(RMPixelInfo); }
