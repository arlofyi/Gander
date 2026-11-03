package fyi.arlo.gander.core.data.pairing

/**
 * Test-only builder for the pairing URI, a transcription of `QrPayload::to_uri` and `encode` in
 * `host/gander-core/src/pairing/qr.rs`: RFC 3986 unreserved characters stay literal, every other
 * byte of the UTF-8 text becomes `%XX`, the comma between addresses is literal, `m` is omitted when
 * there is no MagicDNS name.
 */
object QrUris {
    fun encode(text: String): String = buildString {
        for (b in text.toByteArray(Charsets.UTF_8)) {
            val c = (b.toInt() and 0xFF).toChar()
            if (c in 'A'..'Z' || c in 'a'..'z' || c in '0'..'9' || c == '-' || c == '.' || c == '_' || c == '~') {
                append(c)
            } else {
                append('%').append("%02X".format(b.toInt() and 0xFF))
            }
        }
    }

    fun build(
        hostId: String,
        addresses: List<String>,
        port: Int,
        hostName: String,
        secret: String,
        magicDns: String?,
    ): String {
        val uri = "gander://pair?v=1&id=${encode(hostId)}&a=${addresses.joinToString(",") { encode(it) }}" +
            "&p=$port&n=${encode(hostName)}&s=${encode(secret)}"
        return if (magicDns != null) "$uri&m=${encode(magicDns)}" else uri
    }
}
