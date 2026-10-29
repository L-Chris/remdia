package dev.remdia;

import android.media.MediaCodec;
import android.media.MediaFormat;
import android.media.MediaMuxer;
import java.io.File;
import java.io.FileInputStream;
import java.nio.ByteBuffer;
import java.util.HashMap;
import org.json.JSONArray;
import org.json.JSONObject;

/** System MP4/WebM mux; private bounded disk spool, then bounded Bytes to any sink. */
final class Mux extends Bridge.Node {
    private MediaMuxer mux;
    private File spool;
    private FileInputStream input;
    private final HashMap<Integer, Integer> tracks = new HashMap<>();
    private final long limit;
    private long read;
    private boolean draining;
    Mux(JSONObject settings) throws Exception {
        limit = settings.optLong("max_output_bytes", 256L * 1024 * 1024);
        if (limit <= 0 || limit > 1024L * 1024 * 1024) throw new IllegalArgumentException("Invalid mux spool limit");
        if (Bridge.context == null) throw new IllegalStateException("Remdia.initialize(context) is required for mux spooling");
        try {
            spool = File.createTempFile("remdia-mux-", ".part", Bridge.context.getCacheDir());
            String format = settings.optString("format", "mp4");
            if (!format.equals("mp4") && !format.equals("webm")) throw new UnsupportedOperationException("Unsupported mux format");
            mux = new MediaMuxer(spool.getAbsolutePath(), format.equals("mp4") ? MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4 : MediaMuxer.OutputFormat.MUXER_OUTPUT_WEBM);
            JSONArray list = settings.optJSONArray("tracks");
            if (list == null) {
                int rate = settings.optInt("rate", 16000), channels = settings.optInt("channels", 1);
                int[] rates = {96000,88200,64000,48000,44100,32000,24000,22050,16000,12000,11025,8000,7350};
                int position = -1; for (int i = 0; i < rates.length; i++) if (rates[i] == rate) position = i;
                if (position < 0 || channels < 1 || channels > 2 || !format.equals("mp4")) throw new IllegalArgumentException("Explicit track descriptions required");
                int config = (2 << 11) | (position << 7) | (channels << 3);
                MediaFormat f = MediaFormat.createAudioFormat("audio/mp4a-latm", rate, channels);
                f.setByteBuffer("csd-0", ByteBuffer.wrap(new byte[]{(byte)(config >> 8), (byte)config}));
                tracks.put(0, mux.addTrack(f));
            } else for (int i = 0; i < list.length(); i++) {
                JSONObject track = list.getJSONObject(i);
                if (tracks.put(track.getInt("index"), mux.addTrack(Bridge.format(track))) != null) throw new IllegalArgumentException("Duplicate mux track");
            }
            mux.start(); descriptor = Bridge.descriptor("packet", "bytes", 65536, null);
        } catch (Exception e) { close(); throw e; }
    }
    public boolean submit(String header, ByteBuffer bytes) throws Exception {
        if (draining) throw new IllegalStateException("Mux is draining");
        JSONObject h = new JSONObject(header), packet = h.getJSONObject("info");
        if (!packet.getString("kind").equals("packet")) throw new IllegalArgumentException("Expected compressed packet");
        Integer track = tracks.get(h.getInt("stream_id")); if (track == null) throw new IllegalArgumentException("Unknown mux stream");
        if (spool.length() > limit - bytes.remaining()) throw new IllegalArgumentException("Mux spool exceeds limit");
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        info.set(0, bytes.remaining(), Bridge.timestamp(packet), packet.optBoolean("keyframe", false) ? MediaCodec.BUFFER_FLAG_KEY_FRAME : 0);
        mux.writeSampleData(track, bytes, info); generation = h.getLong("generation"); return true;
    }
    public Bridge.Output receive() throws Exception {
        if (done || !draining) return null;
        byte[] bytes = new byte[65536]; int count = input.read(bytes);
        if (count < 0) { done = true; return null; }
        read += count; if (read > limit) throw new IllegalArgumentException("Mux output exceeds limit");
        return new Bridge.Output(Bridge.header(generation, 0, Bridge.bytesInfo("container")), java.util.Arrays.copyOf(bytes, count));
    }
    public void drain() throws Exception {
        if (draining) return;
        mux.stop(); mux.release(); mux = null;
        if (spool.length() > limit) throw new IllegalArgumentException("Final mux output exceeds limit");
        input = new FileInputStream(spool); draining = true;
    }
    public void reset(long g, long target) { throw new UnsupportedOperationException("Mux graphs are single pass"); }
    public void close() throws java.io.IOException {
        try { if (mux != null) { mux.release(); mux = null; } }
        finally { try { if (input != null) { input.close(); input = null; } } finally { if (spool != null && spool.exists() && !spool.delete()) spool.deleteOnExit(); } }
    }
}
