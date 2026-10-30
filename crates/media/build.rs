//! Tells the code which FFmpeg API it is built against. ffmpeg-sys-next
//! `links = "ffmpeg"`, so cargo passes its `ffmpeg_X_Y` version flags to
//! this script as `DEP_FFMPEG_FFMPEG_X_Y`.

fn main() {
    // libavcodec 61.19 (7.1): avcodec_get_supported_config replaces
    // AVCodec's sample_fmts and friends, and AVVulkanDeviceContext gains
    // `qf[]`.
    // libavcodec 63 (9.0): AVVulkanDeviceContext's fixed queue fields go,
    // and AVVkFrame's `access` becomes VkAccessFlagBits2.
    for flag in ["ffmpeg_7_1", "ffmpeg_9_0"] {
        println!("cargo::rustc-check-cfg=cfg({flag})");
        let var = format!("DEP_FFMPEG_{}", flag.to_ascii_uppercase());
        if std::env::var(var).as_deref() == Ok("true") {
            println!("cargo::rustc-cfg={flag}");
        }
    }
}
