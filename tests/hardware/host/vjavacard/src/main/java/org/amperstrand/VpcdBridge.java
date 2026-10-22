package org.amperstrand;

import java.io.InputStream;
import java.io.OutputStream;
import java.net.Socket;
import java.util.Arrays;
import javax.smartcardio.CommandAPDU;
import javax.smartcardio.ResponseAPDU;

import com.licel.jcardsim.io.JavaxSmartCardInterface;

import javacard.framework.AID;
import javacard.framework.APDU;
import javacard.framework.Applet;
import javacard.framework.ISO7816;
import javacard.framework.ISOException;
import javacard.framework.Util;

/**
 * vjavacard — a REAL virtual JavaCard behind vpcd (#261 tier b).
 *
 * Bridges jcardsim (Java Card 3.0.5 simulator on a host JVM) to a vpcd
 * listener so pcscd sees a reader whose card EXECUTES applet code —
 * real SELECT/AID dispatch, real applet logic, real CAP loading — with
 * zero hardware.
 *
 * Wire protocol mirrors the firmware client (vpcd.rs) and the tier-a
 * table card (tests/hardware/nfc/vpicc_table.py): every message is a
 * 2-byte big-endian length + payload; controls are 1 byte (OFF/ON
 * fire-and-forget — NO reply, an empty ack de-syncs the driver's next
 * probe); ATR/RESET answer the ATR; longer payloads are C-APDUs.
 */
public final class VpcdBridge {
    static final byte[] ATR = hex("3B858131FE458073C8211084"); // J3R180 shape
    static final byte[] SELF_MARK = hex("564A41564143415244"); // "VJAVACARD"

    /**
     * A self-marking echo applet: SELECT answers a FCI carrying the
     * VJAVACARD marker; any other INS echoes the INF back with SW 9000
     * (never card data — everything it returns is either the marker,
     * the caller's own bytes, or a status word).
     */
    public static class EchoApplet extends Applet {
        public static final byte[] AID_BYTES = hex("A00000015145434F01");
        public static void install(byte[] b, short off, byte len) {
            new EchoApplet().register(); // under the install AID (jcardsim convention)
        }

        public void process(APDU apdu) {
            byte[] buf = apdu.getBuffer();
            switch (buf[ISO7816.OFFSET_INS]) {
                case (byte) 0xA4: { // SELECT
                    short lc = (short) (buf[ISO7816.OFFSET_LC] & 0xFF);
                    byte[] aid = Arrays.copyOfRange(buf, ISO7816.OFFSET_CDATA,
                            ISO7816.OFFSET_CDATA + lc);
                    if (!Arrays.equals(aid, AID_BYTES)) {
                        ISOException.throwIt(ISO7816.SW_FILE_NOT_FOUND);
                    }
                    // FCI: 6F [ 84 <aid> 90 <marker> ]  (self-marked)
                    short p = 0;
                    byte[] fci = new byte[64];
                    fci[p++] = (byte) 0x6F;
                    fci[p++] = (byte) ((2 + AID_BYTES.length) + (2 + SELF_MARK.length));
                    fci[p++] = (byte) 0x84;
                    fci[p++] = (byte) AID_BYTES.length;
                    p = (short) Util.arrayCopyNonAtomic(AID_BYTES, (short) 0, fci, p,
                            (short) AID_BYTES.length);
                    fci[p++] = (byte) 0x90; // proprietary tag: the marker
                    fci[p++] = (byte) SELF_MARK.length;
                    p = (short) Util.arrayCopyNonAtomic(SELF_MARK, (short) 0, fci, p,
                            (short) SELF_MARK.length);
                    byte[] resp = Arrays.copyOf(fci, p + 2);
                    resp[p] = (byte) 0x90;
                    resp[p + 1] = 0x00;
                    System.arraycopy(resp, 0, buf, 0, resp.length);
                    apdu.setOutgoingAndSend((short) 0, (short) resp.length);
                    return;
                }
                default: {
                    short lc = (short) (buf[ISO7816.OFFSET_LC] & 0xFF);
                    byte[] resp = new byte[lc + 2];
                    Util.arrayCopyNonAtomic(buf, ISO7816.OFFSET_CDATA, resp, (short) 0, lc);
                    resp[lc] = (byte) 0x90;
                    resp[lc + 1] = 0x00;
                    System.arraycopy(resp, 0, buf, 0, resp.length);
                    apdu.setOutgoingAndSend((short) 0, (short) resp.length);
                }
            }
        }
    }

