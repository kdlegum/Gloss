package com.gloss.app

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.OpenableColumns
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@TauriPlugin
class SharePlugin(private val activity: Activity) : Plugin(activity) {

    override fun onNewIntent(intent: Intent) {
        extractUri(intent)?.let { pendingUri = it }
    }

    @Command
    fun consumePendingShare(invoke: Invoke) {
        val uriStr = pendingUri
        pendingUri = null
        Thread {
            val fileName = uriStr?.let { queryDisplayName(it) }
            invoke.resolve(
                JSObject()
                    .put("uri", uriStr)
                    .put("fileName", fileName)
            )
        }.start()
    }

    private fun extractUri(intent: Intent?): String? {
        if (intent?.action != Intent.ACTION_SEND) return null
        if (intent.type != "application/pdf") return null
        val uri: Uri? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(Intent.EXTRA_STREAM)
        }
        return uri?.toString()
    }

    private fun queryDisplayName(uriStr: String): String? {
        return try {
            val uri = Uri.parse(uriStr)
            activity.contentResolver.query(
                uri,
                arrayOf(OpenableColumns.DISPLAY_NAME),
                null, null, null
            )?.use { cursor ->
                if (cursor.moveToFirst()) cursor.getString(0) else null
            }
        } catch (_: Exception) {
            null
        }
    }

    companion object {
        @JvmField
        @Volatile
        var pendingUri: String? = null
    }
}
