package dev.remdia;

import android.content.Context;
import android.os.Build;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;
import org.json.JSONArray;
import org.json.JSONObject;

/** Java facade over the same native Engine and plugin graph as the Rust SDK. */
public final class Remdia {
    public static final long DEFAULT_BUDGET = 96L * 1024 * 1024;
    private static boolean initialized;
    private Remdia() {}
    public static synchronized void initialize(Context context) throws Exception {
        if (initialized) return;
        if (Build.VERSION.SDK_INT < 28) throw new UnsupportedOperationException("Android API 28+ required");
        Bridge.initialize(context);
        String manifests = null;
        for (String abi : Build.SUPPORTED_ABIS) {
            try (InputStream input = context.getAssets().open("remdia/" + abi + "/manifests.json")) {
                ByteArrayOutputStream bytes = new ByteArrayOutputStream(); byte[] buffer = new byte[4096]; int count;
                while ((count = input.read(buffer)) >= 0) { if (bytes.size() > 65536 - count) throw new IllegalArgumentException("Oversized manifest"); bytes.write(buffer, 0, count); }
                manifests = bytes.toString("UTF-8"); break;
            } catch (java.io.FileNotFoundException missing) { /* Try the next packaged ABI. */ }
        }
        if (manifests == null) throw new UnsupportedOperationException("No remdia pack for this Android ABI");
        JSONArray packs = new JSONArray(manifests);
        boolean unified = false;
        for (int i = 0; i < packs.length(); i++) {
            String name = packs.getJSONObject(i).getString("library");
            if (!name.startsWith("libremdia_") || !name.endsWith(".so") || name.contains("/") || name.contains("\\")) throw new IllegalArgumentException("Invalid library name");
            System.loadLibrary(name.substring(3, name.length() - 3));
            if (name.equals("libremdia_sdk.so")) unified = true;
        }
        if (!unified) System.loadLibrary("remdia_android_host"); nativeInit(manifests); initialized = true;
    }
    public static JSONObject capabilities() throws Exception { return new JSONObject(nativeCapabilities(DEFAULT_BUDGET)); }
    public static JSONObject request(String source) throws Exception {
        return new JSONObject().put("source", source).put("policy", "system_preferred").put("task", "playback")
            .put("audio_rate", 0).put("audio_channels", 2).put("hardware", true).put("threads", 2).put("timeout_ms", 30000)
            .put("max_frame_bytes", 32 * 1024 * 1024).put("max_packet_bytes", 8 * 1024 * 1024);
    }
    /** Open/read/seek/close a Session on one worker; cancellation may cross threads. */
    public static Session open(JSONObject request, long budget, Cancellation cancellation) {
        cancellation.claim();
        try { nativeOpen(cancellation.id, request.toString(), budget); return new Session(cancellation); }
        catch (RuntimeException failure) { cancellation.close(); throw failure; }
    }
    public static Session image(byte[] input, JSONArray operations, long budget, Cancellation cancellation) {
        cancellation.claim();
        try { nativeImage(cancellation.id, input, operations.toString(), budget); return new Session(cancellation); }
        catch (RuntimeException failure) { cancellation.close(); throw failure; }
    }
    public static Session graph(JSONObject graph, long budget, Cancellation cancellation) {
        cancellation.claim();
        try { nativeGraph(cancellation.id, graph.toString(), budget); return new Session(cancellation); }
        catch (RuntimeException failure) { cancellation.close(); throw failure; }
    }
    public static final class Cancellation implements AutoCloseable {
        private volatile long id = nativeCancelCreate();
        private final AtomicBoolean claimed = new AtomicBoolean();
        private void claim() { if (id == 0 || !claimed.compareAndSet(false, true)) throw new IllegalStateException("Cancellation belongs to one job"); }
        public void cancel() { long current = id; if (current != 0) nativeCancel(current); }
        @Override public synchronized void close() { if (id != 0) { nativeReleaseCancellation(id); id = 0; } }
    }
    public static final class Session implements AutoCloseable {
        private final Cancellation cancellation;
        private long id;
        private Session(Cancellation cancellation) { this.cancellation = cancellation; id = cancellation.id; }
        public Frame read() { if (id == 0) throw new IllegalStateException("Session is closed"); return nativeRead(id); }
        public JSONObject info() throws Exception { if (id == 0) throw new IllegalStateException("Session is closed"); return new JSONObject(nativeInfo(id)); }
        public Frame artwork() { if (id == 0) throw new IllegalStateException("Session is closed"); return nativeArtwork(id); }
        public void seek(long microseconds) { if (id == 0) throw new IllegalStateException("Session is closed"); nativeSeek(id, microseconds); }
        public void cancel() { cancellation.cancel(); }
        @Override public void close() { if (id != 0) { nativeClose(id); id = 0; cancellation.close(); } }
    }
    /** Native planes survive seek/session close until this Frame closes. */
    public static final class Frame implements AutoCloseable {
        private final AtomicLong id;
        public final String metadata;
        private final ByteBuffer[] planes;
        Frame(long id, String metadata, ByteBuffer[] planes) {
            this.id = new AtomicLong(id); this.metadata = metadata; this.planes = planes;
            for (int i = 0; i < planes.length; i++) planes[i] = planes[i].asReadOnlyBuffer();
        }
        public int planeCount() { return planes.length; }
        /** Do not access a retained view after Frame.close(). */
        public ByteBuffer plane(int index) { if (id.get() == 0) throw new IllegalStateException("Frame is closed"); return planes[index].asReadOnlyBuffer(); }
        @Override public void close() { long current = id.getAndSet(0); if (current != 0) nativeReleaseFrame(current); }
    }
    private static native void nativeInit(String manifests);
    private static native long nativeCancelCreate();
    private static native void nativeCancel(long id);
    private static native void nativeReleaseCancellation(long id);
    private static native void nativeOpen(long id, String request, long budget);
    private static native void nativeImage(long id, byte[] input, String operations, long budget);
    private static native void nativeGraph(long id, String graph, long budget);
    private static native Frame nativeRead(long id);
    private static native String nativeInfo(long id);
    private static native Frame nativeArtwork(long id);
    private static native void nativeSeek(long id, long target);
    private static native void nativeClose(long id);
    private static native void nativeReleaseFrame(long id);
    private static native String nativeCapabilities(long budget);
}
