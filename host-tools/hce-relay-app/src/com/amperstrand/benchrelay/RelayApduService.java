package com.amperstrand.benchrelay;

import android.nfc.cardemulation.HostApduService;
import android.os.Bundle;

/**
 * Bench HCE relay service (#529): every C-APDU the reader sends is
 * relayed to the bench host's "card brain" (see RelayServer for the
 * wire protocol; host tool: host-tools/hce_relay.py). Responses go
 * back through sendResponseApdu — processCommandApdu never blocks the
 * main thread. No host / host timeout => SW 6F00: loud, never a silent
 * fake card.
 */
public class RelayApduService extends HostApduService {

    @Override
    public void onCreate() {
        super.onCreate();
        RelayServer.get().start();
    }

    @Override
    public byte[] processCommandApdu(byte[] apdu, Bundle extras) {
        new Thread(() -> {
            byte[] resp = RelayServer.get().relayFrame(apdu);
            if (resp == null || resp.length < 2) {
                resp = new byte[]{(byte) 0x6F, (byte) 0x00};
            }
            sendResponseApdu(resp);
        }, "relay-apdu").start();
        return null; // async: response via sendResponseApdu
    }

    @Override
    public void onDeactivated(int reason) {
        RelayServer.get().notifyDeactivated(reason);
    }
}
