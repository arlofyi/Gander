package fyi.arlo.gander.core.data.pairing

import fyi.arlo.gander.core.net.HostEndpoint

/** Which of the two pairing flows is running (pairing spec). */
enum class PairingMethod {
    /** Flow A: the phone scanned the QR code, which carries the pin and the window secret. */
    QR,

    /** Flow B: the user typed an address; trust on first use for the first contact only, SAS checked by eye. */
    MANUAL,
}

/** Where to pair with, from either a scanned code or typed text. */
sealed interface PairingTarget {
    val method: PairingMethod

    /** Addresses to try in order. */
    val addresses: List<String>
    val port: Int

    /** What to call the computer on the progress screens before it has told us its name. */
    val label: String

    /** Flow A. */
    class Qr(val invite: PairingInvite) : PairingTarget {
        override val method get() = PairingMethod.QR
        override val addresses get() = invite.addresses + listOfNotNull(invite.magicDns)
        override val port get() = invite.port
        override val label get() = invite.hostName.ifBlank { invite.addresses.first() }

        override fun toString(): String = "Qr(<invite>)"
    }

    /** Flow B. */
    data class Manual(val address: ManualAddress) : PairingTarget {
        override val method get() = PairingMethod.MANUAL
        override val addresses get() = listOf(address.host)
        override val port get() = address.port
        override val label get() = address.host
    }
}

/** An address typed by the user: an IP literal or a DNS name, with an optional port. */
data class ManualAddress(val host: String, val port: Int) {
    /** The text to put back in the entry field (brackets around IPv6 when a port is shown). */
    fun toEntryText(): String = when {
        port == HostEndpoint.DEFAULT_PORT -> host
        host.contains(':') -> "[$host]:$port"
        else -> "$host:$port"
    }

    companion object {
        /**
         * Parses `host`, `host:port`, `[v6]` or `[v6]:port` (a bare IPv6 literal is fine too).
         * Whitespace around the text is ignored. Returns null for anything that is not an IP
         * literal or a plausible host name (RFC 1123 labels), or a port outside 1..65535.
         */
        fun parse(text: String): ManualAddress? {
            val input = text.trim()
            if (input.isEmpty()) return null
            val host: String
            val portText: String?
            if (input.startsWith("[")) {
                val close = input.indexOf(']')
                if (close < 0) return null
                host = input.substring(1, close)
                val rest = input.substring(close + 1)
                portText = when {
                    rest.isEmpty() -> null
                    rest.startsWith(":") -> rest.substring(1)
                    else -> return null
                }
                if (!IpLiteral.isIpv6(host)) return null
            } else if (input.count { it == ':' } > 1) {
                if (!IpLiteral.isIpv6(input)) return null
                host = input
                portText = null
            } else {
                val colon = input.indexOf(':')
                host = if (colon < 0) input else input.substring(0, colon)
                portText = if (colon < 0) null else input.substring(colon + 1)
                if (!IpLiteral.isIpv4(host) && !isHostName(host)) return null
            }
            val port = if (portText == null) {
                HostEndpoint.DEFAULT_PORT
            } else {
                portText.takeIf { it.isNotEmpty() && it.length <= 5 && it.all { c -> c in '0'..'9' } }
                    ?.toInt()?.takeIf { it in 1..65535 } ?: return null
            }
            return ManualAddress(host, port)
        }

        private fun isHostName(name: String): Boolean {
            val trimmed = name.removeSuffix(".")
            if (trimmed.isEmpty() || trimmed.length > 253) return false
            // A name whose last label is all digits is a malformed IPv4 address (999.1.1.1), not a name.
            if (trimmed.substringAfterLast('.').all { it in '0'..'9' }) return false
            return trimmed.split('.').all { label ->
                label.length in 1..63 &&
                    label.all { it in 'a'..'z' || it in 'A'..'Z' || it in '0'..'9' || it == '-' } &&
                    !label.startsWith('-') && !label.endsWith('-')
            }
        }
    }
}
