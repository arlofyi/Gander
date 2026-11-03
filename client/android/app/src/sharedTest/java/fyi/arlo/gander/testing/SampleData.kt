package fyi.arlo.gander.testing

import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.net.SpkiPin
import fyi.arlo.gander.core.net.protocol.Capabilities
import fyi.arlo.gander.core.net.protocol.ClipboardMode
import fyi.arlo.gander.core.net.protocol.DeviceSummary
import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.EntryFlag
import fyi.arlo.gander.core.net.protocol.EntryKind
import fyi.arlo.gander.core.net.protocol.FetchCapabilities
import fyi.arlo.gander.core.net.protocol.HashAlgo
import fyi.arlo.gander.core.net.protocol.HostDetails
import fyi.arlo.gander.core.net.protocol.HostOs
import fyi.arlo.gander.core.net.protocol.Limits
import fyi.arlo.gander.core.net.protocol.MeResponse
import fyi.arlo.gander.core.net.protocol.NetworkAdapter
import fyi.arlo.gander.core.net.protocol.NetworkInfo
import fyi.arlo.gander.core.net.protocol.OfficePreview
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.core.net.protocol.PreviewKinds
import fyi.arlo.gander.core.net.protocol.RelayPath
import fyi.arlo.gander.core.net.protocol.SearchCapability
import fyi.arlo.gander.core.net.protocol.ShareInfo
import fyi.arlo.gander.core.net.protocol.StreamCapabilities
import fyi.arlo.gander.core.net.protocol.StreamDecision
import fyi.arlo.gander.core.net.protocol.VideoCodec
import java.util.Base64

/** Builders for hosts, shares and entries. Nothing here is a secret: the pin is a fixed byte pattern. */
object SampleData {
    /** A syntactically valid host id (base64url SHA-256) that matches no real certificate. */
    val HOST_ID: String by lazy { pin().hostId }

    fun pin(): SpkiPin = SpkiPin.fromHostId(
        Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32) { it.toByte() }),
    )

    fun host(
        name: String = "studio-pc",
        id: String = HOST_ID,
        lastSeenAtMillis: Long? = null,
        pairedAs: String? = "Pixel 9",
    ): PairedHost = PairedHost(
        id = id,
        name = name,
        pin = pin(),
        addresses = listOf("100.101.102.103", "fd7a:115c:a1e0::1"),
        os = HostOs.WINDOWS,
        pairedAs = pairedAs,
        lastSeenAtMillis = lastSeenAtMillis,
    )

    fun share(
        id: String = "shr_projects",
        name: String = "Projects",
        permissions: List<Permission> = listOf(Permission.LIST, Permission.PREVIEW, Permission.FETCH, Permission.STREAM),
        pathDisplay: String? = null,
        search: SearchCapability? = null,
    ) = ShareInfo(id, name, pathDisplay, permissions, showHidden = false, streamDefault = StreamDecision.ASK, search = search)

    fun file(
        name: String,
        size: Long = 1024,
        mtime: String = "2026-10-30T09:12:44Z",
        flags: List<EntryFlag> = emptyList(),
        preview: PreviewKind = PreviewKind.NONE,
        path: String? = null,
        share: String? = null,
    ) = Entry(
        name = name,
        kind = EntryKind.FILE,
        size = size,
        mtime = mtime,
        ctime = null,
        ext = name.substringAfterLast('.', "").ifEmpty { null },
        mime = null,
        flags = flags,
        thumb = preview == PreviewKind.IMAGE,
        preview = preview,
        stream = StreamDecision.ASK,
        share = share,
        path = path,
    )

    fun dir(
        name: String,
        mtime: String = "2026-10-29T08:00:00Z",
        flags: List<EntryFlag> = emptyList(),
        path: String? = null,
    ) = Entry(
        name = name,
        kind = EntryKind.DIR,
        size = 0,
        mtime = mtime,
        ctime = null,
        ext = null,
        mime = null,
        flags = flags,
        thumb = false,
        preview = PreviewKind.NONE,
        stream = StreamDecision.REFUSED,
        path = path,
    )

    fun me(
        relayed: RelayPath? = null,
        deviceName: String = "Pixel 9",
        hostName: String = "studio-pc",
        clipboard: ClipboardMode = ClipboardMode.OFF,
    ) = MeResponse(
        device = DeviceSummary("dev_01", deviceName, "2026-10-01T10:00:00Z", null),
        host = HostDetails(
            id = HOST_ID,
            name = hostName,
            os = HostOs.WINDOWS,
            osVersion = "10.0.26200",
            appVersion = "0.1.0",
            addresses = listOf("100.101.102.103"),
            magicDns = "studio-pc.tail1234.ts.net",
        ),
        network = NetworkInfo(NetworkAdapter.TAILSCALE, identity = null, relayed = relayed),
        capabilities = Capabilities(
            list = true,
            search = SearchCapability.BOUNDED,
            recent = true,
            events = true,
            thumbSizes = listOf(128, 256, 512),
            previewMaxWidth = 2048,
            previewKinds = PreviewKinds(
                image = true,
                pdf = true,
                text = true,
                office = OfficePreview.RENDER,
                videoPoster = true,
                raw = true,
            ),
            fetch = FetchCapabilities(range = true, hash = listOf(HashAlgo.BLAKE3, HashAlgo.SHA256)),
            stream = StreamCapabilities(
                enabled = true,
                codecs = listOf(VideoCodec.H264),
                maxSessions = 1,
                audio = false,
                input = true,
                fitToPhone = true,
                clipboard = clipboard,
            ),
            write = false,
        ),
        limits = Limits(listMax = 1000, searchMax = 500, concurrency = 8),
    )
}
