package fyi.arlo.gander.core.data.diagnostics

import java.security.MessageDigest
import java.security.SecureRandom
import java.util.Locale

/**
 * Removes what must not leave the phone from a diagnostics export (client spec, "Export
 * diagnostics produces a redacted log"). Two layers:
 *
 * - **Structured fields** are built with [maskAddress] and [tag], so the report never contains the
 *   raw value in the first place: addresses are masked, host, device, login and file names are
 *   replaced by a short hash that stays the same inside one export (so two lines about the same
 *   file can be matched) and differs between exports (a per-export salt; names cannot be looked up
 *   in a dictionary of common names).
 * - **[redact]** is the safety net over the finished text: bearer tokens and token-like fields,
 *   pairing links, the exact `secrets` and `names` this phone knows, URLs, IP addresses, file
 *   system paths and login names are replaced even if something upstream let them through.
 *
 * Host ids (SPKI pins) are hashes of public keys, not secrets, and stay as they are.
 *
 * Paths: a Windows path (drive letter or UNC) is blanked to the end of its line, because folder and
 * file names on Windows have spaces and where a name ends cannot be told; a POSIX path ends at the
 * next whitespace. The finished report holds no paths to begin with.
 *
 * @param secrets literal values to blank out wherever they appear (the bearer tokens of the paired
 *   hosts); values shorter than [MIN_LITERAL] characters are ignored so a short value cannot mangle text.
 * @param names literal values to replace by [tag], as value to kind (`"studio-pc" to "host"`).
 */
