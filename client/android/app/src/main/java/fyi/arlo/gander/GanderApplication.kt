package fyi.arlo.gander

import android.app.Application
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader

/**
 * Process entry point. Owns the single [AppGraph] (manual DI, no framework).
 *
 * Package layout (android-client-v1 spec); later work packages slot in without moving files:
 *
 * ```
 * fyi.arlo.gander
 *   core.net          pinned OkHttp client, trust manager, HostEndpoint, /events socket (WP-21)
 *   core.data         Room tables, token store, DataStore settings, file cache (WP-22, WP-23)
 *   ui.theme          Material 3 theme
 *   ui.navigation     NavHost and route names
 *   ui.hosts          Hosts home screen (WP-23)
 *   ui.pairing        Add host: QR scan, manual entry, SAS (WP-22)
 *   ui.browser        Shares and browser (WP-23, WP-24)
 *   ui.preview        Page, image, text and video previews
 *   ui.fetch          Fetch engine UI and notifications
 *   ui.stream         Stream viewer (Phase 2)
 *   ui.settings       Settings and storage screens
 * ```
 */
class GanderApplication : Application(), SingletonImageLoader.Factory {

    /** Created on first use so tests can construct the application without side effects. */
    val graph: AppGraph by lazy(LazyThreadSafetyMode.SYNCHRONIZED) { AppGraph(this) }

    /** Coil asks here for the app-wide loader (thumbnails over the pinned host clients, WP-24). */
    override fun newImageLoader(context: PlatformContext): ImageLoader = graph.imageLoader
}
