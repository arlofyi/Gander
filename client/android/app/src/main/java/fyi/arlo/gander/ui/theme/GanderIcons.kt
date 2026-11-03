package fyi.arlo.gander.ui.theme

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.automirrored.outlined.Backspace
import androidx.compose.material.icons.automirrored.outlined.InsertDriveFile
import androidx.compose.material.icons.automirrored.outlined.KeyboardReturn
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.automirrored.outlined.Sort
import androidx.compose.material.icons.automirrored.outlined.ViewList
import androidx.compose.material.icons.automirrored.outlined.VolumeOff
import androidx.compose.material.icons.automirrored.outlined.VolumeUp
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.AudioFile
import androidx.compose.material.icons.outlined.Check
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Cloud
import androidx.compose.material.icons.outlined.Code
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.DesktopWindows
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.FilterList
import androidx.compose.material.icons.outlined.Fingerprint
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.FolderZip
import androidx.compose.material.icons.outlined.GridView
import androidx.compose.material.icons.outlined.Hub
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.Keyboard
import androidx.compose.material.icons.outlined.KeyboardHide
import androidx.compose.material.icons.outlined.LaptopWindows
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.Mouse
import androidx.compose.material.icons.outlined.Movie
import androidx.compose.material.icons.outlined.OndemandVideo
import androidx.compose.material.icons.outlined.Pause
import androidx.compose.material.icons.outlined.Person
import androidx.compose.material.icons.outlined.PhoneAndroid
import androidx.compose.material.icons.outlined.PhotoCamera
import androidx.compose.material.icons.outlined.PictureAsPdf
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.QrCodeScanner
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Schedule
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.Shield
import androidx.compose.material.icons.outlined.SignalCellularAlt
import androidx.compose.material.icons.outlined.Slideshow
import androidx.compose.material.icons.outlined.Storage
import androidx.compose.material.icons.outlined.TableChart
import androidx.compose.material.icons.outlined.TabletAndroid
import androidx.compose.material.icons.outlined.TouchApp
import androidx.compose.material.icons.outlined.ViewInAr
import androidx.compose.material.icons.outlined.Visibility
import androidx.compose.material.icons.outlined.VisibilityOff
import androidx.compose.material.icons.outlined.WarningAmber
import androidx.compose.material.icons.outlined.WebAsset
import androidx.compose.material.icons.outlined.ZoomOutMap
import androidx.compose.ui.graphics.vector.ImageVector

/**
 * The app's icon set by meaning (design document section 6): Material outlined glyphs, filled only
 * for play. One place to change a glyph so the same meaning always draws the same icon.
 */
object GanderIcons {
    val Back: ImageVector get() = Icons.AutoMirrored.Outlined.ArrowBack
    val Close: ImageVector get() = Icons.Outlined.Close
    val More: ImageVector get() = Icons.Outlined.MoreVert
    val Search: ImageVector get() = Icons.Outlined.Search
    val Sort: ImageVector get() = Icons.AutoMirrored.Outlined.Sort
    val Filter: ImageVector get() = Icons.Outlined.FilterList
    val Grid: ImageVector get() = Icons.Outlined.GridView
    val List: ImageVector get() = Icons.AutoMirrored.Outlined.ViewList
    val Check: ImageVector get() = Icons.Outlined.Check
    val Chevron: ImageVector get() = Icons.Outlined.ChevronRight
    val Add: ImageVector get() = Icons.Outlined.Add

    // Files
    val Folder: ImageVector get() = Icons.Outlined.Folder
    val File: ImageVector get() = Icons.AutoMirrored.Outlined.InsertDriveFile
    val Doc: ImageVector get() = Icons.Outlined.Description
    val Pdf: ImageVector get() = Icons.Outlined.PictureAsPdf
    val Image: ImageVector get() = Icons.Outlined.Image
    val Video: ImageVector get() = Icons.Outlined.Movie
    val Audio: ImageVector get() = Icons.Outlined.AudioFile
    val Slides: ImageVector get() = Icons.Outlined.Slideshow
    val Sheet: ImageVector get() = Icons.Outlined.TableChart
    val Cube: ImageVector get() = Icons.Outlined.ViewInAr
    val Code: ImageVector get() = Icons.Outlined.Code
    val Archive: ImageVector get() = Icons.Outlined.FolderZip
    val Cloud: ImageVector get() = Icons.Outlined.Cloud

    // Devices
    val Desktop: ImageVector get() = Icons.Outlined.DesktopWindows
    val Laptop: ImageVector get() = Icons.Outlined.LaptopWindows
    val Phone: ImageVector get() = Icons.Outlined.PhoneAndroid
    val Tablet: ImageVector get() = Icons.Outlined.TabletAndroid

    // The three modes (used identically everywhere)
    val Preview: ImageVector get() = Icons.Outlined.Visibility
    val Fetch: ImageVector get() = Icons.Outlined.Download
    val Stream: ImageVector get() = Icons.Outlined.OndemandVideo

    // Status and trust
    val Lock: ImageVector get() = Icons.Outlined.Lock
    val Shield: ImageVector get() = Icons.Outlined.Shield
    val Warn: ImageVector get() = Icons.Outlined.WarningAmber
    val Info: ImageVector get() = Icons.Outlined.Info
    val Error: ImageVector get() = Icons.Outlined.ErrorOutline
    val Ok: ImageVector get() = Icons.Outlined.CheckCircle
    val Relay: ImageVector get() = Icons.Outlined.Hub
    val Clock: ImageVector get() = Icons.Outlined.Schedule
    val Cellular: ImageVector get() = Icons.Outlined.SignalCellularAlt
    val EyeOff: ImageVector get() = Icons.Outlined.VisibilityOff
    val Fingerprint: ImageVector get() = Icons.Outlined.Fingerprint
    val User: ImageVector get() = Icons.Outlined.Person
    val Link: ImageVector get() = Icons.Outlined.Link

    // Commands
    val Qr: ImageVector get() = Icons.Outlined.QrCodeScanner
    val Camera: ImageVector get() = Icons.Outlined.PhotoCamera
    val Keyboard: ImageVector get() = Icons.Outlined.Keyboard
    val KeyboardHide: ImageVector get() = Icons.Outlined.KeyboardHide
    val Pointer: ImageVector get() = Icons.Outlined.Mouse
    val Touch: ImageVector get() = Icons.Outlined.TouchApp
    val Settings: ImageVector get() = Icons.Outlined.Settings
    val Storage: ImageVector get() = Icons.Outlined.Storage
    val Refresh: ImageVector get() = Icons.Outlined.Refresh
    val Share: ImageVector get() = Icons.Outlined.Share
    val Open: ImageVector get() = Icons.AutoMirrored.Outlined.OpenInNew
    val Pin: ImageVector get() = Icons.Outlined.PushPin
    val Play: ImageVector get() = Icons.Filled.PlayArrow
    val Pause: ImageVector get() = Icons.Outlined.Pause
    val Trash: ImageVector get() = Icons.Outlined.Delete
    val Window: ImageVector get() = Icons.Outlined.WebAsset
    val Zoom: ImageVector get() = Icons.Outlined.ZoomOutMap
    val Sound: ImageVector get() = Icons.AutoMirrored.Outlined.VolumeUp
    val SoundOff: ImageVector get() = Icons.AutoMirrored.Outlined.VolumeOff
    val Backspace: ImageVector get() = Icons.AutoMirrored.Outlined.Backspace
    val Enter: ImageVector get() = Icons.AutoMirrored.Outlined.KeyboardReturn
}
