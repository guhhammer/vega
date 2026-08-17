package dev.guhhammer.vega.background

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat

/**
 * Keeps the app's process alive so the node inside it can keep its socket.
 *
 * This service holds no socket and moves no message. The whole of Vega is
 * already running in this process, in Rust; without a foreground service
 * Android kills that process seconds after the app leaves the screen, and this
 * is the only thing Android accepts as a reason not to. The multicast lock is
 * here for the same reason — it belongs to the process, and this is the part of
 * the process with a lifetime that matches the node's.
 *
 * What it cannot do is beat Doze. An unplugged, stationary, dark phone has its
 * network suspended whatever this service says, and traffic resumes in
 * maintenance windows the system schedules ever more rarely. Only the user can
 * lift that, from the battery optimisation dialog [BackgroundPlugin] opens.
 */
class NodeService : Service() {

    companion object {
        const val ACTION_START = "dev.guhhammer.vega.background.START"
        const val ACTION_STOP = "dev.guhhammer.vega.background.STOP"

        const val EXTRA_TITLE = "title"
        const val EXTRA_BODY = "body"
        const val EXTRA_STOP_LABEL = "stopLabel"

        private const val CHANNEL = "vega-delivery"
        private const val NOTIFICATION = 0x5645

        private const val TAG = "vega"

        private const val DEFAULT_TITLE = "Vega is listening"
        private const val DEFAULT_BODY = "Messages arrive slowly while the phone sleeps."
        private const val DEFAULT_STOP_LABEL = "Stop"

        /** Whether the service is up. Read by the plugin, written only here. */
        @Volatile
        @JvmStatic
        var running: Boolean = false
            private set

        /** Whether the multicast lock is currently held — Wi-Fi only. */
        @Volatile
        @JvmStatic
        var multicastHeld: Boolean = false
            private set
    }

    private var multicast: WifiManager.MulticastLock? = null
    private var wifiWatch: ConnectivityManager.NetworkCallback? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            // The notification's own action. Stopping is a legitimate thing to
            // want — delivery falls back to "only while the app is open", which
            // is where it was before any of this was written.
            stopSelf()
            return START_NOT_STICKY
        }

        // A null intent is a restart after the system killed us, which is what
        // START_STICKY asked for. The defaults carry the notification through
        // it; nothing else here depends on the extras.
        val title = intent?.getStringExtra(EXTRA_TITLE) ?: DEFAULT_TITLE
        val body = intent?.getStringExtra(EXTRA_BODY) ?: DEFAULT_BODY
        val stopLabel = intent?.getStringExtra(EXTRA_STOP_LABEL) ?: DEFAULT_STOP_LABEL

        // Five seconds from startForegroundService() to here, or the system
        // kills the process for it. Nothing slow belongs above this line.
        ServiceCompat.startForeground(
            this,
            NOTIFICATION,
            notification(title, body, stopLabel),
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
            } else {
                0
            },
        )

        running = true
        watchWifi()
        return START_STICKY
    }

    override fun onDestroy() {
        running = false
        stopWatchingWifi()
        releaseMulticast()
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        super.onDestroy()
    }

    /**
     * Hold the multicast lock while there is a Wi-Fi network and not otherwise.
     *
     * There is no multicast peer to find on a cellular connection, and the lock
     * costs battery for as long as it is held — so tying it to Wi-Fi rather
     * than to the service's lifetime is most of its cost avoided.
     */
    private fun watchWifi() {
        if (wifiWatch != null) return

        val connectivity = getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
        if (connectivity == null) {
            Log.w(TAG, "no ConnectivityManager; holding the multicast lock unconditionally")
            acquireMulticast()
            return
        }

        val wifi = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .build()

        val watch = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = acquireMulticast()
            override fun onLost(network: Network) = releaseMulticast()
        }

        try {
            connectivity.registerNetworkCallback(wifi, watch)
            wifiWatch = watch
        } catch (e: SecurityException) {
            Log.w(TAG, "cannot watch Wi-Fi state, holding the multicast lock instead", e)
            acquireMulticast()
        }
    }

    private fun stopWatchingWifi() {
        val watch = wifiWatch ?: return
        wifiWatch = null
        val connectivity = getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
        try {
            connectivity?.unregisterNetworkCallback(watch)
        } catch (e: IllegalArgumentException) {
            // Already gone. Nothing to undo.
            Log.d(TAG, "Wi-Fi callback was already unregistered", e)
        }
    }

    @Synchronized
    private fun acquireMulticast() {
        if (multicast?.isHeld == true) return

        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
        if (wifi == null) {
            Log.w(TAG, "no WifiManager; mDNS discovery will not receive")
            return
        }

        multicast = wifi.createMulticastLock("vega-mdns").apply {
            // Not reference counted: acquire and release are called from
            // network callbacks that can repeat, and a counter that drifts
            // either leaks the lock or drops it while it is still wanted.
            setReferenceCounted(false)
            acquire()
        }
        multicastHeld = true
        Log.i(TAG, "multicast lock held; mDNS can receive")
    }

    @Synchronized
    private fun releaseMulticast() {
        multicast?.let { if (it.isHeld) it.release() }
        multicast = null
        multicastHeld = false
    }

    private fun notification(title: String, body: String, stopLabel: String): Notification {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            // IMPORTANCE_LOW: no sound, no heads-up. This notification is the
            // price of staying alive, not something to interrupt anyone with.
            val channel = NotificationChannel(
                CHANNEL,
                "Delivery",
                NotificationManager.IMPORTANCE_LOW,
            ).apply {
                description = "Shown while Vega can receive messages in the background."
                setShowBadge(false)
            }
            manager.createNotificationChannel(channel)
        }

        val open = packageManager.getLaunchIntentForPackage(packageName)?.let {
            PendingIntent.getActivity(this, 0, it, PendingIntent.FLAG_IMMUTABLE)
        }

        val stop = PendingIntent.getService(
            this,
            1,
            Intent(this, NodeService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE,
        )

        return NotificationCompat.Builder(this, CHANNEL)
            .setContentTitle(title)
            .setContentText(body)
            .setSmallIcon(R.drawable.ic_vega_notification)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .setOngoing(true)
            .setSilent(true)
            .setShowWhen(false)
            .setContentIntent(open)
            .addAction(0, stopLabel, stop)
            .build()
    }
}
