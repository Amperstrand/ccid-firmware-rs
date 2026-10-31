package com.amperstrand.benchrelay;

import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.util.Log;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;

/**
 * Singleton relay core (#529): owns the local-abstract socket server
 * ("bench-hce-relay") independent of the NFC service lifecycle.
 *
 * Started idempotently from BOTH RelayApduService.onCreate (NFC binds
 * the HCE service lazily, only on field activation) and
 * RelayKickstartService (shell: adb shell am start-foreground-service)
 * — the bench host can connect its "card brain" BEFORE any reader
 * activates the phone.
 *
 * Wire protocol (frame = 2-byte BIG-ENDIAN length + payload, the
 * org.amperstrand.VpcdBridge framing):
 *   phone -> host: C-APDU bytes; 1-byte controls 0x01 = link
 *                  deactivated (onDeactivated), 0x02 = hello.
 *   host  -> phone: the exact R-APDU bytes to return.
 */
public final class RelayServer {
    private static final String TAG = "BenchHceRelay";
    private static final String SOCKET_NAME = "bench-hce-relay";
    private static final int HOST_TIMEOUT_UNUSED = 10000;

    private static RelayServer instance;

    private LocalServerSocket server;
    private Thread acceptThread;
    private LocalSocket hostSocket;
    private DataOutputStream hostOut;
    private DataInputStream hostIn;
    private final Object writeLock = new Object();
    private final Object connectLock = new Object();
    private volatile boolean running;

    private RelayServer() {
    }

    public static synchronized RelayServer get() {
        if (instance == null) {
            instance = new RelayServer();
        }
        return instance;
    }

    /** Idempotent: first call binds the socket, later calls are no-ops. */
    public synchronized void start() {
        if (acceptThread != null) {
            return;
        }
        running = true;
        acceptThread = new Thread(this::acceptLoop, "relay-accept");
        acceptThread.start();
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
            try {
                LocalSocket s = server.accept();
                synchronized (connectLock) {
                    closeHostLocked();
                    hostSocket = s;
                    hostIn = new DataInputStream(s.getInputStream());
                    hostOut = new DataOutputStream(s.getOutputStream());
                    Log.i(TAG, "host connected");
                }
                sendControl((byte) 0x02);
            } catch (IOException e) {
                if (running) {
                    Log.w(TAG, "accept failed: " + e);
                    try {
                        Thread.sleep(500);
                    } catch (InterruptedException ie) {
                        return;
                    }
                }
            }
        }
    }

    private void closeHostLocked() {
        try {
            if (hostSocket != null) hostSocket.close();
        } catch (IOException ignored) {
        }
        hostSocket = null;
        hostIn = null;
        hostOut = null;
    }

    private void sendControl(byte code) {
        synchronized (writeLock) {
            try {
                DataOutputStream out = hostOut;
                if (out != null) {
                    out.writeShort(1);
                    out.writeByte(code);
                    out.flush();
                }
            } catch (IOException e) {
                Log.w(TAG, "sendControl failed: " + e);
            }
        }
    }

    /** Frame the C-APDU out, block for the R-APDU back (or 6F00). */
    public byte[] relayFrame(byte[] apdu) {
        DataInputStream in;
        synchronized (connectLock) {
            if (hostIn == null || hostOut == null) {
                Log.w(TAG, "C-APDU but no host connected -> 6F00");
                return null;
            }
            in = hostIn;
        }
        try {
            synchronized (writeLock) {
                hostOut.writeShort(apdu.length);
                hostOut.write(apdu);
                hostOut.flush();
            }
            // One exchange at a time: the host brain is single-threaded
            // and ISO-DEP is half-duplex — at most one exchange is in
            // flight, writeLock keeps framing unambiguous anyway.
            synchronized (writeLock) {
                int len = ((int) in.readShort()) & 0xFFFF;
                if (len > 65536) throw new IOException("bad length " + len);
                byte[] resp = new byte[len];
                in.readFully(resp);
                return resp;
            }
        } catch (IOException e) {
            Log.w(TAG, "relay failed (" + e + ") -> dropping host");
            synchronized (connectLock) {
                closeHostLocked();
            }
            return null;
        }
    }

    public void notifyDeactivated(int reason) {
        Log.i(TAG, "deactivated reason=" + reason);
        sendControl((byte) 0x01);
    }
}
