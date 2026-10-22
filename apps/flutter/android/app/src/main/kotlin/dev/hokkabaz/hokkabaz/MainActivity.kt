package dev.hokkabaz.hokkabaz

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
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
    }
}
