package fyi.arlo.gander

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import fyi.arlo.gander.core.data.UiError
import fyi.arlo.gander.core.data.UiErrorKind
import fyi.arlo.gander.core.net.protocol.Permission
import fyi.arlo.gander.testing.SampleData
import fyi.arlo.gander.ui.browser.SharesContent
import fyi.arlo.gander.ui.browser.SharesTags
import fyi.arlo.gander.ui.browser.SharesUiState
import fyi.arlo.gander.ui.theme.GanderTheme
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class SharesScreenTest {
    @get:Rule
    val composeRule = createComposeRule()

    private fun str(id: Int, vararg args: Any) =
        InstrumentationRegistry.getInstrumentation().targetContext.getString(id, *args)

    private val opened = mutableListOf<String>()
    private var retried = 0

    private fun show(state: SharesUiState) {
        composeRule.setContent {
            GanderTheme(dynamicColor = false) {
                SharesContent(state, onBack = {}, onOpenShare = { opened += it }, onRefresh = { retried++ })
            }
        }
    }

    @Test
    fun sharesShowTheirNamePathAndPermissionChips() {
        show(
            SharesUiState(
                hostName = "studio-pc",
                loading = false,
                shares = listOf(
                    SampleData.share(name = "Projects", pathDisplay = "D:\\Projects"),
                    SampleData.share(id = "shr_ro", name = "Photos", permissions = listOf(Permission.LIST, Permission.PREVIEW)),
                ),
            ),
        )
        composeRule.onNodeWithText("studio-pc").assertIsDisplayed()
        composeRule.onNodeWithText("Projects").assertIsDisplayed()
        composeRule.onNodeWithText("D:\\Projects").assertIsDisplayed()
        composeRule.onNodeWithText("Photos").assertIsDisplayed()
        composeRule.onNodeWithTag(SharesTags.card("shr_ro")).performClick()
        assertEquals(listOf("shr_ro"), opened)
    }

    @Test
    fun aShareWithoutFetchPermissionCrossesItsFetchChipOut() {
        show(
            SharesUiState(
                loading = false,
                shares = listOf(SampleData.share(permissions = listOf(Permission.LIST, Permission.PREVIEW))),
            ),
        )
        composeRule.onNodeWithText(str(R.string.permission_preview)).assertIsDisplayed()
        // Not allowed modes stay visible, crossed out, and say so (design document 3.4).
        composeRule.onNodeWithContentDescription(str(R.string.permission_not_allowed, str(R.string.permission_fetch)))
            .assertIsDisplayed()
        composeRule.onNodeWithContentDescription(str(R.string.permission_not_allowed, str(R.string.permission_stream)))
            .assertIsDisplayed()
        composeRule.onNodeWithContentDescription(str(R.string.permission_not_allowed, str(R.string.permission_preview)))
            .assertDoesNotExist()
    }

    @Test
    fun cachedSharesCarryTheCachedChip() {
        show(
            SharesUiState(
                loading = false,
                shares = listOf(SampleData.share()),
                cachedAtMillis = System.currentTimeMillis() - 2 * 3_600_000L,
            ),
        )
        composeRule.onNodeWithText("Cached", substring = true).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.cached_hint)).assertIsDisplayed()
    }

    @Test
    fun offlineWithNothingStoredExplainsAndOffersRetry() {
        show(SharesUiState(loading = false, error = UiError(UiErrorKind.OFFLINE)))
        composeRule.onNodeWithText(str(R.string.error_offline)).assertIsDisplayed()
        composeRule.onNodeWithText(str(R.string.action_retry)).performClick()
        assertEquals(1, retried)
    }

    @Test
    fun noSharesIsExplained() {
        show(SharesUiState(loading = false))
        composeRule.onNodeWithText(str(R.string.shares_empty_title)).assertIsDisplayed()
    }
}
