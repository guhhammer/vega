package dev.guhhammer.vega.background

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

/** What the persistent notification should say. Mirrors `Notice` in Rust. */
@InvokeArg
class Notice {
    var title: String = "Vega is listening"
    var body: String = "Messages arrive slowly while the phone sleeps."
    var stopLabel: String = "Stop"
}

/**
 * The Rust side's handle on Android's background rules.
 *
 * Every command here is called from Rust through `run_mobile_plugin`, never
 * from JavaScript — which is why the plugin declares no commands to the ACL and
 * the app's capabilities file still grants no plugin surface at all.
 */
@TauriPlugin
class BackgroundPlugin(private val activity: Activity) : Plugin(activity) {

    /**
     * Start the foreground service, and the multicast lock with it.
     *
     * Idempotent: starting a running service re-delivers the notification and
     * changes nothing else, which is what makes it safe on every resume.
     */
    @Command
    fun start(invoke: Invoke) {
        val notice = invoke.parseArgs(Notice::class.java)

        askForNotifications()

        val intent = Intent(activity, NodeService::class.java).apply {
            action = NodeService.ACTION_START
            putExtra(NodeService.EXTRA_TITLE, notice.title)
            putExtra(NodeService.EXTRA_BODY, notice.body)
            putExtra(NodeService.EXTRA_STOP_LABEL, notice.stopLabel)
        }
        ContextCompat.startForegroundService(activity, intent)

        // Optimistic: the service has five seconds to call startForeground and
        // has not necessarily done it yet. `state` is the honest reading.
        invoke.resolve(currentState())
    }

    /** Stop the service. Delivery goes back to "only while the app is open". */
    @Command
    fun stop(invoke: Invoke) {
        activity.stopService(Intent(activity, NodeService::class.java))
        invoke.resolve(currentState())
    }

    /** What is actually running, right now. */
    @Command
    fun state(invoke: Invoke) {
        invoke.resolve(currentState())
    }

    /**
     * Open the system's battery optimisation dialog.
     *
     * The dialog is asynchronous and this returns as soon as it is on screen,
     * so the state reported here is the state *before* the answer. Read [state]
     * again on the next resume to find out what the user said.
     */
    @Command
    fun requestBatteryExemption(invoke: Invoke) {
        if (!isExempt()) {
            try {
                activity.startActivity(
                    Intent(
                        Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                        Uri.parse("package:${activity.packageName}"),
                    ),
                )
            } catch (e: android.content.ActivityNotFoundException) {
                // Some builds ship without the settings activity. Nothing to
                // recover — the app works without the exemption, slowly.
                invoke.reject("this device has no battery optimisation dialog", e)
                return
            }
        }
        invoke.resolve(currentState())
    }

    private fun currentState(): JSObject = JSObject().apply {
        put("running", NodeService.running)
        put("multicastHeld", NodeService.multicastHeld)
        put("batteryExempt", isExempt())
    }

    private fun isExempt(): Boolean {
        val power = activity.getSystemService(Context.POWER_SERVICE) as? PowerManager ?: return false
        return power.isIgnoringBatteryOptimizations(activity.packageName)
    }

    /**
     * Ask for POST_NOTIFICATIONS, and carry on regardless of the answer.
     *
     * A foreground service's notification is shown whether or not the
     * permission was granted, so refusing it costs the user nothing and blocks
     * nothing here. Asking is only so the prompt arrives with the app on screen
     * rather than never.
     */
    private fun askForNotifications() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return

        val granted = ContextCompat.checkSelfPermission(
            activity,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED

        if (!granted) {
            ActivityCompat.requestPermissions(
                activity,
                arrayOf(Manifest.permission.POST_NOTIFICATIONS),
                0,
            )
        }
    }
}
