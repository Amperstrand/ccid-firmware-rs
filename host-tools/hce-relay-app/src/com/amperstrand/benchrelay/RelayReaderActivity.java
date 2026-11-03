package com.amperstrand.benchrelay;

import android.app.Activity;
import android.content.Intent;
import android.nfc.NfcAdapter;
import android.nfc.Tag;
import android.nfc.tech.IsoDep;
import android.os.Bundle;
import android.view.Gravity;
import android.widget.TextView;

/**
 * Phone-B ReaderMode companion (#529): a transparent activity started
 * over adb (`am start -n
 * com.amperstrand.benchrelay/.RelayReaderActivity`) that enables
 * reader mode and hands every discovered ISO-DEP tag to
 * RelayReaderServer. Finishing the activity disables reader mode and
 * invalidates the tag session — lifecycle-aware teardown.
 *
 * The card-safety authority is the HOST brain (hce_relay.py
 * --pcsc-policy): this side is deliberately a dumb transceive pipe.
 */
public class RelayReaderActivity extends Activity
        implements NfcAdapter.ReaderCallback {

    private NfcAdapter adapter;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        TextView label = new TextView(this);
        label.setText("bench reader-relay active\nkeep this screen on");
        label.setGravity(Gravity.CENTER);
        setContentView(label);
        adapter = NfcAdapter.getDefaultAdapter(this);
        RelayReaderServer.get().start();
    }

    @Override
    protected void onResume() {
        super.onResume();
        if (adapter != null) {
            adapter.enableReaderMode(this, this,
                    NfcAdapter.FLAG_READER_NFC_A
                            | NfcAdapter.FLAG_READER_SKIP_NDEF_CHECK,
                    null);
        }
    }

    @Override
    protected void onPause() {
        if (adapter != null) {
            adapter.disableReaderMode(this);
        }
        RelayReaderServer.get().closeTag();
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        RelayReaderServer.get().stop();
        super.onDestroy();
    }

    @Override
    public void onTagDiscovered(Tag tag) {
        IsoDep isoDep = IsoDep.get(tag);
        if (isoDep == null) {
            return;
        }
        String info = "uid=" + bytesToHex(tag.getId())
                + " max=" + isoDep.getMaxTransceiveLength()
                + " timeout=" + isoDep.getTimeout();
        RelayReaderServer.get().setTag(isoDep, info);
    }

    private static String bytesToHex(byte[] bytes) {
        StringBuilder sb = new StringBuilder();
        for (byte b : bytes) sb.append(String.format("%02x", b));
        return sb.toString();
    }
}
