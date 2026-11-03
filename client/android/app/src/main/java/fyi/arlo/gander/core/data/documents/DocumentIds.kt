package fyi.arlo.gander.core.data.documents

import java.util.Base64

/**
 * What a Files-app document id points at (WP-60). Ids are built from the host id, the share id and
 * the share-relative path only: the bearer token, the host's addresses and anything else secret
 * never enter one, so an id that leaks into another app's recents or a URI grant reveals nothing
 * beyond the names the user already sees.
 */
sealed interface DocumentRef {
    val hostId: String

    /** The root of one paired computer; its children are the shares. */
    data class Host(override val hostId: String) : DocumentRef

    /** A share root ([path] empty) or a file or folder in a share ([path] share-relative, `/` separators). */
    data class Item(override val hostId: String, val shareId: String, val path: String) : DocumentRef {
        val isShareRoot: Boolean get() = path.isEmpty()
    }
}

/**
 * Encodes and decodes document ids: `g1:<host>[:<share>:<path>]`, each part base64url (no
 * padding) of its UTF-8, so `:` and `/` inside a name cannot be confused with the separators and
 * the id is the same every time for the same file (stable across processes and app updates).
 *
 * Decoding is strict: anything that is not exactly what [encode] writes, or a path with an empty,
 * `.` or `..` segment, a backslash or a NUL, is rejected (null). Callers treat that as "not found".
 * The host resolves paths safely anyway; this keeps a forged id from even being sent.
 */
object DocumentIds {
    private const val PREFIX = "g1"
    private const val SEP = ':'

    /** The single document of the "Unlock Gander to browse" root shown while the app lock is engaged. */
    const val LOCKED = "locked"

    private val encoder = Base64.getUrlEncoder().withoutPadding()
    private val decoder = Base64.getUrlDecoder()

    fun encode(ref: DocumentRef): String = when (ref) {
        is DocumentRef.Host -> "$PREFIX$SEP${b64(ref.hostId)}"
        is DocumentRef.Item -> "$PREFIX$SEP${b64(ref.hostId)}$SEP${b64(ref.shareId)}$SEP${b64(ref.path)}"
    }

    fun host(hostId: String): String = encode(DocumentRef.Host(hostId))

    fun item(hostId: String, shareId: String, path: String): String = encode(DocumentRef.Item(hostId, shareId, path))

    fun decode(id: String): DocumentRef? {
        val parts = id.split(SEP)
        if (parts.firstOrNull() != PREFIX) return null
        return when (parts.size) {
            2 -> unb64(parts[1])?.takeIf { it.isNotEmpty() }?.let { DocumentRef.Host(it) }
            4 -> {
                val host = unb64(parts[1])?.takeIf { it.isNotEmpty() } ?: return null
                val share = unb64(parts[2])?.takeIf { it.isNotEmpty() } ?: return null
                val path = unb64(parts[3]) ?: return null
                if (!isCleanPath(path)) return null
                DocumentRef.Item(host, share, path)
            }
            else -> null
        }
    }

    /**
     * `DocumentsProvider.isChildDocument`: true when [child] is strictly below [parent] (any
     * depth), on the same host and, for items, in the same share.
     */
    fun isDescendant(parent: DocumentRef, child: DocumentRef): Boolean {
        if (parent.hostId != child.hostId || child !is DocumentRef.Item) return false
        return when (parent) {
            is DocumentRef.Host -> true
            is DocumentRef.Item -> parent.shareId == child.shareId && when {
                parent.path.isEmpty() -> child.path.isNotEmpty()
                else -> child.path.startsWith(parent.path + "/")
            }
        }
    }

    /** `""` (share root) or `a/b/c` with no empty, `.` or `..` segment, no `\` and no NUL. */
    fun isCleanPath(path: String): Boolean {
        if (path.isEmpty()) return true
        if ('\\' in path || '\u0000' in path) return false
        return path.split('/').none { it.isEmpty() || it == "." || it == ".." }
    }

    private fun b64(text: String): String = encoder.encodeToString(text.toByteArray(Charsets.UTF_8))

    private fun unb64(text: String): String? {
        if (text.any { !(it.isLetterOrDigit() && it.code < 128) && it != '-' && it != '_' }) return null
        val bytes = try {
            decoder.decode(text)
        } catch (_: IllegalArgumentException) {
            return null
        }
        val decoded = String(bytes, Charsets.UTF_8)
        // Only the canonical spelling is accepted, so one document has exactly one id.
        return decoded.takeIf { b64(it) == text }
    }
}
