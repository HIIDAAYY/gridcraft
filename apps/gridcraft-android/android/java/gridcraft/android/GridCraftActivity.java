// GridCraft for Android: the activity that hosts the Rust app (libgridcraft_android.so).
//
// NativeActivity runs the app; this subclass adds what NDK code can't do alone. Rust calls the
// public methods below from its own thread (see src/bridge.rs); they start work on the UI thread
// and return at once. Answers go back as files in getFilesDir()/inbox, written under a .tmp name
// and renamed when complete (see src/mailbox.rs for the format).
//
// MIT OR Apache-2.0, like the rest of GridCraft.

package gridcraft.android;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import android.content.SharedPreferences;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.ParcelFileDescriptor;
import android.print.PageRange;
import android.print.PrintAttributes;
import android.print.PrintDocumentAdapter;
import android.print.PrintDocumentInfo;
import android.print.PrintManager;
import android.os.Build;
import android.provider.OpenableColumns;
import android.text.InputType;
import android.util.Log;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.widget.Toast;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.Locale;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicLong;

public class GridCraftActivity extends NativeActivity {
    private static final String TAG = "GridCraft";
    private static final int REQ_OPEN = 41;
    private static final int REQ_SAVE = 42;
    /** Largest file read into the app (the Rust side caps it too). */
    private static final long MAX_BYTES = 512L << 20;

    private final ExecutorService io = Executors.newSingleThreadExecutor();
    private final AtomicLong seq = new AtomicLong();
    private ClipboardManager clipboard;
    /** What the on-screen keyboard types into (see TextInput). */
    private TextInput textInput;
    /** The insets last reported, so an unchanged layout reports nothing. */
    private String lastInsets = "";
    /** Whether the app wants the on-screen keyboard (UI thread only). */
    private boolean keyboardWanted = false;

