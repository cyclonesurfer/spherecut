//! Tells the code which FFmpeg API it is built against. ffmpeg-sys-next
//! `links = "ffmpeg"`, so cargo passes its `ffmpeg_X_Y` version flags to
//! this script as `DEP_FFMPEG_FFMPEG_X_Y`.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(ffmpeg_7_1)");
    // libavcodec 61.19: avcodec_get_supported_config replaces AVCodec's
    // sample_fmts and friends.
    if std::env::var("DEP_FFMPEG_FFMPEG_7_1").as_deref() == Ok("true") {
        println!("cargo::rustc-cfg=ffmpeg_7_1");
    }
}
