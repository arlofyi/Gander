package fyi.arlo.gander.core.data.hosts

import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.net.HostEndpoint
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.core.net.protocol.Capabilities
import fyi.arlo.gander.core.net.protocol.HostOs
import fyi.arlo.gander.core.net.protocol.RelayPath

/**
 * A computer this phone is paired with: everything needed to reach it (addresses, port, pin) plus
 * what the Hosts screen shows. The bearer token is NOT part of this class; it lives behind
 * [HostRepository.token] so that a `toString()` of a host can never leak it.
 */
data class PairedHost(
    /** The host id: base64url SHA-256 of the host certificate's SPKI, equal to `pin.hostId`. */
    val id: String,
    /** Display name; starts as the host's own name and can be renamed on the phone. */
    val name: String,
    val pin: SpkiPin,
    /** Tailnet IPv4, IPv6 and MagicDNS name, in preference order (never empty). */
    val addresses: List<String>,
    val port: Int = HostEndpoint.DEFAULT_PORT,
    val os: HostOs = HostOs.UNKNOWN,
    /** The device name this host knows the phone by (`/me.device.name`), for "paired as". */
    val pairedAs: String? = null,
    /** Wall-clock millis of the last successful contact, null if never reached since pairing. */
    val lastSeenAtMillis: Long? = null,
    /** Last `/me.capabilities`; later screens use it for default actions and honest hints. */
    val capabilities: Capabilities? = null,
) {
    init {
        require(addresses.isNotEmpty()) { "a paired host needs at least one address" }
    }
}

/** What the Hosts screen knows about a host right now. */
sealed interface HostStatus {
    /** No answer yet this session. */
    data object Checking : HostStatus

    /**
     * The host answered. [relay] is `/me.network.relayed` (null when direct, unknown or not
     * applicable); [notice] is set when the host is reachable but refused `/me` for a reason the
     * user should see (for example a network identity mismatch).
     */
    data class Online(val relay: RelayPath? = null, val notice: UiError? = null) : HostStatus {
        /** True when the path to the host goes through a relay (DERP or a peer relay). */
        val isRelayed: Boolean get() = relay == RelayPath.DERP || relay == RelayPath.PEER_RELAY
    }

    /** The host could not be used; [error] says why (offline, wrong certificate, revoked...). */
    data class Unavailable(val error: UiError) : HostStatus
}

/**
 * True when the host refused this phone's token (revoked on the computer, or unknown there): the
 * only ways forward are pairing again or forgetting the host, and the Hosts screen says exactly that.
 */
val HostStatus.isRemovedOnHost: Boolean
    get() = this is HostStatus.Unavailable && error.kind == fyi.arlo.gander.core.data.UiErrorKind.REVOKED
