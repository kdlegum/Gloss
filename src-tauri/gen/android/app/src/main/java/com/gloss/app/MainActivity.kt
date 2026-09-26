package com.gloss.app

import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    // Capture any share intent before Tauri initialises so it's ready when
    // the frontend calls consume_pending_share on startup.
    extractShareUri(intent)?.let { SharePlugin.pendingUri = it }
    super.onCreate(savedInstanceState)
  }

  // onNewIntent for the already-running case is handled by TauriActivity
  // → pluginManager.onNewIntent → SharePlugin.onNewIntent

  private fun extractShareUri(intent: Intent?): String? {
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
}
