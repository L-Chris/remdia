package dev.remdia;

import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.ColorSpace;
import android.graphics.ImageDecoder;
import android.graphics.Paint;
import android.graphics.Rect;
import android.media.ExifInterface;
import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import org.json.JSONObject;

final class Images {
    private Images() {}
    static final class ProbeOnly extends RuntimeException { private static final long serialVersionUID = 1L; }
    static String probe(byte[] input) throws Exception {
        final JSONObject result = new JSONObject();
        int orientation = 1;
        try { orientation = new ExifInterface(new ByteArrayInputStream(input)).getAttributeInt(ExifInterface.TAG_ORIENTATION, 1); }
        catch (java.io.IOException ignored) { /* Not every image carries EXIF. */ }
        final int orient = orientation >= 1 && orientation <= 8 ? orientation : 1;
        try {
            ImageDecoder.decodeBitmap(ImageDecoder.createSource(ByteBuffer.wrap(input)), (decoder, info, source) -> {
                try {
                    int w = info.getSize().getWidth(), h = info.getSize().getHeight();
                    result.put("width", orient >= 5 ? h : w).put("height", orient >= 5 ? w : h)
                        .put("frames", frameCount(input)).put("orientation", orient)
                        .put("format", info.getMimeType().replace("image/", "").replace("x-ms-bmp", "bmp"));
                } catch (Exception e) { throw new IllegalArgumentException(e); }
                throw new ProbeOnly();
            });
        } catch (ProbeOnly expected) { return result.toString(); }
        throw new IllegalArgumentException("Image probe did not return metadata");
    }
    private static int frameCount(byte[] input) {
        // Metadata-only frame counting; decoding remains entirely in ImageDecoder.
        if (input.length >= 13 && input[0] == 'G' && input[1] == 'I' && input[2] == 'F') {
            int offset = 13, count = 0;
            if ((input[10] & 128) != 0) offset += 3 * (1 << ((input[10] & 7) + 1));
            while (offset < input.length) {
                int kind = input[offset++] & 255;
                if (kind == 0x3b) return Math.max(count, 1);
                if (kind == 0x21) { if (offset >= input.length) break; offset++; }
                else if (kind == 0x2c) {
                    if (offset + 9 > input.length) break;
                    int packed = input[offset + 8] & 255; offset += 9;
                    if ((packed & 128) != 0) offset += 3 * (1 << ((packed & 7) + 1));
                    offset++; count++;
                } else break;
                while (offset < input.length) { int size = input[offset++] & 255; if (size == 0) break; offset += size; }
                if (count > 1024) return count;
            }
            throw new IllegalArgumentException("Truncated GIF metadata");
        }
        if (input.length >= 12 && input[0] == 'R' && input[1] == 'I' && input[8] == 'W') {
            int count = 0; long offset = 12;
            while (offset + 8 <= input.length) {
                int p = (int)offset;
                long size = ((long)input[p+4]&255) | (((long)input[p+5]&255)<<8) | (((long)input[p+6]&255)<<16) | (((long)input[p+7]&255)<<24);
                if (input[p] == 'A' && input[p+1] == 'N' && input[p+2] == 'M' && input[p+3] == 'F') count++;
                offset += 8 + size + (size & 1);
                if (offset > input.length) throw new IllegalArgumentException("Truncated WebP metadata");
            }
            return Math.max(count, 1);
        }
        return 1;
    }
    static byte[] decode(byte[] input, int maxWidth, int maxHeight, long maxPixels, long limit) throws Exception {
        Bitmap bitmap = ImageDecoder.decodeBitmap(ImageDecoder.createSource(ByteBuffer.wrap(input)), (decoder, info, source) -> {
            int width = info.getSize().getWidth(), height = info.getSize().getHeight();
            if (width <= 0 || height <= 0 || width > 65536 || height > 65536 || (long)width * height > 64L * 1024 * 1024)
                throw new IllegalArgumentException("Image source dimensions exceed limit");
            double scale = Math.min(1, Math.min(Math.min((double)maxWidth/width, (double)maxHeight/height), Math.sqrt((double)maxPixels / ((long)width * height))));
            int w = Math.max(1, (int)Math.floor(width * scale)), h = Math.max(1, (int)Math.floor(height * scale));
            check(w, h, limit);
            decoder.setAllocator(ImageDecoder.ALLOCATOR_SOFTWARE);
            decoder.setTargetColorSpace(ColorSpace.get(ColorSpace.Named.SRGB));
            decoder.setTargetSize(w, h);
        });
        try { return pixels(bitmap, limit); } finally { bitmap.recycle(); }
    }
    static byte[] scale(ByteBuffer input, int width, int height, int dw, int dh, long limit) throws Exception {
        check(dw, dh, limit); Bitmap source = bitmap(input, width, height);
        Bitmap output = Bitmap.createBitmap(dw, dh, Bitmap.Config.ARGB_8888);
        try {
            new Canvas(output).drawBitmap(source, null, new Rect(0, 0, dw, dh), new Paint(Paint.FILTER_BITMAP_FLAG));
            return pixels(output, limit);
        } finally { source.recycle(); output.recycle(); }
    }
    static byte[] encode(ByteBuffer input, int width, int height, boolean jpeg, double quality, long limit) throws Exception {
        Bitmap bitmap = bitmap(input, width, height);
        final int maximum = Math.toIntExact(Math.min(limit, Integer.MAX_VALUE));
        ByteArrayOutputStream bytes = new ByteArrayOutputStream() {
            private void checkSize(int n) { if (n < 0 || count > maximum - n) throw new IllegalArgumentException("Encoded image exceeds limit"); }
            @Override public synchronized void write(int b) { checkSize(1); super.write(b); }
            @Override public synchronized void write(byte[] b, int offset, int length) { checkSize(length); super.write(b, offset, length); }
        };
        try {
            if (!bitmap.compress(jpeg ? Bitmap.CompressFormat.JPEG : Bitmap.CompressFormat.PNG, (int)Math.round(quality * 100), bytes))
                throw new IllegalStateException("System image encoding failed");
            return bytes.toByteArray();
        } finally { bitmap.recycle(); }
    }
    private static void check(int w, int h, long limit) {
        if (w <= 0 || h <= 0 || (long)w * h > limit / 4 || (long)w * h > Integer.MAX_VALUE / 4)
            throw new IllegalArgumentException("Image output exceeds limit");
    }
    private static Bitmap bitmap(ByteBuffer input, int w, int h) {
        check(w, h, (long)input.remaining() + 8);
        int[] colors = new int[Math.multiplyExact(w, h)];
        for (int i = 0; i < colors.length; i++) { int r = input.get()&255, g = input.get()&255, b = input.get()&255, a = input.get()&255; colors[i] = (a<<24)|(r<<16)|(g<<8)|b; }
        // setPixels accepts straight ARGB and stores the premultiplied Canvas form.
        return Bitmap.createBitmap(colors, w, h, Bitmap.Config.ARGB_8888);
    }
    private static byte[] pixels(Bitmap bitmap, long limit) {
        int w = bitmap.getWidth(), h = bitmap.getHeight(); check(w, h, limit);
        ByteBuffer output = ByteBuffer.allocate(Math.addExact(8, Math.multiplyExact(Math.multiplyExact(w, h), 4))).order(java.nio.ByteOrder.LITTLE_ENDIAN);
        output.putInt(w).putInt(h);
        int[] row = new int[w];
        for (int y = 0; y < h; y++) {
            bitmap.getPixels(row, 0, w, 0, y, w, 1);
            for (int color : row) output.put((byte)(color>>16)).put((byte)(color>>8)).put((byte)color).put((byte)(color>>24));
        }
        return output.array();
    }
}
