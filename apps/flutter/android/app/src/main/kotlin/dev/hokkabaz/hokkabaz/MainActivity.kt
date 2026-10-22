package dev.hokkabaz.hokkabaz

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import android.content.Intent
import android.os.Build
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        // The Rust core encrypts its secrets file with this key; the key itself is wrapped by the
        // Android Keystore and never leaves the device in clear text.
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "hokkabaz/keystore")
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "dataKey" -> try {
                        result.success(KeystoreKey.dataKey(applicationContext))
                    } catch (e: Exception) {
                        result.error("keystore", e.message, null)
                    }
                    else -> result.notImplemented()
                }
            }
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "hokkabaz/runtime")
            .setMethodCallHandler { call, result ->
                try {
                    when (call.method) {
                        "start" -> {
                            val i = Intent(applicationContext, RunService::class.java)
                                .putExtra("text", call.argument<String>("text"))
                            if (Build.VERSION.SDK_INT >= 26) startForegroundService(i) else startService(i)
                            result.success(null)
                        }
                        "stop" -> {
                            startService(
                                Intent(applicationContext, RunService::class.java).setAction(RunService.ACTION_STOP)
                            )
                            result.success(null)
                        }
                        "notify" -> {
                            Runtime.notify(
                                applicationContext,
                                call.argument<Int>("id") ?: 1,
                                call.argument<String>("title") ?: "Hokkabaz",
                                call.argument<String>("text") ?: "",
                            )
                            result.success(null)
                        }
                        "cancel" -> {
                            Runtime.cancel(applicationContext, call.argument<Int>("id") ?: 1)
                            result.success(null)
                        }
                        "battery" -> result.success(Runtime.battery(applicationContext))
                        "requestNotifications" -> {
                            if (Build.VERSION.SDK_INT >= 33) {
                                requestPermissions(arrayOf("android.permission.POST_NOTIFICATIONS"), 7)
                            }
                            result.success(null)
                        }
                        else -> result.notImplemented()
                    }
                } catch (e: Exception) {
                    result.error("runtime", e.message, null)
                }
            }
    }
}
