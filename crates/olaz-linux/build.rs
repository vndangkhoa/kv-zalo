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

    let ffmpeg_pkgs = [
        "libavformat",
        "libavcodec",
        "libswresample",
        "libswscale",
        "libavutil",
    ];
    let ffmpeg_flags = match (
        pkg_config_flags(&["--cflags"], &ffmpeg_pkgs),
        pkg_config_flags(&["--libs"], &ffmpeg_pkgs),
    ) {
        (Some(cf), Some(lb)) => Some((cf, lb)),
        _ => None,
    };

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR must be set"));
    let out_so = out_dir.join("libgstolazauto.so");
    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_string());

    let mut cmd = Command::new(&cc);
    cmd.args(["-shared", "-fPIC", "-O2", "-Wall", "-Wextra"]);
    cmd.args(&gst_cflags);
    if let Some((ref ff_cflags, _)) = ffmpeg_flags {
        cmd.arg("-DOLAZ_HAVE_FFMPEG=1");
        cmd.args(ff_cflags);
    }
    cmd.arg("gst/gstolazauto.c");
    cmd.arg("-o");
    cmd.arg(&out_so);
    cmd.args(&gst_libs);
    if let Some((_, ref ff_libs)) = ffmpeg_flags {
        cmd.args(ff_libs);
    }
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
        }
        Err(err) => {
            println!("cargo:warning=failed to invoke `{cc}` for gst/gstolazauto.c: {err}");
        }
    }
}
