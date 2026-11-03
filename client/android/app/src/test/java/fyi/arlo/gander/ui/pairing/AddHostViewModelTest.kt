package fyi.arlo.gander.ui.pairing

import fyi.arlo.gander.core.data.hosts.PairedHost
import fyi.arlo.gander.core.data.pairing.PairingFailure
import fyi.arlo.gander.core.data.pairing.PairingFlow
import fyi.arlo.gander.core.data.pairing.PairingMethod
import fyi.arlo.gander.core.data.pairing.PairingTarget
import fyi.arlo.gander.core.data.pairing.QrUris
import fyi.arlo.gander.core.data.pairing.SasCode
import fyi.arlo.gander.core.data.pairing.WaitingForApproval
import fyi.arlo.gander.testing.MainDispatcherRule
import fyi.arlo.gander.testing.SampleData
import kotlinx.coroutines.CompletableDeferred
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class AddHostViewModelTest {
    @get:Rule
    val main = MainDispatcherRule()

    /** A scripted [PairingFlow]: records targets, lets each test decide how an attempt ends. */
    private class ScriptedPairing : PairingFlow {
        val targets = mutableListOf<PairingTarget>()
        var script: suspend (PairingTarget, (WaitingForApproval) -> Unit) -> PairedHost = { _, _ -> throw IllegalStateException("no script") }

        override suspend fun pair(target: PairingTarget, onWaiting: (WaitingForApproval) -> Unit): PairedHost {
            targets += target
            return script(target, onWaiting)
        }
    }

    private val pairing = ScriptedPairing()
    private val viewModel = AddHostViewModel(pairing)
    private val state get() = viewModel.state.value
    private val paired = SampleData.host()
    private val secret = "A".repeat(43) // placeholder, not a window secret

    private val validCode = QrUris.build(SampleData.HOST_ID, listOf("100.101.102.103"), 42637, "Studio PC", secret, null)

    @Test
    fun startsOnTheChooser() {
        assertEquals(AddHostUiState.Choose, state)
    }

    @Test
    fun backStepsThroughTheFlowAndOnlyLeavesFromTheStart() {
        assertFalse("nothing to step back to", viewModel.onBack())
        viewModel.onChooseScan()
        assertTrue(viewModel.onBack())
        assertEquals(AddHostUiState.Choose, state)
        viewModel.onChooseManual()
        assertTrue(viewModel.onBack())
        assertEquals(AddHostUiState.Choose, state)
    }

    // QR ----------------------------------------------------------------------------------------

    @Test
    fun aValidCodeStartsPairingAndTheWholeFlowEndsDone() {
        val gate = CompletableDeferred<PairedHost>()
        pairing.script = { target, onWaiting ->
            onWaiting(WaitingForApproval(target.label, SasCode(482_913), target.method))
            gate.await()
        }
        viewModel.onChooseScan()
        viewModel.onQrScanned(validCode)

        assertEquals(AddHostUiState.Waiting("Studio PC", "482 913", PairingMethod.QR), state)
        val target = pairing.targets.single() as PairingTarget.Qr
        assertEquals(SampleData.HOST_ID, target.invite.hostId)
        assertEquals(secret, target.invite.secret.value)

        gate.complete(paired)
        assertEquals(AddHostUiState.Done(paired), state)
    }

    @Test
    fun connectingShowsTheHostNameFromTheCodeUntilTheCodeIsReady() {
        pairing.script = { _, _ -> CompletableDeferred<PairedHost>().await() }
        viewModel.onChooseScan()
        viewModel.onQrScanned(validCode)
        assertEquals(AddHostUiState.Connecting("Studio PC"), state)
    }

    @Test
    fun badCodesKeepTheCameraOnAndSayWhy() {
        viewModel.onChooseScan()
        viewModel.onQrScanned("https://example.com")
        assertEquals(AddHostUiState.Scan(ScanNotice.NOT_GANDER), state)
        val same = state
        viewModel.onQrScanned("https://example.com") // the next frame shows the same code
        assertSame("a repeated frame is not news", same, state)
        viewModel.onQrScanned(validCode.replace("v=1", "v=9"))
        assertEquals(AddHostUiState.Scan(ScanNotice.VERSION), state)
        viewModel.onQrScanned(validCode.replace("p=42637", "p=0"))
        assertEquals(AddHostUiState.Scan(ScanNotice.MALFORMED), state)
        assertTrue(pairing.targets.isEmpty())
    }

    @Test
    fun framesThatArriveAfterLeavingTheScannerAreIgnored() {
        pairing.script = { _, _ -> CompletableDeferred<PairedHost>().await() }
        viewModel.onChooseScan()
        viewModel.onQrScanned(validCode)
        viewModel.onQrScanned(validCode) // a late frame from the analyzer thread
        assertEquals(1, pairing.targets.size)
        viewModel.onCancel()
        viewModel.onQrScanned(validCode) // not on the scan screen any more
        assertEquals(1, pairing.targets.size)
        assertEquals(AddHostUiState.Choose, state)
    }

    // Manual ------------------------------------------------------------------------------------

    @Test
    fun manualEntryValidatesThenPairs() {
        pairing.script = { target, onWaiting ->
            onWaiting(WaitingForApproval(target.label, SasCode(7), target.method))
            CompletableDeferred<PairedHost>().await()
        }
        viewModel.onChooseManual()
        viewModel.onManualAddressChanged("not an address")
        viewModel.onManualSubmit()
        assertEquals(AddHostUiState.Manual("not an address", invalid = true), state)
        assertTrue(pairing.targets.isEmpty())

        viewModel.onManualAddressChanged("10.0.2.2:5000")
        assertEquals(AddHostUiState.Manual("10.0.2.2:5000", invalid = false), state)
        viewModel.onManualSubmit()
        val target = pairing.targets.single() as PairingTarget.Manual
        assertEquals("10.0.2.2", target.address.host)
        assertEquals(5000, target.address.port)
        assertEquals(AddHostUiState.Waiting("10.0.2.2", "000 007", PairingMethod.MANUAL), state)
    }

    // Failures and retry ------------------------------------------------------------------------

    @Test
    fun aFailureShowsItsKindAndQrRetryGoesBackToScanning() {
        pairing.script = { _, _ -> throw PairingFailure(PairingFailure.Kind.DENIED) }
        viewModel.onChooseScan()
        viewModel.onQrScanned(validCode)
        assertEquals(AddHostUiState.Failed(PairingFailure.Kind.DENIED, null, PairingMethod.QR, retrySame = false), state)
        viewModel.onRetry()
        assertEquals(AddHostUiState.Scan(), state)
    }

    @Test
    fun anUnreachableComputerCanBeRetriedWithTheSameCode() {
        var attempts = 0
        pairing.script = { _, _ ->
            attempts++
            if (attempts == 1) throw PairingFailure(PairingFailure.Kind.UNREACHABLE)
            paired
        }
        viewModel.onChooseScan()
        viewModel.onQrScanned(validCode)
        assertEquals(PairingFailure.Kind.UNREACHABLE, (state as AddHostUiState.Failed).kind)
        assertTrue((state as AddHostUiState.Failed).retrySame)
        viewModel.onRetry()
        assertEquals(AddHostUiState.Done(paired), state)
        assertEquals(2, pairing.targets.size)
        assertSame("the same code is reused", pairing.targets[0], pairing.targets[1])
    }

    @Test
    fun manualRetryComesBackToTheFormWithTheAddressFilledIn() {
        pairing.script = { _, _ -> throw PairingFailure(PairingFailure.Kind.CLOSED) }
        viewModel.onChooseManual()
        viewModel.onManualAddressChanged("studio-pc.tail1.ts.net:4000")
        viewModel.onManualSubmit()
        assertEquals(PairingFailure.Kind.CLOSED, (state as AddHostUiState.Failed).kind)
        viewModel.onRetry()
        assertEquals(AddHostUiState.Manual("studio-pc.tail1.ts.net:4000"), state)
    }

    @Test
    fun theFailureCarriesTheComputersOwnWordsButNeverAnExceptionText() {
        pairing.script = { _, _ -> throw PairingFailure(PairingFailure.Kind.CODE_REJECTED, detail = "This pairing code is not valid.") }
        viewModel.onChooseScan()
        viewModel.onQrScanned(validCode)
        assertEquals("This pairing code is not valid.", (state as AddHostUiState.Failed).detail)

        pairing.script = { _, _ -> throw IllegalStateException("https://100.101.102.103:42637/v1/pair?s=$secret") }
        viewModel.onCancel()
        viewModel.onChooseScan()
        viewModel.onQrScanned(validCode)
        val failed = state as AddHostUiState.Failed
        // An unexpected exception is a bug in the app, said as such, with the class name only.
        assertEquals(PairingFailure.Kind.APP_ERROR, failed.kind)
        assertEquals("IllegalStateException", failed.detail)
        assertFalse(failed.toString().contains(secret))
    }

    @Test
    fun unexpectedExceptionsAreMappedToAMeaningfulKind() {
        val cases = listOf(
            java.io.IOException("reset") to PairingFailure.Kind.UNREACHABLE,
            kotlinx.serialization.SerializationException("bad json") to PairingFailure.Kind.INCOMPATIBLE,
            java.security.KeyStoreException("keystore") to PairingFailure.Kind.STORAGE,
            RuntimeException("boom") to PairingFailure.Kind.APP_ERROR,
        )
        for ((thrown, kind) in cases) {
            pairing.script = { _, _ -> throw thrown }
            viewModel.onCancel()
            viewModel.onChooseScan()
            viewModel.onQrScanned(validCode)
            assertEquals(thrown.javaClass.simpleName, kind, (state as AddHostUiState.Failed).kind)
        }
    }

    // Cancel ------------------------------------------------------------------------------------

    @Test
    fun cancelAndBackAbandonTheAttempt() {
        var cancelled = 0
        pairing.script = { target, onWaiting ->
            onWaiting(WaitingForApproval(target.label, SasCode(1), target.method))
            try {
                CompletableDeferred<PairedHost>().await()
            } finally {
                cancelled++
            }
        }
        viewModel.onChooseScan()
        viewModel.onQrScanned(validCode)
        assertTrue(state is AddHostUiState.Waiting)
        viewModel.onCancel()
        assertEquals(1, cancelled)
        assertEquals(AddHostUiState.Choose, state)

        viewModel.onChooseScan()
        viewModel.onQrScanned(validCode)
        assertTrue(viewModel.onBack())
        assertEquals(2, cancelled)
        assertEquals(AddHostUiState.Choose, state)
    }

    // FLAG_SECURE -------------------------------------------------------------------------------

    @Test
    fun theWindowIsSecureWhileTheQrCodeOrTheSasIsVisible() {
        assertFalse(AddHostUiState.Choose.showsSecrets)
        assertTrue(AddHostUiState.Scan().showsSecrets)
        assertFalse(AddHostUiState.Manual().showsSecrets)
        assertTrue(AddHostUiState.Connecting("pc").showsSecrets)
        assertTrue(AddHostUiState.Waiting("pc", "123 456", PairingMethod.QR).showsSecrets)
        assertFalse(AddHostUiState.Failed(PairingFailure.Kind.DENIED, null, PairingMethod.QR, false).showsSecrets)
        assertFalse(AddHostUiState.Done(paired).showsSecrets)
    }
}
