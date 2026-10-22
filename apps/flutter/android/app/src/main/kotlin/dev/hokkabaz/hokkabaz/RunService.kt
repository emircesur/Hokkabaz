package dev.hokkabaz.hokkabaz

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.BatteryManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager

/// Keeps the process alive while an agent turn or flow run is in progress, so Android does not
/// freeze it as soon as the screen turns off. Started and stopped from Dart (`hokkabaz/runtime`).
class RunService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }
        val text = intent?.getStringExtra("text") ?: "Working in the background"
        val notification = Runtime.build(this, Runtime.CHANNEL_RUNS, "Hokkabaz", text, ongoing = true)
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
        return START_NOT_STICKY
    }

    companion object {
        const val ACTION_STOP = "dev.hokkabaz.hokkabaz.STOP_RUN"
        const val NOTIFICATION_ID = 4001
    }
}

object Runtime {
    const val CHANNEL_RUNS = "runs"
    const val CHANNEL_APPROVALS = "approvals"

    private fun ensureChannels(ctx: Context) {
        if (Build.VERSION.SDK_INT < 26) return
        val nm = ctx.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_RUNS, "Background runs", NotificationManager.IMPORTANCE_LOW)
        )
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_APPROVALS, "Approval requests", NotificationManager.IMPORTANCE_HIGH)
        )
    }

    fun build(ctx: Context, channel: String, title: String, text: String, ongoing: Boolean): Notification {
        ensureChannels(ctx)
        val open = PendingIntent.getActivity(
            ctx, 0, Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val b = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(ctx, channel) else Notification.Builder(ctx)
        return b.setContentTitle(title)
            .setContentText(text)
            .setSmallIcon(ctx.applicationInfo.icon)
            .setContentIntent(open)
            .setOngoing(ongoing)
            .setAutoCancel(!ongoing)
            .build()
    }

    fun notify(ctx: Context, id: Int, title: String, text: String) {
        ctx.getSystemService(NotificationManager::class.java)
            .notify(id, build(ctx, CHANNEL_APPROVALS, title, text, ongoing = false))
    }

    fun cancel(ctx: Context, id: Int) {
        ctx.getSystemService(NotificationManager::class.java).cancel(id)
    }

    fun battery(ctx: Context): Map<String, Any> {
        val bm = ctx.getSystemService(Context.BATTERY_SERVICE) as BatteryManager
        val pm = ctx.getSystemService(Context.POWER_SERVICE) as PowerManager
        return mapOf(
            "level" to bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY),
            "charging" to bm.isCharging,
            "powerSave" to pm.isPowerSaveMode,
        )
    }
}
