package fyi.arlo.gander.ui.lock

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import fyi.arlo.gander.R
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import fyi.arlo.gander.ui.components.ButtonKind
import fyi.arlo.gander.ui.components.GanderButton
import fyi.arlo.gander.ui.theme.GanderIcons
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme
import fyi.arlo.gander.ui.theme.LensMark
import fyi.arlo.gander.core.data.security.AppLock
import fyi.arlo.gander.core.data.security.LockState
import fyi.arlo.gander.ui.pairing.SecureWindowEffect
import fyi.arlo.gander.ui.pairing.findActivity

object LockTags {
    const val SCREEN = "app-lock-screen"
    const val UNLOCK = "app-lock-unlock"
    const val TURN_OFF = "app-lock-turn-off"
    const val CONTENT = "app-lock-content"
}

/**
 * Wraps the app's UI with the app lock. The content is not composed at all until the first unlock
 * of this activity (a cold start does not browse, ping or load anything behind the lock). After
 * that it stays composed so the user returns to the same screen, and while locked it is covered by
 * an opaque screen that takes every touch, hidden from accessibility services, and the window is
 * `FLAG_SECURE` (no screenshots, blank recents thumbnail).
 *
 * [onTurnOffLock] is the way out when the phone no longer has a screen lock to check against.
 */
@Composable
fun AppLockGate(
    lock: AppLock,
    authenticator: DeviceAuthenticator,
    onTurnOffLock: () -> Unit,
    content: @Composable () -> Unit,
) {
    val state by lock.state.collectAsStateWithLifecycle()
    var everUnlocked by remember { mutableStateOf(false) }
    LaunchedEffect(state) { if (state == LockState.UNLOCKED) everUnlocked = true }
    SecureWindowEffect(enabled = state == LockState.LOCKED)

    Box(Modifier.fillMaxSize()) {
        if (everUnlocked || state == LockState.UNLOCKED) {
            val covered = state != LockState.UNLOCKED
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .testTag(LockTags.CONTENT)
                    .then(if (covered) Modifier.clearAndSetSemantics { } else Modifier),
            ) { content() }
        }
        when (state) {
            LockState.UNKNOWN -> Surface(Modifier.fillMaxSize(), color = GanderTheme.colors.bg) {}
            LockState.LOCKED -> LockScreen(authenticator, onUnlocked = lock::unlock, onTurnOffLock = onTurnOffLock)
            LockState.UNLOCKED -> Unit
        }
    }
}

/** The lock screen. Asks for the fingerprint, face or screen lock by itself when it appears; the button asks again. */
@Composable
fun LockScreen(
    authenticator: DeviceAuthenticator,
    onUnlocked: () -> Unit,
    onTurnOffLock: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val title = stringResource(R.string.lock_prompt_title)
    val subtitle = stringResource(R.string.lock_prompt_subtitle)
    var asking by remember { mutableStateOf(false) }
    var availability by remember { mutableStateOf(authenticator.availability()) }
    var declined by remember { mutableStateOf(false) }

    fun ask() {
        availability = authenticator.availability()
        if (availability != AuthAvailability.AVAILABLE || asking) return
        asking = true
        declined = false
        authenticator.authenticate(title, subtitle) { result ->
            asking = false
            if (result == AuthResult.Success) onUnlocked() else declined = true
        }
    }

    // The prompt opens by itself once each time the lock screen appears (cold start, or coming
    // back after the timeout). Cancelling leaves it to the Unlock button: asking again on every
    // resume would loop, because on API 26-29 the screen-lock fallback is its own activity and
    // cancelling it resumes this one.
    var prompted by remember { mutableStateOf(false) }
    LifecycleResumeEffect(Unit) {
        if (!prompted) {
            prompted = true
            ask()
        }
        onPauseOrDispose { }
    }
    BackHandler { context.findActivity()?.moveTaskToBack(true) }

    val c = GanderTheme.colors
    Surface(
        modifier = modifier
            .fillMaxSize()
            .testTag(LockTags.SCREEN)
            .pointerInput(Unit) { },
        color = c.bg,
        contentColor = c.text,
    ) {
        Column(Modifier.fillMaxSize().systemBarsPadding()) {
            Column(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
                    .padding(32.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                LensMark(size = 104.dp)
                Text(
                    stringResource(R.string.lock_title),
                    style = MaterialTheme.typography.headlineMedium.copy(fontSize = 24.sp),
                    textAlign = TextAlign.Center,
                    modifier = Modifier.padding(top = 12.dp),
                )
                when (availability) {
                    AuthAvailability.NOT_SET_UP -> Unit
                    AuthAvailability.UNAVAILABLE -> {
                        Text(
                            stringResource(R.string.lock_unavailable),
                            style = MaterialTheme.typography.bodyMedium,
                            color = c.text2,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.widthIn(max = 300.dp),
                        )
                        GanderButton(
                            text = stringResource(R.string.lock_try_again),
                            onClick = ::ask,
                            kind = ButtonKind.TONAL,
                            modifier = Modifier.padding(top = 24.dp).testTag(LockTags.UNLOCK),
                        )
                    }
                    AuthAvailability.AVAILABLE -> {
                        Text(
                            stringResource(if (declined) R.string.lock_declined else R.string.lock_body),
                            style = MaterialTheme.typography.bodyMedium,
                            color = c.text2,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.widthIn(max = 300.dp),
                        )
                        GanderButton(
                            text = stringResource(R.string.lock_unlock),
                            onClick = ::ask,
                            icon = GanderIcons.Fingerprint,
                            modifier = Modifier.padding(top = 24.dp).testTag(LockTags.UNLOCK),
                        )
                        Text(
                            stringResource(R.string.lock_downloads_note),
                            style = MaterialTheme.typography.bodySmall.copy(fontSize = 12.5.sp),
                            color = c.text3,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.padding(top = 28.dp).widthIn(max = 300.dp),
                        )
                    }
                }
            }
            if (availability == AuthAvailability.NOT_SET_UP) {
                // "If the phone lock was removed" (mockup 27): the way out, in a panel at the bottom.
                Surface(
                    shape = GanderShape.Panel,
                    color = c.surface2,
                    modifier = Modifier.padding(16.dp).fillMaxWidth(),
                ) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(
                            stringResource(R.string.lock_removed_title),
                            style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.SemiBold),
                        )
                        Text(stringResource(R.string.lock_not_set_up), style = MaterialTheme.typography.bodyMedium, color = c.text2)
                        GanderButton(
                            text = stringResource(R.string.lock_turn_off),
                            onClick = onTurnOffLock,
                            kind = ButtonKind.TONAL,
                            modifier = Modifier.testTag(LockTags.TURN_OFF),
                        )
                    }
                }
            }
        }
    }
}
