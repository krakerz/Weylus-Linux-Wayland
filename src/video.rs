use std::os::raw::{c_int, c_uchar, c_void};
use std::time::Instant;

use tracing::warn;

use crate::cerror::CError;
use crate::codec::{Codec, CodecOrder, Packetizer};

unsafe extern "C" {
    fn init_video_encoder(
        rust_ctx: *mut c_void,
        width_in: c_int,
        height_in: c_int,
        width_out: c_int,
        height_out: c_int,
        try_vaapi: c_int,
        try_nvenc: c_int,
        bitrate_kbps: c_int,
        frame_rate: c_int,
        codec: c_int,
    ) -> *mut c_void;
    fn request_keyframe(handle: *mut c_void);
    #[cfg(test)]
    fn get_codec_name(handle: *mut c_void) -> *const std::os::raw::c_char;
    #[cfg(test)]
    fn avcodec_find_encoder_by_name(name: *const std::os::raw::c_char) -> *const c_void;
    fn open_video(handle: *mut c_void, err: *mut CError);
    fn destroy_video_encoder(handle: *mut c_void);
    fn encode_video_frame(handle: *mut c_void, micros: c_int, err: *mut CError);

    fn fill_rgb(ctx: *mut c_void, data: *const u8, err: *mut CError);
    fn fill_rgb0(ctx: *mut c_void, data: *const u8, err: *mut CError);
    fn fill_bgr0(ctx: *mut c_void, data: *const u8, stride: c_int, err: *mut CError);
}

// called by lib/encode_video.c once per encoded packet
#[unsafe(no_mangle)]
extern "C" fn write_video_packet(
    video_encoder: *mut c_void,
    buf: *const c_uchar,
    buf_size: c_int,
    pts_ms: i64,
    keyframe: c_int,
) {
    // SAFETY: rust_ctx is the boxed VideoEncoder that owns the C context and outlives every
    // encode call; buf/buf_size describe the packet ffmpeg owns for the duration of this call.
    let (video_encoder, data) = unsafe {
        (
            (video_encoder as *mut VideoEncoder)
                .as_mut()
                .expect("video encoder pointer is set before encoding"),
            std::slice::from_raw_parts(buf, buf_size as usize),
        )
    };
    // wraps after ~49 days; the client only uses it as a chunk timestamp
    let pts_ms = pts_ms as u32;
    if let Some(message) = video_encoder
        .packetizer
        .packetize(data, pts_ms, keyframe != 0)
    {
        (video_encoder.write_data)(&message);
    }
}

