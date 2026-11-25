package dev.antigravity.mobile.workspace

import android.content.ContentResolver
import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.security.MessageDigest

// Streams Android Storage Access Framework (API 21+) document trees to/from an app-local mirror
// in 8 KB chunks so Rust tools and Git inspection operate without loading whole trees into memory.
object SafWorkspaceSync {
    private const val MAX_SINGLE_FILE_BYTES = 4L * 1024L * 1024L
    private const val MAX_SYNCED_FILES = 400

    fun mirrorTreeFromSaf(context: Context, treeUri: Uri): File {
        val hash = sha256Short(treeUri.toString())
        val targetDir = File(context.filesDir, "workspaces/saf-$hash")
        targetDir.mkdirs()

        val rootDocId = DocumentsContract.getTreeDocumentId(treeUri)
        val resolver = context.contentResolver
        var counter = 0

        fun walk(docId: String, currentLocalDir: File, depth: Int) {
            if (depth > 6 || counter >= MAX_SYNCED_FILES) return
            currentLocalDir.mkdirs()
            val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, docId)
            val projection = arrayOf(
                DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                DocumentsContract.Document.COLUMN_DISPLAY_NAME,
                DocumentsContract.Document.COLUMN_MIME_TYPE,
                DocumentsContract.Document.COLUMN_SIZE
            )
            resolver.query(childrenUri, projection, null, null, null)?.use { cursor ->
                while (cursor.moveToNext() && counter < MAX_SYNCED_FILES) {
                    val childId = cursor.getString(0) ?: continue
                    val name = cursor.getString(1) ?: continue
                    val mime = cursor.getString(2) ?: ""
                    val size = if (cursor.isNull(3)) 0L else cursor.getLong(3)

                    if (name == "node_modules" || name == "target" || name == "build" || name == ".gradle") {
                        continue
                    }

                    if (mime == DocumentsContract.Document.MIME_TYPE_DIR) {
                        walk(childId, File(currentLocalDir, name), depth + 1)
                    } else if (size <= MAX_SINGLE_FILE_BYTES) {
                        val docUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, childId)
                        val destFile = File(currentLocalDir, name)
                        copyUriToFileStreaming(resolver, docUri, destFile)
                        counter++
                    }
                }
            }
        }

        walk(rootDocId, targetDir, 0)
        return targetDir
    }

    fun flushDirtyPathsToSaf(
        context: Context,
        treeUri: Uri,
        localRoot: File,
        dirtyEntries: List<String>
    ) {
        if (dirtyEntries.isEmpty()) return
        val resolver = context.contentResolver
        val rootDocId = DocumentsContract.getTreeDocumentId(treeUri)

        for (entry in dirtyEntries) {
            try {
                if (entry.startsWith("DELETED:")) {
                    val rel = entry.removePrefix("DELETED:")
                    findDocumentIdByRelativePath(resolver, treeUri, rootDocId, rel)?.let { docId ->
                        val docUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, docId)
                        DocumentsContract.deleteDocument(resolver, docUri)
                    }
                } else {
                    val localFile = File(localRoot, entry)
                    if (localFile.isFile) {
                        val docUri = ensureDocumentForRelativePath(resolver, treeUri, rootDocId, entry)
                        if (docUri != null) {
                            copyFileToUriStreaming(resolver, localFile, docUri)
                        }
                    }
                }
            } catch (_: Throwable) {
                // Keep workspace functional if user revoked external SAF folder permission mid-session.
            }
        }
    }

    private fun ensureDocumentForRelativePath(
        resolver: ContentResolver,
        treeUri: Uri,
        rootDocId: String,
        relPath: String
    ): Uri? {
        val segments = relPath.split('/').filter { it.isNotBlank() }
        if (segments.isEmpty()) return null
        var parentDocId = rootDocId

        for (idx in 0 until segments.size - 1) {
            val dirName = segments[idx]
            val existing = findChildDocument(resolver, treeUri, parentDocId, dirName)
            parentDocId = if (existing != null) {
                existing
            } else {
                val parentUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, parentDocId)
                val created = DocumentsContract.createDocument(
                    resolver,
                    parentUri,
                    DocumentsContract.Document.MIME_TYPE_DIR,
                    dirName
                ) ?: return null
                DocumentsContract.getDocumentId(created)
            }
        }

        val fileName = segments.last()
        val existingFileId = findChildDocument(resolver, treeUri, parentDocId, fileName)
        return if (existingFileId != null) {
            DocumentsContract.buildDocumentUriUsingTree(treeUri, existingFileId)
        } else {
            val parentUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, parentDocId)
            DocumentsContract.createDocument(resolver, parentUri, "text/plain", fileName)
        }
    }

    private fun findDocumentIdByRelativePath(
        resolver: ContentResolver,
        treeUri: Uri,
        rootDocId: String,
        relPath: String
    ): String? {
        val segments = relPath.split('/').filter { it.isNotBlank() }
        var currentId = rootDocId
        for (seg in segments) {
            currentId = findChildDocument(resolver, treeUri, currentId, seg) ?: return null
        }
        return currentId
    }

    private fun findChildDocument(
        resolver: ContentResolver,
        treeUri: Uri,
        parentDocId: String,
        targetName: String
    ): String? {
        val childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, parentDocId)
        val proj = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME
        )
        resolver.query(childrenUri, proj, null, null, null)?.use { c ->
            while (c.moveToNext()) {
                val id = c.getString(0) ?: continue
                val name = c.getString(1) ?: continue
                if (name == targetName) return id
            }
        }
        return null
    }

    private fun copyUriToFileStreaming(resolver: ContentResolver, src: Uri, dst: File) {
        dst.parentFile?.mkdirs()
        resolver.openInputStream(src)?.use { input ->
            FileOutputStream(dst).use { output ->
                val buf = ByteArray(8192)
                while (true) {
                    val n = input.read(buf)
                    if (n <= 0) break
                    output.write(buf, 0, n)
                }
            }
        }
    }

    private fun copyFileToUriStreaming(resolver: ContentResolver, src: File, dst: Uri) {
        resolver.openOutputStream(dst, "wt")?.use { output ->
            FileInputStream(src).use { input ->
                val buf = ByteArray(8192)
                while (true) {
                    val n = input.read(buf)
                    if (n <= 0) break
                    output.write(buf, 0, n)
                }
            }
        }
    }

    private fun sha256Short(input: String): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(input.toByteArray())
        val sb = StringBuilder()
        for (i in 0 until 6) {
            sb.append(String.format("%02x", digest[i]))
        }
        return sb.toString()
    }
}
