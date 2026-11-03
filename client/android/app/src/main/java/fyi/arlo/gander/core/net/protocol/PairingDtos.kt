package fyi.arlo.gander.core.net.protocol

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

// Pairing DTOs (docs/specs/2026-11-01-pairing-and-trust.md). The QR secret and the issued token
// are secrets: the toString overrides keep them out of logs and crash messages.

@Serializable
data class PairingDevice(
    val name: String,
    val platform: String,
    val model: String,
    @SerialName("app_version") val appVersion: String,
)

/** `POST /v1/pair` body. [secret] comes from the QR code (flow A) and is absent in flow B. */
@Serializable
data class PairRequest(
    val secret: String? = null,
    val device: PairingDevice,
    /** 16 random bytes, base64url. */
    val nonce: String,
) {
    override fun toString(): String =
        "PairRequest(secret=${if (secret == null) "none" else "<redacted>"}, device=$device, nonce=$nonce)"
}

/** `POST /v1/pair` 200: pending; the SAS is shown on both screens. */
@Serializable
data class PairStarted(
    @SerialName("pairing_id") val pairingId: String,
    /** Six digits grouped 3+3, for example `482 913`. */
    val sas: String,
    @SerialName("host_nonce") val hostNonce: String,
    @SerialName("expires_at") val expiresAt: String,
)

/** `GET /v1/pair/{id}` 202 (pending) and 403 (denied). */
@Serializable
data class PairPollStatus(val state: PairPollState)

@Serializable
enum class PairPollState {
    @SerialName("pending") PENDING,
    @SerialName("denied") DENIED,
}

/** `GET /v1/pair/{id}` 200: approved. [token] is returned exactly once; store it, never log it. */
@Serializable
data class PairApproved(
    val token: String,
    @SerialName("device_id") val deviceId: String,
    val host: HostDetails,
) {
    override fun toString(): String = "PairApproved(token=<redacted>, deviceId=$deviceId, host=$host)"
}
