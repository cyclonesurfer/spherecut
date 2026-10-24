#!/bin/sh
# Builds an FFmpeg release from its GitHub tag into a prefix of its own,
# for `cargo xtask compat` (D2):
#
#   scripts/build-ffmpeg.sh n8.1.3 ~/ffmpeg      # gives ~/ffmpeg/n8.1.3-full
#   cargo xtask compat ~/ffmpeg/n8.1.3-full --test ~/ffmpeg/n8.1.3-full
#
# n7.1.3 and n8.1.3 have been built with it on Ubuntu 24.04 (PROGRESS.md).
#
# The build has what Pat's Fedora FFmpeg uses (D2): VA-API and Vulkan
# with libdrm, libx264, libx265, libdav1d, libsvtav1, libopus and
# libvorbis, and libshaderc for the Vulkan filters. Shared libraries, no
# programs or docs. The configure line and logs are kept next to the
# prefix, and the source is deleted after.
#
# On Ubuntu 24.04 it needs:
#   sudo apt install build-essential git nasm pkg-config cmake libvulkan1 \
#     libva-dev libdrm-dev libx264-dev libx265-dev libdav1d-dev \
#     libsvtav1enc-dev libopus-dev libvorbis-dev libshaderc-dev
# None of these changes what the system FFmpeg's bindings are built from.
#
# FFmpeg 8.1's Vulkan needs headers of 1.3.277 or later (Ubuntu 24.04 has
# 1.3.275): where pkg-config has none, Khronos Vulkan-Headers v1.4.354 go
# into <dest>/vulkan-headers-v1.4.354, with a vulkan.pc over the system's
# loader (libvulkan.so.1), and only the FFmpeg build sees them. Their
# include directory is also passed with --extra-cflags: 7.1's configure
# test-compiles vulkan.h without the package's Cflags, and fails to find
# the vk_video headers it includes.
set -eu

if [ $# -ne 2 ]; then
    echo "usage: $0 <tag, such as n8.1.3> <destination directory>" >&2
    exit 2
fi
tag=$1
mkdir -p "$2"
dest=$(cd "$2" && pwd)
prefix=$dest/$tag-full
src=$dest/src-$tag
vk_tag=v1.4.354
vk=$dest/vulkan-headers-$vk_tag

if [ -e "$prefix" ]; then
    echo "$prefix exists: remove it to build again" >&2
    exit 1
fi

missing=
for lib in libva libva-drm libdrm x264 x265 dav1d SvtAv1Enc opus vorbis shaderc; do
    pkg-config --exists "$lib" || missing="$missing $lib"
done
if [ -n "$missing" ]; then
    echo "pkg-config finds no$missing: install the packages listed in $0" >&2
    exit 1
fi

vk_cflags=
if ! pkg-config --atleast-version=1.3.277 vulkan 2>/dev/null; then
    if [ ! -f "$vk/lib/pkgconfig/vulkan.pc" ]; then
        loader=$(ldconfig -p | awk '/libvulkan\.so\.1 .*x86-64/ { print $NF; exit }')
        if [ -z "$loader" ]; then
            echo "no Vulkan loader (libvulkan.so.1): install libvulkan1" >&2
            exit 1
        fi
        rm -rf "$src-vk"
        git -c advice.detachedHead=false clone -q --depth 1 --branch "$vk_tag" \
            https://github.com/KhronosGroup/Vulkan-Headers.git "$src-vk"
        cmake -S "$src-vk" -B "$src-vk/build" -DCMAKE_INSTALL_PREFIX="$vk" >/dev/null
        cmake --install "$src-vk/build" >/dev/null
        rm -rf "$src-vk"
        mkdir -p "$vk/lib/pkgconfig"
        ln -sf "$loader" "$vk/lib/libvulkan.so"
        cat >"$vk/lib/pkgconfig/vulkan.pc" <<EOF
prefix=$vk
includedir=\${prefix}/include
libdir=\${prefix}/lib

Name: Vulkan-Loader
Description: Khronos Vulkan-Headers $vk_tag over the system loader ($loader)
Version: ${vk_tag#v}
Libs: -L\${libdir} -lvulkan
Cflags: -I\${includedir}
EOF
    fi
    PKG_CONFIG_PATH=$vk/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}
    export PKG_CONFIG_PATH
    vk_cflags=-I$vk/include
fi

rm -rf "$src"
git -c advice.detachedHead=false clone -q --depth 1 --branch "$tag" https://github.com/FFmpeg/FFmpeg.git "$src"
set -- --prefix="$prefix" --enable-shared --disable-static --disable-programs \
    --disable-doc --enable-gpl --enable-vaapi --enable-libx264 --enable-libx265 \
    --enable-libdav1d --enable-vulkan --enable-libshaderc --enable-libdrm \
    --enable-libopus --enable-libvorbis --enable-libsvtav1
if [ -n "$vk_cflags" ]; then
    set -- "$@" --extra-cflags="$vk_cflags"
fi
echo "PKG_CONFIG_PATH=${PKG_CONFIG_PATH:-} ./configure $*" >"$prefix.configure-line"

# step NAME LOG COMMAND...: runs COMMAND in the source directory with its
# output in LOG, and if it fails, says so with the log's last 20 lines
# (the source is kept, for a look at what failed).
step() {
    step_name=$1
    step_log=$2
    shift 2
    echo "==> $step_name $tag (log: $step_log)"
    if ! (cd "$src" && "$@") >"$step_log" 2>&1; then
        echo "$step_name failed: see $step_log, which ends:" >&2
        tail -n 20 "$step_log" >&2
        exit 1
    fi
}
step configure "$prefix.configure.log" ./configure "$@"
step make "$prefix.make.log" make -j"$(nproc)"
step install "$prefix.install.log" make install
rm -rf "$src"
echo "==> FFmpeg $tag in $prefix: libavcodec $(PKG_CONFIG_PATH=$prefix/lib/pkgconfig pkg-config --modversion libavcodec)"
