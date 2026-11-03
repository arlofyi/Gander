package fyi.arlo.gander.core.data.actions

import fyi.arlo.gander.core.data.browse.HostConnector
import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.protocol.Entry

/**
 * `GET /shares/{share}/stat?path=&meta=1` for the file sheet: the entry as the host sees it now,
 * with the type-specific metadata it can get cheaply. Throws [fyi.arlo.gander.core.net.GanderError]
 * and nothing else.
 */
fun interface EntryStatSource {
    suspend fun stat(host: PairedHost, shareId: String, path: String): Entry
}

/** [EntryStatSource] over the pinned connection of each host. */
class ConnectorEntryStatSource(private val connector: HostConnector) : EntryStatSource {
    override suspend fun stat(host: PairedHost, shareId: String, path: String): Entry =
        connector.connect(host).api.stat(shareId, path, meta = true)
}
