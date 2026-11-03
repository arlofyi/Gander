// SPDX-License-Identifier: Apache-2.0
// Moved from the stream spike (spikes/stream/capture, spikes/stream/encode) as production code.
//! One asynchronous hardware encoder MFT bound to a D3D11 device.
//!
//! Events (`METransformNeedInput`, `METransformHaveOutput`, ...) are pulled by a dedicated thread
//! with a blocking `GetEvent`, timestamped on arrival (QPC, 100 ns) and forwarded over a channel,
//! so the caller can wait on the channel with a timeout and measure latency without polling.

use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use super::clock;
use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI, IMF2DBuffer,
    IMFActivate, IMFDXGIDeviceManager, IMFMediaEventGenerator, IMFMediaType, IMFShutdown,
    IMFTransform, MEError, METransformDrainComplete, METransformHaveOutput, METransformNeedInput,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NONE, MF_LOW_LATENCY, MF_MT_AVG_BITRATE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE,
    MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK, MFCreateDXGIDeviceManager,
    MFCreateDXGISurfaceBuffer, MFCreateMediaType, MFCreateSample, MFMediaType_Video,
    MFNominalRange_16_235, MFSampleExtension_CleanPoint, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR,
    eAVEncH264VProfile_Base, eAVEncH264VProfile_ConstrainedBase, eAVEncH264VProfile_High,
    eAVEncH264VProfile_Main, eAVEncH265VProfile_Main_420_8,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, Interface};

use super::mft::Codec;

/// Encoder settings.
#[derive(Debug, Clone)]
pub struct Config {
    /// Codec.
    pub codec: Codec,
    /// Encoded width (even).
    pub width: u32,
    /// Encoded height (even).
    pub height: u32,
    /// Frame rate (frames per second, integer).
    pub fps: u32,
    /// Target bitrate, bits per second (CBR).
    pub bitrate: u32,
    /// GOP length in frames (an IDR every `gop` frames); 0 = encoder default.
    pub gop: u32,
    /// `CODECAPI_AVLowLatencyMode` and `MF_LOW_LATENCY` (on by default).
    pub low_latency: bool,
}

impl Config {
    /// Defaults: GOP of two seconds.
    pub fn new(codec: Codec, width: u32, height: u32, fps: u32, bitrate: u32) -> Config {
        Config {
            codec,
            width,
            height,
            fps,
            bitrate,
            gop: fps * 2,
            low_latency: true,
        }
    }
}

/// An encoder event, with the QPC time (100 ns) at which the event thread received it.
#[derive(Debug, Clone)]
pub enum Event {
    /// The MFT accepts one more input sample.
    NeedInput(i64),
    /// One output sample is ready for [`Encoder::take_output`].
    HaveOutput(i64),
    /// Drain finished (after [`Encoder::drain`]).
    DrainComplete(i64),
    /// `MEError` or a failed `GetEvent`.
    Error(String),
    /// Any other event type (number).
    Other(i32),
}

/// One encoded access unit.
#[derive(Debug, Clone)]
pub struct Packet {
    /// Annex-B bytes.
    pub data: Vec<u8>,
    /// Sample time (100 ns), as fed.
    pub time: i64,
    /// `MFSampleExtension_CleanPoint` (key frame).
    pub keyframe: bool,
}

struct SendGen(IMFMediaEventGenerator);
// SAFETY: the event generator of an asynchronous MFT is free-threaded (MTA); it is used by the
// event thread only.
unsafe impl Send for SendGen {}

/// A running hardware encoder.
pub struct Encoder {
    activate: IMFActivate,
    mft: IMFTransform,
    codec_api: Option<ICodecAPI>,
    _manager: IMFDXGIDeviceManager,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
    provides_samples: bool,
    output_size: u32,
    /// Settings as applied: (name, Ok or the error text).
    pub applied: Vec<(String, std::result::Result<(), String>)>,
    /// The H.264 / HEVC profile the output type accepted.
    pub profile: String,
    /// Output type renegotiations (`MF_E_TRANSFORM_STREAM_CHANGE`).
    pub stream_changes: u32,
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Encoder(profile={})", self.profile)
    }
}

fn err_text(e: &windows::core::Error) -> String {
    format!("0x{:08X}", e.code().0)
}

fn pack(a: u32, b: u32) -> u64 {
    (u64::from(a) << 32) | u64::from(b)
}

