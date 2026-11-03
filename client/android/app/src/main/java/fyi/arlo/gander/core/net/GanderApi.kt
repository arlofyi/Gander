package fyi.arlo.gander.core.net

import fyi.arlo.gander.core.net.protocol.Entry
import fyi.arlo.gander.core.net.protocol.HashAlgo
import fyi.arlo.gander.core.net.protocol.HashResponse
import fyi.arlo.gander.core.net.protocol.HostHello
import fyi.arlo.gander.core.net.protocol.ListResponse
import fyi.arlo.gander.core.net.protocol.MeResponse
import fyi.arlo.gander.core.net.protocol.PairApproved
import fyi.arlo.gander.core.net.protocol.PairPollState
import fyi.arlo.gander.core.net.protocol.PairPollStatus
import fyi.arlo.gander.core.net.protocol.PairRequest
import fyi.arlo.gander.core.net.protocol.PairStarted
import fyi.arlo.gander.core.net.protocol.PreviewInfo
import fyi.arlo.gander.core.net.protocol.RecentResponse
import fyi.arlo.gander.core.net.protocol.SearchResponse
import fyi.arlo.gander.core.net.protocol.SharesResponse
import fyi.arlo.gander.core.net.protocol.StreamCreated
import fyi.arlo.gander.core.net.protocol.StreamRequest
import fyi.arlo.gander.core.net.protocol.TokenRotateResponse
import fyi.arlo.gander.core.net.protocol.ZipPlan
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.DeserializationStrategy
import kotlinx.serialization.SerializationStrategy
import okhttp3.Call
import okhttp3.Callback
import okhttp3.HttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import java.io.IOException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/**
 * Typed calls for protocol v1 against one host. Every function throws [GanderError] (never a raw
 * OkHttp or serialization exception), so screens can `catch (e: GanderError)` and explain it.
 *
 * Binary endpoints (thumb, preview page, preview text, file) are exposed as [Request] builders:
 * run them on [GanderHttpClients.transfer] from Coil, Media3 or the fetch engine.
 */
class GanderApi(private val client: OkHttpClient, private val endpoint: HostEndpoint) {

    constructor(clients: GanderHttpClients) : this(clients.api, clients.endpoint)

    // Host and identity -----------------------------------------------------------------------

    suspend fun host(): HostHello = get(url("host"), HostHello.serializer())

    /** Also refreshes [endpoint] from `host.addresses` and `magic_dns` (client spec). */
    suspend fun me(): MeResponse = get(url("me"), MeResponse.serializer()).also { me ->
        endpoint.updateAddresses(me.host.addresses + listOfNotNull(me.host.magicDns))
    }

    suspend fun rotateToken(): TokenRotateResponse =
        post(url("token/rotate"), null, TokenRotateResponse.serializer())

    // Pairing ----------------------------------------------------------------------------------

    suspend fun startPairing(request: PairRequest): PairStarted =
        post(url("pair"), encode(PairRequest.serializer(), request), PairStarted.serializer())

    /** One poll of `GET /v1/pair/{id}`; poll every 1-2 s until it is not [PairPoll.Pending]. */
    suspend fun pollPairing(pairingId: String): PairPoll {
        val request = Request.Builder().url(url("pair").newBuilder().addPathSegment(pairingId).build()).build()
        return execute(request) { response, body ->
            when (response.code) {
                200 -> PairPoll.Approved(decode(body, PairApproved.serializer()))
                202 -> PairPoll.Pending
                403 -> {
                    val status = runCatching { decode(body, PairPollStatus.serializer()) }.getOrNull()
                    if (status?.state == PairPollState.DENIED) {
                        PairPoll.Denied
                    } else {
                        throw GanderError.fromResponse(403, body, response.header("Retry-After"))
                    }
                }
                else -> throw GanderError.fromResponse(response.code, body, response.header("Retry-After"))
            }
        }
    }

    // Browse -----------------------------------------------------------------------------------

    suspend fun shares(): SharesResponse = get(url("shares"), SharesResponse.serializer())

