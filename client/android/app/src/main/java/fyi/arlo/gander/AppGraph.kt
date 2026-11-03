package fyi.arlo.gander

import android.content.Context
import android.os.Build
import android.os.SystemClock
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.preferencesDataStoreFile
import fyi.arlo.gander.core.data.actions.ActionRefusals
import fyi.arlo.gander.core.data.actions.ConnectivityNetworkStateMonitor
import fyi.arlo.gander.core.data.actions.ConnectorEntryStatSource
import fyi.arlo.gander.core.data.actions.DataStoreDefaultActionPrefs
import fyi.arlo.gander.core.data.actions.DefaultActionPrefsRepository
import fyi.arlo.gander.core.data.actions.EntryStatSource
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.actions.NetworkStateMonitor
import fyi.arlo.gander.core.data.actions.PackageManagerHandlerLookup
import fyi.arlo.gander.core.data.actions.PhoneHandlerLookup
import coil3.ImageLoader
import coil3.disk.DiskCache
import fyi.arlo.gander.core.data.browse.BrowseRepository
import fyi.arlo.gander.core.data.browse.BrowserPrefsRepository
import fyi.arlo.gander.core.data.browse.DataStoreBrowserPrefs
import fyi.arlo.gander.core.data.browse.DefaultBrowseRepository
import fyi.arlo.gander.core.data.browse.HostConnector
import fyi.arlo.gander.core.data.browse.PinnedHostConnector
import fyi.arlo.gander.core.data.cache.CoilDerivedCaches
import fyi.arlo.gander.core.data.cache.DerivedCaches
import fyi.arlo.gander.core.data.db.GanderDatabase
import fyi.arlo.gander.core.data.db.RoomHostRepository
import fyi.arlo.gander.core.data.db.RoomListingSnapshotStore
import fyi.arlo.gander.core.data.db.RoomSharesSnapshotStore
import fyi.arlo.gander.core.data.actions.CachedFileLookup
import fyi.arlo.gander.core.data.db.RoomFetchStore
import fyi.arlo.gander.core.data.diagnostics.DiagnosticsCollector
import fyi.arlo.gander.core.data.diagnostics.DiagnosticsDevice
import fyi.arlo.gander.core.data.fetch.CacheEnforcer
import fyi.arlo.gander.core.data.fetch.ClientsFetchConnector
import fyi.arlo.gander.core.data.fetch.FetchEngine
import fyi.arlo.gander.core.data.fetch.FetchFiles
import fyi.arlo.gander.core.data.fetch.FetchSettingsSource
import fyi.arlo.gander.core.data.fetch.FetchStore
import fyi.arlo.gander.core.data.fetch.OfflineCachedFileLookup
import fyi.arlo.gander.core.data.fetch.OfflineFiles
import fyi.arlo.gander.ui.fetch.FetchService
import fyi.arlo.gander.core.data.hosts.ConnectivityPrivateNetworkMonitor
import fyi.arlo.gander.core.data.hosts.ConnectorHostProber
import fyi.arlo.gander.core.data.hosts.ApiTokenRotator
import fyi.arlo.gander.core.data.hosts.HostForgetter
import fyi.arlo.gander.core.data.hosts.HostProber
import fyi.arlo.gander.core.data.hosts.HostRepository
import fyi.arlo.gander.core.data.hosts.KeystoreTokenStore
import fyi.arlo.gander.core.data.hosts.PrivateNetworkMonitor
import fyi.arlo.gander.core.data.hosts.TokenRotator
import fyi.arlo.gander.core.data.hosts.TokenStore
import fyi.arlo.gander.core.data.pairing.DeviceInfo
import fyi.arlo.gander.core.data.pairing.PairingFlow
import fyi.arlo.gander.core.data.pairing.PairingService
import fyi.arlo.gander.core.data.preview.ClientPreviewSource
import fyi.arlo.gander.core.data.preview.PinnedVideoPlayerFactory
import fyi.arlo.gander.core.data.preview.PreviewPageComponents
import fyi.arlo.gander.core.data.preview.PreviewPageLoader
import fyi.arlo.gander.core.data.preview.PreviewTileLoader
import fyi.arlo.gander.core.data.preview.PreviewSource
import fyi.arlo.gander.core.data.preview.VideoPlayerFactory
import fyi.arlo.gander.core.data.preview.createPreviewDiskCache
import fyi.arlo.gander.core.data.security.AppLock
import fyi.arlo.gander.core.data.stream.DataStoreStreamPrefs
import fyi.arlo.gander.core.data.stream.StreamPrefsRepository
import fyi.arlo.gander.core.data.settings.DataStoreSettings
import fyi.arlo.gander.core.data.settings.SettingsFetchSource
import fyi.arlo.gander.core.data.settings.SettingsRepository
import fyi.arlo.gander.core.data.thumbs.ClientThumbSource
import fyi.arlo.gander.core.data.thumbs.PerHostSchedulers
import fyi.arlo.gander.core.data.thumbs.ThumbLoader
import fyi.arlo.gander.core.data.thumbs.UnknownThumbHostException
import fyi.arlo.gander.core.data.thumbs.createThumbnailImageLoader
import fyi.arlo.gander.core.net.GanderApi
import fyi.arlo.gander.core.net.GanderHttpClients
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.withTimeoutOrNull

