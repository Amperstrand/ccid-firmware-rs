package com.amperstrand.benchrelay;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

/**
 * Shell-kickable foreground service owning the relay socket (#529):
 *   adb shell am start-foreground-service \
 *       -n com.amperstrand.benchrelay/.RelayKickstartService
 *
 * The HostApduService alone is NOT enough: the NFC stack binds it
 * lazily (first field activation), but the bench host must connect its
 * card brain BEFORE any reader taps the phone. This service starts
 * RelayServer (idempotent) and holds the process alive with a
 * foreground notification — which also keeps Samsung's app freezer
 * (FreecessController, the 2026-10-31 bench lesson) away from the
 * relay.
 */
public class RelayKickstartService extends Service {
    private static final String TAG = "BenchHceRelay";
    private static final String CHANNEL_ID = "bench-relay";
    private static final int NOTIF_ID = 1;

    @Override
    public void onCreate() {
        super.onCreate();
        if (Build.VERSION.SDK_INT >= 26) {
            NotificationManager nm = getSystemService(NotificationManager.class);
            NotificationChannel ch = new NotificationChannel(
                    CHANNEL_ID, "Bench HCE relay",
                    NotificationManager.IMPORTANCE_MIN);
            nm.createNotificationChannel(ch);
            startForeground(NOTIF_ID, new Notification.Builder(this, CHANNEL_ID)
                    .setContentTitle("Bench HCE relay")
                    .setContentText("card brain socket: localabstract:bench-hce-relay")
                    .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
                    .build());
        } else {
            startForeground(NOTIF_ID, new Notification.Builder(this)
                    .setContentTitle("Bench HCE relay").build());
        }
        RelayServer.get().start();
        Log.i(TAG, "kickstart: relay server ensured");
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        RelayServer.get().start();
        return START_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
