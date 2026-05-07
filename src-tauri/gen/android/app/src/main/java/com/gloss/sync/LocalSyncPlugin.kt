package com.gloss.sync

import android.app.Activity
import android.content.ContentResolver
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.json.JSONArray
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.net.URLDecoder
import java.nio.charset.StandardCharsets

@InvokeArg
internal class TreeArgs {
  lateinit var treeUri: String
}

@InvokeArg
internal class RelativeArgs {
  lateinit var treeUri: String
  var relativePath: String = ""
}

@InvokeArg
internal class TextArgs {
  lateinit var treeUri: String
  lateinit var relativePath: String
  var contents: String = ""
}

@InvokeArg
internal class CopyPathToTreeArgs {
  lateinit var treeUri: String
  lateinit var sourcePath: String
  lateinit var destRelativePath: String
}

@InvokeArg
internal class CopyTreeToPathArgs {
  lateinit var treeUri: String
  lateinit var sourceRelativePath: String
  lateinit var destPath: String
}

private data class DocumentRef(
  val uri: Uri,
  val documentId: String,
  val name: String,
  val isDir: Boolean,
  val size: Long?
)

@TauriPlugin
class LocalSyncPlugin(private val activity: Activity) : Plugin(activity) {
  private val resolver: ContentResolver
    get() = activity.contentResolver

  @Command
  fun pickSyncFolder(invoke: Invoke) {
    val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
      addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
      addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
      addFlags(Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
      addFlags(Intent.FLAG_GRANT_PREFIX_URI_PERMISSION)
    }
    startActivityForResult(invoke, intent, "onPickSyncFolder")
  }

  @ActivityCallback
  fun onPickSyncFolder(invoke: Invoke, result: ActivityResult) {
    if (result.resultCode != Activity.RESULT_OK) {
      invoke.reject("cancelled")
      return
    }

    val uri = result.data?.data
    if (uri == null) {
      invoke.reject("No folder was selected")
      return
    }

    var takeFlags = (result.data?.flags ?: 0) and (
      Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
    )
    if (takeFlags == 0) {
      takeFlags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
    }
    try {
      resolver.takePersistableUriPermission(uri, takeFlags)
    } catch (err: Exception) {
      invoke.reject("Failed to keep Android folder permission: ${err.message ?: err}")
      return
    }

    val response = JSObject()
      .put("treeUri", uri.toString())
      .put("label", folderLabel(uri))
    invoke.resolve(response)
  }

  @Command
  fun folderReady(invoke: Invoke) {
    val args = invoke.parseArgs(TreeArgs::class.java)
    runSaf(invoke) {
      val root = rootRef(Uri.parse(args.treeUri))
      JSObject()
        .put("exists", true)
        .put("isDir", root.isDir)
        .put("size", root.size ?: 0L)
    }
  }

  @Command
  fun ensureDir(invoke: Invoke) {
    val args = invoke.parseArgs(RelativeArgs::class.java)
    runSaf(invoke) {
      ensureDir(Uri.parse(args.treeUri), args.relativePath)
      JSObject()
    }
  }

  @Command
  fun exists(invoke: Invoke) {
    val args = invoke.parseArgs(RelativeArgs::class.java)
    runSaf(invoke) {
      val treeUri = Uri.parse(args.treeUri)
      val doc = resolve(treeUri, args.relativePath, createDirs = false)
      JSObject()
        .put("exists", doc != null)
        .put("isDir", doc?.isDir ?: false)
        .put("size", doc?.size ?: 0L)
    }
  }

  @Command
  fun readTextFile(invoke: Invoke) {
    val args = invoke.parseArgs(RelativeArgs::class.java)
    runSaf(invoke) {
      val treeUri = Uri.parse(args.treeUri)
      val doc = resolve(treeUri, args.relativePath, createDirs = false)
        ?: throw IllegalStateException("File not found: ${args.relativePath}")
      if (doc.isDir) {
        throw IllegalStateException("Expected a file, got directory: ${args.relativePath}")
      }
      val bytes = resolver.openInputStream(doc.uri)?.use { it.readBytes() }
        ?: throw IllegalStateException("Could not read ${args.relativePath}")
      JSObject().put("contents", String(bytes, StandardCharsets.UTF_8))
    }
  }

  @Command
  fun writeTextFileAtomic(invoke: Invoke) {
    val args = invoke.parseArgs(TextArgs::class.java)
    runSaf(invoke) {
      writeBytesAtomic(
        Uri.parse(args.treeUri),
        args.relativePath,
        args.contents.toByteArray(StandardCharsets.UTF_8),
        mimeForName(args.relativePath)
      )
      JSObject()
    }
  }