    public static void main(String[] args) throws Exception {
        String host = args.length > 0 ? args[0] : "127.0.0.1";
        int port = args.length > 1 ? Integer.parseInt(args[1]) : 35972;

        JavaxSmartCardInterface sim = new JavaxSmartCardInterface();
        if (args.length >= 5 && "--applet".equals(args[2])) {
            // --applet <fqcn> <aidHex>: load a REAL applet class through the
            // jcardsim loadApplet path (installApplet = load + create +
            // register). For SmartPGP the classes ship inside the CAP
            // artifact (APPLET-INF/classes in the ant-javacard JAR); install
            // params follow the Java Card convention [aidLen][aid] and the
            // applet registers itself under that AID.
            String fqcn = args[3];
            byte[] aidBytes = hex(args[4]);
            AID aid = new AID(aidBytes, (short) 0, (byte) aidBytes.length);
            byte[] params = new byte[1 + aidBytes.length];
            params[0] = (byte) aidBytes.length;
            System.arraycopy(aidBytes, 0, params, 1, aidBytes.length);
            sim.installApplet(aid, Class.forName(fqcn), params, (short) 0,
                    (byte) params.length);
            System.out.println("vjavacard: applet " + fqcn + " installed, AID="
                    + hexStr(aidBytes));
        } else {
            AID aid = new AID(EchoApplet.AID_BYTES, (short) 0,
                    (byte) EchoApplet.AID_BYTES.length);
            sim.installApplet(aid, EchoApplet.class);
            System.out.println("vjavacard: echo applet installed, AID="
                    + hexStr(EchoApplet.AID_BYTES));
        }

        while (true) {
            try (Socket sock = new Socket(host, port)) {
                System.out.println("vjavacard: connected to " + host + ":" + port);
                serve(sim, sock);
            } catch (Exception e) {
                Thread.sleep(500); // reconnect loop (firmware pattern)
            }
        }
    }

    static volatile boolean appletSelected = false;

    static void serve(JavaxSmartCardInterface sim, Socket sock) throws Exception {
        sock.setSoTimeout(5000);
        InputStream in = sock.getInputStream();
        OutputStream out = sock.getOutputStream();
        byte[] hdr = new byte[2];
        while (true) {
            if (readFully(in, hdr) < 2) return;
            int len = ((hdr[0] & 0xFF) << 8) | (hdr[1] & 0xFF);
            byte[] msg = new byte[len];
            if (len > 0 && readFully(in, msg) < len) return;
            if (len == 1) {
                if (msg[0] == 0x04 || msg[0] == 0x02) {
                    send(out, ATR);
                }
                if (msg[0] == 0x00) {
                    // power-off deselects — real-card semantics; the next
                    // session must SELECT again before applet commands.
                    appletSelected = false;
                    // NOTE: do NOT call sim.reset() here — jcardsim 2.2.2's
                    // reset() leaves the runtime so a subsequent
                    // selectApplet()+transmitCommand throws the
                    // no-current-applet SystemException (bench-verified
                    // 2026-10-22: every post-reset SELECT 6F00). Flag-only
                    // matches observed 2.2.2 power-cycle behavior.
                }
                // OFF/ON: no reply (see class doc)
                continue;
            }
            CommandAPDU capdu = new CommandAPDU(msg);
            if (Boolean.getBoolean("vjc.debug")) {
                System.out.println("vjc: >> " + Integer.toHexString(capdu.getCLA()) + " "
                        + Integer.toHexString(capdu.getINS()) + " " + capdu.getP1()
                        + " " + capdu.getP2() + " lc=" + capdu.getData().length);
            }
            try {
                // jcardsim 2.2.2 does not auto-dispatch SELECT: the card
                // runtime needs the applet selected before transmit.
                if (capdu.getINS() == 0xA4 && capdu.getP1() == 0x04
                        && capdu.getData().length > 0) {
                    byte[] aidBytes = capdu.getData();
                    if (aidBytes.length > 16 || aidBytes.length < 5) {
                        send(out, new byte[]{(byte) 0x6A, (byte) 0x82});
                        continue;
                    }
                    AID sel = new AID(aidBytes, (short) 0, (byte) aidBytes.length);
                    boolean ok = sim.selectApplet(sel);
                    appletSelected = ok;
                    if (!ok) {
                        send(out, new byte[]{(byte) 0x6A, (byte) 0x82});
                        continue;
                    }
                } else if (capdu.getINS() != 0xA4 && !appletSelected) {
                    // no SELECT yet this session — mirror a card with no
                    // current applet
                    send(out, new byte[]{(byte) 0x6D, (byte) 0x00});
                    continue;
                }
                ResponseAPDU rapdu = sim.transmitCommand(capdu);
                if (Boolean.getBoolean("vjc.debug")) {
                    System.out.println("vjc: << " + Integer.toHexString(rapdu.getSW()));
                }
                send(out, rapdu.getBytes());
            } catch (Throwable t) {
                System.out.println("vjavacard: applet threw: " + t);
                t.printStackTrace();
                send(out, new byte[]{(byte) 0x6F, 0x00}); // ISO 6F00: no diagnosis
            }
        }
    }

    static int readFully(InputStream in, byte[] buf) throws Exception {
        int off = 0;
        while (off < buf.length) {
            int r = in.read(buf, off, buf.length - off);
            if (r < 0) return off;
            off += r;
        }
        return off;
    }

    static byte[] hex(String h) {
        byte[] out = new byte[h.length() / 2];
        for (int i = 0; i < out.length; i++) {
            out[i] = (byte) Integer.parseInt(h.substring(2 * i, 2 * i + 2), 16);
        }
        return out;
    }

    static String hexStr(byte[] b) {
        StringBuilder sb = new StringBuilder();
        for (byte x : b) sb.append(String.format("%02X", x));
        return sb.toString();
    }

    static void send(OutputStream out, byte[] payload) throws Exception {
        int len = payload.length;
        out.write((len >> 8) & 0xFF);
        out.write(len & 0xFF);
        out.write(payload);
        out.flush();
    }
}