/**
 * Manual dependency graph, one instance per process (held by [GanderApplication]).
 *
 * Later packages add their singletons here as lazy properties: the fetch engine, the Coil image
 * loader (WP-24), the file sheet's default-action rules (WP-25). Screens receive what they need
 * through constructor parameters or a ViewModel factory that reads from this graph; there is no
 * service locator beyond [Context.appGraph].
 *
 * What WP-23 and WP-22 put here:
 * - [tokenStore] is the Keystore-backed store (WP-22).
 * - [hostRepository] is Room-backed; [pairingFlow] calls `add` on it after pairing.
 */
class AppGraph(val appContext: Context) {

    /** Process-lifetime scope for work that must outlive a screen (DataStore). */
    private val appScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** [appScope] for screens that must finish something after they are gone (the stream viewer's `DELETE /stream`). */
    val processScope: CoroutineScope get() = appScope

    val database: GanderDatabase by lazy { GanderDatabase.create(appContext) }

    /** Settings DataStore (`datastore/settings.preferences_pb`); WP-28 adds its keys to the same file. */
    val settingsDataStore: DataStore<Preferences> by lazy {
        PreferenceDataStoreFactory.create(scope = appScope) { appContext.preferencesDataStoreFile("settings") }
    }

    /** The user's settings (WP-28): cellular rule, verification, cache cap, Stream-first types, app lock. */
    val settingsRepository: SettingsRepository by lazy { DataStoreSettings(settingsDataStore) }

    /** Bearer tokens: AES-GCM under an Android Keystore key, blobs in `datastore/tokens.preferences_pb`. */
    val tokenStore: TokenStore by lazy { KeystoreTokenStore.create(appContext, appScope) }

    val hostRepository: HostRepository by lazy { RoomHostRepository(database.hostDao(), tokenStore) }

    /** The Add host flow (QR and manual pairing); success stores the host and token in [hostRepository]. */
    val pairingFlow: PairingFlow by lazy {
        PairingService(
            hosts = hostRepository,
            device = { DeviceInfo.fromSystem(appContext, BuildConfig.VERSION_NAME) },
            appVersion = BuildConfig.VERSION_NAME,
        )
    }

    private val pinnedConnector: PinnedHostConnector by lazy {
        PinnedHostConnector(hostRepository, BuildConfig.VERSION_NAME)
    }

    val hostConnector: HostConnector get() = pinnedConnector

    /** The pinned clients of a paired host, for byte streams: thumbnails, preview pages, Media3. */
    val hostClients: suspend (hostId: String) -> GanderHttpClients = { hostId ->
        val host = hostRepository.get(hostId) ?: throw UnknownThumbHostException(hostId)
        pinnedConnector.clients(host)
    }

    /** Preview info, pages and text of every host (WP-26). */
    val previewSource: PreviewSource by lazy { ClientPreviewSource(hostClients) }

    /** Builds the ExoPlayer of a video preview over the host's pinned OkHttp client. */
    val videoPlayerFactory: VideoPlayerFactory by lazy { PinnedVideoPlayerFactory(hostClients) }

    /**
     * Coil's loader for the whole app (see [GanderApplication]): thumbnails and preview pages over
     * each host's pinned `transfer` client, each with its own disk cache.
     */
    val previewDiskCache: DiskCache by lazy { createPreviewDiskCache(appContext) }

    /**
     * One scheduler per host for preview pages and deep-zoom tiles alike: the host renders one
     * preview at a time per device, so a tile queues behind the page the user waits for (WP-67).
     */
    private val previewSchedulers: PerHostSchedulers by lazy { PerHostSchedulers(PreviewPageLoader.HOST_PREVIEW_SLOTS) }

    /** Thumbnail bytes over the pinned clients; Coil and the Files-app provider (WP-60) share it. */
    val thumbLoader: ThumbLoader by lazy { ThumbLoader(ClientThumbSource(hostClients)) }

    val imageLoader: ImageLoader by lazy {
        createThumbnailImageLoader(
            context = appContext,
            loader = thumbLoader,
            previews = PreviewPageComponents(
                loader = PreviewPageLoader(previewSource, schedulerFor = previewSchedulers),
                diskCache = previewDiskCache,
                tiles = PreviewTileLoader(previewSource, schedulerFor = previewSchedulers),
            ),
        )
    }

    val hostProber: HostProber by lazy { ConnectorHostProber(hostConnector, hostRepository) }

    val privateNetworkMonitor: PrivateNetworkMonitor by lazy { ConnectivityPrivateNetworkMonitor(appContext) }

    val browseRepository: BrowseRepository by lazy {
        DefaultBrowseRepository(
            connector = hostConnector,
            listings = RoomListingSnapshotStore(database.listingSnapshotDao()),
            sharesStore = RoomSharesSnapshotStore(database.shareDao()),
        )
    }

    val browserPrefs: BrowserPrefsRepository by lazy { DataStoreBrowserPrefs(settingsDataStore) }

    /** The stream viewer's remembered touch mode per host (WP-54). */
    val streamPrefs: StreamPrefsRepository by lazy { DataStoreStreamPrefs(settingsDataStore) }

    // File sheet (WP-25): `stat?meta=1`, the remembered action per extension, and the phone-side
    // inputs of the default-action rule table.
    val entryStatSource: EntryStatSource by lazy { ConnectorEntryStatSource(hostConnector) }

    val defaultActionPrefs: DefaultActionPrefsRepository by lazy { DataStoreDefaultActionPrefs(settingsDataStore) }

    val networkStateMonitor: NetworkStateMonitor by lazy { ConnectivityNetworkStateMonitor(appContext) }

    /** Where the preview, fetch and stream screens report a refused action so the file sheet can say why. */
    val actionRefusals: ActionRefusals by lazy { InMemoryActionRefusals() }

    val phoneHandlerLookup: PhoneHandlerLookup by lazy { PackageManagerHandlerLookup(appContext) }

    // Fetch engine (WP-27): Room jobs and cached files, bytes under filesDir/fetched/, the queue
    // running in the process scope with the foreground service as its keep-alive. WP-28 backs
    // [fetchSettings] with DataStore and builds the LRU cache ([cacheEnforcer]) and Storage screen on [offlineFiles].
    val fetchFiles: FetchFiles by lazy { FetchFiles(appContext.filesDir) }

    val fetchStore: FetchStore by lazy { RoomFetchStore(database.fetchDao()) }

    val fetchSettings: FetchSettingsSource by lazy { SettingsFetchSource(settingsRepository) }

    val offlineFiles: OfflineFiles by lazy { OfflineFiles(fetchStore, fetchFiles) }

    /** "Saved copy" for the file sheet: the current version of the file is on the phone. */
    val savedCopies: OfflineCachedFileLookup by lazy { OfflineCachedFileLookup(offlineFiles) }

    val cachedFileLookup: CachedFileLookup get() = savedCopies

    val fetchEngine: FetchEngine by lazy {
        FetchEngine(
            store = fetchStore,
            files = fetchFiles,
            hosts = hostRepository,
            connector = ClientsFetchConnector { host -> pinnedConnector.clients(host) },
            network = networkStateMonitor,
            scope = appScope,
            settings = fetchSettings,
            refusals = actionRefusals,
        ).also { engine ->
            engine.onWorkQueued = { FetchService.start(appContext) }
            // Downloads can finish with no screen open (foreground service): the cap still applies.
            cacheEnforcer.start()
        }
    }

    // Cache, offline, settings, app lock (WP-28).

    /** Thumbnail and preview disk caches, which can always be rebuilt from the computer. */
    val derivedCaches: DerivedCaches by lazy { CoilDerivedCaches({ imageLoader }, { previewDiskCache }) }

    /** Keeps the fetched files under the user's cap, least recently used first, never touching Keep offline. */
    val cacheEnforcer: CacheEnforcer by lazy {
        CacheEnforcer(
            offline = offlineFiles,
            capBytes = settingsRepository.settings.map { it.cacheCapBytes },
            scope = appScope,
        )
    }

    /** Forgetting a computer removes its token, shares, fetched files, thumbnails, previews and prefs. */
    val hostForgetter: HostForgetter by lazy {
        HostForgetter(
            hosts = hostRepository,
            derivedCaches = derivedCaches,
            browserPrefs = browserPrefs,
            purgeFetched = { hostId -> fetchEngine.forgetHost(hostId) },
            onForgotten = { hostId -> pinnedConnector.evict(hostId) },
            streamPrefs = streamPrefs,
        )
    }

    /** `POST /token/rotate` on the host's pinned client, then the new token into the Keystore-backed store. */
    val tokenRotator: TokenRotator by lazy {
        ApiTokenRotator(hostRepository) { host -> GanderApi(pinnedConnector.clients(host)).rotateToken() }
    }

    val appLock: AppLock by lazy { AppLock(SystemClock::elapsedRealtime) }

    /** The redacted diagnostics report (Settings, "Export diagnostics"). */
    val diagnosticsCollector: DiagnosticsCollector by lazy {
        DiagnosticsCollector(
            device = {
                DiagnosticsDevice(
                    appVersion = BuildConfig.VERSION_NAME,
                    androidRelease = Build.VERSION.RELEASE,
                    sdk = Build.VERSION.SDK_INT,
                    manufacturer = Build.MANUFACTURER,
                    model = Build.MODEL,
                )
            },
            settings = { settingsRepository.current() },
            network = { networkStateMonitor.current() },
            // The monitor reports "no VPN" first and "VPN" a moment later if there is one.
            privateNetworkUp = { withTimeoutOrNull(VPN_PROBE_MS) { privateNetworkMonitor.vpnActive.first { it } } ?: false },
            hosts = hostRepository,
            cachedFiles = { offlineFiles.all.first() },
            jobs = { fetchEngine.jobs.first() },
            derivedCacheBytes = { derivedCaches.sizeBytes() },
            freeBytes = { fetchFiles.usableSpace() },
        )
    }

    /** Starts the work that must run whenever the app is up (called from `MainActivity`). */
    fun startBackgroundWork() {
        cacheEnforcer.start()
    }

    private companion object {
        const val VPN_PROBE_MS = 300L
    }
}

/** The process-wide [AppGraph]. Safe to call from any [Context]. */
val Context.appGraph: AppGraph
    get() = (applicationContext as GanderApplication).graph