    /**
     * One page of a directory. Pass the previous [ListResponse.etag] as [ifNoneMatch] to get
     * [Listing.NotModified] instead of a re-sent page.
     */
    suspend fun list(
        share: String,
        path: String = "",
        cursor: String? = null,
        limit: Int? = null,
        sort: String? = null,
        order: String? = null,
        kinds: String? = null,
        ext: String? = null,
        ifNoneMatch: String? = null,
    ): Listing {
        val url = shareUrl(share, "list", path)
            .q("cursor", cursor).q("limit", limit).q("sort", sort).q("order", order).q("kinds", kinds).q("ext", ext)
            .build()
        val request = Request.Builder().url(url).apply { ifNoneMatch?.let { header("If-None-Match", it) } }.build()
        return execute(request) { response, body ->
            when {
                response.code == 304 -> Listing.NotModified
                response.isSuccessful -> Listing.Page(decode(body, ListResponse.serializer()))
                else -> throw GanderError.fromResponse(response.code, body, response.header("Retry-After"))
            }
        }
    }

    suspend fun stat(share: String, path: String, meta: Boolean = false): Entry =
        get(shareUrl(share, "stat", path).q("meta", if (meta) 1 else null).build(), Entry.serializer())

    suspend fun search(
        share: String,
        q: String,
        path: String = "",
        limit: Int? = null,
        cursor: String? = null,
        /** `content` also searches inside files (an indexed share); null is the host's default, names. */
        scope: String? = null,
    ): SearchResponse =
        get(
            shareUrl(share, "search", path).q("q", q).q("limit", limit).q("cursor", cursor).q("scope", scope).build(),
            SearchResponse.serializer(),
        )

    suspend fun recent(limit: Int? = null, since: String? = null): RecentResponse =
        get(url("recent").newBuilder().q("limit", limit).q("since", since).build(), RecentResponse.serializer())

    // Previews and hashes ----------------------------------------------------------------------

    suspend fun previewInfo(share: String, path: String): PreviewInfo =
        get(shareUrl(share, "preview/info", path).build(), PreviewInfo.serializer())

    suspend fun hash(share: String, path: String, algo: HashAlgo): HashResponse =
        get(shareUrl(share, "hash", path).q("algo", algo.wire).build(), HashResponse.serializer())

    /** [size] must be one of `capabilities.thumb_sizes`; the host answers 400 otherwise. */
    fun thumbRequest(share: String, path: String, size: Int, priority: ThumbPriority? = null): Request =
        Request.Builder().url(
            shareUrl(share, "thumb", path).q("size", size).q("priority", priority?.wire).build(),
        ).build()

    fun previewPageRequest(share: String, path: String, page: Int = 1, width: Int? = null, format: String? = null) =
        Request.Builder().url(
            shareUrl(share, "preview", path).q("page", page).q("width", width).q("format", format).build(),
        ).build()

    /** One 512 px tile of a page (`GET /preview/tile`, WP-67); the host answers `image/webp`. */
    fun previewTileRequest(share: String, path: String, page: Int, level: Int, x: Int, y: Int): Request =
        Request.Builder().url(
            shareUrl(share, "preview/tile", path).q("page", page).q("level", level).q("x", x).q("y", y).build(),
        ).build()

    fun previewTextRequest(share: String, path: String, offset: Long = 0, limit: Int? = null): Request =
        Request.Builder().url(shareUrl(share, "preview/text", path).q("offset", offset).q("limit", limit).build())
            .build()

    /** URL of the file bytes; add `Range`/`If-Range` for resume. HEAD works on the same URL. */
    fun fileUrl(share: String, path: String): HttpUrl = shareUrl(share, "file", path).build()

    // Folder zip (WP-68) -----------------------------------------------------------------------

    /** What zipping the folder at [path] would hold (`dry_run=1`); `413 too_large` when it is over the host's limits. */
    suspend fun zipPlan(share: String, path: String): ZipPlan =
        get(zipUrl(share, path).newBuilder().addQueryParameter("dry_run", "1").build(), ZipPlan.serializer())

    /**
     * URL of the folder as one zip, streamed (no `Range`, no resume: a broken download starts
     * over). The `share` is a query parameter here, not a path segment.
     */
    fun zipUrl(share: String, path: String): HttpUrl =
        url("zip").newBuilder().addQueryParameter("share", share).addQueryParameter("path", path).build()

    // Stream -----------------------------------------------------------------------------------

