use std::env;
use std::path::Path;
use std::process::Command;

// Library versions of FFmpeg 6.1 (Ubuntu 24.04, the oldest supported distro) and libva 2.20.
const MIN_LIBAVCODEC: &str = "60.31.102";
const MIN_LIBAVFORMAT: &str = "60.16.100";
const MIN_LIBAVFILTER: &str = "9.12.100";
const MIN_LIBAVUTIL: &str = "58.29.100";
const MIN_LIBVA: &str = "1.20.0";

fn probe(name: &str, min_version: &str) -> pkg_config::Library {
    pkg_config::Config::new()
        .atleast_version(min_version)
        .probe(name)
        .unwrap_or_else(|err| {
            panic!(
                "Weylus needs {name} >= {min_version} (FFmpeg 6.1 / libva 2.20 or newer) and its \
                 development files, found via pkg-config: {err}"
            )
        })
}

fn build_js() {
    println!("cargo:rerun-if-changed=ts/lib.ts");

    let js_needs_update = || -> Result<bool, Box<dyn std::error::Error>> {
        Ok(Path::new("ts/lib.ts").metadata()?.modified()?
            > Path::new("www/static/lib.js").metadata()?.modified()?)
    }()
    .unwrap_or(true);
    if !js_needs_update {
        return;
    }

    let local_tsc = Path::new("node_modules/.bin/tsc");
    let mut tsc_command = if local_tsc.exists() {
        Command::new(local_tsc)
    } else {
        Command::new("tsc")
    };
    match tsc_command.status() {
        Err(err) => {
            println!(
                "cargo:warning=Failed to call tsc (run `npm install` first): {}",
                err
            );
            std::process::exit(1);
        }
        Ok(status) => {
            if !status.success() {
                match status.code() {
                    Some(code) => println!("cargo:warning=tsc failed with exitcode: {}", code),
                    None => println!("cargo:warning=tsc terminated by signal."),
                };
                std::process::exit(2);
            }
        }
    }
}

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        panic!("Weylus supports Linux only.");
    }

    build_js();

    let libs = [
        probe("libavcodec", MIN_LIBAVCODEC),
        probe("libavformat", MIN_LIBAVFORMAT),
        probe("libavfilter", MIN_LIBAVFILTER),
        probe("libavutil", MIN_LIBAVUTIL),
        probe("libva", MIN_LIBVA),
    ];

    println!("cargo:rerun-if-changed=lib/encode_video.c");
    let mut cc_video = cc::Build::new();
    cc_video.file("lib/encode_video.c");
    for lib in &libs {
        cc_video.includes(&lib.include_paths);
    }
    cc_video.define("HAS_NVENC", None);
    cc_video.define("HAS_VAAPI", None);
    cc_video.compile("video");

    println!("cargo:rerun-if-changed=lib/error.h");
    println!("cargo:rerun-if-changed=lib/error.c");
    println!("cargo:rerun-if-changed=lib/log.h");
    println!("cargo:rerun-if-changed=lib/log.c");
    cc::Build::new().file("lib/error.c").compile("error");
    cc::Build::new().file("lib/log.c").compile("log");

    println!("cargo:rerun-if-changed=lib/linux/uinput.c");
    cc::Build::new().file("lib/linux/uinput.c").compile("linux");
}
