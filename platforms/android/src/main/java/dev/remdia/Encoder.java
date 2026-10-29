package dev.remdia;

import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import org.json.JSONObject;

/** Independent system AAC-LC encoder; deliberately does not claim MP3 support. */
final class Encoder extends Bridge.Node {
    private MediaCodec codec;
    private final int rate, channels;
    private boolean draining, sentEos;
    private long lastPts;
    Encoder(JSONObject settings) throws Exception {
        rate = settings.optInt("rate", 16000); channels = settings.optInt("channels", 1);
        if (rate < 8000 || rate > 48000 || channels < 1 || channels > 2) throw new IllegalArgumentException("Unsupported AAC input profile");
        if (!settings.optString("codec", "aac").equals("aac")) throw new UnsupportedOperationException("Only AAC-LC system encoding is admitted");
        MediaFormat format = MediaFormat.createAudioFormat("audio/mp4a-latm", rate, channels);
        format.setInteger(MediaFormat.KEY_AAC_PROFILE, MediaCodecInfo.CodecProfileLevel.AACObjectLC);
        int bitrate = settings.optInt("bitrate", 64000);
        if (bitrate < 16000 || bitrate > 320000) throw new IllegalArgumentException("Invalid AAC bitrate");
        format.setInteger(MediaFormat.KEY_BIT_RATE, bitrate);
        try {
            MediaCodecInfo selected = Bridge.findCodec(format, true, true);
            codec = MediaCodec.createByCodecName(selected.getName());
            codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE); codec.start();
            descriptor = Bridge.descriptor("audio", "packet", 1024 * 1024, null);
        } catch (Exception e) { close(); throw e; }
    }
    public boolean submit(String header, ByteBuffer input) throws Exception {
        if (draining || done) throw new IllegalStateException("Encoder is draining");
        JSONObject h = new JSONObject(header), frame = h.getJSONObject("info");
        if (!frame.getString("kind").equals("audio") || !frame.getString("format").equals("f32le")
            || frame.getInt("rate") != rate || frame.getInt("channels") != channels || input.remaining() % (channels * 4) != 0)
            throw new IllegalArgumentException("AAC encoder requires matching interleaved float PCM");
        int index = codec.dequeueInputBuffer(0); if (index < 0) return false;
        ByteBuffer target = codec.getInputBuffer(index).order(ByteOrder.LITTLE_ENDIAN); target.clear(); input.order(ByteOrder.LITTLE_ENDIAN);
        if (input.remaining() / 2 > target.remaining()) throw new IllegalArgumentException("PCM block exceeds system encoder capacity; split into 1024-frame blocks");
        while (input.hasRemaining()) {
            float sample = input.getFloat(); if (!Float.isFinite(sample)) throw new IllegalArgumentException("Non-finite PCM");
            target.putShort((short)Math.max(-32768, Math.min(32767, Math.round(sample * 32768))));
        }
        generation = h.getLong("generation"); lastPts = frame.getLong("pts_us");
        codec.queueInputBuffer(index, 0, target.position(), lastPts, 0); return true;
    }
    public Bridge.Output receive() throws Exception {
        if (done) return null;
        if (draining && !sentEos) {
            int index = codec.dequeueInputBuffer(0);
            if (index >= 0) { codec.queueInputBuffer(index, 0, 0, Math.max(lastPts, 0), MediaCodec.BUFFER_FLAG_END_OF_STREAM); sentEos = true; }
        }
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo(); int index = codec.dequeueOutputBuffer(info, 0);
        if (index < 0) return null;
        try {
            if ((info.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0) done = true;
            if (info.size == 0 || (info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0) return null;
            if (info.size > descriptor.getLong("max_output_bytes")) throw new IllegalArgumentException("Encoded packet exceeds limit");
            ByteBuffer data = codec.getOutputBuffer(index).duplicate(); data.position(info.offset).limit(Math.addExact(info.offset, info.size));
            JSONObject packet = new JSONObject().put("kind", "packet").put("pts", info.presentationTimeUs).put("dts", info.presentationTimeUs)
                .put("time_base", new JSONObject().put("num", 1).put("den", 1000000)).put("duration", 0)
                .put("trim_start", 0).put("trim_end", 0).put("keyframe", true);
            return new Bridge.Output(Bridge.header(generation, 0, packet), Bridge.copy(data));
        } finally { codec.releaseOutputBuffer(index, false); }
    }
    public void drain() { draining = true; }
    public void reset(long g, long target) { codec.flush(); generation = g; done = draining = sentEos = false; }
    public void close() { if (codec != null) { try { codec.stop(); } finally { codec.release(); codec = null; } } }
}