class DiagnosticsRedactor(
    secrets: Collection<String> = emptyList(),
    names: Map<String, String> = emptyMap(),
    private val salt: ByteArray = randomSalt(),
) {
    private val secretValues = secrets.filter { it.length >= MIN_LITERAL }.distinct().sortedByDescending { it.length }
    private val nameValues = names.filterKeys { it.length >= MIN_LITERAL }.entries.sortedByDescending { it.key.length }

    /** `kind#1a2b3c4d`: a stable pseudonym for [value] within this export. */
    fun tag(kind: String, value: String): String {
        val digest = MessageDigest.getInstance("SHA-256").run {
            update(salt)
            update(value.lowercase(Locale.ROOT).toByteArray(Charsets.UTF_8))
            digest()
        }
        return kind + "#" + digest.take(HASH_BYTES).joinToString("") { "%02x".format(it) }
    }

    /**
     * An address with the part that identifies the machine hidden: `100.64.*.*` for IPv4,
     * `fd7a:115c:*` for IPv6, `*.ts.net` / `*.local` for a name. The first two IP groups are kept
     * on purpose: they tell a tailnet address from a LAN one, which is what troubleshooting needs.
     */
    fun maskAddress(address: String): String {
        val a = address.trim().removePrefix("[").removeSuffix("]")
        if (a.isEmpty()) return a
        return when {
            IPV4.matchEntire(a) != null -> maskIpv4(a)
            ':' in a -> a.split(':').take(2).joinToString(":") + ":*"
            '.' in a -> if (a.lowercase(Locale.ROOT).endsWith(".ts.net")) "*.ts.net" else "*." + a.substringAfterLast('.')
            else -> "*"
        }
    }

    /** [text] with everything listed in the class comment replaced. Applying it twice changes nothing more. */
    fun redact(text: String): String {
        var out = text
        for (secret in secretValues) out = out.replace(secret, REDACTED)
        for ((value, kind) in nameValues) out = out.replace(value, tag(kind, value), ignoreCase = true)
        out = PAIRING_LINK.replace(out, "gander://pair?$REDACTED")
        out = BEARER.replace(out) { "Bearer $REDACTED" }
        out = SECRET_FIELD.replace(out) { it.groupValues[1] + REDACTED }
        out = URL.replace(out) { maskUrl(it) }
        out = IPV4_ANYWHERE.replace(out) { if (validIpv4(it.value)) maskIpv4(it.value) else it.value }
        out = IPV6_ANYWHERE.replace(out) { if (looksLikeIpv6(it.value)) maskAddress(it.value) else it.value }
        out = WINDOWS_PATH.replace(out, PATH)
        out = UNC_PATH.replace(out, PATH)
        out = POSIX_PATH.replace(out, PATH)
        out = EMAIL.replace(out, LOGIN)
        out = LOGIN_FIELD.replace(out) { it.groupValues[1] + LOGIN }
        out = DOMAIN_USER.replace(out, LOGIN)
        return out
    }

    private fun maskUrl(match: MatchResult): String {
        val scheme = match.groupValues[1]
        val authority = match.groupValues[2].substringAfterLast('@') // user:password@ never survives
        val rest = match.groupValues[3]
        val (host, port) = splitHostPort(authority)
        val shownHost = if (host.contains('*') || host.startsWith("<")) host else maskAddress(host)
        val hostAndPort = (if (':' in shownHost) "[$shownHost]" else shownHost) + (port?.let { ":$it" } ?: "")
        return scheme + "://" + hostAndPort + if (rest.isEmpty() || rest == "/") rest else "/$PATH"
    }

    private fun splitHostPort(authority: String): Pair<String, String?> {
        if (authority.startsWith("[")) {
            val end = authority.indexOf(']')
            if (end > 0) {
                val port = authority.substring(end + 1).removePrefix(":").takeIf { it.isNotEmpty() }
                return authority.substring(1, end) to port
            }
        }
        val colon = authority.lastIndexOf(':')
        if (colon > 0 && authority.indexOf(':') == colon) return authority.substring(0, colon) to authority.substring(colon + 1)
        return authority to null
    }

    private fun maskIpv4(address: String): String {
        val parts = address.split('.')
        return "${parts[0]}.${parts[1]}.*.*"
    }

    private fun validIpv4(text: String): Boolean =
        text.split('.').let { parts -> parts.size == 4 && parts.all { (it.toIntOrNull() ?: 256) in 0..255 } }

    /** Times such as `10:00:00` match the pattern too; real IPv6 has `::` or at least four groups. */
    private fun looksLikeIpv6(text: String): Boolean = "::" in text || text.count { it == ':' } >= 3

    companion object {
        const val REDACTED = "<redacted>"
        const val PATH = "<path>"
        const val LOGIN = "<login>"
        const val MIN_LITERAL = 4
        private const val HASH_BYTES = 4

        fun randomSalt(): ByteArray = ByteArray(16).also { SecureRandom().nextBytes(it) }

        private val IPV4 = Regex("""\d{1,3}(\.\d{1,3}){3}""")
        private val IPV4_ANYWHERE = Regex("""(?<![\w.])\d{1,3}(?:\.\d{1,3}){3}(?!\w|\.\d)""")
        private val IPV6_ANYWHERE = Regex("""(?<![\w:])[0-9a-fA-F]{1,4}(?::[0-9a-fA-F]{0,4}){2,7}(?![\w:*])""")
        private val URL = Regex("""(?i)\b(https?)://([^/\s"'<>?#]+)([^\s"'<>]*)""")
        private val PAIRING_LINK = Regex("""(?i)gander://pair\S*""")
        private val BEARER = Regex("""(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{4,}""")
        private val SECRET_FIELD = Regex(
            """(?i)(["']?\b(?:token|secret|password|passwd|authorization|api[_-]?key|pairing[_-]?secret)["']?\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s,;&}"']+)""",
        )
        private val WINDOWS_PATH = Regex("""(?i)(?<!\w)[a-z]:[\\/][^\r\n"'<>|*?]*""")
        private val UNC_PATH = Regex("""\\\\[^\r\n"'<>|*?]+""")
        private val POSIX_PATH = Regex("""(?<![\w:/.<>*#-])/(?:[^\s/"'<>|:*]+/)+[^\s/"'<>|:*]*""")
        private val EMAIL = Regex("""[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}""")
        private val LOGIN_FIELD = Regex("""(?i)(\b(?:login|user(?:name)?|account|owner)\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s,;&}"']+)""")
        private val DOMAIN_USER = Regex("""(?<![\w\\])[A-Za-z0-9_.-]{2,}\\[A-Za-z0-9_.-]{2,}(?![\w\\])""")
    }
}
