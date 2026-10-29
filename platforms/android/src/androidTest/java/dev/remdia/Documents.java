package dev.remdia;
import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.res.AssetFileDescriptor;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import java.io.File;
import java.io.FileNotFoundException;

/** Test-only provider exercises actual SAF-style content URI -> descriptor input. */
public final class Documents extends ContentProvider {
    public boolean onCreate() { return true; }
    public String getType(Uri uri) { return "application/octet-stream"; }
    public Cursor query(Uri uri, String[] projection, String selection, String[] args, String order) { return null; }
    public Uri insert(Uri uri, ContentValues values) { throw new UnsupportedOperationException(); }
    public int delete(Uri uri, String selection, String[] args) { throw new UnsupportedOperationException(); }
    public int update(Uri uri, ContentValues values, String selection, String[] args) { throw new UnsupportedOperationException(); }
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        String name = uri.getLastPathSegment();
        if (!"r".equals(mode) || name == null || name.contains("/") || name.contains("..")) throw new FileNotFoundException("Invalid test resource");
        return ParcelFileDescriptor.open(new File(getContext().getFilesDir(), name), ParcelFileDescriptor.MODE_READ_ONLY);
    }
    public AssetFileDescriptor openAssetFile(Uri uri, String mode) throws FileNotFoundException {
        return new AssetFileDescriptor(openFile(uri, mode), 0, AssetFileDescriptor.UNKNOWN_LENGTH);
    }
}
