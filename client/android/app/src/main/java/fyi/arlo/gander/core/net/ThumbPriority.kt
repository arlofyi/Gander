package fyi.arlo.gander.core.net

/**
 * The `priority` query parameter of `GET /shares/{share}/thumb` (preview-renderers spec,
 * "Scheduling"): the host renders thumbnails of rows on screen before prefetched ones.
 * Hosts ignore unknown query parameters (protocol spec, compatibility rules), so an older host
 * simply treats every request alike.
 */
enum class ThumbPriority(val wire: String) {
    /** The row is on screen now. */
    VISIBLE("visible"),

    /** Ahead of the viewport, nobody is looking at it yet. */
    PREFETCH("prefetch"),
}
