package fyi.arlo.gander.spike.files

import android.app.Activity
import android.media.MediaPlayer
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.VideoView

/**
 * Fallback video client: platform VideoView (MediaPlayer, stagefright MediaExtractor). Plays a
 * scripted sequence so the read pattern includes start, steady playback and seeks:
 * play 5 s, seek to 50 %, play 4 s, seek to 90 %, play 3 s, seek to 10 %, play 4 s, finish.
 */
class PlayerActivity : Activity() {
    private lateinit var video: VideoView
    private val main = Handler(Looper.getMainLooper())

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        video = VideoView(this)
        setContentView(video)
        val uri = intent.data
        spikeLog("PLAYER_START uri=$uri")
        video.setOnPreparedListener { mp: MediaPlayer ->
            spikeLog("PLAYER_PREPARED duration_ms=${mp.duration} w=${mp.videoWidth} h=${mp.videoHeight}")
            mp.setOnSeekCompleteListener { spikeLog("PLAYER_SEEK_DONE pos_ms=${it.currentPosition}") }
            video.start()
            script(mp.duration)
        }
        video.setOnErrorListener { _, what, extra ->
            spikeLog("PLAYER_ERROR what=$what extra=$extra")
            true
        }
        video.setOnCompletionListener { spikeLog("PLAYER_COMPLETE") }
        video.setVideoURI(uri)
    }

    private fun script(durationMs: Int) {
        val steps = listOf(
            5_000L to (durationMs * 0.5).toInt(),
            9_000L to (durationMs * 0.9).toInt(),
            12_000L to (durationMs * 0.1).toInt(),
        )
        for ((at, pos) in steps) {
            main.postDelayed({
                spikeLog("PLAYER_SEEK_TO pos_ms=$pos from_ms=${video.currentPosition}")
                video.seekTo(pos)
            }, at)
        }
        main.postDelayed({
            spikeLog("PLAYER_FINISH pos_ms=${video.currentPosition}")
            finish()
        }, 16_000L)
    }

    override fun onDestroy() {
        spikeLog("PLAYER_DESTROY")
        video.stopPlayback()
        super.onDestroy()
    }
}