    suspend fun startStream(request: StreamRequest): StreamCreated =
        post(url("stream"), encode(StreamRequest.serializer(), request), StreamCreated.serializer())

    suspend fun endStream(session: String, closeApp: Boolean = false) {
        val url = url("stream").newBuilder().addPathSegment(session).q("close_app", closeApp).build()
        execute(Request.Builder().url(url).delete().build()) { response, body ->
            if (!response.isSuccessful) {
                throw GanderError.fromResponse(response.code, body, response.header("Retry-After"))
            }
        }
    }

    // Plumbing ---------------------------------------------------------------------------------

    /** `https://<current address>:<port>/v1/<path>` */
    fun url(path: String): HttpUrl = endpoint.baseUrl().newBuilder().addPathSegments(path).build()

    private fun shareUrl(share: String, op: String, path: String): HttpUrl.Builder =
        endpoint.baseUrl().newBuilder()
            .addPathSegment("shares").addPathSegment(share).addPathSegments(op)
            .addQueryParameter("path", path)

    private fun HttpUrl.Builder.q(name: String, value: Any?): HttpUrl.Builder =
        if (value == null) this else addQueryParameter(name, value.toString())

    private suspend fun <T> get(url: HttpUrl, strategy: DeserializationStrategy<T>): T =
        execute(Request.Builder().url(url).build()) { response, body -> successBody(response, body, strategy) }

    private suspend fun <T> post(url: HttpUrl, json: String?, strategy: DeserializationStrategy<T>): T {
        val request = Request.Builder().url(url).post((json ?: "").toRequestBody(JSON)).build()
        return execute(request) { response, body -> successBody(response, body, strategy) }
    }

    private fun <T> successBody(response: Response, body: String, strategy: DeserializationStrategy<T>): T {
        if (!response.isSuccessful) throw GanderError.fromResponse(response.code, body, response.header("Retry-After"))
        return decode(body, strategy)
    }

    private fun <T> decode(body: String, strategy: DeserializationStrategy<T>): T = try {
        GanderJson.json.decodeFromString(strategy, body)
    } catch (e: IllegalArgumentException) { // includes SerializationException
        throw GanderError.MalformedResponse(e)
    }

    private fun <T> encode(strategy: SerializationStrategy<T>, value: T): String =
        GanderJson.json.encodeToString(strategy, value)

    private suspend fun <T> execute(request: Request, handle: (Response, String) -> T): T {
        try {
            return client.newCall(request).await().use { response ->
                if (response.body.contentLength() > MAX_JSON_BYTES) {
                    throw GanderError.MalformedResponse(IOException("response body over $MAX_JSON_BYTES bytes"))
                }
                val body = response.body.source().let { source ->
                    source.request(MAX_JSON_BYTES + 1)
                    if (source.buffer.size > MAX_JSON_BYTES) {
                        throw GanderError.MalformedResponse(IOException("response body over $MAX_JSON_BYTES bytes"))
                    }
                    source.buffer.readUtf8()
                }
                handle(response, body)
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: GanderError) {
            throw e
        } catch (e: Exception) {
            throw GanderError.fromThrowable(e)
        }
    }

    companion object {
        private val JSON = "application/json".toMediaType()

        /** JSON answers are small (list pages cap at 1000 entries); refuse anything absurd. */
        private const val MAX_JSON_BYTES = 16L * 1024 * 1024
    }
}

/** Result of [GanderApi.list]. */
sealed interface Listing {
    data class Page(val response: ListResponse) : Listing
    data object NotModified : Listing
}

/** Result of [GanderApi.pollPairing]. */
sealed interface PairPoll {
    data object Pending : PairPoll
    data object Denied : PairPoll
    data class Approved(val approved: PairApproved) : PairPoll
}

/** Suspends until the call completes; cancelling the coroutine cancels the call. */
suspend fun Call.await(): Response = suspendCancellableCoroutine { continuation ->
    continuation.invokeOnCancellation { cancel() }
    enqueue(
        object : Callback {
            override fun onResponse(call: Call, response: Response) {
                continuation.resume(response) { _, value, _ -> value.close() }
            }

            override fun onFailure(call: Call, e: IOException) {
                continuation.resumeWithException(e)
            }
        },
    )
}
