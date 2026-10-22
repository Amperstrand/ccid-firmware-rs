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
     * The FCI a JCRE answers a successful applet SELECT with:
     * {@code 6F [ 84 <aid> 90 <self-mark> ]} + SW 9000. EchoApplet returns
     * exactly these bytes from its own process(); the bridge re-uses the
     * builder to answer the SELECT on behalf of applets that do not
     * implement INS=A4 themselves (JCRE semantics — e.g. a physical
     * J3R180 answers SmartPGP's SELECT with an FCI + 9000 while the
     * applet itself never sees the INS).
     */
    static byte[] selectFci(byte[] aid) {
        int body = (2 + aid.length) + (2 + SELF_MARK.length);
        byte[] out = new byte[2 + body + 2];
        int p = 0;
        out[p++] = (byte) 0x6F;
        out[p++] = (byte) body;
        out[p++] = (byte) 0x84; // DF name
        out[p++] = (byte) aid.length;
        System.arraycopy(aid, 0, out, p, aid.length);
        p += aid.length;
        out[p++] = (byte) 0x90; // proprietary tag: the marker
        out[p++] = (byte) SELF_MARK.length;
        System.arraycopy(SELF_MARK, 0, out, p, SELF_MARK.length);
        p += SELF_MARK.length;
        out[p++] = (byte) 0x90; // SW 9000
        out[p] = 0x00;
        return out;
    }

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
                    // FCI: 6F [ 84 <aid> 90 <marker> ] + 9000 (self-marked)
                    byte[] resp = selectFci(AID_BYTES);
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
                }
                // OFF/ON: no reply (see class doc)
                continue;
            }
            CommandAPDU capdu = new CommandAPDU(msg);
            try {
                // jcardsim 2.2.2 does not auto-dispatch SELECT: the card
                // runtime needs the applet selected before transmit.
                byte[] selectedAid = null;
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
                    selectedAid = aidBytes;
                } else if (capdu.getINS() != 0xA4 && !appletSelected) {
                    // no SELECT yet this session — mirror a card with no
                    // current applet
                    send(out, new byte[]{(byte) 0x6D, (byte) 0x00});
                    continue;
                }
                ResponseAPDU rapdu = sim.transmitCommand(capdu);
                byte[] resp = rapdu.getBytes();
                if (selectedAid != null && resp.length >= 2
                        && resp[resp.length - 2] == (byte) 0x6D
                        && resp[resp.length - 1] == 0x00) {
                    // JCRE-style SELECT answering: the applet was selected
                    // but declines the re-delivered SELECT INS (6D00 — e.g.
                    // SmartPGP does not implement INS=A4). A real JCRE
                    // (physical J3R180) answers the SELECT itself with the
                    // applet's FCI + 9000, so FCI-checking clients (gp -c,
                    // wallets) see a sane answer. Applets that DO answer
                    // their own SELECT (EchoApplet) keep their response.
                    resp = selectFci(selectedAid);
                }
                send(out, resp);
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