impl Encoder {
    /// Activates the MFT, binds it to `device` and configures it. `device` must be on the adapter
    /// the MFT belongs to (see [`super::mft::hardware_encoders`]).
    pub fn new(activate: &IMFActivate, device: &ID3D11Device, cfg: &Config) -> Result<Encoder> {
        // SAFETY: activation of a listed MFT.
        let mft: IMFTransform = unsafe { activate.ActivateObject() }.context("ActivateObject")?;
        // SAFETY: plain attribute access on a live MFT.
        let attrs = unsafe { mft.GetAttributes() }.context("GetAttributes")?;
        // SAFETY: as above.
        let is_async = unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
        anyhow::ensure!(
            is_async,
            "encoder MFT is not asynchronous (not a hardware MFT?)"
        );
        // SAFETY: as above.
        unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }.context("ASYNC_UNLOCK")?;
        let mut applied = Vec::new();
        // SAFETY: as above.
        let r = unsafe { attrs.SetUINT32(&MF_LOW_LATENCY, u32::from(cfg.low_latency)) };
        applied.push(("MF_LOW_LATENCY".into(), r.map_err(|e| err_text(&e))));

        let mut token = 0u32;
        let mut manager = None;
        // SAFETY: valid out pointers.
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }
            .context("MFCreateDXGIDeviceManager")?;
        let manager = manager.context("no device manager")?;
        // SAFETY: `device` is multithread protected ([`super::d3d::create_device`]).
        unsafe { manager.ResetDevice(device, token) }.context("ResetDevice")?;
        // SAFETY: the MFT AddRefs the manager; the pointer is valid for the call.
        unsafe { mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize) }
            .context("MFT_MESSAGE_SET_D3D_MANAGER (device on another adapter?)")?;

        let codec_api: Option<ICodecAPI> = mft.cast().ok();
        let set = |api: &Option<ICodecAPI>,
                   applied: &mut Vec<(String, std::result::Result<(), String>)>,
                   name: &str,
                   key: &GUID,
                   v: VARIANT| {
            let r = match api {
                // SAFETY: valid GUID and VARIANT pointers for the call.
                Some(a) => unsafe { a.SetValue(key, &v) }.map_err(|e| err_text(&e)),
                None => Err("no ICodecAPI".into()),
            };
            applied.push((name.into(), r));
        };
        set(
            &codec_api,
            &mut applied,
            "RateControlMode=CBR",
            &CODECAPI_AVEncCommonRateControlMode,
            VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
        );
        set(
            &codec_api,
            &mut applied,
            "MeanBitRate",
            &CODECAPI_AVEncCommonMeanBitRate,
            VARIANT::from(cfg.bitrate),
        );
        set(
            &codec_api,
            &mut applied,
            "LowLatencyMode",
            &CODECAPI_AVLowLatencyMode,
            VARIANT::from(cfg.low_latency),
        );
        if cfg.gop > 0 {
            set(
                &codec_api,
                &mut applied,
                "GOPSize",
                &CODECAPI_AVEncMPVGOPSize,
                VARIANT::from(cfg.gop),
            );
        }
        set(
            &codec_api,
            &mut applied,
            "BPictureCount=0",
            &CODECAPI_AVEncMPVDefaultBPictureCount,
            VARIANT::from(0u32),
        );

        // Output type first (hardware encoders require it), trying profiles in order.
        let profiles: Vec<(&str, i32)> = match cfg.codec {
            Codec::H264 => vec![
                ("constrained-baseline", eAVEncH264VProfile_ConstrainedBase.0),
                ("baseline", eAVEncH264VProfile_Base.0),
                ("main", eAVEncH264VProfile_Main.0),
                ("high", eAVEncH264VProfile_High.0),
            ],
            Codec::Hevc => vec![("main", eAVEncH265VProfile_Main_420_8.0)],
        };
        let mut profile = None;
        let mut last_err = None;
        for (name, p) in profiles {
            let out = Self::output_type(cfg, Some(p))?;
            // SAFETY: valid media type.
            match unsafe { mft.SetOutputType(0, &out, 0) } {
                Ok(()) => {
                    profile = Some(name.to_owned());
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let profile = match profile {
            Some(p) => p,
            None => {
                let out = Self::output_type(cfg, None)?;
                // SAFETY: valid media type.
                unsafe { mft.SetOutputType(0, &out, 0) }
                    .with_context(|| format!("SetOutputType (profiles refused: {last_err:?})"))?;
                "encoder default".to_owned()
            }
        };
        // SAFETY: plain media type creation and attribute setters.
        let input: IMFMediaType = unsafe { MFCreateMediaType() }?;
        // SAFETY: as above.
        unsafe {
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, pack(cfg.width, cfg.height))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, pack(cfg.fps, 1))?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            input.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            mft.SetInputType(0, &input, 0)
                .context("SetInputType NV12")?;
        }

        // SAFETY: plain getter.
        let info = unsafe { mft.GetOutputStreamInfo(0) }.context("GetOutputStreamInfo")?;
        let provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;

        let generator: IMFMediaEventGenerator = mft.cast().context("IMFMediaEventGenerator")?;
        let (tx, rx) = channel();
        let gen_ = SendGen(generator);
        let thread = std::thread::Builder::new()
            .name("mft-events".into())
            .spawn(move || {
                let gen_ = gen_;
                loop {
                    // SAFETY: blocking call on a live generator; returns MF_E_SHUTDOWN after
                    // IMFShutdown::Shutdown, which ends the loop.
                    let ev = match unsafe { gen_.0.GetEvent(MF_EVENT_FLAG_NONE) } {
                        Ok(ev) => ev,
                        Err(e) => {
                            let _ = tx.send(Event::Error(format!("GetEvent {}", err_text(&e))));
                            return;
                        }
                    };
                    let t = clock::now_100ns();
                    // SAFETY: plain getters on a live event.
                    let kind = unsafe { ev.GetType() }.unwrap_or(0) as i32;
                    let msg = match kind {
                        k if k == METransformNeedInput.0 => Event::NeedInput(t),
                        k if k == METransformHaveOutput.0 => Event::HaveOutput(t),
                        k if k == METransformDrainComplete.0 => Event::DrainComplete(t),
                        k if k == MEError.0 => {
                            // SAFETY: plain getter.
                            let st = unsafe { ev.GetStatus() }.map(|h| h.0).unwrap_or(0);
                            Event::Error(format!("MEError 0x{st:08X}"))
                        }
                        k => Event::Other(k),
                    };
                    let end = matches!(msg, Event::DrainComplete(_) | Event::Error(_));
                    if tx.send(msg).is_err() || end {
                        return;
                    }
                }
            })
            .context("spawn event thread")?;

        // SAFETY: plain streaming messages.
        unsafe {
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(Encoder {
            activate: activate.clone(),
            mft,
            codec_api,
            _manager: manager,
            events: rx,
            thread: Some(thread),
            provides_samples,
            output_size: info.cbSize.max(4 << 20),
            applied,
            profile,
            stream_changes: 0,
        })
    }

    fn output_type(cfg: &Config, profile: Option<i32>) -> Result<IMFMediaType> {
        // SAFETY: plain media type creation and attribute setters.
        unsafe {
            let t: IMFMediaType = MFCreateMediaType()?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            t.SetGUID(&MF_MT_SUBTYPE, &cfg.codec.subtype())?;
            t.SetUINT32(&MF_MT_AVG_BITRATE, cfg.bitrate)?;
            t.SetUINT64(&MF_MT_FRAME_SIZE, pack(cfg.width, cfg.height))?;
            t.SetUINT64(&MF_MT_FRAME_RATE, pack(cfg.fps, 1))?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            if let Some(p) = profile {
                t.SetUINT32(&MF_MT_MPEG2_PROFILE, p as u32)?;
            }
            Ok(t)
        }
    }

    /// Next event, waiting up to `timeout`.
    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        match self.events.recv_timeout(timeout) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Event::Error("event thread ended".into())),
        }
    }

    /// Feeds one NV12 texture (on the encoder's device). Call once per `NeedInput`.
    pub fn feed(
        &mut self,
        nv12: &ID3D11Texture2D,
        time_100ns: i64,
        duration_100ns: i64,
    ) -> Result<()> {
        // SAFETY: the texture is a live D3D11 resource on the manager's device; the buffer and
        // sample hold references to it until the encoder releases them.
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, false)
                .context("MFCreateDXGISurfaceBuffer")?;
            if let Ok(b2) = buffer.cast::<IMF2DBuffer>()
                && let Ok(len) = b2.GetContiguousLength()
            {
                let _ = buffer.SetCurrentLength(len);
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time_100ns)?;
            sample.SetSampleDuration(duration_100ns)?;
            self.mft
                .ProcessInput(0, &sample, 0)
                .context("ProcessInput")?;
        }
        Ok(())
    }

    /// Asks for the next encoded picture to be an IDR.
    pub fn force_keyframe(&self) -> Result<()> {
        let api = self.codec_api.as_ref().context("no ICodecAPI")?;
        // SAFETY: valid GUID and VARIANT pointers.
        unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32)) }
            .context("ForceKeyFrame")?;
        Ok(())
    }

    /// Changes the CBR target while streaming (`CODECAPI_AVEncCommonMeanBitRate`). Added by WP-43
    /// to follow the congestion controller. Measured: takes effect on the NVIDIA H.264 MFT; the
    /// Intel Quick Sync MFT returns S_OK but keeps its configured rate (re-create the encoder).
    pub fn set_bitrate(&self, bits_per_second: u32) -> Result<()> {
        let api = self.codec_api.as_ref().context("no ICodecAPI")?;
        // SAFETY: valid GUID and VARIANT pointers.
        unsafe {
            api.SetValue(
                &CODECAPI_AVEncCommonMeanBitRate,
                &VARIANT::from(bits_per_second),
            )
        }
        .context("MeanBitRate")?;
        Ok(())
    }

    /// Changes the CBR target through an `MEEncodingParameters` event carrying
    /// `CODECAPI_AVEncCommonMeanBitRate` (`IMFTransform::ProcessEvent`), the other documented
    /// route for dynamic encoder settings. Added by WP-43 for comparison with [`Self::set_bitrate`].
    pub fn set_bitrate_event(&self, bits_per_second: u32) -> Result<()> {
        // SAFETY: plain event creation, attribute setter and a call on a live MFT.
        unsafe {
            let ev = windows::Win32::Media::MediaFoundation::MFCreateMediaEvent(
                windows::Win32::Media::MediaFoundation::MEEncodingParameters.0 as u32,
                &GUID::zeroed(),
                windows::core::HRESULT(0),
                None,
            )
            .context("MFCreateMediaEvent")?;
            ev.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, bits_per_second)?;
            self.mft
                .ProcessEvent(0, &ev)
                .context("ProcessEvent(MEEncodingParameters)")?;
        }
        Ok(())
    }

    /// Collects one output sample after a `HaveOutput` event. `None` when the event was an output
    /// type change (handled here).
    pub fn take_output(&mut self) -> Result<Option<Packet>> {
        let sample = if self.provides_samples {
            None
        } else {
            // SAFETY: plain sample/buffer creation.
            unsafe {
                let s = MFCreateSample()?;
                let b =
                    windows::Win32::Media::MediaFoundation::MFCreateMemoryBuffer(self.output_size)?;
                s.AddBuffer(&b)?;
                Some(s)
            }
        };
        let mut buf = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        // SAFETY: one output buffer descriptor; the MFT fills pSample (or uses ours).
        let r = unsafe { self.mft.ProcessOutput(0, &mut buf, &mut status) };
        let [b] = buf;
        let sample = std::mem::ManuallyDrop::into_inner(b.pSample);
        drop(std::mem::ManuallyDrop::into_inner(b.pEvents));
        match r {
            Ok(()) => {}
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                self.stream_changes += 1;
                // SAFETY: plain type negotiation.
                unsafe {
                    let t = self.mft.GetOutputAvailableType(0, 0)?;
                    self.mft.SetOutputType(0, &t, 0)?;
                }
                return Ok(None);
            }
            Err(e) => bail!("ProcessOutput: {}", err_text(&e)),
        }
        let sample = sample.context("ProcessOutput returned no sample")?;
        // SAFETY: plain getters and a locked read of the contiguous buffer.
        unsafe {
            let time = sample.GetSampleTime().unwrap_or(0);
            let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut p = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut p, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(p, len as usize).to_vec();
            buffer.Unlock()?;
            Ok(Some(Packet {
                data,
                time,
                keyframe,
            }))
        }
    }

    /// Starts draining: the remaining outputs arrive, then `DrainComplete`.
    pub fn drain(&self) -> Result<()> {
        // SAFETY: plain streaming messages.
        unsafe {
            self.mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: plain shutdown sequence on a live MFT; Shutdown wakes the event thread.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(s) = self.mft.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
            let _ = self.activate.ShutdownObject();
        }
        if let Some(t) = self.thread.take() {
            // Join only if the thread ended at once; otherwise detach it (it ends when its
            // GetEvent returns MF_E_SHUTDOWN). Waiting here stalled an Intel rate change
            // (encoder re-creation) for over a second in the WP-50 loopback runs.
            for _ in 0..2 {
                if t.is_finished() {
                    let _ = t.join();
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}
