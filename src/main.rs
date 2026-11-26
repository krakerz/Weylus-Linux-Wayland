#![cfg_attr(feature = "bench", feature(test))]
#[cfg(not(target_os = "linux"))]
compile_error!("Weylus supports Linux only.");

#[cfg(feature = "bench")]
extern crate test;

#[macro_use]
extern crate bitflags;

use clap::CommandFactory;
use clap_complete::generate;
use signal_hook::iterator::Signals;
use signal_hook::{consts::TERM_SIGNALS, low_level::signal_name};
use tracing::{error, info, warn};

use std::sync::mpsc;

use config::{Config, get_config};

mod access;
mod capturable;
mod cerror;
mod clients;
mod codec;
mod config;
mod desktop;
mod input;
mod log;
mod protocol;
mod update;
mod video;
mod web;
mod websocket;
mod weylus;

fn main() {
    // Detect before an update can replace the binary: afterwards /proc/self/exe reads "(deleted)".
    let install_kind = update::install_kind();
    if matches!(install_kind, update::InstallKind::AppImage { .. })
        && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
    {
        // WebKitGTK's DMA-BUF renderer shows a blank window on some driver/compositor combinations.
        // SAFETY: no other thread exists yet, so nothing reads the environment concurrently.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }

    let (sender, receiver) = mpsc::sync_channel::<String>(100);

    log::setup_logging(sender);

    let conf = get_config();

    if let Some(shell) = conf.completions {
        generate(
            shell,
            &mut Config::command(),
            "weylus",
            &mut std::io::stdout(),
        );
        return;
    }

    if conf.print_index_html {
        print!("{}", web::INDEX_HTML);
        return;
    }
    if conf.print_access_html {
        print!("{}", web::ACCESS_HTML);
        return;
    }
    if conf.print_style_css {
        print!("{}", web::STYLE_CSS);
        return;
    }
    if conf.print_lib_js {
        print!("{}", web::LIB_JS);
        return;
    }

    info!(
        "Weylus {} ({})",
        update::current_version(),
        install_kind.describe()
    );

    {
        if let Err(err) = gstreamer::init() {
            error!(
                "Failed to initialize gstreamer, screen capturing will most likely not work \
                 on Wayland: {}",
                err
            );
        }
    }

    if conf.no_gui {
        if conf.check_updates && install_kind.update_blocker().is_none() {
            std::thread::spawn(|| match update::check() {
                Ok(Some(update)) => info!(
                    "Weylus {} is available: https://github.com/krakerz/Weylus-Linux-Wayland/releases/latest",
                    update.version
                ),
                Ok(None) => {}
                Err(err) => tracing::debug!("Update check failed: {err:#}"),
            });
        }
        let mut weylus = crate::weylus::Weylus::new();
        if !weylus.start(&conf, |msg| match msg {
            web::Web2UiMessage::UInputInaccessible => {
                warn!(std::include_str!("strings/uinput_error.txt"))
            }
            // without a window nobody can answer; dropping the reply refuses the device
            web::Web2UiMessage::ApproveClient(_)
            | web::Web2UiMessage::ApprovalExpired(_)
            | web::Web2UiMessage::ClientsChanged(_) => {}
        }) {
            error!("Weylus failed to start, see the log above.");
            std::process::exit(1);
        }
        {
            let mut signals = Signals::new(TERM_SIGNALS).unwrap();
            let sig = signals.forever().next().expect("signal iterator ended");
            info!(
                "Shutting down after receiving signal {signame} ({sig})...",
                signame = signal_name(sig).unwrap_or("UNKNOWN SIGNAL")
            );
            std::thread::spawn(move || {
                let sig = signals.forever().next().expect("signal iterator ended");
                warn!(
                    "Received second signal {signame} ({sig}) while shutting down \
                    gracefully, proceeding with forceful shutdown...",
                    signame = signal_name(sig).unwrap_or("UNKNOWN SIGNAL")
                );
                std::process::exit(1);
            });
            weylus.stop();
        }
    } else {
        desktop::run(conf, receiver);
        update::spawn_pending_relaunch();
    }
}

#[cfg(feature = "bench")]
#[cfg(test)]
mod tests {
    use super::*;
    use capturable::{Capturable, Recorder};
    use test::Bencher;

    #[cfg(target_os = "linux")]
    #[bench]
    fn bench_capture_wayland(b: &mut Bencher) {
        gstreamer::init().unwrap();
        let root = capturable::pipewire::get_capturables(false)
            .unwrap()
            .remove(0);
        let mut r = root.recorder(false).unwrap();
        let _ = r.capture();
        b.iter(|| {
            r.capture().unwrap();
        });
    }

    #[cfg(target_os = "linux")]
    #[bench]
    fn bench_video_wayland(b: &mut Bencher) {
        gstreamer::init().unwrap();
        let root = capturable::pipewire::get_capturables(false)
            .unwrap()
            .remove(0);
        let mut r = root.recorder(false).unwrap();
        let (width, height) = r.capture().unwrap().size();

        let opts = video::EncoderOptions {
            try_vaapi: true,
            try_nvenc: true,
            ..Default::default()
        };
        let mut encoder =
            video::VideoEncoder::new(width, height, width, height, |_| {}, opts).unwrap();
        b.iter(|| encoder.encode(r.capture().unwrap()));
    }

    #[cfg(target_os = "linux")]
    #[bench]
    fn bench_video_vaapi(b: &mut Bencher) {
        const WIDTH: usize = 1920;
        const HEIGHT: usize = 1080;
        const N: usize = 60;
        let mut bufs = vec![vec![0u8; SIZE]; N];
        for i in 0..N {
            for j in 0..SIZE {
                bufs[i][j] = ((i * SIZE + j) % 256) as u8;
            }
        }

        let opts = video::EncoderOptions {
            try_vaapi: true,
            try_nvenc: false,
            ..Default::default()
        };
        let mut encoder =
            video::VideoEncoder::new(WIDTH, HEIGHT, WIDTH, HEIGHT, |_| {}, opts).unwrap();
        const SIZE: usize = WIDTH * HEIGHT * 4;
        let mut i = 0;
        b.iter(|| {
            encoder.encode(video::PixelProvider::Bgr0(WIDTH, HEIGHT, &bufs[i % N]));
            i += 1;
        });
    }

    #[cfg(target_os = "linux")]
    #[bench]
    fn bench_video_x264(b: &mut Bencher) {
        const WIDTH: usize = 1920;
        const HEIGHT: usize = 1080;
        const N: usize = 60;
        let mut bufs = vec![vec![0u8; SIZE]; N];
        for i in 0..N {
            for j in 0..SIZE {
                bufs[i][j] = ((i * SIZE + j) % 256) as u8;
            }
        }

        let opts = video::EncoderOptions {
            try_vaapi: false,
            try_nvenc: false,
            ..Default::default()
        };
        let mut encoder =
            video::VideoEncoder::new(WIDTH, HEIGHT, WIDTH, HEIGHT, |_| {}, opts).unwrap();
        const SIZE: usize = WIDTH * HEIGHT * 4;
        let mut i = 0;
        b.iter(|| {
            encoder.encode(video::PixelProvider::Bgr0(WIDTH, HEIGHT, &bufs[i % N]));
            i += 1;
        });
    }

    #[cfg(target_os = "linux")]
    #[bench]
    fn bench_video_nvenc(b: &mut Bencher) {
        const WIDTH: usize = 1920;
        const HEIGHT: usize = 1080;
        const N: usize = 60;
        let mut bufs = vec![vec![0u8; SIZE]; N];
        for i in 0..N {
            for j in 0..SIZE {
                bufs[i][j] = ((i * SIZE + j) % 256) as u8;
            }
        }

        let opts = video::EncoderOptions {
            try_vaapi: false,
            try_nvenc: true,
            ..Default::default()
        };
        let mut encoder =
            video::VideoEncoder::new(WIDTH, HEIGHT, WIDTH, HEIGHT, |_| {}, opts).unwrap();
        const SIZE: usize = WIDTH * HEIGHT * 4;
        let mut i = 0;
        b.iter(|| {
            encoder.encode(video::PixelProvider::Bgr0(WIDTH, HEIGHT, &bufs[i % N]));
            i += 1;
        });
    }
}
