package dev.remdia;

import android.content.Context;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.media.MediaFormat;
import android.os.Build;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import org.json.JSONArray;
import org.json.JSONObject;

/** Private framework implementation of the ordinary remdia component protocol. */
public final class Bridge {
    static Context context;
    private Bridge() {}
    static void initialize(Context value) { context = value.getApplicationContext(); }
    public static String imageProbe(byte[] input) throws Exception { return Images.probe(input); }
    public static byte[] imageDecode(byte[] input, int w, int h, long pixels, long limit) throws Exception { return Images.decode(input, w, h, pixels, limit); }
    public static byte[] imageScale(ByteBuffer input, int w, int h, int dw, int dh, long limit) throws Exception { return Images.scale(input, w, h, dw, dh, limit); }
    public static byte[] imageEncode(ByteBuffer input, int w, int h, boolean jpeg, double quality, long limit) throws Exception { return Images.encode(input, w, h, jpeg, quality, limit); }
    public static boolean imageSupports(String mime) {
        if (Build.VERSION.SDK_INT >= 29) return android.graphics.ImageDecoder.isMimeTypeSupported(mime);
        // API 28 has ImageDecoder but no isMimeTypeSupported query. Advertise
        // only the mandatory image formats; do not infer optional HEIF support.
        return mime.equals("image/jpeg") || mime.equals("image/png") || mime.equals("image/webp") || mime.equals("image/gif") || mime.equals("image/bmp");
    }
    public static abstract class Node implements AutoCloseable {
        JSONObject descriptor;
        long generation = 1;
        boolean done;
        public final String info() { return descriptor.toString(); }
        public boolean submit(String header, ByteBuffer bytes) throws Exception { throw new UnsupportedOperationException("Component has no input"); }
        public abstract Output receive() throws Exception;
        public Output command(String command) throws Exception { return null; }
        public final boolean ended() { return done; }
        public abstract void drain() throws Exception;
        public abstract void reset(long generation, long targetUs) throws Exception;
        public abstract void close() throws java.io.IOException;
    }
    public static final class Output {
        public final String header;
        public final byte[] bytes;
        Output(JSONObject header, byte[] bytes) { this.header = header.toString(); this.bytes = bytes; }
    }
    public static Node open(String component, String settings) throws Exception {
        if (Build.VERSION.SDK_INT < 28) throw new UnsupportedOperationException("Android API 28+ required");
        JSONObject s = new JSONObject(settings);
        switch (component) {
            case "android.demux": return new Demux(s);
            case "android.audio.decode": return new Decoder(s, false);
            case "android.video.decode": return new Decoder(s, true);
            case "android.audio.encode": return new Encoder(s);
            case "android.mux": return new Mux(s);
            case "android.audio.capabilities":
            case "android.video.capabilities":
            case "android.demux.capabilities": return new Capabilities(component, s);
            default: throw new UnsupportedOperationException("Unknown Android component");
        }
    }
    static JSONObject port(String kind) throws Exception { return new JSONObject().put("kind", kind); }
    static JSONObject descriptor(String input, String output, long max, JSONObject media) throws Exception {
        return new JSONObject().put("input", input == null ? JSONObject.NULL : port(input))
            .put("output", port(output)).put("max_output_bytes", max).put("media", media == null ? JSONObject.NULL : media);
    }
    static JSONObject header(long generation, int stream, JSONObject info) throws Exception {
        return new JSONObject().put("generation", generation).put("stream_id", stream).put("info", info);
    }
    static JSONObject bytesInfo(String format) throws Exception { return new JSONObject().put("kind", "bytes").put("format", format); }
    static long timestamp(JSONObject packet) throws Exception {
        JSONObject tb = packet.getJSONObject("time_base");
        if (tb.getLong("num") <= 0 || tb.getLong("den") <= 0) throw new IllegalArgumentException("Invalid packet time base");
        java.math.BigInteger value = java.math.BigInteger.valueOf(packet.getLong("pts"))
            .multiply(java.math.BigInteger.valueOf(tb.getLong("num")))
            .multiply(java.math.BigInteger.valueOf(1000000))
            .divide(java.math.BigInteger.valueOf(tb.getLong("den")));
        if (value.bitLength() > 63) throw new ArithmeticException("Packet timestamp overflow");
        return value.longValue();
    }
    static String mime(String codec) {
        switch (codec) {
            case "mp3": return "audio/mpeg";
            case "aac": return "audio/mp4a-latm";
            case "flac": return "audio/flac";
            case "opus": return "audio/opus";
            case "vorbis": return "audio/vorbis";
            case "h264": return "video/avc";
            case "hevc": return "video/hevc";
            case "vp8": return "video/x-vnd.on2.vp8";
            case "vp9": return "video/x-vnd.on2.vp9";
            case "av1": return "video/av01";
            case "ac3": return "audio/ac3";
            case "eac3": return "audio/eac3";
            default: return codec.startsWith("pcm_") ? "audio/raw" : codec;
        }
    }
    static String codec(String mime) {
        for (String c : new String[]{"mp3","aac","flac","opus","vorbis","h264","hevc","vp8","vp9","av1","ac3","eac3"})
            if (mime(c).equals(mime)) return c;
        return mime.equals("audio/raw") ? "pcm_s16le" : mime;
    }
    static int integer(MediaFormat f, String key, int fallback) { return f.containsKey(key) ? f.getInteger(key) : fallback; }
    static byte[] copy(ByteBuffer bytes) { byte[] result = new byte[bytes.remaining()]; bytes.get(result); return result; }
    static MediaFormat format(JSONObject track) throws Exception {
        JSONObject initialization = track.optJSONObject("initialization");
        MediaFormat f = track.getString("kind").equals("video")
            ? MediaFormat.createVideoFormat(mime(track.getString("codec")), track.getInt("width"), track.getInt("height"))
            : MediaFormat.createAudioFormat(mime(track.getString("codec")), track.getInt("sample_rate"), track.getInt("channels"));
        if (initialization != null) {
            for (int i = 0; i < 3; i++) {
                JSONArray data = initialization.optJSONArray("csd-" + i);
                if (data != null) { byte[] bytes = new byte[data.length()]; for (int n = 0; n < bytes.length; n++) bytes[n] = (byte)data.getInt(n); f.setByteBuffer("csd-" + i, ByteBuffer.wrap(bytes)); }
            }
            if (track.getString("kind").equals("audio")) f.setInteger(MediaFormat.KEY_PCM_ENCODING, initialization.optInt("pcm_encoding", 2));
        }
        return f;
    }
    static JSONObject initialization(MediaFormat f) throws Exception {
        JSONObject result = new JSONObject().put("mime", f.getString(MediaFormat.KEY_MIME))
            .put("pcm_encoding", integer(f, MediaFormat.KEY_PCM_ENCODING, 2))
            .put("encoder_delay", integer(f, MediaFormat.KEY_ENCODER_DELAY, 0))
            .put("encoder_padding", integer(f, MediaFormat.KEY_ENCODER_PADDING, 0));
        for (int i = 0; i < 3; i++) if (f.containsKey("csd-" + i)) {
            byte[] bytes = copy(f.getByteBuffer("csd-" + i).duplicate());
            JSONArray array = new JSONArray(); for (byte b : bytes) array.put(b & 255);
            result.put("csd-" + i, array);
        }
        return result;
    }
    static MediaCodecInfo findCodec(MediaFormat format, boolean encoder, boolean hardware) throws Exception {
        String mime = format.getString(MediaFormat.KEY_MIME);
        MediaCodecInfo software = null;
        for (MediaCodecInfo info : new MediaCodecList(MediaCodecList.REGULAR_CODECS).getCodecInfos()) {
            if (info.isEncoder() != encoder) continue;
            for (String type : info.getSupportedTypes()) if (type.equalsIgnoreCase(mime)) {
                MediaCodecInfo.CodecCapabilities caps = info.getCapabilitiesForType(type);
                if (!caps.isFormatSupported(format)) continue;
                if (!encoder && mime.startsWith("video/")) {
                    boolean cpu = false;
                    for (int color : caps.colorFormats) if (color == MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible) cpu = true;
                    if (!cpu) continue;
                }
                boolean sw = Build.VERSION.SDK_INT >= 29 ? info.isSoftwareOnly() : info.getName().startsWith("OMX.google.") || info.getName().startsWith("c2.android.");
                if (!hardware && !sw) continue;
                if (!sw && hardware) return info;
                if (software == null) software = info;
            }
        }
        if (software == null) throw new UnsupportedOperationException("No system codec for " + mime);
        return software;
    }
    static final class Capabilities extends Node {
        final byte[] value;
        boolean emitted;
        Capabilities(String component, JSONObject settings) throws Exception {
            JSONObject caps = new JSONObject().put("provider", "android").put("api", Build.VERSION.SDK_INT);
            if (component.equals("android.demux.capabilities")) {
                String source = settings.optString("source", "");
                boolean supported = !source.contains("://") || source.startsWith("fd://") || source.startsWith("content://");
                caps.put("supported", supported).put("reason", supported ? "" : "Only local files, content URIs and file descriptors are admitted");
            } else {
                JSONArray decode = new JSONArray(), encode = new JSONArray(), codecs = new JSONArray();
                for (MediaCodecInfo info : new MediaCodecList(MediaCodecList.REGULAR_CODECS).getCodecInfos()) for (String type : info.getSupportedTypes()) {
                    String kind = component.startsWith("android.audio") ? "audio/" : "video/";
                    if (!type.startsWith(kind)) continue;
                    (info.isEncoder() ? encode : decode).put(codec(type));
                    JSONObject c = new JSONObject().put("name", info.getName()).put("mime", type).put("encoder", info.isEncoder());
                    if (Build.VERSION.SDK_INT >= 29) c.put("hardware", info.isHardwareAccelerated());
                    codecs.put(c);
                }
                JSONArray implementedEncode = new JSONArray();
                if (component.startsWith("android.audio")) {
                    decode.put("pcm_s16le").put("pcm_f32le");
                    for (int i = 0; i < encode.length(); i++) if (encode.getString(i).equals("aac")) { implementedEncode.put("aac"); break; }
                }
                caps.put("decode", decode).put("encode", implementedEncode).put("system_encode", encode).put("codecs", codecs)
                    .put("resample", false).put("pcm_convert", "android.audio.convert").put("mp3_gapless", false);
            }
            value = caps.toString().getBytes(StandardCharsets.UTF_8);
            if (value.length > 65536) throw new IllegalArgumentException("Capability list exceeds limit");
            descriptor = descriptor(null, "bytes", 65536, null);
        }
        public Output receive() throws Exception { if (emitted) { done = true; return null; } emitted = true; return new Output(header(generation, 0, bytesInfo("json")), value); }
        public void drain() { done = true; }
        public void reset(long g, long target) { generation = g; emitted = done = false; }
        public void close() {}
    }
}