    // UI thread only: the save waiting for the save picker.
    private long pendingTicket = -1;
    private File pendingFile;
    private String pendingName;
    // UI thread only: what the open picker is for.
    private String openPurpose = "workbook";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        new File(getFilesDir(), "inbox").mkdirs();
        // The keyboard's text field: one invisible pixel in the corner.
        textInput = new TextInput(this);
        addContentView(textInput, new ViewGroup.LayoutParams(1, 1));
        // Android draws the app under the status bar, navigation bar and keyboard: tell the app
        // how much of each edge they cover, so it keeps its content clear of them.
        View decor = getWindow().getDecorView();
        decor.setOnApplyWindowInsetsListener((v, insets) -> {
            reportInsets(insets);
            return v.onApplyWindowInsets(insets);
        });
        darkSystemBarIcons();
        clipboard = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
        if (clipboard != null) {
            clipboard.addPrimaryClipChangedListener(this::mirrorClipboard);
        }
        // A restart after the system ended the app keeps its intent: the file was already read.
        if (savedInstanceState == null) {
            handleIncoming(getIntent());
        }
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleIncoming(intent);
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        // Android lets only the focused app read the clipboard.
        if (hasFocus) {
            mirrorClipboard();
            // Coming back from a picker that had the keyboard up: Android brings it back with
            // the window. Only the app decides whether it shows.
            if (!keyboardWanted) {
                setKeyboardVisible(false);
                // Android may bring the keyboard back for the text field a moment later.
                getWindow().getDecorView().postDelayed(() -> {
                    if (!keyboardWanted) {
                        setKeyboardVisible(false);
                    }
                }, 250);
            }
        }
    }

    @Override
    protected void onDestroy() {
        io.shutdown();
        super.onDestroy();
    }

    /** Text the keyboard typed that has no key code (accents, emoji, a whole suggested word). */
    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        if (event.getAction() == KeyEvent.ACTION_MULTIPLE && event.getKeyCode() == KeyEvent.KEYCODE_UNKNOWN) {
            String chars = event.getCharacters();
            if (chars != null && !chars.isEmpty()) {
                post("text", chars.getBytes(StandardCharsets.UTF_8));
            }
            return true;
        }
        return super.dispatchKeyEvent(event);
    }

    // ---- Called from Rust ------------------------------------------------------------------

    /** The system picker for a workbook or a picture. */
    public void openDocument(final String purpose) {
        runOnUiThread(() -> {
            openPurpose = "picture".equals(purpose) ? "picture" : "workbook";
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            if ("picture".equals(openPurpose)) {
                intent.setType("image/*");
            } else {
                // Many providers report spreadsheets as application/octet-stream: show every
                // file, and the app goes by the name.
                intent.setType("*/*");
                intent.putExtra(Intent.EXTRA_MIME_TYPES, new String[] {
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    "application/vnd.ms-excel.sheet.macroEnabled.12",
                    "application/vnd.ms-excel",
                    "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
                    "application/vnd.oasis.opendocument.spreadsheet",
                    "text/csv", "text/comma-separated-values", "text/tab-separated-values", "text/plain",
                    "application/json", "application/octet-stream"});
            }
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                    | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            try {
                startActivityForResult(intent, REQ_OPEN);
            } catch (Exception e) {
                Log.e(TAG, "no document picker", e);
                toast("No file picker is available on this device.");
                post("done", openPurpose.getBytes(StandardCharsets.UTF_8));
            }
        });
    }

    /** The save picker for the staged file at path, suggesting name. Reports the ticket's outcome
     *  with the name the user chose and the file's address. */
    public void saveDocument(final long ticket, final String path, final String name, final String mime) {
        runOnUiThread(() -> startSavePicker(ticket, new File(path), name, mime));
    }

    /** Copy the working copy at path over its original at uri, unless the original changed
     *  elsewhere since the app last read or wrote it (then it is left alone: a conflict). */
    public void writeBack(final String path, final String uriText) {
        io.execute(() -> {
            final Uri uri = Uri.parse(uriText);
            String outcome;
            String reason = "";
            try {
                String before = stamp(uri);
                String known = stamps().getString(uriText, null);
                if (known != null && before != null && !known.equals(before)) {
                    Log.w(TAG, "not writing " + uri + ": it changed elsewhere (" + known + " -> " + before + ")");
                    outcome = "conflict";
                } else {
                    copyFileToUri(new File(path), uri);
                    rememberStamp(uri);
                    outcome = "ok";
                }
            } catch (Exception e) {
                Log.e(TAG, "write back to " + uri + " failed", e);
                outcome = "fail";
                reason = e.getMessage() == null ? e.toString() : e.getMessage();
            }
            post("sync", (path + "\n" + outcome + "\n" + reason.replace('\n', ' ')).getBytes(StandardCharsets.UTF_8));
        });
    }

    /** A web or mail link, in the app the system picks. */
    public void openUrl(final String url) {
        runOnUiThread(() -> {
            try {
                Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse(url));
                intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                startActivity(intent);
            } catch (Exception e) {
                Log.w(TAG, "can't open " + url, e);
                toast("No app can open this link.");
            }
        });
    }

    /** Android's print dialog for a PDF. */
    public void printPdf(final String path, final String name) {
        runOnUiThread(() -> {
            PrintManager pm = (PrintManager) getSystemService(PRINT_SERVICE);
            if (pm == null) {
                toast("Printing is not available on this device.");
                return;
            }
            pm.print(name, new PdfAdapter(new File(path), name), new PrintAttributes.Builder().build());
        });
    }

    /** Put a copy on the system clipboard: as a table (html, when not empty) and as text. */
    public void setClipboard(final String html, final String text) {
        runOnUiThread(() -> {
            if (clipboard != null) {
                ClipData clip = html == null || html.isEmpty()
                        ? ClipData.newPlainText("GridCraft", text)
                        : ClipData.newHtmlText("GridCraft", text, html);
                clipboard.setPrimaryClip(clip);
            }
        });
    }

    /** Show or hide the on-screen keyboard. */
    public void setKeyboardVisible(final boolean visible) {
        runOnUiThread(() -> {
            keyboardWanted = visible;
            InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            if (imm == null || textInput == null) {
                return;
            }
            if (visible) {
                // The text field takes focus only while the app wants the keyboard: a focused
                // text field brings the keyboard back whenever the window regains focus (after a
                // picker or another app), with nothing to type into.
                textInput.setFocusable(true);
                textInput.setFocusableInTouchMode(true);
                textInput.requestFocus();
                // Bind the keyboard to the text field afresh: after another window (a picker) had
                // the keyboard, Android can show it again still bound to that window's connection,
                // and the keys then reach nothing.
                imm.restartInput(textInput);
                imm.showSoftInput(textInput, 0);
            } else {
                imm.hideSoftInputFromWindow(textInput.getWindowToken(), 0);
                textInput.clearFocus();
                textInput.setFocusable(false);
                textInput.setFocusableInTouchMode(false);
            }
        });
    }

    // ---- Answers ---------------------------------------------------------------------------

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == REQ_OPEN) {
            final String purpose = openPurpose;
            final Uri uri = resultCode == RESULT_OK && data != null ? data.getData() : null;
            final boolean writable = uri != null && "workbook".equals(purpose) && keepPermission(uri, true);
            io.execute(() -> {
                if (uri != null) {
                    deliver(uri, purpose, writable);
                }
                post("done", purpose.getBytes(StandardCharsets.UTF_8));
            });
        } else if (requestCode == REQ_SAVE) {
            final long ticket = pendingTicket;
            final File staged = pendingFile;
            final String requested = pendingName;
            pendingTicket = -1;
            pendingFile = null;
            pendingName = null;
            if (ticket < 0) {
                return;
            }
            if (resultCode != RESULT_OK || data == null || data.getData() == null) {
                if (staged != null) {
                    staged.delete();
                }
                postSave(ticket, "cancel", requested, null);
                return;
            }
            final Uri uri = data.getData();
            keepPermission(uri, true);
            io.execute(() -> {
                try {
                    copyFileToUri(staged, uri);
                    rememberStamp(uri);
                    postSave(ticket, "ok", requested, displayName(uri) + "\n" + uri);
                } catch (Exception e) {
                    Log.e(TAG, "save failed", e);
                    postSave(ticket, "fail", requested, e.getMessage() == null ? e.toString() : e.getMessage());
                } finally {
                    if (staged != null) {
                        staged.delete();
                    }
                }
            });
        }
    }

    private void startSavePicker(long ticket, File staged, String name, String mime) {
        if (pendingTicket >= 0) {
            staged.delete();
            postSave(ticket, "fail", name, "another save is still waiting for a location");
            return;
        }
        Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType(mime == null || mime.isEmpty() ? "application/octet-stream" : mime);
        intent.putExtra(Intent.EXTRA_TITLE, name);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
        pendingTicket = ticket;
        pendingFile = staged;
        pendingName = name;
        try {
            startActivityForResult(intent, REQ_SAVE);
        } catch (Exception e) {
            Log.e(TAG, "no save picker", e);
            pendingTicket = -1;
            pendingFile = null;
            pendingName = null;
            staged.delete();
            postSave(ticket, "fail", name, "no save picker is available on this device");
        }
    }

    /** "Open with GridCraft" from a file manager, or a file shared to the app. */
    private void handleIncoming(Intent intent) {
        if (intent == null) {
            return;
        }
        Uri uri = null;
        boolean edit = false;
        if (Intent.ACTION_VIEW.equals(intent.getAction()) || Intent.ACTION_EDIT.equals(intent.getAction())) {
            uri = intent.getData();
            edit = true;
        } else if (Intent.ACTION_SEND.equals(intent.getAction())) {
            uri = intent.getParcelableExtra(Intent.EXTRA_STREAM);
        }
        if (uri == null) {
            return;
        }
        final Uri incoming = uri;
        // A shared file is a copy to work on; one opened for viewing or editing may be saved back
        // when the sender granted writing (for as long as it lets the app keep that permission).
        boolean writable = edit && (intent.getFlags() & Intent.FLAG_GRANT_WRITE_URI_PERMISSION) != 0;
        if (writable && (intent.getFlags() & Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION) != 0) {
            keepPermission(incoming, true);
        }
        final boolean write = writable;
        io.execute(() -> deliver(incoming, "workbook", write));
    }

    /** Stream one file into the inbox: its name, what it's for, its address when the app may
     *  write back to it, then its bytes. */
    private void deliver(Uri uri, String purpose, boolean writable) {
        String name = displayName(uri);
        File dir = new File(getFilesDir(), "inbox");
        File tmp = new File(dir, String.format(Locale.ROOT, "%013d-%06d.open.tmp", System.currentTimeMillis(), seq.incrementAndGet()));
        try (InputStream in = getContentResolver().openInputStream(uri); OutputStream out = new FileOutputStream(tmp)) {
            if (in == null) {
                throw new IOException("the file can't be read");
            }
            String header = name.replace('\n', ' ') + "\n" + purpose + "\n" + (writable ? uri.toString() : "") + "\n";
            out.write(header.getBytes(StandardCharsets.UTF_8));
            byte[] buf = new byte[1 << 16];
            long total = 0;
            int n;
            while ((n = in.read(buf)) > 0) {
                total += n;
                if (total > MAX_BYTES) {
                    throw new IOException("larger than 512 MB");
                }
                out.write(buf, 0, n);
            }
        } catch (Exception e) {
            Log.e(TAG, "can't read " + uri, e);
            tmp.delete();
            toast("Couldn't open " + name + ": " + e.getMessage());
            return;
        }
        if (writable) {
            rememberStamp(uri);
        }
        if (!tmp.renameTo(new File(dir, tmp.getName().substring(0, tmp.getName().length() - 4)))) {
            tmp.delete();
        }
    }

    private void copyFileToUri(File from, Uri to) throws IOException {
        if (from == null || !from.isFile()) {
            throw new IOException("nothing to save");
        }
        OutputStream out = null;
        for (String mode : new String[] {"wt", "rwt", "w"}) {
            try {
                out = getContentResolver().openOutputStream(to, mode);
                if (out != null) {
                    break;
                }
            } catch (Exception ignored) {
                // The provider doesn't take this mode; try the next.
            }
        }
        if (out == null) {
            throw new IOException("the location can't be written");
        }
        try (InputStream in = new FileInputStream(from); OutputStream o = out) {
            byte[] buf = new byte[1 << 16];
            int n;
            while ((n = in.read(buf)) > 0) {
                o.write(buf, 0, n);
            }
        }
    }

    private String displayName(Uri uri) {
        String name = null;
        try (Cursor c = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst() && !c.isNull(0)) {
                name = c.getString(0);
            }
        } catch (Exception ignored) {
            // Some providers don't answer queries: fall back to the path.
        }
        if (name == null || name.isEmpty()) {
            name = uri.getLastPathSegment();
        }
        if (name == null || name.isEmpty()) {
            name = "document";
        }
        int slash = Math.max(name.lastIndexOf('/'), name.lastIndexOf('\\'));
        return slash >= 0 ? name.substring(slash + 1) : name;
    }

    // ---- Files the app may write back to ---------------------------------------------------

    /** The size and modification time of each file the app last read or wrote, by address. */
    private SharedPreferences stamps() {
        return getSharedPreferences("file-stamps", MODE_PRIVATE);
    }

    /** What the provider says of the file at uri now: "size/modified", or null when it won't say. */
    private String stamp(Uri uri) {
        try (Cursor c = getContentResolver().query(uri, null, null, null, null)) {
            if (c == null || !c.moveToFirst()) {
                return null;
            }
            int size = c.getColumnIndex(OpenableColumns.SIZE);
            int modified = c.getColumnIndex("last_modified");
            String s = size >= 0 && !c.isNull(size) ? Long.toString(c.getLong(size)) : "?";
            String m = modified >= 0 && !c.isNull(modified) ? Long.toString(c.getLong(modified)) : "?";
            return "?".equals(s) && "?".equals(m) ? null : s + "/" + m;
        } catch (Exception e) {
            return null;
        }
    }

    private void rememberStamp(Uri uri) {
        String now = stamp(uri);
        if (now != null) {
            stamps().edit().putString(uri.toString(), now).apply();
        } else {
            stamps().edit().remove(uri.toString()).apply();
        }
    }

    /** Keep access to uri after the app restarts; whether the app may write to it now. */
    private boolean keepPermission(Uri uri, boolean write) {
        try {
            int flags = Intent.FLAG_GRANT_READ_URI_PERMISSION | (write ? Intent.FLAG_GRANT_WRITE_URI_PERMISSION : 0);
            getContentResolver().takePersistableUriPermission(uri, flags);
        } catch (Exception e) {
            // Not every provider grants lasting access.
            Log.i(TAG, "no lasting access to " + uri + ": " + e.getMessage());
        }
        return checkCallingOrSelfUriPermission(uri, Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
                == android.content.pm.PackageManager.PERMISSION_GRANTED;
    }

    // ---- Inbox -----------------------------------------------------------------------------

    private void post(String kind, byte[] data) {
        File dir = new File(getFilesDir(), "inbox");
        dir.mkdirs();
        String base = String.format(Locale.ROOT, "%013d-%06d.%s", System.currentTimeMillis(), seq.incrementAndGet(), kind);
        File tmp = new File(dir, base + ".tmp");
        try (FileOutputStream out = new FileOutputStream(tmp)) {
            out.write(data);
        } catch (IOException e) {
            Log.e(TAG, "inbox write failed", e);
            tmp.delete();
            return;
        }
        if (!tmp.renameTo(new File(dir, base))) {
            tmp.delete();
        }
    }

    private void postSave(long ticket, String outcome, String requested, String rest) {
        String text = ticket + "\n" + outcome + "\n" + (requested == null ? "" : requested) + (rest != null ? "\n" + rest : "");
        post("save", text.getBytes(StandardCharsets.UTF_8));
    }

    private void mirrorClipboard() {
        String text = "";
        String html = "";
        try {
            if (clipboard != null && clipboard.hasPrimaryClip()) {
                ClipData clip = clipboard.getPrimaryClip();
                if (clip != null && clip.getItemCount() > 0) {
                    ClipData.Item item = clip.getItemAt(0);
                    CharSequence t = item.coerceToText(this);
                    text = t == null ? "" : t.toString();
                    html = item.getHtmlText() == null ? "" : item.getHtmlText();
                }
            }
        } catch (Exception e) {
            Log.i(TAG, "clipboard unreadable: " + e.getMessage());
            return;
        }
        // The table first: the app reads the text, then its table.
        writeAtomically("clipboard.html", html);
        writeAtomically("clipboard.txt", text);
    }

    private void writeAtomically(String name, String content) {
        File file = new File(getFilesDir(), name);
        File tmp = new File(getFilesDir(), name + ".tmp");
        try (FileOutputStream out = new FileOutputStream(tmp)) {
            out.write(content.getBytes(StandardCharsets.UTF_8));
        } catch (IOException e) {
            tmp.delete();
            return;
        }
        if (!tmp.renameTo(file)) {
            tmp.delete();
        }
    }

    // ---- Keyboard text and keys -------------------------------------------------------------

    /** Text the keyboard typed. Enter and Tab inside it go as keys, so they act as keys do. */
    void postText(String text) {
        if (text == null || text.isEmpty()) {
            return;
        }
        StringBuilder run = new StringBuilder();
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            if (c == '\n' || c == '\t') {
                if (run.length() > 0) {
                    post("text", run.toString().getBytes(StandardCharsets.UTF_8));
                    run.setLength(0);
                }
                postKey(c == '\n' ? KeyEvent.KEYCODE_ENTER : KeyEvent.KEYCODE_TAB, 0);
            } else {
                run.append(c);
            }
        }
        if (run.length() > 0) {
            post("text", run.toString().getBytes(StandardCharsets.UTF_8));
        }
    }

    void postKey(int keyCode, int metaState) {
        post("key", (keyCode + "\n" + metaState).getBytes(StandardCharsets.UTF_8));
    }

    /** A key event the keyboard sent: a printable key without Ctrl or Alt is text, the rest keys. */
    void postKeyEvent(KeyEvent event) {
        if (event.getAction() != KeyEvent.ACTION_DOWN) {
            if (event.getAction() == KeyEvent.ACTION_MULTIPLE && event.getCharacters() != null) {
                postText(event.getCharacters());
            }
            return;
        }
        int meta = event.getMetaState();
        boolean shortcut = (meta & (KeyEvent.META_CTRL_ON | KeyEvent.META_ALT_ON)) != 0;
        int code = event.getKeyCode();
        if (!shortcut && code != KeyEvent.KEYCODE_ENTER && code != KeyEvent.KEYCODE_TAB) {
            int ch = event.getUnicodeChar(meta);
            if (ch != 0 && (ch & KeyCharacterMap.COMBINING_ACCENT) == 0 && !Character.isISOControl(ch)) {
                postText(new String(Character.toChars(ch)));
                return;
            }
        }
        postKey(code, meta);
    }

    // ---- Window insets ---------------------------------------------------------------------

    private void reportInsets(WindowInsets insets) {
        int left, top, right, bottom;
        if (Build.VERSION.SDK_INT >= 30) {
            android.graphics.Insets i = insets.getInsets(WindowInsets.Type.systemBars()
                    | WindowInsets.Type.displayCutout() | WindowInsets.Type.ime());
            left = i.left;
            top = i.top;
            right = i.right;
            bottom = i.bottom;
        } else {
            left = insets.getSystemWindowInsetLeft();
            top = insets.getSystemWindowInsetTop();
            right = insets.getSystemWindowInsetRight();
            bottom = insets.getSystemWindowInsetBottom();
        }
        String text = left + " " + top + " " + right + " " + bottom;
        if (!text.equals(lastInsets)) {
            lastInsets = text;
            post("insets", text.getBytes(StandardCharsets.UTF_8));
        }
    }

    /** Until the app says otherwise: its default look is light, so dark bar icons. */
    private void darkSystemBarIcons() {
        applyBarIcons(true);
    }

    /** The app's background behind the status and navigation bars is light (dark icons) or dark
     *  (light icons). Called from Rust whenever the app's theme changes; the system's own dark
     *  mode doesn't decide this, the app's look does. */
    public void setLightSystemBars(final boolean light) {
        runOnUiThread(() -> applyBarIcons(light));
    }

    private void applyBarIcons(boolean light) {
        if (Build.VERSION.SDK_INT >= 30) {
            WindowInsetsController c = getWindow().getInsetsController();
            if (c != null) {
                int mask = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                        | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                c.setSystemBarsAppearance(light ? mask : 0, mask);
            }
        } else {
            View decor = getWindow().getDecorView();
            int flags = decor.getSystemUiVisibility();
            decor.setSystemUiVisibility(light ? flags | View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR
                    : flags & ~View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR);
        }
    }

    private void toast(final String message) {
        runOnUiThread(() -> Toast.makeText(this, message, Toast.LENGTH_LONG).show());
    }

    /** Hands a finished PDF to the print framework. */
    private static final class PdfAdapter extends PrintDocumentAdapter {
        private final File file;
        private final String name;

        PdfAdapter(File file, String name) {
            this.file = file;
            this.name = name;
        }

        @Override
        public void onLayout(PrintAttributes oldAttributes, PrintAttributes newAttributes, CancellationSignal cancel,
                LayoutResultCallback callback, Bundle extras) {
            if (cancel.isCanceled()) {
                callback.onLayoutCancelled();
                return;
            }
            PrintDocumentInfo info = new PrintDocumentInfo.Builder(name + ".pdf")
                    .setContentType(PrintDocumentInfo.CONTENT_TYPE_DOCUMENT)
                    .setPageCount(PrintDocumentInfo.PAGE_COUNT_UNKNOWN)
                    .build();
            callback.onLayoutFinished(info, true);
        }

        @Override
        public void onWrite(PageRange[] pages, ParcelFileDescriptor destination, CancellationSignal cancel,
                WriteResultCallback callback) {
            try (InputStream in = new FileInputStream(file); OutputStream out = new FileOutputStream(destination.getFileDescriptor())) {
                byte[] buf = new byte[1 << 16];
                int n;
                while ((n = in.read(buf)) > 0) {
                    if (cancel.isCanceled()) {
                        callback.onWriteCancelled();
                        return;
                    }
                    out.write(buf, 0, n);
                }
                callback.onWriteFinished(new PageRange[] {PageRange.ALL_PAGES});
            } catch (IOException e) {
                callback.onWriteFailed(e.getMessage());
            }
        }
    }

    /**
     * An invisible view the on-screen keyboard types into. NativeActivity's own view has no
     * InputConnection, so keyboards fall back to a mode that drops spaces and suggestions. This
     * one takes everything the keyboard sends and passes it to the app through the inbox, in
     * order: committed text as text, a word still being composed as the difference from the
     * last version (backspaces, then the new letters), and every other key as a key.
     */
    static final class TextInput extends View {
        private final GridCraftActivity activity;

        TextInput(GridCraftActivity activity) {
            super(activity);
            this.activity = activity;
            // Focusable only while the app shows the keyboard (setKeyboardVisible).
            setFocusable(false);
            setFocusableInTouchMode(false);
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo out) {
            // No automatic capitals or corrections from the keyboard: it can't see the cell, and
            // formulas and codes are typed exactly.
            out.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
            out.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                    | EditorInfo.IME_ACTION_NONE;
            out.initialSelStart = 0;
            out.initialSelEnd = 0;
            return new Connection(this, activity);
        }
    }

    static final class Connection extends BaseInputConnection {
        private final GridCraftActivity activity;
        /** The word the keyboard is composing, as already typed into the document. */
        private String composing = "";

        Connection(View view, GridCraftActivity activity) {
            super(view, false);
            this.activity = activity;
        }

        /** Change what the document holds for the composing word from composing to next. */
        private void retype(String next) {
            int common = 0;
            int max = Math.min(composing.length(), next.length());
            while (common < max && composing.charAt(common) == next.charAt(common)) {
                common++;
            }
            // Don't split a surrogate pair.
            if (common > 0 && Character.isHighSurrogate(composing.charAt(common - 1))) {
                common--;
            }
            int remove = composing.codePointCount(common, composing.length());
            for (int i = 0; i < remove; i++) {
                activity.postKey(KeyEvent.KEYCODE_DEL, 0);
            }
            activity.postText(next.substring(common));
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            retype(text == null ? "" : text.toString());
            composing = "";
            return true;
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            String next = text == null ? "" : text.toString();
            retype(next);
            composing = next;
            return true;
        }

        @Override
        public boolean finishComposingText() {
            composing = "";
            return true;
        }

        @Override
        public boolean setComposingRegion(int start, int end) {
            // The app doesn't share its text with the keyboard, so there is nothing to recompose.
            return true;
        }

        @Override
        public boolean deleteSurroundingText(int beforeLength, int afterLength) {
            if (!composing.isEmpty()) {
                retype("");
                composing = "";
                beforeLength = 0;
            }
            for (int i = 0; i < beforeLength; i++) {
                activity.postKey(KeyEvent.KEYCODE_DEL, 0);
            }
            for (int i = 0; i < afterLength; i++) {
                activity.postKey(KeyEvent.KEYCODE_FORWARD_DEL, 0);
            }
            return true;
        }

        @Override
        public boolean deleteSurroundingTextInCodePoints(int beforeLength, int afterLength) {
            return deleteSurroundingText(beforeLength, afterLength);
        }

        @Override
        public boolean sendKeyEvent(KeyEvent event) {
            if (!composing.isEmpty() && event.getAction() == KeyEvent.ACTION_DOWN) {
                composing = "";
            }
            activity.postKeyEvent(event);
            return true;
        }

        @Override
        public boolean performEditorAction(int actionCode) {
            activity.postKey(KeyEvent.KEYCODE_ENTER, 0);
            return true;
        }

        @Override
        public CharSequence getTextBeforeCursor(int length, int flags) {
            // Keyboards look here to decide on capitals and what Backspace deletes: an empty
            // answer makes them send Backspace as a key, which the app handles.
            return "";
        }

        @Override
        public CharSequence getTextAfterCursor(int length, int flags) {
            return "";
        }

        @Override
        public int getCursorCapsMode(int reqModes) {
            return 0;
        }
    }
}
