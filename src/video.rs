use std::os::raw::{c_int, c_uchar, c_void};
use std::time::Instant;

use tracing::warn;

use crate::cerror::CError;

unsafe extern "C" {
    fn init_video_encoder(
        rust_ctx: *mut c_void,
        width_in: c_int,
        height_in: c_int,
        width_out: c_int,
        height_out: c_int,
        try_vaapi: c_int,
        try_nvenc: c_int,
    ) -> *mut c_void;
    #[cfg(test)]
    fn get_codec_name(handle: *mut c_void) -> *const std::os::raw::c_char;
    fn open_video(handle: *mut c_void, err: *mut CError);
    fn destroy_video_encoder(handle: *mut c_void);
    fn encode_video_frame(handle: *mut c_void, micros: c_int, err: *mut CError);

    fn fill_rgb(ctx: *mut c_void, data: *const u8, err: *mut CError);
    fn fill_rgb0(ctx: *mut c_void, data: *const u8, err: *mut CError);
    fn fill_bgr0(ctx: *mut c_void, data: *const u8, stride: c_int, err: *mut CError);
}

// this is used as callback in lib/encode_video.c via ffmpegs AVIOContext
#[unsafe(no_mangle)]
extern "C" fn write_video_packet(
    video_encoder: *mut c_void,
    buf: *const c_uchar,
    buf_size: c_int,
) -> c_int {
    let video_encoder = unsafe { (video_encoder as *mut VideoEncoder).as_mut().unwrap() };
    (video_encoder.write_data)(unsafe { std::slice::from_raw_parts(buf, buf_size as usize) });
    0
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

#[derive(Clone, Copy)]
pub struct EncoderOptions {
    pub try_vaapi: bool,
    pub try_nvenc: bool,
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
        });
        let handle = unsafe {
            init_video_encoder(
                video_encoder.as_mut() as *mut _ as *mut c_void,
                width_in as c_int,
                height_in as c_int,
                width_out as c_int,
                height_out as c_int,
                options.try_vaapi.into(),
                options.try_nvenc.into(),
            )
        };
        video_encoder.handle = handle;

        let mut err = CError::new();
        unsafe { open_video(video_encoder.handle, &mut err) };
        if err.is_err() {
            return Err(Box::new(err));
        }
        Ok(video_encoder)
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Encodes ten gray frames and returns the codec that was used and the bytes written.
    fn encode_frames(
        size_in: (usize, usize),
        size_out: (usize, usize),
        options: EncoderOptions,
    ) -> (Option<String>, usize) {
        let bytes = Arc::new(AtomicUsize::new(0));
        let counter = bytes.clone();
        let mut encoder = VideoEncoder::new(
            size_in.0,
            size_in.1,
            size_out.0,
            size_out.1,
            move |data| {
                counter.fetch_add(data.len(), Ordering::Relaxed);
            },
            options,
        )
        .unwrap_or_else(|err| panic!("failed to create encoder: {err}"));
        let frame = vec![0x80u8; size_in.0 * size_in.1 * 4];
        for _ in 0..10 {
            encoder.encode(PixelProvider::Bgr0(size_in.0, size_in.1, &frame));
        }
        (encoder.codec_name(), bytes.load(Ordering::Relaxed))
    }

    #[test]
    fn software_encoder_encodes_frames() {
        let (codec, bytes) = encode_frames(
            (640, 480),
            (640, 480),
            EncoderOptions {
                try_vaapi: false,
                try_nvenc: false,
            },
        );
        assert_eq!(codec.as_deref(), Some("libx264"));
        assert!(bytes > 0, "no data was written");
    }

    // Regression test for the VA-API scaler: hwupload needs its device before the filter graph
    // is initialized, otherwise the encoder silently falls back to libx264.
    #[test]
    fn vaapi_encoder_encodes_frames() {
        if !Path::new("/dev/dri/renderD128").exists() {
            eprintln!("SKIPPED: no VA-API render node");
            return;
        }
        let (codec, bytes) = encode_frames(
            (3440, 1440),
            (1720, 720),
            EncoderOptions {
                try_vaapi: true,
                try_nvenc: false,
            },
        );
        assert_eq!(codec.as_deref(), Some("h264_vaapi"));
        assert!(bytes > 0, "no data was written");
    }
}
