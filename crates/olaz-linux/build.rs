use std::env;
use std::path::PathBuf;
use std::process::Command;

fn pkg_config_flags(args: &[&str], pkgs: &[&str]) -> Option<Vec<String>> {
    let out = Command::new("pkg-config")
        .args(args)
        .args(pkgs)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    Some(text.split_whitespace().map(str::to_string).collect())
}

fn main() {
    println!("cargo:rerun-if-changed=gst/gstolazauto.c");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rustc-check-cfg=cfg(has_embedded_gst_plugin)");
    println!("cargo:rustc-check-cfg=cfg(has_embedded_gst_ffmpeg_helper)");

    let gst_pkgs = [
        "gstreamer-1.0",
        "gstreamer-base-1.0",
        "gstreamer-audio-1.0",
        "gstreamer-video-1.0",
    ];
    let (Some(gst_cflags), Some(gst_libs)) = (
        pkg_config_flags(&["--cflags"], &gst_pkgs),
        pkg_config_flags(&["--libs"], &gst_pkgs),
    ) else {
        println!(
            "cargo:warning=GStreamer 1.0 development headers not found via pkg-config; skipping embedded libgstolazauto.so"
        );
        return;
    };

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR must be set"));
    let out_so = out_dir.join("libgstolazauto.so");
    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_string());

    // 1. Build universal `libgstolazauto.so` linked ONLY against stable `libgst*-1.0.so.0`
    //    so a single binary loads cleanly across Arch/Omarchy, Ubuntu 22.04/24.04, and Debian 12+
    //    regardless of which FFmpeg major SONAME (`libavformat.so.{58..63}`) the host ships.
    let mut cmd = Command::new(&cc);
    cmd.args(["-shared", "-fPIC", "-O2", "-Wall", "-Wextra"]);
    cmd.args(&gst_cflags);
    cmd.arg("gst/gstolazauto.c");
    cmd.arg("-o");
    cmd.arg(&out_so);
    cmd.args(&gst_libs);
    cmd.args(["-ldl", "-lm"]);

    match cmd.output() {
        Ok(output) if output.status.success() => {
            println!("cargo:rustc-cfg=has_embedded_gst_plugin");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            println!(
                "cargo:warning=failed to compile gst/gstolazauto.c with `{cc}` (status {}): {}",
                output.status,
                stderr.lines().next().unwrap_or("unknown error")
            );
            return;
        }
        Err(err) => {
            println!("cargo:warning=failed to invoke `{cc}` for gst/gstolazauto.c: {err}");
            return;
        }
    }

    // 2. Optionally build `olazav.gsthelper` (loaded via `dlopen` by `libgstolazauto.so` when
    //    the runtime host's FFmpeg SONAMEs match, silently skipped without scanner warnings otherwise).
    let ffmpeg_pkgs = [
        "libavformat",
        "libavcodec",
        "libswresample",
        "libswscale",
        "libavutil",
    ];
    if let (Some(ff_cflags), Some(ff_libs)) = (
        pkg_config_flags(&["--cflags"], &ffmpeg_pkgs),
        pkg_config_flags(&["--libs"], &ffmpeg_pkgs),
    ) {
        let out_helper = out_dir.join("olazav.gsthelper");
        let mut ff_cmd = Command::new(&cc);
        ff_cmd.args([
            "-shared",
            "-fPIC",
            "-O2",
            "-Wall",
            "-Wextra",
            "-DOLAZ_HAVE_FFMPEG=1",
            "-DOLAZ_FFMPEG_HELPER=1",
        ]);
        ff_cmd.args(&gst_cflags);
        ff_cmd.args(&ff_cflags);
        ff_cmd.arg("gst/gstolazauto.c");
        ff_cmd.arg("-o");
        ff_cmd.arg(&out_helper);
        ff_cmd.args(&gst_libs);
        ff_cmd.args(&ff_libs);
        ff_cmd.args(["-ldl", "-lm"]);
        if let Ok(out) = ff_cmd.output() {
            if out.status.success() {
                println!("cargo:rustc-cfg=has_embedded_gst_ffmpeg_helper");
            }
        }
    }
}
