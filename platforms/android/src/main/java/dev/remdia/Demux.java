package dev.remdia;

import android.content.res.AssetFileDescriptor;
import android.media.MediaExtractor;
import android.media.MediaFormat;
import android.media.MediaMetadataRetriever;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.system.Os;
import android.system.OsConstants;
import java.io.File;
import java.nio.ByteBuffer;
import org.json.JSONArray;
import org.json.JSONObject;

final class Demux extends Bridge.Node {
    private final MediaExtractor extractor = new MediaExtractor();
    private ParcelFileDescriptor fd;
    private final int limit;
    private long sourceOffset, sourceLength;
    Demux(JSONObject settings) throws Exception {
        limit = settings.getInt("max_packet_bytes");
        if (limit <= 0 || limit > 64 * 1024 * 1024) throw new IllegalArgumentException("Invalid packet limit");
        try {
            String source = settings.getString("source");
            long offset = 0, length = -1;
            if (source.startsWith("fd://")) {
                Uri uri = Uri.parse(source);
                fd = ParcelFileDescriptor.fromFd(Integer.parseInt(uri.getHost()));
                if (uri.getQueryParameter("offset") != null) offset = Long.parseLong(uri.getQueryParameter("offset"));
                if (uri.getQueryParameter("length") != null) length = Long.parseLong(uri.getQueryParameter("length"));
            } else if (source.startsWith("content://")) {
                if (Bridge.context == null) throw new IllegalStateException("Remdia.initialize(context) is required for content URIs");
                try (AssetFileDescriptor asset = Bridge.context.getContentResolver().openAssetFileDescriptor(Uri.parse(source), "r")) {
                    if (asset == null) throw new IllegalArgumentException("Cannot open document");
                    fd = ParcelFileDescriptor.dup(asset.getFileDescriptor()); offset = asset.getStartOffset(); length = asset.getDeclaredLength();
                }
            } else {
                if (source.contains("://")) throw new UnsupportedOperationException("Unsupported source scheme");
                fd = ParcelFileDescriptor.open(new File(source), ParcelFileDescriptor.MODE_READ_ONLY);
            }
            if (offset < 0 || length < -1) throw new IllegalArgumentException("Invalid source range");
            long size = Os.fstat(fd.getFileDescriptor()).st_size;
            Os.lseek(fd.getFileDescriptor(), 0, OsConstants.SEEK_CUR);
            if (length == -1) length = size - offset;
            if (length <= 0 || offset > size || length > size - offset) throw new IllegalArgumentException("Source range exceeds seekable file");
            extractor.setDataSource(fd.getFileDescriptor(), offset, length);
            sourceOffset = offset; sourceLength = length;
            JSONArray tracks = new JSONArray(); long duration = 0;
            for (int index = 0; index < extractor.getTrackCount(); index++) {
                MediaFormat f = extractor.getTrackFormat(index);
                String mime = f.getString(MediaFormat.KEY_MIME);
                String kind = mime.startsWith("audio/") ? "audio" : mime.startsWith("video/") ? "video" : "other";
                if (kind.equals("other")) continue;
                if (f.containsKey(MediaFormat.KEY_DURATION)) duration = Math.max(duration, f.getLong(MediaFormat.KEY_DURATION));
                JSONObject track = new JSONObject().put("index", index).put("kind", kind).put("codec", Bridge.codec(mime))
                    .put("time_base", new JSONObject().put("num", 1).put("den", 1000000))
                    .put("width", Bridge.integer(f, MediaFormat.KEY_WIDTH, 0)).put("height", Bridge.integer(f, MediaFormat.KEY_HEIGHT, 0))
                    .put("sample_rate", Bridge.integer(f, MediaFormat.KEY_SAMPLE_RATE, 0)).put("channels", Bridge.integer(f, MediaFormat.KEY_CHANNEL_COUNT, 0))
                    .put("attached_picture", false).put("metadata", new JSONObject()).put("initialization", Bridge.initialization(f).put("dts_available", false));
                tracks.put(track); extractor.selectTrack(index);
            }
            if (tracks.length() == 0) throw new UnsupportedOperationException("No audio or video tracks");
            JSONObject metadata = new JSONObject();
            MediaMetadataRetriever retriever = new MediaMetadataRetriever();
            try {
                retriever.setDataSource(fd.getFileDescriptor(), sourceOffset, sourceLength);
                int[] keys = {MediaMetadataRetriever.METADATA_KEY_TITLE, MediaMetadataRetriever.METADATA_KEY_ARTIST, MediaMetadataRetriever.METADATA_KEY_ALBUM};
                String[] names = {"title", "artist", "album"};
                for (int n = 0; n < keys.length; n++) { String value = retriever.extractMetadata(keys[n]); if (value != null && value.length() <= 4096) metadata.put(names[n], value); }
            } catch (RuntimeException unavailable) { /* Metadata is optional; extractor remains authoritative. */ }
            finally { retriever.release(); }
            JSONObject media = new JSONObject().put("container", "android-extractor").put("duration_us", duration == 0 ? JSONObject.NULL : duration)
                .put("origin_us", 0).put("metadata", metadata).put("tracks", tracks);
            descriptor = Bridge.descriptor(null, "packet", limit, media);
        } catch (Exception e) { close(); throw e; }
    }
    public Bridge.Output receive() throws Exception {
        if (done) return null;
        int index = extractor.getSampleTrackIndex();
        if (index < 0) { done = true; return null; }
        long size = extractor.getSampleSize();
        if (size < 0 || size > limit) throw new IllegalArgumentException("Compressed packet exceeds limit");
        ByteBuffer data = ByteBuffer.allocateDirect((int)size);
        long pts = extractor.getSampleTime(); int flags = extractor.getSampleFlags();
        if ((flags & MediaExtractor.SAMPLE_FLAG_ENCRYPTED) != 0) throw new UnsupportedOperationException("DRM packets are not admitted");
        int count = extractor.readSampleData(data, 0);
        if (count < 0 || count > size) throw new IllegalArgumentException("Invalid extractor sample");
        data.position(0).limit(count);
        JSONObject info = new JSONObject().put("kind", "packet").put("pts", pts).put("dts", pts).put("duration", 0)
            .put("time_base", new JSONObject().put("num", 1).put("den", 1000000)).put("trim_start", 0).put("trim_end", 0)
            .put("keyframe", (flags & MediaExtractor.SAMPLE_FLAG_SYNC) != 0);
        extractor.advance();
        return new Bridge.Output(Bridge.header(generation, index, info), Bridge.copy(data));
    }
    public void drain() { done = true; }
    public Bridge.Output command(String command) throws Exception {
        if (!new JSONObject(command).optString("op").equals("artwork")) return null;
        MediaMetadataRetriever retriever = new MediaMetadataRetriever();
        try {
            retriever.setDataSource(fd.getFileDescriptor(), sourceOffset, sourceLength);
            byte[] picture = retriever.getEmbeddedPicture();
            if (picture == null) return null;
            if (picture.length > 8 * 1024 * 1024) throw new IllegalArgumentException("Embedded artwork exceeds limit");
            return new Bridge.Output(Bridge.header(generation, 0, new JSONObject().put("kind", "artwork").put("codec", "system-image")), picture);
        } finally { retriever.release(); }
    }
    public void reset(long g, long target) { extractor.seekTo(Math.max(0, target), MediaExtractor.SEEK_TO_PREVIOUS_SYNC); generation = g; done = false; }
    public void close() throws java.io.IOException { extractor.release(); if (fd != null) { fd.close(); fd = null; } }
}
