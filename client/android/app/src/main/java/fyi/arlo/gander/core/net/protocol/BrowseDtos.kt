package fyi.arlo.gander.core.net.protocol

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

// Shares, listings, search, recent, previews and hashes.

@Serializable
data class ShareInfo(
    val id: String,
    val name: String,
    /** Only present if the owner enabled "show folder path to devices". */
    @SerialName("path_display") val pathDisplay: String? = null,
    val permissions: List<Permission>,
    @SerialName("show_hidden") val showHidden: Boolean,
    @SerialName("stream_default") val streamDefault: StreamDecision,
    /**
     * How `/search` works in this share (WP-66): [SearchCapability.INDEXED] when the share's folder
     * is in the computer's Windows Search index. Null from a host that predates it; read
     * `capabilities.search` then.
     */
    val search: SearchCapability? = null,
)

@Serializable
data class SharesResponse(val shares: List<ShareInfo>)

@Serializable
data class ImageMeta(
    val width: Int,
    val height: Int,
    val orientation: Int?,
    /** EXIF local time as stored (`2026:10:28 18:40:01`), not ISO-8601. */
    @SerialName("taken_at") val takenAt: String?,
    val camera: String?,
)

@Serializable
data class VideoMeta(
    val width: Int,
    val height: Int,
    @SerialName("duration_ms") val durationMs: Long,
    val codec: String?,
)

@Serializable
data class PdfMeta(val pages: Int)

@Serializable
data class AudioMeta(
    @SerialName("duration_ms") val durationMs: Long,
    val title: String?,
    val artist: String?,
)

@Serializable
data class OfficeMeta(val pages: Int?)

/** `EntryMeta` (stat with `meta=1`): exactly one of the fields is set, keyed by type. */
@Serializable
data class EntryMeta(
    val image: ImageMeta? = null,
    val video: VideoMeta? = null,
    val pdf: PdfMeta? = null,
    val audio: AudioMeta? = null,
    val office: OfficeMeta? = null,
)

/** One file or directory, from list, stat, search and recent. */
@Serializable
data class Entry(
    /** Exactly as stored on the host, no normalisation. */
    val name: String,
    val kind: EntryKind,
    val size: Long,
    val mtime: String,
    val ctime: String?,
    val ext: String? = null,
    val mime: String? = null,
    val flags: List<EntryFlag>,
    /** A prediction that the type is thumbnailable, not a promise. */
    val thumb: Boolean,
    val preview: PreviewKind,
    val stream: StreamDecision,
    /** Set in recent results. */
    val share: String? = null,
    /** Share-relative path; set in search and recent results. */
    val path: String? = null,
    val meta: EntryMeta? = null,
)

/**
 * `GET /v1/zip?dry_run=1` (WP-68): what a folder download would hold. [entries] counts the files
 * and folders below the folder asked for, [bytes] the total size of the files (the zip stores them
 * uncompressed), [skipped] the items left out (links, unsendable names; listed in
 * `_gander-skipped.txt` inside the zip) and [cloudOnly] the cloud placeholder files the computer
 * would have to download first.
 */
@Serializable
data class ZipPlan(
    val entries: Long,
    val bytes: Long,
    val skipped: Long,
    @SerialName("cloud_only") val cloudOnly: Long,
)

@Serializable
data class ListResponse(
    val path: String,
    val entries: List<Entry>,
    @SerialName("next_cursor") val nextCursor: String?,
    val truncated: Boolean,
    val unlistable: Int,
    val etag: String,
)

@Serializable
data class SearchResponse(
    val entries: List<Entry>,
    val partial: Boolean,
    @SerialName("next_cursor") val nextCursor: String?,
    /** What answered (WP-66); null from a host that predates it (a walk). */
    val via: SearchVia? = null,
)

@Serializable
data class RecentResponse(val entries: List<Entry>, val partial: Boolean)

@Serializable
data class PreviewInfo(
    val kind: PreviewKind,
    val pages: Int?,
    val width: Int?,
    val height: Int?,
    @SerialName("text_bytes") val textBytes: Long?,
    val renderer: String,
    @SerialName("estimated_ms") val estimatedMs: Long,
    /** Edge length of a deep-zoom tile in pixels (512); null when the host has no deep zoom for the file. */
    @SerialName("tile_size") val tileSize: Int? = null,
    /** Most levels any page has; null when there is no deep zoom. */
    @SerialName("tile_levels") val tileLevels: Int? = null,
    /** Top-level pixel size and level count of each page, index 0 being page 1; null when there is no deep zoom. */
    @SerialName("tile_pages") val tilePages: List<TilePage>? = null,
)

/** One page of `PreviewInfo.tile_pages`: the natural-resolution size of the page and how many levels its pyramid has. */
@Serializable
data class TilePage(val width: Int, val height: Int, val levels: Int)

@Serializable
data class HashResponse(
    val algo: HashAlgo,
    val hex: String,
    val size: Long,
    val etag: String,
)
