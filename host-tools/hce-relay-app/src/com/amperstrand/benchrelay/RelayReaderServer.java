package com.amperstrand.benchrelay;

import android.nfc.tech.IsoDep;
import android.util.Log;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;

/**
 * Phone-B reader relay core (#529 second-phone leg): the REVERSED
 * counterpart of RelayServer. The HOST drives: it sends C-APDU frames
 * down (adb forward tcp:4872 -> localabstract:bench-reader-relay) and
 * this server answers with the physical tag's R-APDU via the
 * ReaderMode IsoDep handle.
 *
 * Wire (same 2-byte BE length + payload framing as the HCE link):
 *   host -> phone: C-APDU bytes to send to the tag
 *   phone -> host: the tag's R-APDU; 1-byte controls 0x01 = tag lost,
 *                  0x03 = tag present (payload after 0x03: tag info)
 *
 * Session discipline mirrors the host contract: one tag per session,
 * strictly sequential exchanges, tag loss is announced and the session
 * is invalidated — no reconnect behind the host's back, no blind
 * retransmit. The host's card-safety policy (hce_relay.py) is the
 * authority on what may be sent; this side is a dumb pipe by design.
 */
public final class RelayReaderServer {
    private static final String TAG = "BenchReaderRelay";
    private static final String SOCKET_NAME = "bench-reader-relay";

    private static RelayReaderServer instance;
    private LocalServerSocket server;
    private Thread acceptThread;
    private volatile boolean running;

    private IsoDep tag;           // guarded by tagLock; ONE live tag
    private String tagInfo = "";
    private final Object tagLock = new Object();

    private RelayReaderServer() {
    }

    public static synchronized RelayReaderServer get() {
        if (instance == null) {
            instance = new RelayReaderServer();
        }
        return instance;
    }

    /** Idempotent start (called from the reader activity's onCreate). */
    public synchronized void start() {
        if (acceptThread != null) {
            return;
        }
        running = true;
        acceptThread = new Thread(this::acceptLoop, "reader-relay-accept");
        acceptThread.start();
    }

    public synchronized void stop() {
        running = false;
        closeTag();
        try {
            if (server != null) server.close();
        } catch (IOException ignored) {
        }
        acceptThread = null;
    }

    /** ReaderMode callback: a tag entered the field. */
    public void setTag(IsoDep isoDep, String info) {
        synchronized (tagLock) {
            closeTagLocked();
            try {
                isoDep.connect();
                isoDep.setTimeout(5000);   // relay-latency headroom
                this.tag = isoDep;
                this.tagInfo = info;
                Log.i(TAG, "tag present: " + info);
            } catch (IOException e) {
                Log.w(TAG, "tag connect failed: " + e);
                this.tag = null;
            }
        }
    }

    /** Field lost / activity paused: invalidate the session loudly. */
    public void closeTag() {
        synchronized (tagLock) {
            closeTagLocked();
        }
    }

    private void closeTagLocked() {
        if (tag != null) {
            try {
                tag.close();
            } catch (IOException ignored) {
            }
            tag = null;
        }
    }

    private void acceptLoop() {
        try {
            server = new LocalServerSocket(SOCKET_NAME);
            Log.i(TAG, "listening on localabstract:" + SOCKET_NAME);
        } catch (IOException e) {
            Log.e(TAG, "LocalServerSocket failed: " + e);
            return;
        }
        while (running) {
            try (LocalSocket s = server.accept()) {
                Log.i(TAG, "host connected");
                serveHost(s);
            } catch (IOException e) {
                if (running) {
                    try {
                        Thread.sleep(500);
                    } catch (InterruptedException ie) {
                        return;
                    }
                }
            }
        }
    }

    private void serveHost(LocalSocket s) throws IOException {
        DataInputStream in = new DataInputStream(s.getInputStream());
        DataOutputStream out = new DataOutputStream(s.getOutputStream());
        // announce the current tag (if any) right after the host binds
        synchronized (tagLock) {
            if (tag != null) {
                writeControl(out, (byte) 0x03, tagInfo.getBytes());
            }
        }
        while (running) {
            int len = ((int) in.readShort()) & 0xFFFF;
            if (len > 65535) throw new IOException("bad length");
            byte[] capdu = new byte[len];
            in.readFully(capdu);
            IsoDep t;
            synchronized (tagLock) {
                t = tag;
            }
            if (t == null || !t.isConnected()) {
                writeControl(out, (byte) 0x01, null);
                continue;
            }
            byte[] rapdu;
            try {
                rapdu = t.transceive(capdu);
            } catch (IOException e) {
                Log.w(TAG, "transceive failed -> tag lost: " + e);
                closeTag();
                writeControl(out, (byte) 0x01, null);
                continue;
            }
            out.writeShort(rapdu.length);
            out.write(rapdu);
            out.flush();
        }
    }

    private void writeControl(DataOutputStream out, byte code, byte[] info)
            throws IOException {
        out.writeShort(1 + (info == null ? 0 : info.length));
        out.writeByte(code);
        if (info != null) out.write(info);
        out.flush();
    }
}
