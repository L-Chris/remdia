package dev.remdia;

import android.graphics.ImageFormat;
import android.graphics.Rect;
import android.media.AudioFormat;
import android.media.Image;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.os.Build;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import org.json.JSONArray;
import org.json.JSONObject;

final class Decoder extends Bridge.Node {
    private MediaCodec codec;
    private final boolean video, hardware;
    private final JSONObject track;
    private final int stream;
    private final long limit;
    private MediaFormat output;
    private boolean draining, sentEos;
    private long lastPts;
    private int delay, padding;
    private byte[] tail = new byte[0];
    private long tailPts;
    private long audioAnchor, audioEmitted;
    private boolean audioAnchored;
    Decoder(JSONObject settings, boolean video) throws Exception {
        this.video = video;
        this.track = settings.getJSONObject("track");
        stream = track.getInt("index"); limit = settings.getLong("max_frame_bytes");
        MediaFormat format = Bridge.format(track);
        if (video) format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible);
        MediaCodecInfo selected = Bridge.findCodec(format, false, settings.optBoolean("hardware", true));
        hardware = Build.VERSION.SDK_INT >= 29 && selected.isHardwareAccelerated();
        try {
            codec = MediaCodec.createByCodecName(selected.getName());
            codec.configure(format, null, null, 0); codec.start();
            output = format;
            configureOutput(); resetTrim();
        } catch (Exception e) { close(); throw e; }
    }
    private void resetTrim() {
        JSONObject initialization = track.optJSONObject("initialization");
        delay = initialization == null ? 0 : initialization.optInt("encoder_delay", 0);
        padding = initialization == null ? 0 : initialization.optInt("encoder_padding", 0);
        if (delay < 0 || padding < 0 || delay > track.optInt("sample_rate", 48000) * 2 || padding > track.optInt("sample_rate", 48000) * 2)
            throw new IllegalArgumentException("Invalid gapless trim");
        tail = new byte[0];
        audioAnchor = audioEmitted = 0; audioAnchored = false;
    }
    private void configureOutput() throws Exception {
        long max = video ? videoSize()[2] : 2 * 1024 * 1024;
        if (max <= 0 || max > limit) throw new IllegalArgumentException("Decoded frame exceeds limit");
        descriptor = Bridge.descriptor("packet", video ? "video" : "audio", max, null);
    }
    private int[] videoSize() {
        int width = output.getInteger(MediaFormat.KEY_WIDTH), height = output.getInteger(MediaFormat.KEY_HEIGHT);
        int left = Bridge.integer(output, "crop-left", 0), top = Bridge.integer(output, "crop-top", 0);
        int w = Bridge.integer(output, "crop-right", width - 1) + 1 - left;
        int h = Bridge.integer(output, "crop-bottom", height - 1) + 1 - top;
        int transfer = Bridge.integer(output, MediaFormat.KEY_COLOR_TRANSFER, 3);
        if (w <= 0 || h <= 0 || (w & 1) != 0 || (h & 1) != 0 || left < 0 || top < 0 || w > width - left || h > height - top || transfer == 6 || transfer == 7)
            throw new UnsupportedOperationException("Only even-sized SDR video is admitted");
        return new int[]{w, h, Math.toIntExact(Math.multiplyExact((long)w, h) * 3 / 2)};
    }
    public boolean submit(String header, ByteBuffer bytes) throws Exception {
        if (draining || done) throw new IllegalStateException("Decoder is draining");
        int index = codec.dequeueInputBuffer(0); if (index < 0) return false;
        JSONObject h = new JSONObject(header), packet = h.getJSONObject("info");
        if (!packet.getString("kind").equals("packet")) throw new IllegalArgumentException("Expected packet");
        ByteBuffer input = codec.getInputBuffer(index); input.clear();
        if (bytes.remaining() > input.remaining()) throw new IllegalArgumentException("Packet exceeds codec input capacity");
        int size = bytes.remaining(); input.put(bytes); lastPts = Bridge.timestamp(packet);
        generation = h.getLong("generation");
        codec.queueInputBuffer(index, 0, size, lastPts, 0);
        return true;
    }
    public Bridge.Output receive() throws Exception {
        if (done) return null;
        if (draining && !sentEos) {
            int index = codec.dequeueInputBuffer(0);
            if (index >= 0) { codec.queueInputBuffer(index, 0, 0, Math.max(0, lastPts), MediaCodec.BUFFER_FLAG_END_OF_STREAM); sentEos = true; }
        }
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        int index = codec.dequeueOutputBuffer(info, 0);
        if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
            output = codec.getOutputFormat(); configureOutput();
            return new Bridge.Output(new JSONObject().put("reconfigured", true), new byte[0]);
        }
        if (index < 0) return null;
        try {
            if ((info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0) done = true;
            if (info.size == 0 || (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0) return null;
            return video ? video(index, info) : audio(index, info);
        } finally { codec.releaseOutputBuffer(index, false); }
    }
    private Bridge.Output audio(int index, MediaCodec.BufferInfo info) throws Exception {
        int rate = output.getInteger(MediaFormat.KEY_SAMPLE_RATE), channels = output.getInteger(MediaFormat.KEY_CHANNEL_COUNT);
        int encoding = Bridge.integer(output, MediaFormat.KEY_PCM_ENCODING, AudioFormat.ENCODING_PCM_16BIT);
        if (encoding != AudioFormat.ENCODING_PCM_FLOAT && encoding != AudioFormat.ENCODING_PCM_32BIT
            && encoding != AudioFormat.ENCODING_PCM_24BIT_PACKED && encoding != AudioFormat.ENCODING_PCM_8BIT && encoding != AudioFormat.ENCODING_PCM_16BIT)
            throw new UnsupportedOperationException("Unsupported PCM encoding");
        int sampleBytes = encoding == AudioFormat.ENCODING_PCM_FLOAT || encoding == AudioFormat.ENCODING_PCM_32BIT ? 4 : encoding == AudioFormat.ENCODING_PCM_24BIT_PACKED ? 3 : encoding == AudioFormat.ENCODING_PCM_8BIT ? 1 : 2;
        if (channels < 1 || channels > 6 || rate < 8000 || rate > 192000 || info.size % (channels * sampleBytes) != 0)
            throw new UnsupportedOperationException("Unsupported PCM layout");
        ByteBuffer input = codec.getOutputBuffer(index).duplicate().order(ByteOrder.LITTLE_ENDIAN);
        input.position(info.offset).limit(Math.addExact(info.offset, info.size));
        int samples = info.size / (channels * sampleBytes), skip = Math.min(samples, delay); delay -= skip;
        input.position(input.position() + skip * channels * sampleBytes); samples -= skip;
        long size = (long)samples * channels * 4;
        if (size + tail.length > descriptor.getLong("max_output_bytes")) throw new IllegalArgumentException("PCM output exceeds limit");
        ByteBuffer data = ByteBuffer.allocate(Math.toIntExact(size + tail.length)).order(ByteOrder.LITTLE_ENDIAN);
        data.put(tail);
        if (!audioAnchored && samples > 0) { audioAnchor = info.presentationTimeUs + (long)skip * 1000000 / rate; audioAnchored = true; }
        tailPts = audioAnchor + audioEmitted * 1000000 / rate;
        while (input.hasRemaining()) {
            float sample;
            if (encoding == AudioFormat.ENCODING_PCM_FLOAT) sample = input.getFloat();
            else if (sampleBytes == 4) sample = input.getInt() / 2147483648.0f;
            else if (sampleBytes == 3) { int n = (input.get() & 255) | ((input.get() & 255) << 8) | (input.get() << 16); sample = n / 8388608.0f; }
            else if (sampleBytes == 1) sample = ((input.get() & 255) - 128) / 128.0f;
            else sample = input.getShort() / 32768.0f;
            if (!Float.isFinite(sample)) throw new IllegalArgumentException("Non-finite PCM sample");
            data.putFloat(sample);
        }
        byte[] all = data.array(); int keep = Math.min(all.length, Math.multiplyExact(padding, channels * 4));
        int emit = all.length - keep; tail = java.util.Arrays.copyOfRange(all, emit, all.length);
        if (emit == 0) return null;
        JSONObject frame = new JSONObject().put("kind", "audio").put("pts_us", tailPts).put("rate", rate).put("channels", channels)
            .put("samples", emit / (channels * 4)).put("format", "f32le");
        audioEmitted += emit / (channels * 4);
        return new Bridge.Output(Bridge.header(generation, stream, frame), java.util.Arrays.copyOf(all, emit));
    }
    private Bridge.Output video(int index, MediaCodec.BufferInfo info) throws Exception {
        int[] dimensions = videoSize(); int w = dimensions[0], h = dimensions[1];
        try (Image image = codec.getOutputImage(index)) {
            if (image == null || image.getFormat() != ImageFormat.YUV_420_888) throw new UnsupportedOperationException("System codec did not expose 8-bit YUV planes");
            Rect crop = image.getCropRect();
            if (crop.width() != w || crop.height() != h || (crop.left & 1) != 0 || (crop.top & 1) != 0) throw new UnsupportedOperationException("Unsupported video aperture");
            Image.Plane[] p = image.getPlanes(); if (p.length != 3) throw new IllegalArgumentException("Invalid YUV planes");
            byte[] pixels = new byte[dimensions[2]];
            for (int plane = 0; plane < 3; plane++) {
                ByteBuffer bytes = p[plane].getBuffer(); int stride = p[plane].getRowStride(), pixel = p[plane].getPixelStride();
                int left = plane == 0 ? crop.left : crop.left / 2, top = plane == 0 ? crop.top : crop.top / 2;
                int rows = plane == 0 ? h : h / 2, cols = plane == 0 ? w : w / 2;
                for (int y = 0; y < rows; y++) for (int x = 0; x < cols; x++) {
                    int from = Math.addExact(bytes.position(), Math.addExact(Math.multiplyExact(y + top, stride), Math.multiplyExact(x + left, pixel)));
                    int to = plane == 0 ? y * w + x : w * h + y * w + x * 2 + plane - 1;
                    pixels[to] = bytes.get(from);
                }
            }
            JSONArray planes = new JSONArray().put(new JSONObject().put("stride", w).put("rows", h))
                .put(new JSONObject().put("stride", w).put("rows", h / 2));
            int standard = Bridge.integer(output, MediaFormat.KEY_COLOR_STANDARD, 0);
            if (standard == 6) throw new UnsupportedOperationException("BT.2020 video is outside the initial SDR profile");
            String color = standard == 2 || standard == 4 ? "bt601" : standard == 1 ? "bt709" : "unspecified";
            JSONObject frame = new JSONObject().put("kind", "video").put("pts_us", info.presentationTimeUs).put("duration_us", 0)
                .put("width", w).put("height", h).put("format", "nv12").put("planes", planes)
                .put("sample_aspect_ratio", new JSONObject().put("num", 1).put("den", 1)).put("color_space", color)
                .put("color_range", Bridge.integer(output, MediaFormat.KEY_COLOR_RANGE, 2) == 1 ? "full" : "limited")
                .put("color_primaries", color).put("color_transfer", "bt709").put("hardware_decoded", hardware);
            return new Bridge.Output(Bridge.header(generation, stream, frame), pixels);
        }
    }
    public void drain() { draining = true; }
    public void reset(long g, long target) { codec.flush(); generation = g; done = draining = sentEos = false; resetTrim(); if (target > 0) delay = 0; }
    public void close() { if (codec != null) { try { codec.stop(); } finally { codec.release(); codec = null; } } }
}