  @Command
  fun listRecursive(invoke: Invoke) {
    val args = invoke.parseArgs(RelativeArgs::class.java)
    runSaf(invoke) {
      val treeUri = Uri.parse(args.treeUri)
      val start = resolve(treeUri, args.relativePath, createDirs = false)
        ?: throw IllegalStateException("Directory not found: ${args.relativePath}")
      if (!start.isDir) {
        throw IllegalStateException("Expected a directory, got file: ${args.relativePath}")
      }
      val entries = JSONArray()
      listRecursive(treeUri, start, normalizedRelative(args.relativePath), entries)
      JSObject().put("entries", entries)
    }
  }

  @Command
  fun deleteFile(invoke: Invoke) {
    val args = invoke.parseArgs(RelativeArgs::class.java)
    runSaf(invoke) {
      val doc = resolve(Uri.parse(args.treeUri), args.relativePath, createDirs = false)
      if (doc != null) {
        DocumentsContract.deleteDocument(resolver, doc.uri)
      }
      JSObject()
    }
  }

  @Command
  fun copyPathToTree(invoke: Invoke) {
    val args = invoke.parseArgs(CopyPathToTreeArgs::class.java)
    runSaf(invoke) {
      val source = File(args.sourcePath)
      if (!source.isFile) {
        throw IllegalStateException("Source file is missing: ${args.sourcePath}")
      }
      copyLocalFileAtomic(Uri.parse(args.treeUri), args.destRelativePath, source)
      JSObject()
    }
  }

  @Command
  fun copyTreeToPath(invoke: Invoke) {
    val args = invoke.parseArgs(CopyTreeToPathArgs::class.java)
    runSaf(invoke) {
      val source = resolve(Uri.parse(args.treeUri), args.sourceRelativePath, createDirs = false)
        ?: throw IllegalStateException("Source file is missing: ${args.sourceRelativePath}")
      if (source.isDir) {
        throw IllegalStateException("Expected a file, got directory: ${args.sourceRelativePath}")
      }
      val dest = File(args.destPath)
      dest.parentFile?.mkdirs()
      resolver.openInputStream(source.uri)?.use { input ->
        FileOutputStream(dest).use { output ->
          input.copyTo(output)
        }
      } ?: throw IllegalStateException("Could not read ${args.sourceRelativePath}")
      JSObject()
    }
  }

  @Command
  fun hashFile(invoke: Invoke) {
    val args = invoke.parseArgs(RelativeArgs::class.java)
    runSaf(invoke) {
      val source = resolve(Uri.parse(args.treeUri), args.relativePath, createDirs = false)
        ?: throw IllegalStateException("File not found: ${args.relativePath}")
      if (source.isDir) {
        throw IllegalStateException("Expected a file, got directory: ${args.relativePath}")
      }
      var hash = FNV_OFFSET
      val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
      resolver.openInputStream(source.uri)?.use { input ->
        while (true) {
          val count = input.read(buffer)
          if (count <= 0) break
          for (i in 0 until count) {
            hash = hash xor (buffer[i].toLong() and 0xffL)
            hash *= FNV_PRIME
          }
        }
      } ?: throw IllegalStateException("Could not read ${args.relativePath}")
      JSObject().put("hash", java.lang.Long.toUnsignedString(hash, 16).padStart(16, '0'))
    }
  }

  private fun runSaf(invoke: Invoke, block: () -> JSObject) {
    Thread {
      try {
        invoke.resolve(block())
      } catch (err: Exception) {
        invoke.reject(err.message ?: err.toString())
      }
    }.start()
  }

