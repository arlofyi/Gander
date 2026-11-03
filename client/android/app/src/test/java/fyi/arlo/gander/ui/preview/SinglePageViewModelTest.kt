package fyi.arlo.gander.ui.preview

import androidx.lifecycle.SavedStateHandle
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.data.actions.ActionReason
import fyi.arlo.gander.core.data.actions.FileAction
import fyi.arlo.gander.core.data.actions.InMemoryActionRefusals
import fyi.arlo.gander.core.data.browse.EntryTarget
import fyi.arlo.gander.core.data.hosts.InMemoryHostRepository
import fyi.arlo.gander.core.data.preview.PlaybackFailure
import fyi.arlo.gander.core.data.preview.PreviewFailure
import fyi.arlo.gander.core.net.GanderError
import fyi.arlo.gander.core.net.protocol.TilePage
import fyi.arlo.gander.testing.FakePreviewSource
import fyi.arlo.gander.core.net.protocol.PreviewKind
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class SinglePageViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    private val host = SampleData.host()
    private val refusals = InMemoryActionRefusals()
    private val photo = SampleData.file("Holiday.jpg", size = 3_500_000, preview = PreviewKind.IMAGE)
    private val video = SampleData.file("Clip.mp4", size = 90_000_000, preview = PreviewKind.VIDEO_POSTER)

    private fun target(entry: fyi.arlo.gander.core.net.protocol.Entry = photo) =
        EntryTarget(host.id, "shr_projects", "Media/${entry.name}", entry)

    private fun imageVm(paired: fyi.arlo.gander.core.data.hosts.PairedHost? = host) =
        SinglePageViewModel(target(), InMemoryHostRepository(listOfNotNull(paired)), refusals)

    private fun videoVm(saved: SavedStateHandle = SavedStateHandle()) =
        VideoViewModel(target(video), InMemoryHostRepository(listOf(host)), refusals, saved)

    private suspend fun refusalsOf(entry: fyi.arlo.gander.core.net.protocol.Entry = photo) =
        refusals.observe(target(entry)).first()

    // Image ---------------------------------------------------------------------------------------

    @Test
    fun thePictureIsRequestedAtThePhonesWidthClampedToTheHost() {
        val vm = imageVm()
        assertNull("not measured yet", vm.pageRef())
        vm.onViewport(1080)
        val ref = vm.pageRef()!!
        assertEquals(1, ref.page)
        assertEquals(1080, ref.widthPx)
        assertEquals("Media/Holiday.jpg", ref.path)
        assertEquals("2026-10-30T09:12:44Z#3500000", ref.version)
        assertTrue("images are cached on disk by version", ref.persistent)

        vm.onViewport(2800)
        assertEquals(2048, vm.pageRef()!!.widthPx)
    }

    @Test
    fun aLowerHostLimitWins() {
        val capped = host.copy(capabilities = SampleData.me().capabilities.copy(previewMaxWidth = 1024))
        val vm = SinglePageViewModel(target(), InMemoryHostRepository(listOf(capped)), refusals)
        vm.onViewport(1440)
        assertEquals(1024, vm.uiState.value.widthPx)
    }

    @Test
    fun aForgottenHostIsExplainedNotCrashed() {
        val failure = imageVm(paired = null).uiState.value.failure as PreviewFailure.Host
        assertEquals(UiErrorKind.NOT_PAIRED, failure.error.kind)
    }

    @Test
    fun anUnsupportedImageIsExplainedAndTheSheetLearnsWhy() = runTest {
        val vm = imageVm()
        vm.onViewport(1080)
        vm.onFailed(GanderError.Host.UnsupportedType(415, "HEIC codec missing"))
        assertEquals(PreviewFailure.Unsupported, vm.uiState.value.failure)
        assertFalse(vm.uiState.value.loaded)
        assertEquals(ActionReason.NoRenderer("jpg"), refusalsOf()[FileAction.PREVIEW])
    }

    @Test
    fun aBusyRendererIsNotLeftOnTheSheet() = runTest {
        val vm = imageVm()
        vm.onFailed(GanderError.Host.Generating(503, "queued", 100))
        assertEquals(PreviewFailure.StillGenerating, vm.uiState.value.failure)
        assertTrue(refusalsOf().isEmpty())
    }

    @Test
    fun retryClearsTheFailureAndBuildsTheRequestAgain() {
        val vm = imageVm()
        vm.onFailed(GanderError.Host.Generating(503, "queued", 100))
        vm.retry()
        assertNull(vm.uiState.value.failure)
        assertEquals(1, vm.uiState.value.attempt)
        vm.onLoaded()
        assertTrue(vm.uiState.value.loaded)
    }

    @Test
    fun aPictureThatLoadsClearsAnEarlierRefusal() = runTest {
        refusals.report(target(), FileAction.PREVIEW, GanderError.Host.UnsupportedType(415, ""))
        val vm = imageVm()
        vm.onLoaded()
        assertTrue(refusalsOf().isEmpty())
    }

    @Test
    fun aNewWidthMeansAnotherPictureThatIsNotLoadedYet() {
        val vm = imageVm()
        vm.onViewport(1080)
        vm.onLoaded()
        vm.onViewport(720)
        assertFalse(vm.uiState.value.loaded)
        assertEquals(720, vm.pageRef()!!.widthPx)
    }

    // Deep zoom (WP-67) ---------------------------------------------------------------------------

    private val source = FakePreviewSource()
    private val photoTiles = TilePage(6000, 4500, 5)

    private fun tileVm() = SinglePageViewModel(
        target(),
        InMemoryHostRepository(listOf(host)),
        refusals,
        tileInfo = source,
        sleep = {},
        now = { 0L },
    )

    private fun imageInfo(tilePages: List<TilePage>? = listOf(photoTiles), kind: PreviewKind = PreviewKind.IMAGE) =
        FakePreviewSource.pdfInfo(pages = null, kind = kind, tilePages = tilePages)

    @Test
    fun theHostIsAskedForTheTilesOnceAfterThePictureHasLoaded() {
        source.infoHandler = { imageInfo() }
        val vm = tileVm()
        vm.onViewport(1080)
        assertTrue("nothing is asked before the picture is there", source.infoCalls.isEmpty())
        assertNull(vm.uiState.value.tilePage)

        vm.onLoaded()
        assertEquals(photoTiles, vm.uiState.value.tilePage)
        assertEquals(listOf("Media/Holiday.jpg"), source.infoCalls)

        vm.onViewport(720)
        vm.onLoaded()
        assertEquals("asked once, not for every width", 1, source.infoCalls.size)
        assertEquals("and the answer is kept", photoTiles, vm.uiState.value.tilePage)
    }

    @Test
    fun aPictureTheHostCannotTileHasNoTilePage() {
        source.infoHandler = { imageInfo(tilePages = null) }
        val vm = tileVm()
        vm.onLoaded()
        assertNull(vm.uiState.value.tilePage)
        assertTrue(vm.uiState.value.loaded)
    }

    @Test
    fun aFailingInfoNeverGetsInTheWayOfThePicture() {
        source.infoHandler = { throw GanderError.Host.Internal(500, "boom") }
        val vm = tileVm()
        vm.onLoaded()
        assertNull(vm.uiState.value.tilePage)
        assertNull("no error panel for a missing deep zoom", vm.uiState.value.failure)
        assertTrue(vm.uiState.value.loaded)
        assertEquals(1, source.infoCalls.size)
    }

    @Test
    fun aBusyHostIsWaitedForBriefly() {
        var calls = 0
        source.infoHandler = {
            if (++calls < 3) throw GanderError.Host.Generating(503, "queued", 400)
            imageInfo()
        }
        val vm = tileVm()
        vm.onLoaded()
        assertEquals(photoTiles, vm.uiState.value.tilePage)
        assertEquals(3, source.infoCalls.size)
    }

    @Test
    fun onlyAnImageKindGetsTiles() {
        source.infoHandler = { imageInfo(kind = PreviewKind.PDF) }
        val vm = tileVm()
        vm.onLoaded()
        assertNull(vm.uiState.value.tilePage)
    }

    @Test
    fun aViewerWithoutAnInfoSourceNeverAsks() {
        val vm = imageVm()
        vm.onLoaded()
        assertNull(vm.uiState.value.tilePage)
        assertTrue(source.infoCalls.isEmpty())
    }

    @Test
    fun aVideoPosterNeverAsksForTilesAndNeverHasThem() {
        // the video viewer is built without an info source: the poster is a still of a video, not a page to zoom into
        val vm = videoVm()
        vm.onViewport(1080)
        vm.onLoaded()
        assertTrue(vm.uiState.value.loaded)
        assertNull(vm.uiState.value.tilePage)
        assertTrue(source.infoCalls.isEmpty())
    }

    @Test
    fun aTileRequestNamesTheFileVersionAndPageOne() {
        val vm = tileVm()
        val ref = vm.tileRef(TileKey(level = 4, x = 11, y = 8))
        assertEquals(1, ref.page)
        assertEquals(4, ref.level)
        assertEquals(11, ref.x)
        assertEquals(8, ref.y)
        assertEquals("Media/Holiday.jpg", ref.path)
        assertEquals("2026-10-30T09:12:44Z#3500000", ref.version)
    }

    // Video ---------------------------------------------------------------------------------------

    @Test
    fun theVideoStartsOnlyWhenTheUserTapsPlay() {
        val vm = videoVm()
        assertFalse("opening a video costs one poster frame, not the first megabytes", vm.video.value.started)
        vm.start()
        assertTrue(vm.video.value.started)
    }

    @Test
    fun aMissingPosterIsNotAnError() = runTest {
        val vm = videoVm()
        vm.onViewport(1080)
        vm.onFailed(GanderError.Host.UnsupportedType(415, "no poster"))
        assertNull(vm.uiState.value.failure)
        assertTrue(refusalsOf(video).isEmpty())
    }

    @Test
    fun aSharesThatPreviewsButCannotFetchExplainsWhyTheVideoDoesNotPlay() = runTest {
        // the video's bytes come from /file, which the host answers with permission_denied when the share lacks fetch
        val vm = videoVm()
        vm.start()
        vm.onPlaybackFailure(PlaybackFailure.Host(GanderError.Host.PermissionDenied(403, "needs fetch permission")))
        val failure = vm.video.value.failure as PreviewFailure.Host
        assertEquals(UiErrorKind.NO_PERMISSION, failure.error.kind)
        assertEquals("needs fetch permission", failure.error.detail)
        assertEquals(
            ActionReason.NoPermission(FileAction.PREVIEW.permission, "needs fetch permission"),
            refusalsOf(video)[FileAction.PREVIEW],
        )
    }

    @Test
    fun aPhoneThatCannotDecodeTheVideoSaysSoAndOffersFetch() = runTest {
        val vm = videoVm()
        vm.start()
        vm.onPlaybackFailure(PlaybackFailure.CodecUnsupported)
        assertEquals(PreviewFailure.CodecUnsupported, vm.video.value.failure)
        assertTrue("the host did nothing wrong: the sheet keeps Preview on", refusalsOf(video).isEmpty())
    }

    @Test
    fun aDroppedConnectionIsRetryableAndNotLeftOnTheSheet() = runTest {
        val vm = videoVm()
        vm.start()
        vm.onPlaybackFailure(PlaybackFailure.Host(GanderError.Unreachable(listOf("100.64.0.1"), null)))
        assertEquals(UiErrorKind.OFFLINE, ((vm.video.value.failure) as PreviewFailure.Host).error.kind)
        assertTrue(refusalsOf(video).isEmpty())

        vm.retryPlayback()
        assertNull(vm.video.value.failure)
        assertEquals(1, vm.video.value.attempt)
        assertTrue(vm.video.value.started)
    }

    @Test
    fun anUnknownPlayerErrorOffersRetry() {
        val vm = videoVm()
        vm.start()
        vm.onPlaybackFailure(PlaybackFailure.Other(1000))
        val failure = vm.video.value.failure as PreviewFailure.Host
        assertEquals(UiErrorKind.OTHER, failure.error.kind)
        assertTrue(failure.canRetry)
    }

    @Test
    fun startedAndPositionSurviveARotationAndProcessDeath() {
        val saved = SavedStateHandle()
        val vm = videoVm(saved)
        vm.start()
        vm.onPosition(61_500)

        val restored = videoVm(saved)
        assertTrue(restored.video.value.started)
        assertEquals(61_500, restored.video.value.positionMs)
    }

    @Test
    fun playbackThatBeginsClearsAnEarlierRefusal() = runTest {
        refusals.report(target(video), FileAction.PREVIEW, GanderError.Host.PermissionDenied(403, "needs fetch"))
        assertNotNull(refusalsOf(video)[FileAction.PREVIEW])
        val vm = videoVm()
        vm.start()
        vm.onPlaybackStarted()
        assertTrue(refusalsOf(video).isEmpty())
    }
}
