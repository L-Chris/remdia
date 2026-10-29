#include "image_bridge.h"
#include <ImageIO/ImageIO.h>
#include <CoreGraphics/CoreGraphics.h>
#include <CoreFoundation/CoreFoundation.h>
#include <math.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>

static CGImageSourceRef source(const uint8_t *data, size_t size) {
    if (!data || !size || size > (size_t)LONG_MAX) return NULL;
    CFDataRef bytes = CFDataCreateWithBytesNoCopy(NULL, data, (CFIndex)size, kCFAllocatorNull);
    if (!bytes) return NULL;
    const void *key = kCGImageSourceShouldCache, *value = kCFBooleanFalse;
    CFDictionaryRef options = CFDictionaryCreate(NULL, &key, &value, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CGImageSourceRef result = CGImageSourceCreateWithData(bytes, options);
    CFRelease(options); CFRelease(bytes); return result;
}
static uint32_t number(CFDictionaryRef props, CFStringRef key, uint32_t fallback) {
    CFTypeRef value = CFDictionaryGetValue(props, key);
    int64_t n = fallback;
    if (value && CFGetTypeID(value) == CFNumberGetTypeID()) CFNumberGetValue((CFNumberRef)value, kCFNumberSInt64Type, &n);
    return n > 0 && n <= UINT32_MAX ? (uint32_t)n : fallback;
}
static int probe(CGImageSourceRef s, RmImageProbe *out) {
    CFDictionaryRef p = CGImageSourceCopyPropertiesAtIndex(s, 0, NULL);
    if (!p) return 2;
    out->width = number(p, kCGImagePropertyPixelWidth, 0);
    out->height = number(p, kCGImagePropertyPixelHeight, 0);
    out->orientation = number(p, kCGImagePropertyOrientation, 1);
    size_t count = CGImageSourceGetCount(s);
    out->frames = count <= UINT32_MAX ? (uint32_t)count : 0;
    CFStringRef type = CGImageSourceGetType(s);
    if (type) CFStringGetCString(type, out->format, sizeof(out->format), kCFStringEncodingUTF8);
    CFRelease(p);
    return out->width && out->height && out->frames ? 0 : 2;
}
int rm_image_probe(const uint8_t *data, size_t size, RmImageProbe *out) {
    memset(out, 0, sizeof(*out));
    CGImageSourceRef s = source(data, size); if (!s) return 1;
    int status = probe(s, out); CFRelease(s); return status;
}
void rm_image_free(RmImage *out) { free(out->data); memset(out, 0, sizeof(*out)); }

static int raster(CGImageRef image, uint32_t w, uint32_t h, size_t limit, RmImage *out) {
    memset(out, 0, sizeof(*out));
    if (!w || !h || w > SIZE_MAX / 4 / h || (size_t)w * h * 4 > limit) return 3;
    size_t stride = (size_t)w * 4, size = stride * h;
    uint8_t *pixels = calloc(1, size); if (!pixels) return 3;
    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef context = CGBitmapContextCreate(pixels, w, h, 8, stride, space, kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big);
    CGColorSpaceRelease(space);
    if (!context) { free(pixels); return 4; }
    CGContextSetInterpolationQuality(context, kCGInterpolationHigh);
    CGContextSetBlendMode(context, kCGBlendModeCopy);
    CGContextDrawImage(context, CGRectMake(0, 0, w, h), image);
    CGContextRelease(context);
    for (size_t i = 0; i < size; i += 4) {
        unsigned alpha = pixels[i + 3];
        for (size_t c = 0; c < 3; ++c) {
            unsigned value = alpha ? (pixels[i+c] * 255u + alpha / 2) / alpha : 0;
            pixels[i+c] = (uint8_t)(value > 255 ? 255 : value);
        }
    }
    out->width = w; out->height = h; out->stride = stride; out->size = size; out->data = pixels;
    return 0;
}
int rm_image_decode(const uint8_t *data, size_t size, uint32_t maxw, uint32_t maxh, size_t maxpixels, size_t limit, RmImage *out) {
    CGImageSourceRef s = source(data, size); if (!s) return 1;
    RmImageProbe p = {0}; int status = probe(s, &p);
    if (status) { CFRelease(s); return status; }
    double w = p.width, h = p.height;
    if (p.orientation >= 5 && p.orientation <= 8) { double t = w; w = h; h = t; }
    double scale = fmin(1.0, fmin(maxw / w, maxh / h));
    scale = fmin(scale, sqrt(maxpixels / (w * h)));
    int64_t edge = (int64_t)fmax(1, floor(fmax(w, h) * scale));
    CFNumberRef n = CFNumberCreate(NULL, kCFNumberSInt64Type, &edge);
    const void *keys[] = {kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceThumbnailMaxPixelSize, kCGImageSourceShouldCacheImmediately};
    const void *values[] = {kCFBooleanTrue, kCFBooleanTrue, n, kCFBooleanTrue};
    CFDictionaryRef options = CFDictionaryCreate(NULL, keys, values, 4, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CGImageRef image = CGImageSourceCreateThumbnailAtIndex(s, 0, options);
    CFRelease(options); CFRelease(n); CFRelease(s);
    if (!image) return 2;
    size_t actualw = CGImageGetWidth(image), actualh = CGImageGetHeight(image);
    if (actualw > maxw || actualh > maxh || actualw > maxpixels / actualh) status = 3;
    else status = raster(image, (uint32_t)actualw, (uint32_t)actualh, limit, out);
    CGImageRelease(image); return status;
}
static CGImageRef image_from(const uint8_t *pixels, uint32_t w, uint32_t h, size_t stride) {
    if (!pixels || !w || !h || stride < (size_t)w * 4 || stride > SIZE_MAX / h) return NULL;
    CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGDataProviderRef provider = CGDataProviderCreateWithData(NULL, pixels, stride * h, NULL);
    CGImageRef image = CGImageCreate(w, h, 8, 32, stride, space, kCGImageAlphaLast | kCGBitmapByteOrder32Big, provider, NULL, false, kCGRenderingIntentDefault);
    CGDataProviderRelease(provider); CGColorSpaceRelease(space); return image;
}
int rm_image_scale(const uint8_t *data, uint32_t w, uint32_t h, size_t stride, uint32_t outw, uint32_t outh, size_t limit, RmImage *out) {
    CGImageRef image = image_from(data, w, h, stride); if (!image) return 2;
    int status = raster(image, outw, outh, limit, out); CGImageRelease(image); return status;
}
typedef struct { uint8_t *data; size_t size, limit; int failed; } Writer;
static size_t append(void *context, const void *bytes, size_t count) {
    Writer *writer = context;
    if (count > writer->limit - writer->size) { writer->failed = 3; return 0; }
    uint8_t *next = realloc(writer->data, writer->size + count);
    if (!next && count) { writer->failed = 3; return 0; }
    writer->data = next;
    if (count) memcpy(next + writer->size, bytes, count);
    writer->size += count; return count;
}
int rm_image_encode(const uint8_t *data, uint32_t w, uint32_t h, size_t stride, int jpeg, double quality, size_t limit, RmImage *out) {
    memset(out, 0, sizeof(*out));
    CGImageRef image = image_from(data, w, h, stride); if (!image) return 2;
    Writer writer = {NULL, 0, limit, 0};
    CGDataConsumerCallbacks callbacks = {append, NULL};
    CGDataConsumerRef consumer = CGDataConsumerCreate(&writer, &callbacks);
    CGImageDestinationRef destination = CGImageDestinationCreateWithDataConsumer(consumer, jpeg ? CFSTR("public.jpeg") : CFSTR("public.png"), 1, NULL);
    CGDataConsumerRelease(consumer);
    if (!destination) { CGImageRelease(image); return 1; }
    CFNumberRef q = CFNumberCreate(NULL, kCFNumberDoubleType, &quality);
    const void *key = kCGImageDestinationLossyCompressionQuality, *value = q;
    CFDictionaryRef properties = CFDictionaryCreate(NULL, &key, &value, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CGImageDestinationAddImage(destination, image, properties);
    int success = CGImageDestinationFinalize(destination);
    CFRelease(properties); CFRelease(q); CFRelease(destination); CGImageRelease(image);
    if (!success || writer.failed) { free(writer.data); return writer.failed ? writer.failed : 4; }
    out->data = writer.data; out->size = writer.size; return 0;
}
int rm_image_supports(const char *type, int encode) {
    CFStringRef t = CFStringCreateWithCString(NULL, type, kCFStringEncodingUTF8); if (!t) return 0;
    CFArrayRef list = encode ? CGImageDestinationCopyTypeIdentifiers() : CGImageSourceCopyTypeIdentifiers();
    int found = CFArrayContainsValue(list, CFRangeMake(0, CFArrayGetCount(list)), t);
    CFRelease(list); CFRelease(t); return found;
}