  private fun rootRef(treeUri: Uri): DocumentRef {
    val docId = DocumentsContract.getTreeDocumentId(treeUri)
    val docUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, docId)
    return queryDocument(docUri, docId, "")
      ?: throw IllegalStateException("Selected folder is no longer accessible")
  }

  private fun resolve(treeUri: Uri, relativePath: String, createDirs: Boolean): DocumentRef? {
    var current = rootRef(treeUri)
    val parts = pathParts(relativePath)
    if (parts.isEmpty()) return current

    for ((index, part) in parts.withIndex()) {
      val child = findChild(treeUri, current.documentId, part)
      if (child == null) {
        if (!createDirs || index != parts.lastIndex) return null
        val created = DocumentsContract.createDocument(
          resolver,
          current.uri,
          Document.MIME_TYPE_DIR,
          part
        ) ?: throw IllegalStateException("Could not create directory: $relativePath")
        return queryDocument(created, DocumentsContract.getDocumentId(created), part)
      }
      current = child
    }
    return current
  }

  private fun ensureDir(treeUri: Uri, relativePath: String): DocumentRef {
    var current = rootRef(treeUri)
    for (part in pathParts(relativePath)) {
      val existing = findChild(treeUri, current.documentId, part)
      current = if (existing != null) {
        if (!existing.isDir) {
          throw IllegalStateException("Expected directory but found file: $relativePath")
        }
        existing
      } else {
        val created = DocumentsContract.createDocument(
          resolver,
          current.uri,
          Document.MIME_TYPE_DIR,
          part
        ) ?: throw IllegalStateException("Could not create directory: $relativePath")
        queryDocument(created, DocumentsContract.getDocumentId(created), part)
          ?: throw IllegalStateException("Could not inspect created directory: $relativePath")
      }
    }
    return current
  }

  private fun parentDir(treeUri: Uri, relativePath: String): Pair<DocumentRef, String> {
    val parts = pathParts(relativePath)
    if (parts.isEmpty()) {
      throw IllegalStateException("File path was empty")
    }
    val parent = if (parts.size == 1) {
      rootRef(treeUri)
    } else {
      ensureDir(treeUri, parts.dropLast(1).joinToString("/"))
    }
    return parent to parts.last()
  }

  private fun writeBytesAtomic(treeUri: Uri, relativePath: String, bytes: ByteArray, mime: String) {
    val (parent, name) = parentDir(treeUri, relativePath)
    val tmpName = "$name.tmp"
    findChild(treeUri, parent.documentId, tmpName)?.let {
      DocumentsContract.deleteDocument(resolver, it.uri)
    }

    val tmp = DocumentsContract.createDocument(resolver, parent.uri, mime, tmpName)
      ?: throw IllegalStateException("Could not create temporary file: $relativePath")
    resolver.openOutputStream(tmp, "w")?.use { output ->
      output.write(bytes)
    } ?: throw IllegalStateException("Could not write temporary file: $relativePath")

    publishTempDocument(treeUri, parent, name, tmp, mime, relativePath)
  }

  private fun copyLocalFileAtomic(treeUri: Uri, relativePath: String, source: File) {
    val (parent, name) = parentDir(treeUri, relativePath)
    val mime = mimeForName(relativePath)
    val tmpName = "$name.tmp"
    findChild(treeUri, parent.documentId, tmpName)?.let {
      DocumentsContract.deleteDocument(resolver, it.uri)
    }

    val tmp = DocumentsContract.createDocument(resolver, parent.uri, mime, tmpName)
      ?: throw IllegalStateException("Could not create temporary file: $relativePath")
    FileInputStream(source).use { input ->
      resolver.openOutputStream(tmp, "w")?.use { output ->
        input.copyTo(output)
      } ?: throw IllegalStateException("Could not write temporary file: $relativePath")
    }

    publishTempDocument(treeUri, parent, name, tmp, mime, relativePath)
  }

  private fun publishTempDocument(
    treeUri: Uri,
    parent: DocumentRef,
    name: String,
    tmp: Uri,
    mime: String,
    relativePath: String
  ) {
    findChild(treeUri, parent.documentId, name)?.let {
      DocumentsContract.deleteDocument(resolver, it.uri)
    }

    val renamed = try {
      DocumentsContract.renameDocument(resolver, tmp, name)
    } catch (_: Exception) {
      null
    }
    if (renamed != null && findChild(treeUri, parent.documentId, name) != null) return

    val finalUri = DocumentsContract.createDocument(resolver, parent.uri, mime, name)
      ?: throw IllegalStateException("Could not create file: $relativePath")
    resolver.openInputStream(tmp)?.use { input ->
      resolver.openOutputStream(finalUri, "w")?.use { output ->
        input.copyTo(output)
      } ?: throw IllegalStateException("Could not write file: $relativePath")
    } ?: throw IllegalStateException("Could not read temporary file: $relativePath")
    DocumentsContract.deleteDocument(resolver, tmp)
  }

  private fun findChild(treeUri: Uri, parentDocumentId: String, name: String): DocumentRef? {
    val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, parentDocumentId)
    val projection = arrayOf(
      Document.COLUMN_DOCUMENT_ID,
      Document.COLUMN_DISPLAY_NAME,
      Document.COLUMN_MIME_TYPE,
      Document.COLUMN_SIZE
    )
    resolver.query(childrenUri, projection, null, null, null)?.use { cursor ->
      val idIndex = cursor.getColumnIndexOrThrow(Document.COLUMN_DOCUMENT_ID)
      val nameIndex = cursor.getColumnIndexOrThrow(Document.COLUMN_DISPLAY_NAME)
      val mimeIndex = cursor.getColumnIndexOrThrow(Document.COLUMN_MIME_TYPE)
      val sizeIndex = cursor.getColumnIndex(Document.COLUMN_SIZE)
      while (cursor.moveToNext()) {
        val displayName = cursor.getString(nameIndex)
        if (displayName != name) continue
        val docId = cursor.getString(idIndex)
        val mime = cursor.getString(mimeIndex)
        val uri = DocumentsContract.buildDocumentUriUsingTree(treeUri, docId)
        val size = if (sizeIndex >= 0 && !cursor.isNull(sizeIndex)) cursor.getLong(sizeIndex) else null
        return DocumentRef(uri, docId, displayName, mime == Document.MIME_TYPE_DIR, size)
      }
    }
    return null
  }

  private fun queryDocument(uri: Uri, fallbackDocumentId: String, fallbackName: String): DocumentRef? {
    val projection = arrayOf(
      Document.COLUMN_DOCUMENT_ID,
      Document.COLUMN_DISPLAY_NAME,
      Document.COLUMN_MIME_TYPE,
      Document.COLUMN_SIZE
    )
    resolver.query(uri, projection, null, null, null)?.use { cursor ->
      if (!cursor.moveToFirst()) return null
      val idIndex = cursor.getColumnIndex(Document.COLUMN_DOCUMENT_ID)
      val nameIndex = cursor.getColumnIndex(Document.COLUMN_DISPLAY_NAME)
      val mimeIndex = cursor.getColumnIndex(Document.COLUMN_MIME_TYPE)
      val sizeIndex = cursor.getColumnIndex(Document.COLUMN_SIZE)
      val docId = if (idIndex >= 0) cursor.getString(idIndex) else fallbackDocumentId
      val name = if (nameIndex >= 0) cursor.getString(nameIndex) else fallbackName
      val mime = if (mimeIndex >= 0) cursor.getString(mimeIndex) else Document.MIME_TYPE_DIR
      val size = if (sizeIndex >= 0 && !cursor.isNull(sizeIndex)) cursor.getLong(sizeIndex) else null
      return DocumentRef(uri, docId, name, mime == Document.MIME_TYPE_DIR, size)
    }
    return null
  }

  private fun listRecursive(treeUri: Uri, dir: DocumentRef, prefix: String, out: JSONArray) {
    val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, dir.documentId)
    val projection = arrayOf(
      Document.COLUMN_DOCUMENT_ID,
      Document.COLUMN_DISPLAY_NAME,
      Document.COLUMN_MIME_TYPE,
      Document.COLUMN_SIZE
    )
    resolver.query(childrenUri, projection, null, null, null)?.use { cursor ->
      val idIndex = cursor.getColumnIndexOrThrow(Document.COLUMN_DOCUMENT_ID)
      val nameIndex = cursor.getColumnIndexOrThrow(Document.COLUMN_DISPLAY_NAME)
      val mimeIndex = cursor.getColumnIndexOrThrow(Document.COLUMN_MIME_TYPE)
      val sizeIndex = cursor.getColumnIndex(Document.COLUMN_SIZE)
      while (cursor.moveToNext()) {
        val docId = cursor.getString(idIndex)
        val name = cursor.getString(nameIndex)
        val mime = cursor.getString(mimeIndex)
        val isDir = mime == Document.MIME_TYPE_DIR
        val size = if (sizeIndex >= 0 && !cursor.isNull(sizeIndex)) cursor.getLong(sizeIndex) else 0L
        val path = if (prefix.isEmpty()) name else "$prefix/$name"
        out.put(
          JSObject()
            .put("path", path)
            .put("isDir", isDir)
            .put("size", size)
        )
        if (isDir) {
          val childUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, docId)
          listRecursive(treeUri, DocumentRef(childUri, docId, name, true, size), path, out)
        }
      }
    }
  }

  private fun pathParts(relativePath: String): List<String> {
    val normalized = normalizedRelative(relativePath)
    if (normalized.isEmpty()) return emptyList()
    val parts = normalized.split("/")
    if (parts.any { it.isEmpty() || it == "." || it == ".." }) {
      throw IllegalStateException("Invalid relative path: $relativePath")
    }
    return parts
  }

  private fun normalizedRelative(relativePath: String): String {
    return relativePath.trim().replace('\\', '/').trim('/')
  }

  private fun mimeForName(path: String): String {
    val lower = path.lowercase()
    return when {
      lower.endsWith(".pdf") -> "application/pdf"
      lower.endsWith(".db") -> "application/vnd.sqlite3"
      lower.endsWith(".json") -> "application/json"
      lower.endsWith(".txt") || lower.endsWith(".stignore") -> "text/plain"
      else -> "application/octet-stream"
    }
  }

  private fun folderLabel(uri: Uri): String {
    return try {
      val raw = DocumentsContract.getTreeDocumentId(uri)
      val tail = raw.substringAfterLast(':').ifEmpty { raw }
      URLDecoder.decode(tail, StandardCharsets.UTF_8.name()).ifEmpty { "Gloss Sync" }
    } catch (_: Exception) {
      uri.lastPathSegment ?: "Gloss Sync"
    }
  }

  companion object {
    private const val FNV_OFFSET = -3750763034362895579L
    private const val FNV_PRIME = 1099511628211L
  }
}
