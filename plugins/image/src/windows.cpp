#include "image_bridge.h"
#include <windows.h>
#include <wincodec.h>
#include <wrl/client.h>
#include <algorithm>
#include <cmath>
#include <cstdlib>
#include <cstring>
#include <vector>
#include <new>
#include <cstdio>
using Microsoft::WRL::ComPtr;
struct Apartment {
    HRESULT result = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    ~Apartment() { if (SUCCEEDED(result)) CoUninitialize(); }
};
static thread_local char last_error[512] = {};
extern "C" const char *rm_image_last_error(void) { return last_error; }
static int failure(HRESULT hr, const char *operation = "WIC") {
    snprintf(last_error, sizeof(last_error), "%s: HRESULT 0x%08lx", operation, static_cast<unsigned long>(hr));
    if (hr == WINCODEC_ERR_COMPONENTNOTFOUND || hr == WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT) return 1;
    if (hr == E_OUTOFMEMORY || hr == STG_E_MEDIUMFULL) return 3;
    if (hr == WINCODEC_ERR_BADIMAGE || hr == WINCODEC_ERR_BADHEADER || hr == WINCODEC_ERR_STREAMREAD) return 2;
    return 4;
}
#define CHECK(call) do { HRESULT result = (call); if (FAILED(result)) return failure(result, #call); } while (0)
static HRESULT factory(ComPtr<IWICImagingFactory>& f) {
    return CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&f));
}
static HRESULT decoder(IWICImagingFactory *f, const uint8_t *data, size_t size, ComPtr<IWICBitmapDecoder>& out) {
    if (!data || !size || size > MAXDWORD) return E_INVALIDARG;
    ComPtr<IWICStream> stream;
    HRESULT hr = f->CreateStream(&stream); if (FAILED(hr)) return hr;
    hr = stream->InitializeFromMemory(const_cast<BYTE *>(data), static_cast<DWORD>(size)); if (FAILED(hr)) return hr;
    return f->CreateDecoderFromStream(stream.Get(), nullptr, WICDecodeMetadataCacheOnDemand, &out);
}
static UINT orientation(IWICBitmapFrameDecode *frame) {
    ComPtr<IWICMetadataQueryReader> reader;
    if (FAILED(frame->GetMetadataQueryReader(&reader))) return 1;
    for (const wchar_t *query : {L"/app1/ifd/{ushort=274}", L"/ifd/{ushort=274}"}) {
        PROPVARIANT value; PropVariantInit(&value);
        HRESULT hr = reader->GetMetadataByName(query, &value);
        UINT n = SUCCEEDED(hr) && value.vt == VT_UI2 ? value.uiVal : 0;
        PropVariantClear(&value); if (n >= 1 && n <= 8) return n;
    }
    return 1;
}
static HRESULT describe(IWICBitmapDecoder *d, IWICBitmapFrameDecode *frame, RmImageProbe *out) {
    memset(out, 0, sizeof(*out)); HRESULT hr = frame->GetSize(&out->width, &out->height); if (FAILED(hr)) return hr;
    hr = d->GetFrameCount(&out->frames); if (FAILED(hr)) return hr;
    out->orientation = orientation(frame);
    GUID format; hr = d->GetContainerFormat(&format); if (FAILED(hr)) return hr;
    const char *name = "unknown";
    if (format == GUID_ContainerFormatJpeg) name = "public.jpeg";
    else if (format == GUID_ContainerFormatPng) name = "public.png";
    else if (format == GUID_ContainerFormatGif) name = "com.compuserve.gif";
    else if (format == GUID_ContainerFormatTiff) name = "public.tiff";
    else if (format == GUID_ContainerFormatBmp) name = "com.microsoft.bmp";
    else if (format == GUID_ContainerFormatWebp) name = "org.webmproject.webp";
    strncpy_s(out->format, name, _TRUNCATE); return S_OK;
}
int rm_image_probe(const uint8_t *data, size_t size, RmImageProbe *out) {
    Apartment apartment; ComPtr<IWICImagingFactory> f; CHECK(factory(f));
    ComPtr<IWICBitmapDecoder> d; CHECK(decoder(f.Get(), data, size, d));
    ComPtr<IWICBitmapFrameDecode> frame; CHECK(d->GetFrame(0, &frame));
    CHECK(describe(d.Get(), frame.Get(), out)); return 0;
}
void rm_image_free(RmImage *out) { free(out->data); memset(out, 0, sizeof(*out)); }
static int copy(IWICBitmapSource *source, UINT w, UINT h, size_t limit, RmImage *out) {
    memset(out, 0, sizeof(*out));
    if (!w || !h || w > SIZE_MAX / 4 / h || (size_t)w * h * 4 > limit || (size_t)w * h * 4 > MAXDWORD) return 3;
    size_t stride = (size_t)w * 4, size = stride * h;
    uint8_t *data = static_cast<uint8_t *>(malloc(size)); if (!data) return 3;
    HRESULT hr = source->CopyPixels(nullptr, static_cast<UINT>(stride), static_cast<UINT>(size), data);
    if (FAILED(hr)) { free(data); return failure(hr); }
    out->width = w; out->height = h; out->stride = stride; out->size = size; out->data = data; return 0;
}
static int scale(IWICImagingFactory *f, IWICBitmapSource *source, UINT w, UINT h, size_t limit, RmImage *out) {
    ComPtr<IWICFormatConverter> premultiplied; CHECK(f->CreateFormatConverter(&premultiplied));
    CHECK(premultiplied->Initialize(source, GUID_WICPixelFormat32bppPRGBA, WICBitmapDitherTypeNone, nullptr, 0, WICBitmapPaletteTypeCustom));
    ComPtr<IWICBitmapScaler> scaler; CHECK(f->CreateBitmapScaler(&scaler));
    CHECK(scaler->Initialize(premultiplied.Get(), w, h, WICBitmapInterpolationModeFant));
    ComPtr<IWICFormatConverter> rgba; CHECK(f->CreateFormatConverter(&rgba));
    CHECK(rgba->Initialize(scaler.Get(), GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone, nullptr, 0, WICBitmapPaletteTypeCustom));
    return copy(rgba.Get(), w, h, limit, out);
}
int rm_image_decode(const uint8_t *data, size_t size, uint32_t maxw, uint32_t maxh, size_t maxpixels, size_t limit, RmImage *out) {
    Apartment apartment; ComPtr<IWICImagingFactory> f; CHECK(factory(f));
    ComPtr<IWICBitmapDecoder> d; CHECK(decoder(f.Get(), data, size, d));
    ComPtr<IWICBitmapFrameDecode> frame; CHECK(d->GetFrame(0, &frame));
    RmImageProbe p; CHECK(describe(d.Get(), frame.Get(), &p));
    if (!p.width || !p.height || !maxw || !maxh || !maxpixels) return 3;
    ComPtr<IWICBitmapSource> source; CHECK(frame.As(&source));
    UINT contexts = 0;
    HRESULT color_status = frame->GetColorContexts(0, nullptr, &contexts);
    if (color_status == E_NOTIMPL || color_status == WINCODEC_ERR_UNSUPPORTEDOPERATION) contexts = 0;
    else CHECK(color_status);
    if (contexts) {
        ComPtr<IWICColorContext> from, to; CHECK(f->CreateColorContext(&from)); CHECK(f->CreateColorContext(&to));
        IWICColorContext *first = from.Get(); UINT actual = 0; CHECK(frame->GetColorContexts(1, &first, &actual));
        CHECK(to->InitializeFromExifColorSpace(1));
        ComPtr<IWICColorTransform> transform; CHECK(f->CreateColorTransformer(&transform));
        CHECK(transform->Initialize(source.Get(), from.Get(), to.Get(), GUID_WICPixelFormat32bppRGBA));
        CHECK(transform.As(&source));
    }
    WICBitmapTransformOptions options = WICBitmapTransformRotate0;
    switch (p.orientation) {
        case 2: options = WICBitmapTransformFlipHorizontal; break;
        case 3: options = WICBitmapTransformRotate180; break;
        case 4: options = WICBitmapTransformFlipVertical; break;
        case 5: options = static_cast<WICBitmapTransformOptions>(WICBitmapTransformRotate90 | WICBitmapTransformFlipHorizontal); break;
        case 6: options = WICBitmapTransformRotate90; break;
        case 7: options = static_cast<WICBitmapTransformOptions>(WICBitmapTransformRotate270 | WICBitmapTransformFlipHorizontal); break;
        case 8: options = WICBitmapTransformRotate270; break;
    }
    ComPtr<IWICBitmapFlipRotator> rotation; CHECK(f->CreateBitmapFlipRotator(&rotation));
    CHECK(rotation->Initialize(source.Get(), options)); CHECK(rotation.As(&source));
    UINT w, h; CHECK(source->GetSize(&w, &h));
    double factor = std::min(1.0, std::min(static_cast<double>(maxw) / w, static_cast<double>(maxh) / h));
    factor = std::min(factor, sqrt(static_cast<double>(maxpixels) / (static_cast<double>(w) * h)));
    UINT dw = static_cast<UINT>(std::max(1.0, floor(w * factor))), dh = static_cast<UINT>(std::max(1.0, floor(h * factor)));
    return scale(f.Get(), source.Get(), dw, dh, limit, out);
}
int rm_image_scale(const uint8_t *data, uint32_t w, uint32_t h, size_t stride, uint32_t dw, uint32_t dh, size_t limit, RmImage *out) {
    Apartment apartment; ComPtr<IWICImagingFactory> f; CHECK(factory(f));
    if (!w || !h || stride < (size_t)w * 4 || stride > MAXDWORD / h) return 3;
    ComPtr<IWICBitmap> bitmap; CHECK(f->CreateBitmapFromMemory(w, h, GUID_WICPixelFormat32bppRGBA, static_cast<UINT>(stride), static_cast<UINT>(stride * h), const_cast<BYTE *>(data), &bitmap));
    return scale(f.Get(), bitmap.Get(), dw, dh, limit, out);
}
class BoundedStream final : public IStream {
    ULONG references = 1; size_t position = 0;
public:
    std::vector<uint8_t> bytes; size_t limit;
    explicit BoundedStream(size_t maximum): limit(maximum) {}
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void **out) override { if (!out) return E_POINTER; *out = nullptr; if (id == IID_IUnknown || id == IID_ISequentialStream || id == IID_IStream) { *out = static_cast<IStream *>(this); AddRef(); return S_OK; } return E_NOINTERFACE; }
    ULONG STDMETHODCALLTYPE AddRef() override { return ++references; }
    ULONG STDMETHODCALLTYPE Release() override { ULONG n = --references; if (!n) delete this; return n; }
    HRESULT STDMETHODCALLTYPE Read(void *out, ULONG count, ULONG *read) override { size_t n = std::min<size_t>(count, bytes.size() - std::min(position, bytes.size())); if (n) memcpy(out, bytes.data() + position, n); position += n; if (read) *read = static_cast<ULONG>(n); return n == count ? S_OK : S_FALSE; }
    HRESULT STDMETHODCALLTYPE Write(const void *data, ULONG count, ULONG *written) override {
        if (written) *written = 0; if (count > limit - position) return STG_E_MEDIUMFULL;
        try { if (position + count > bytes.size()) bytes.resize(position + count); } catch (...) { return E_OUTOFMEMORY; }
        if (count) memcpy(bytes.data() + position, data, count); position += count; if (written) *written = count; return S_OK;
    }
    HRESULT STDMETHODCALLTYPE Seek(LARGE_INTEGER move, DWORD origin, ULARGE_INTEGER *result) override { if (origin > STREAM_SEEK_END) return E_INVALIDARG; LONGLONG base = origin == STREAM_SEEK_SET ? 0 : origin == STREAM_SEEK_CUR ? static_cast<LONGLONG>(position) : static_cast<LONGLONG>(bytes.size()); if (move.QuadPart < -base || move.QuadPart > static_cast<LONGLONG>(limit) - base) return STG_E_MEDIUMFULL; position = static_cast<size_t>(base + move.QuadPart); if (result) result->QuadPart = position; return S_OK; }
    HRESULT STDMETHODCALLTYPE SetSize(ULARGE_INTEGER size) override { if (size.QuadPart > limit) return STG_E_MEDIUMFULL; try { bytes.resize(static_cast<size_t>(size.QuadPart)); } catch (...) { return E_OUTOFMEMORY; } return S_OK; }
    HRESULT STDMETHODCALLTYPE CopyTo(IStream *, ULARGE_INTEGER, ULARGE_INTEGER *, ULARGE_INTEGER *) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE Commit(DWORD) override { return S_OK; }
    HRESULT STDMETHODCALLTYPE Revert() override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE LockRegion(ULARGE_INTEGER, ULARGE_INTEGER, DWORD) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE UnlockRegion(ULARGE_INTEGER, ULARGE_INTEGER, DWORD) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE Stat(STATSTG *out, DWORD) override { if (!out) return E_POINTER; memset(out, 0, sizeof(*out)); out->type = STGTY_STREAM; out->cbSize.QuadPart = bytes.size(); return S_OK; }
    HRESULT STDMETHODCALLTYPE Clone(IStream **) override { return E_NOTIMPL; }
};
int rm_image_encode(const uint8_t *data, uint32_t w, uint32_t h, size_t stride, int jpeg, double quality, size_t limit, RmImage *out) {
    memset(out, 0, sizeof(*out)); Apartment apartment; ComPtr<IWICImagingFactory> f; CHECK(factory(f));
    if (!w || !h || stride < (size_t)w * 4 || stride > MAXDWORD / h) return 3;
    ComPtr<BoundedStream> stream; stream.Attach(new(std::nothrow) BoundedStream(limit)); if (!stream) return 3;
    ComPtr<IWICBitmapEncoder> encoder; CHECK(f->CreateEncoder(jpeg ? GUID_ContainerFormatJpeg : GUID_ContainerFormatPng, nullptr, &encoder));
    CHECK(encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache));
    ComPtr<IWICBitmapFrameEncode> frame; ComPtr<IPropertyBag2> properties; CHECK(encoder->CreateNewFrame(&frame, &properties));
    if (jpeg && properties) {
        PROPBAG2 option = {}; option.pstrName = const_cast<wchar_t *>(L"ImageQuality"); VARIANT value; VariantInit(&value); value.vt = VT_R4; value.fltVal = static_cast<float>(quality);
        CHECK(properties->Write(1, &option, &value));
    }
    CHECK(frame->Initialize(properties.Get())); CHECK(frame->SetSize(w, h));
    WICPixelFormatGUID format = jpeg ? GUID_WICPixelFormat24bppBGR : GUID_WICPixelFormat32bppRGBA; CHECK(frame->SetPixelFormat(&format));
    ComPtr<IWICBitmap> bitmap; CHECK(f->CreateBitmapFromMemory(w, h, GUID_WICPixelFormat32bppRGBA, static_cast<UINT>(stride), static_cast<UINT>(stride * h), const_cast<BYTE *>(data), &bitmap));
    ComPtr<IWICFormatConverter> converter; CHECK(f->CreateFormatConverter(&converter));
    CHECK(converter->Initialize(bitmap.Get(), format, WICBitmapDitherTypeNone, nullptr, 0, WICBitmapPaletteTypeCustom));
    CHECK(frame->WriteSource(converter.Get(), nullptr)); CHECK(frame->Commit()); CHECK(encoder->Commit());
    out->data = static_cast<uint8_t *>(malloc(stream->bytes.size())); if (!out->data) return 3;
    memcpy(out->data, stream->bytes.data(), stream->bytes.size()); out->size = stream->bytes.size(); return 0;
}
int rm_image_supports(const char *type, int encode) {
    Apartment apartment; ComPtr<IWICImagingFactory> f; if (FAILED(factory(f))) return 0;
    ComPtr<IEnumUnknown> list; if (FAILED(f->CreateComponentEnumerator(encode ? WICEncoder : WICDecoder, WICComponentEnumerateDefault, &list))) return 0;
    GUID wanted;
    if (!strcmp(type, "public.jpeg")) wanted = GUID_ContainerFormatJpeg;
    else if (!strcmp(type, "public.png")) wanted = GUID_ContainerFormatPng;
    else if (!strcmp(type, "org.webmproject.webp")) wanted = GUID_ContainerFormatWebp;
    else if (!strcmp(type, "com.compuserve.gif")) wanted = GUID_ContainerFormatGif;
    else if (!strcmp(type, "public.tiff")) wanted = GUID_ContainerFormatTiff;
    else if (!strcmp(type, "com.microsoft.bmp")) wanted = GUID_ContainerFormatBmp;
    else return 0;
    ComPtr<IUnknown> next;
    while (list->Next(1, &next, nullptr) == S_OK) {
        ComPtr<IWICBitmapCodecInfo> codec; if (SUCCEEDED(next.As(&codec))) { GUID container; if (SUCCEEDED(codec->GetContainerFormat(&container)) && container == wanted) return 1; }
        next.Reset();
    }
    return 0;
}