pub enum PixelProvider<'a> {
    // 8 bits per color
    Rgb(usize, usize, &'a [u8]),
    Rgb0(usize, usize, &'a [u8]),
    Bgr0(usize, usize, &'a [u8]),
    // width, height, stride
    Bgr0S(usize, usize, usize, &'a [u8]),
}

impl<'a> PixelProvider<'a> {
    pub fn size(&self) -> (usize, usize) {
        match self {
            PixelProvider::Rgb(w, h, _) => (*w, *h),
            PixelProvider::Rgb0(w, h, _) => (*w, *h),
            PixelProvider::Bgr0(w, h, _) => (*w, *h),
            PixelProvider::Bgr0S(w, h, _, _) => (*w, *h),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncoderOptions {
    /// tried in order until one opens; ends with H.264
    pub codecs: CodecOrder,
    pub try_vaapi: bool,
    pub try_nvenc: bool,
    /// 0 = constant quality (encoder default)
    pub bitrate_kbps: u32,
    /// only used for rate control when `bitrate_kbps > 0`
    pub frame_rate: u32,
}

type WriteData = Box<dyn FnMut(&[u8])>;

pub struct VideoEncoder {
    handle: *mut c_void,
    width_in: usize,
    height_in: usize,
    width_out: usize,
    height_out: usize,
    write_data: WriteData,
    start_time: Instant,
    packetizer: Packetizer,
}

impl VideoEncoder {
    pub fn new(
        width_in: usize,
        height_in: usize,
        width_out: usize,
        height_out: usize,
        mut write_data: impl FnMut(&[u8]) + 'static,
        options: EncoderOptions,
    ) -> Result<Box<Self>, Box<CError>> {
        let mut video_encoder = Box::new(Self {
            handle: std::ptr::null_mut(),
            width_in,
            height_in,
            width_out,
            height_out,
            write_data: Box::new(move |data| write_data(data)),
            start_time: Instant::now(),
            packetizer: Packetizer::new(Codec::H264),
        });
        let mut last_err = None;
        for codec in options.codecs.iter() {
            // SAFETY: the boxed encoder outlives the C context, which only stores the pointer.
            let handle = unsafe {
                init_video_encoder(
                    video_encoder.as_mut() as *mut _ as *mut c_void,
                    width_in as c_int,
                    height_in as c_int,
                    width_out as c_int,
                    height_out as c_int,
                    options.try_vaapi.into(),
                    options.try_nvenc.into(),
                    options.bitrate_kbps.min(i32::MAX as u32) as c_int,
                    options.frame_rate as c_int,
                    c_int::from(codec.wire_id()),
                )
            };
            let mut err = CError::new();
            // SAFETY: handle was just returned by init_video_encoder.
            unsafe { open_video(handle, &mut err) };
            if err.is_err() {
                warn!("Could not open a {codec} encoder: {err}");
                // SAFETY: a context whose open failed is not initialized; destroy only frees it.
                unsafe { destroy_video_encoder(handle) };
                last_err = Some(err);
                continue;
            }
            video_encoder.handle = handle;
            video_encoder.packetizer = Packetizer::new(codec);
            return Ok(video_encoder);
        }
        Err(Box::new(last_err.expect(
            "CodecOrder always contains H.264, so at least one attempt ran",
        )))
    }

    pub fn encode(&mut self, pixel_provider: PixelProvider) {
        let mut err = CError::new();
        match pixel_provider {
            PixelProvider::Bgr0(w, _, bgr0) => unsafe {
                fill_bgr0(self.handle, bgr0.as_ptr(), (w * 4) as c_int, &mut err);
            },
            PixelProvider::Bgr0S(_, _, stride, bgr0) => unsafe {
                fill_bgr0(self.handle, bgr0.as_ptr(), stride as c_int, &mut err);
            },
            PixelProvider::Rgb(_, _, rgb) => unsafe {
                fill_rgb(self.handle, rgb.as_ptr(), &mut err);
            },
            PixelProvider::Rgb0(_, _, rgb) => unsafe {
                fill_rgb0(self.handle, rgb.as_ptr(), &mut err);
            },
        }
        if err.is_err() {
            warn!("Failed to fill video frame: {}", err);
            return;
        }
        unsafe {
            encode_video_frame(
                self.handle,
                (Instant::now() - self.start_time).as_millis() as c_int,
                &mut err,
            );
        }
        if err.is_err() {
            warn!("Failed to encode video frame: {}", err);
        }
    }

    pub fn request_keyframe(&mut self) {
        // SAFETY: the handle is valid for the lifetime of `self`.
        unsafe { request_keyframe(self.handle) }
    }

    #[cfg(test)]
    pub fn codec_name(&self) -> Option<String> {
        // SAFETY: the handle is valid for the lifetime of `self` and the returned string is a
        // static codec name owned by libavcodec (or null if no codec is open).
        unsafe {
            let name = get_codec_name(self.handle);
            if name.is_null() {
                None
            } else {
                Some(
                    std::ffi::CStr::from_ptr(name)
                        .to_string_lossy()
                        .into_owned(),
                )
            }
        }
    }

    pub fn check_size(
        &self,
        width_in: usize,
        height_in: usize,
        width_out: usize,
        height_out: usize,
    ) -> bool {
        (self.width_in == width_in)
            && (self.height_in == height_in)
            && (self.width_out == width_out)
            && (self.height_out == height_out)
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { destroy_video_encoder(self.handle) }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use crate::codec::{Codec, CodecOrder, FLAG_CONFIG, FLAG_KEY, MESSAGE_VERSION};

    use super::*;

    /// Encodes ten gray frames, requests a keyframe before the sixth, and returns the codec that
    /// was used and the messages written.
    fn encode_frames(
        size_in: (usize, usize),
        size_out: (usize, usize),
        options: EncoderOptions,
    ) -> (Option<String>, Vec<Vec<u8>>) {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let sink = messages.clone();
        let mut encoder = VideoEncoder::new(
            size_in.0,
            size_in.1,
            size_out.0,
            size_out.1,
            move |data| sink.lock().expect("lock poisoned").push(data.to_vec()),
            options,
        )
        .unwrap_or_else(|err| panic!("failed to create encoder: {err}"));
        let frame = vec![0x80u8; size_in.0 * size_in.1 * 4];
        for i in 0..10 {
            if i == 5 {
                encoder.request_keyframe();
            }
            encoder.encode(PixelProvider::Bgr0(size_in.0, size_in.1, &frame));
        }
        let codec = encoder.codec_name();
        let messages = messages.lock().expect("lock poisoned").clone();
        (codec, messages)
    }

    /// The codec string of a CONFIG message (wire format v2: length at byte 8, string from 9).
    fn config_codec_string(msg: &[u8]) -> &str {
        let len = usize::from(msg[8]);
        std::str::from_utf8(&msg[9..9 + len]).expect("codec string is ASCII")
    }

    fn assert_stream(codec: &Option<String>, messages: &[Vec<u8>], expected: Codec) {
        assert!(!messages.is_empty(), "{codec:?}: no data was written");
        assert_eq!(messages[0][0], MESSAGE_VERSION, "{codec:?}: version");
        assert!(
            messages.iter().all(|m| m[2] == expected.wire_id()),
            "{codec:?}: codec id must be {}",
            expected.wire_id()
        );
        assert_eq!(
            messages[0][1],
            FLAG_KEY | FLAG_CONFIG,
            "{codec:?}: first message must be a keyframe with config"
        );
        // gop size 12 > 10 frames, so only the requested keyframe can be another one
        assert!(
            messages[1..].iter().any(|m| m[1] & FLAG_KEY != 0),
            "{codec:?}: requested keyframe not produced"
        );
    }

    #[test]
    fn software_encoder_encodes_frames() {
        let (codec, messages) = encode_frames(
            (640, 480),
            (640, 480),
            EncoderOptions {
                try_vaapi: false,
                try_nvenc: false,
                ..Default::default()
            },
        );
        assert_eq!(codec.as_deref(), Some("libx264"));
        assert_stream(&codec, &messages, Codec::H264);
    }

    #[test]
    fn software_encoder_falls_back_to_h264() {
        let (codec, messages) = encode_frames(
            (640, 480),
            (640, 480),
            EncoderOptions {
                codecs: CodecOrder::from_names(&["av1".into(), "hevc".into()]),
                try_vaapi: false,
                try_nvenc: false,
                ..Default::default()
            },
        );
        assert_eq!(codec.as_deref(), Some("libx264"));
        assert_stream(&codec, &messages, Codec::H264);
        assert!(config_codec_string(&messages[0]).starts_with("avc1."));
    }

    // Regression test for the VA-API scaler: hwupload needs its device before the filter graph
    // is initialized, otherwise the encoder silently falls back to libx264.
    #[test]
    fn vaapi_encoder_encodes_frames() {
        if !Path::new("/dev/dri/renderD128").exists() {
            eprintln!("SKIPPED: no VA-API render node");
            return;
        }
        let (codec, messages) = encode_frames(
            (3440, 1440),
            (1720, 720),
            EncoderOptions {
                try_vaapi: true,
                try_nvenc: false,
                ..Default::default()
            },
        );
        assert_eq!(codec.as_deref(), Some("h264_vaapi"));
        assert_stream(&codec, &messages, Codec::H264);
    }

    fn vaapi_codec_test(name: &str, codec: Codec, prefix: &str) {
        if !Path::new("/dev/dri/renderD128").exists() {
            eprintln!("SKIPPED: no VA-API render node");
            return;
        }
        let c_name = std::ffi::CString::new(name).expect("no NUL in encoder name");
        // SAFETY: c_name is a valid NUL-terminated string; the lookup only reads the registry.
        if unsafe { avcodec_find_encoder_by_name(c_name.as_ptr()) }.is_null() {
            eprintln!("SKIPPED: this FFmpeg has no {name}");
            return;
        }
        let (opened, messages) = encode_frames(
            (3440, 1440),
            (1720, 720),
            EncoderOptions {
                codecs: CodecOrder::from_names(&[format!("{codec:?}").to_lowercase()]),
                try_vaapi: true,
                try_nvenc: false,
                ..Default::default()
            },
        );
        if opened.as_deref() != Some(name) {
            eprintln!("SKIPPED: {name} could not be opened on this GPU (got {opened:?})");
            return;
        }
        assert_stream(&opened, &messages, codec);
        assert!(config_codec_string(&messages[0]).starts_with(prefix));
        if codec == Codec::Av1 {
            let first = &messages[0];
            let payload = &first[9 + usize::from(first[8]) + 4..];
            assert_eq!(
                (payload[0] >> 3) & 0xF,
                1,
                "{opened:?}: payload must start with the sequence header OBU"
            );
        }
    }

    #[test]
    fn vaapi_hevc_encoder_encodes_frames() {
        vaapi_codec_test("hevc_vaapi", Codec::Hevc, "hvc1.");
    }

    #[test]
    fn vaapi_av1_encoder_encodes_frames() {
        vaapi_codec_test("av1_vaapi", Codec::Av1, "av01.");
    }
}
