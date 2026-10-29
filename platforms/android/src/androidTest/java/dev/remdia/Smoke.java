package dev.remdia;

import android.app.Instrumentation;
import android.content.Context;
import android.os.Bundle;
import android.os.Debug;
import android.os.ParcelFileDescriptor;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;
import org.json.JSONArray;
import org.json.JSONObject;

/** Offline certification of the real AAR, shared graph and installed codecs. */
public final class Smoke extends Instrumentation {
    private final JSONArray passed = new JSONArray();
    private Context context;
    private boolean systemOnly;
    private final AtomicLong peakPss = new AtomicLong();
    private final AtomicLong peakNative = new AtomicLong();
    @Override public void onCreate(Bundle arguments) { super.onCreate(arguments); systemOnly = "true".equals(arguments.getString("system_only")); start(); }
    private void check(boolean value, String message) { if (!value) throw new AssertionError(message); }
    private void record(String name) {
        sample();
        passed.put(name); Bundle status = new Bundle(); status.putString("stream", "PASS " + name + "\n"); sendStatus(0, status);
    }
    @Override public void onStart() {
        JSONObject report = new JSONObject(); long started = System.nanoTime();
        AtomicBoolean sampling = new AtomicBoolean(true);
        sample(); final long baseline = peakPss.get();
        Thread sampler = new Thread(() -> { while (sampling.get()) { sample(); try { Thread.sleep(25); } catch (InterruptedException stop) { Thread.currentThread().interrupt(); break; } } });
        sampler.start();
        try {
            context = getTargetContext();
            for (String name : context.getAssets().list("fixtures")) try (InputStream input = context.getAssets().open("fixtures/" + name); FileOutputStream out = new FileOutputStream(new File(context.getFilesDir(), name))) {
                byte[] buffer = new byte[8192]; int n; while ((n = input.read(buffer)) >= 0) out.write(buffer, 0, n);
            }
            Remdia.initialize(context); JSONObject capabilities = Remdia.capabilities(); report.put("capabilities", capabilities);
            check(capabilities.getJSONObject("images").getJSONArray("decode").length() > 0, "Missing system image capabilities"); record("runtime-capabilities");
            images();
            artwork();
            for (String file : new String[]{"audio.wav", "audio.mp3", "audio-tail.flac", "audio.opus", "audio-vorbis.ogg"}) {
                long count = audio(file(file), true); check(count > 1000, "No PCM: " + file); record((systemOnly ? "decode-native-rate-" : "decode-src-") + file);
            }
            seekAndOwnership(); descriptors(); video("av.mp4"); video("av.mkv"); thumbnail(); cancellation(); transcode();
            sample();
            report.put("passed", passed).put("system_only_package", systemOnly).put("app_peak_pss_kib", peakPss.get()).put("app_baseline_pss_kib", baseline)
                .put("app_peak_native_heap_bytes", peakNative.get())
                .put("page_size", android.system.Os.sysconf(android.system.OsConstants._SC_PAGESIZE))
                .put("api", android.os.Build.VERSION.SDK_INT).put("model", android.os.Build.MODEL)
                .put("elapsed_seconds", (System.nanoTime()-started)/1e9).put("success", true);
        } catch (Throwable error) {
            try { report.put("success", false).put("passed", passed).put("error", android.util.Log.getStackTraceString(error)); } catch (Exception ignored) {}
        }
        sampling.set(false); try { sampler.join(); } catch (InterruptedException stop) { Thread.currentThread().interrupt(); }
        try { Files.write(new File(context.getFilesDir(), "report.json").toPath(), report.toString(2).getBytes(java.nio.charset.StandardCharsets.UTF_8)); } catch (Exception ignored) {}
        Bundle result = new Bundle(); result.putString("stream", report.toString()); finish(report.optBoolean("success") ? -1 : 0, result);
    }
    private void sample() {
        Debug.MemoryInfo info = new Debug.MemoryInfo(); Debug.getMemoryInfo(info);
        peakPss.accumulateAndGet(info.getTotalPss(), Math::max);
        peakNative.accumulateAndGet(Debug.getNativeHeapAllocatedSize(), Math::max);
    }
    private String file(String name) { return new File(context.getFilesDir(), name).getAbsolutePath(); }
    private JSONObject request(String path) throws Exception { return Remdia.request(path).put("task", "audio").put("audio_rate", systemOnly ? 0 : 16000).put("audio_channels", 1); }
    private long audio(String path, boolean resample) throws Exception {
        JSONObject request = request(path);
        if (!resample) request.put("audio_rate", 48000).put("policy", "system_only");
        long count = 0;
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session session = Remdia.open(request, Remdia.DEFAULT_BUDGET, cancel)) {
            int rate = request.getInt("audio_rate");
            if (rate == 0) rate = session.info().getJSONObject("media").getJSONArray("tracks").getJSONObject(0).getInt("sample_rate");
            for (;;) { Remdia.Frame frame = session.read(); if (frame == null) break;
                try (Remdia.Frame owned = frame) {
                    JSONObject info = new JSONObject(owned.metadata).getJSONObject("info");
                    check(info.getString("kind").equals("audio") && info.getString("format").equals("f32le"), "Wrong PCM format");
                    check(info.getInt("rate") == rate && info.getInt("channels") == 1, "Wrong PCM profile");
                    ByteBuffer samples = owned.plane(0).order(ByteOrder.LITTLE_ENDIAN); check(samples.isReadOnly(), "Writable shared PCM");
                    while (samples.hasRemaining()) check(Float.isFinite(samples.getFloat()), "Non-finite sample");
                    count += info.getInt("samples");
                }
            }
        }
        return count;
    }
    private byte[] image(byte[] data, JSONArray operations) throws Exception {
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session job = Remdia.image(data, operations, Remdia.DEFAULT_BUDGET, cancel)) {
            Remdia.Frame held = job.read(); check(held != null, "Missing image result");
            try (Remdia.Frame frame = held) { ByteBuffer bytes = frame.plane(0); byte[] result = new byte[bytes.remaining()]; bytes.get(result); check(job.read() == null, "Unexpected extra image result"); return result; }
        }
    }
    private JSONObject op(String operation, JSONObject settings) throws Exception { return new JSONObject().put("operation", operation).put("settings", settings); }
    private JSONArray decode() throws Exception { return new JSONArray().put(op("decode", new JSONObject().put("max_width", 128).put("max_height", 128).put("max_pixels", 16384))); }
    private void images() throws Exception {
        byte[] png = Files.readAllBytes(new File(file("cover.png")).toPath());
        image(png, decode()); record("system-png-decode");
        image(Files.readAllBytes(new File(file("legacy-white.webp")).toPath()), decode()); record("system-webp-decode");
        JSONArray operations = decode().put(op("scale", new JSONObject().put("width", 32).put("height", 24)))
            .put(op("crop", new JSONObject().put("x", 1).put("y", 1).put("width", 20).put("height", 16)))
            .put(op("orient", new JSONObject().put("orientation", 6)))
            .put(op("encode", new JSONObject().put("format", "jpeg").put("quality", 0.95)));
        byte[] jpeg = image(png, operations); check((jpeg[0]&255)==255 && (jpeg[1]&255)==216, "Invalid system JPEG");
        image(jpeg, decode()); record("scale-crop-orient-jpeg-roundtrip");
        byte[] encoded = image(png, decode().put(op("encode", new JSONObject().put("format", "png"))));
        image(encoded, decode()); record("system-png-encode-roundtrip");
        boolean rejected = false;
        try { image(png, new JSONArray().put(op("decode", new JSONObject().put("max_source_pixels", 1)))); }
        catch (IllegalStateException expected) { rejected = true; }
        check(rejected, "Oversized source was accepted"); record("image-source-budget-rejection");
        android.graphics.Bitmap pixels = android.graphics.Bitmap.createBitmap(new int[]{0x80ff0000, 0xff00ff00}, 2, 1, android.graphics.Bitmap.Config.ARGB_8888);
        java.io.ByteArrayOutputStream bytes = new java.io.ByteArrayOutputStream();
        try { check(pixels.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, bytes), "Cannot create alpha fixture"); }
        finally { pixels.recycle(); }
        byte[] rgba = image(bytes.toByteArray(), decode());
        check(java.util.Arrays.equals(rgba, new byte[]{(byte)255,0,0,(byte)128,0,(byte)255,0,(byte)255}), "Straight RGBA alpha changed");
        record("system-image-straight-alpha");
        File rotated = new File(file("orientation.jpg"));
        android.graphics.Bitmap portrait = android.graphics.Bitmap.createBitmap(2, 3, android.graphics.Bitmap.Config.ARGB_8888);
        try (FileOutputStream output = new FileOutputStream(rotated)) { check(portrait.compress(android.graphics.Bitmap.CompressFormat.JPEG, 100, output), "Cannot create EXIF fixture"); }
        finally { portrait.recycle(); }
        android.media.ExifInterface exif = new android.media.ExifInterface(rotated.getAbsolutePath());
        exif.setAttribute(android.media.ExifInterface.TAG_ORIENTATION, "6"); exif.saveAttributes();
        byte[] oriented = Files.readAllBytes(rotated.toPath());
        JSONObject probe = new JSONObject(new String(image(oriented, new JSONArray().put(op("probe", new JSONObject()))), java.nio.charset.StandardCharsets.UTF_8));
        check(probe.getInt("width") == 2 && probe.getInt("height") == 3 && probe.getInt("orientation") == 6, "Incorrect EXIF source geometry: " + probe);
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session job = Remdia.image(oriented, decode(), Remdia.DEFAULT_BUDGET, cancel); Remdia.Frame frame = job.read()) {
            check(frame != null, "Missing oriented image"); JSONObject info = new JSONObject(frame.metadata).getJSONObject("info").getJSONObject("image");
            check(info.getInt("width") == 3 && info.getInt("height") == 2, "EXIF orientation was not applied");
        }
        record("system-image-exif-probe-and-orient");
    }
    private void seekAndOwnership() throws Exception {
        Remdia.Frame held;
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session session = Remdia.open(request(file("audio-tail.flac")), Remdia.DEFAULT_BUDGET, cancel)) {
            held = session.read(); check(held != null, "Missing first frame"); long first = new JSONObject(held.metadata).getLong("generation");
            while (true) { Remdia.Frame frame = session.read(); if (frame == null) break; frame.close(); }
            session.seek(3000000);
            try (Remdia.Frame frame = session.read()) { check(frame != null, "Seek after EOF failed"); JSONObject h = new JSONObject(frame.metadata); check(h.getLong("generation") > first, "Stale generation"); check(h.getJSONObject("info").getLong("pts_us") >= 3000000, "Pre-target audio escaped"); }
        }
        try (Remdia.Frame frame = held) { check(frame.plane(0).remaining() > 0, "Frame invalidated by session close"); }
        record("flac-seek-eof-generation-held-frame");
    }
    private void descriptors() throws Exception {
        try (ParcelFileDescriptor fd = ParcelFileDescriptor.open(new File(file("audio.wav")), ParcelFileDescriptor.MODE_READ_ONLY);
             Remdia.Cancellation cancel = new Remdia.Cancellation();
             Remdia.Session session = Remdia.open(request("fd://" + fd.getFd() + "?offset=0&length=" + new File(file("audio.wav")).length()), Remdia.DEFAULT_BUDGET, cancel)) {
            try (Remdia.Frame frame = session.read()) { check(frame != null, "FD source failed"); }
        }
        record("file-descriptor-input");
        check(audio("content://dev.remdia.smoke.files/audio.wav", true) > 0, "Content URI failed"); record("content-uri-input");
        boolean rejected = false;
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session ignored = Remdia.open(request(file("audio-tail.flac")).put("audio_rate", 16000).put("policy", "system_only"), Remdia.DEFAULT_BUDGET, cancel)) { ignored.info(); }
        catch (IllegalStateException expected) { rejected = true; }
        check(rejected, "SystemOnly admitted third-party SRC"); record("system-only-src-policy");
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session session = Remdia.open(request(file("audio-tail.flac")).put("audio_rate", 0).put("policy", "system_only"), Remdia.DEFAULT_BUDGET, cancel)) {
            try (Remdia.Frame frame = session.read()) { check(frame != null, "Native-rate SystemOnly playback failed");
                check(new JSONObject(frame.metadata).getJSONObject("info").getInt("rate") == session.info().getJSONObject("media").getJSONArray("tracks").getJSONObject(0).getInt("sample_rate"), "Source sample rate was changed"); }
        }
        record("system-only-native-rate-playback");
    }
    private void artwork() throws Exception {
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session session = Remdia.open(request(file("cover.flac")).put("task", "probe"), Remdia.DEFAULT_BUDGET, cancel)) {
            try (Remdia.Frame frame = session.artwork()) {
                check(frame != null, "Missing system embedded artwork"); ByteBuffer plane = frame.plane(0); byte[] bytes = new byte[plane.remaining()]; plane.get(bytes); image(bytes, decode());
            }
        }
        record("system-embedded-artwork-image-graph");
    }
    private void video(String name) throws Exception {
        int frames = 0;
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session session = Remdia.open(Remdia.request(file(name)), Remdia.DEFAULT_BUDGET, cancel)) {
            for (;;) { Remdia.Frame frame = session.read(); if (frame == null) break;
                try (Remdia.Frame held = frame) { JSONObject info = new JSONObject(held.metadata).getJSONObject("info");
                    if (info.getString("kind").equals("video")) { check(held.planeCount() == 2 && info.getString("format").equals("nv12"), "Invalid video planes"); check(held.plane(0).remaining() == info.getInt("width") * info.getInt("height"), "Invalid Y plane"); frames++; }
                }
            }
        }
        check(frames > 0, "No video frames"); record("system-video-" + name);
    }
    private void cancellation() throws Exception {
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session session = Remdia.open(request(file("audio.wav")), Remdia.DEFAULT_BUDGET, cancel)) {
            Thread worker = new Thread(cancel::cancel); worker.start(); worker.join();
            boolean stopped = false; try { Remdia.Frame frame = session.read(); if (frame != null) frame.close(); } catch (IllegalStateException expected) { stopped = true; }
            check(stopped, "Cross-thread cancellation failed");
        }
        record("cross-thread-cancellation");
    }
    private void thumbnail() throws Exception {
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session session = Remdia.open(Remdia.request(file("av.mp4")).put("task", "thumbnail"), Remdia.DEFAULT_BUDGET, cancel); Remdia.Frame frame = session.read()) {
            check(frame != null, "Missing video thumbnail");
            JSONObject info = new JSONObject(frame.metadata).getJSONObject("info");
            check(info.getString("kind").equals("image"), "Thumbnail did not use the image graph");
            JSONObject image = info.getJSONObject("image");
            check(image.getInt("width") > 0 && image.getInt("height") > 0 && image.getInt("width") <= 600 && image.getInt("height") <= 600, "Invalid thumbnail bounds");
        }
        record("system-video-thumbnail-image-graph");
    }
    private JSONObject node(String id, String component, JSONObject settings) throws Exception { return new JSONObject().put("id", id).put("component", component).put("settings", settings); }
    private JSONObject edge(String from, String to) throws Exception { return new JSONObject().put("from", from).put("to", to).put("max_buffers", 2).put("max_bytes", 32*1024*1024); }
    private void transcode() throws Exception {
        JSONObject track;
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session session = Remdia.open(request(file("audio.wav")).put("task", "probe"), Remdia.DEFAULT_BUDGET, cancel)) {
            track = session.info().getJSONObject("media").getJSONArray("tracks").getJSONObject(0);
        }
        String output = file("encoded.m4a"); Files.deleteIfExists(new File(output).toPath());
        int rate = systemOnly ? track.getInt("sample_rate") : 16000;
        JSONArray nodes = new JSONArray().put(node("demux", "android.demux", request(file("audio.wav"))))
            .put(node("decode", "android.audio.decode", new JSONObject().put("track", track).put("max_frame_bytes", 2*1024*1024)))
            .put(node("src", systemOnly ? "android.audio.convert" : "fallback.audio.resample", new JSONObject().put("rate", rate).put("channels", 1)))
            .put(node("encode", "android.audio.encode", new JSONObject().put("rate", rate).put("channels", 1)))
            .put(node("mux", "android.mux", new JSONObject().put("rate", rate).put("channels", 1)))
            .put(node("sink", "native.file.sink", new JSONObject().put("path", output)));
        JSONArray edges = new JSONArray().put(edge("demux","decode")).put(edge("decode","src")).put(edge("src","encode")).put(edge("encode","mux")).put(edge("mux","sink"));
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session graph = Remdia.graph(new JSONObject().put("nodes",nodes).put("edges",edges), Remdia.DEFAULT_BUDGET, cancel)) {
            for (;;) { Remdia.Frame frame = graph.read(); if (frame == null) break; frame.close(); }
        }
        check(new File(output).length() > 0 && audio(output, true) > 0, "AAC/MP4 graph roundtrip failed"); record("independent-filter-system-aac-mp4-sink-roundtrip");
        File race = new File(file("raced.m4a")); Files.deleteIfExists(race.toPath());
        nodes.getJSONObject(5).getJSONObject("settings").put("path", race.getAbsolutePath());
        boolean rejected = false;
        byte[] marker = new byte[]{1, 2, 3, 4};
        try (Remdia.Cancellation cancel = new Remdia.Cancellation(); Remdia.Session graph = Remdia.graph(new JSONObject().put("nodes",nodes).put("edges",edges), Remdia.DEFAULT_BUDGET, cancel)) {
            Files.write(race.toPath(), marker);
            try { for (;;) { Remdia.Frame frame = graph.read(); if (frame == null) break; frame.close(); } }
            catch (IllegalStateException expected) { rejected = true; }
        }
        check(rejected && java.util.Arrays.equals(marker, Files.readAllBytes(race.toPath())), "Output race overwrote an existing file");
        record("atomic-file-sink-no-overwrite-race");
    }
}
