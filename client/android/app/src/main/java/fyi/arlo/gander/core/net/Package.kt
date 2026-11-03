/**
 * Networking layer (WP-21): pinned-SPKI trust manager, per-host OkHttp client, bearer
 * interceptor, `HostEndpoint` address fallback, `GanderError` mapping, `/events` WebSocket and the
 * kotlinx.serialization DTOs mirroring `protocol/openapi.yaml`.
 *
 * Rules that apply to everything added here (docs/security.md): no system trust store, no
 * cleartext, never log tokens, pairing secrets or file contents.
 */
package fyi.arlo.gander.core.net
