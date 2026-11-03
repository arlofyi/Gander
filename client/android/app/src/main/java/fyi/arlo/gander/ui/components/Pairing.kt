package fyi.arlo.gander.ui.components

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import fyi.arlo.gander.ui.theme.GanderShape
import fyi.arlo.gander.ui.theme.GanderTheme

/** "482 913" as TalkBack should say it: digit by digit, a pause between the groups ("4 8 2, 9 1 3"). */
fun sasSpokenForm(code: String): String =
    code.trim().split(Regex("\\s+")).filter { it.isNotEmpty() }.joinToString(", ") { group -> group.toList().joinToString(" ") }

/**
 * The pairing code in two boxed groups of three digits (design document section 8): mono 52 sp,
 * or 40 sp when [small] (flow A, where the QR code already proved the computer's identity). It
 * never animates or re-flows once shown. The node's text is the code as given ("482 913"), its
 * spoken form is digit by digit.
 */
@Composable
fun SasCode(code: String, modifier: Modifier = Modifier, small: Boolean = false) {
    val groups = code.trim().split(Regex("\\s+")).filter { it.isNotEmpty() }
    val spoken = sasSpokenForm(code)
    Row(
        modifier = modifier.clearAndSetSemantics {
            text = AnnotatedString(code)
            contentDescription = spoken
        },
        horizontalArrangement = Arrangement.spacedBy(18.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        groups.forEach { group ->
            Surface(
                shape = GanderShape.TileLarge,
                color = GanderTheme.colors.surface,
                contentColor = GanderTheme.colors.text,
                border = BorderStroke(1.dp, GanderTheme.colors.line),
            ) {
                Text(
                    text = group,
                    style = if (small) GanderTheme.type.sasSmall else GanderTheme.type.sas,
                    modifier = Modifier.padding(horizontal = if (small) 10.dp else 12.dp, vertical = if (small) 8.dp else 12.dp),
                    maxLines = 1,
                    softWrap = false,
                )
            }
        }
    }
}

/** One side of the identity pair: a device tile, its name and two lines of detail. */
data class PairingIdentity(val icon: ImageVector, val name: String, val detail: String)

/**
 * Both identities, joined by a dashed link, so the user always sees which computer and which
 * device are being paired next to the code (design document principle 4).
 */
@Composable
fun PairingIdentityPair(computer: PairingIdentity, device: PairingIdentity, modifier: Modifier = Modifier) {
    val line = GanderTheme.colors.outline
    Row(
        modifier = modifier
            .fillMaxWidth()
            .padding(horizontal = 20.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.Top,
    ) {
        IdentityTile(computer, Modifier.width(130.dp))
        Box(
            modifier = Modifier
                .weight(1f)
                .padding(top = 27.dp)
                .height(2.dp),
        ) {
            Canvas(Modifier.fillMaxWidth().height(2.dp)) {
                drawLine(
                    color = line,
                    start = Offset(0f, size.height / 2f),
                    end = Offset(size.width, size.height / 2f),
                    strokeWidth = size.height,
                    pathEffect = PathEffect.dashPathEffect(floatArrayOf(6.dp.toPx(), 4.dp.toPx())),
                )
            }
        }
        IdentityTile(device, Modifier.width(130.dp))
    }
}

@Composable
private fun IdentityTile(identity: PairingIdentity, modifier: Modifier = Modifier) {
    val c = GanderTheme.colors
    Column(modifier = modifier, horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Surface(shape = MaterialTheme.shapes.large, color = c.surface2, contentColor = c.text2) {
            Box(Modifier.size(56.dp), contentAlignment = Alignment.Center) {
                Icon(identity.icon, contentDescription = null, modifier = Modifier.size(32.dp))
            }
        }
        Text(
            identity.name,
            style = MaterialTheme.typography.bodyMedium.copy(fontWeight = FontWeight.SemiBold),
            textAlign = TextAlign.Center,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
        Text(
            identity.detail,
            style = MaterialTheme.typography.bodySmall,
            color = c.text3,
            textAlign = TextAlign.Center,
            maxLines = 3,
        )
    }
}
