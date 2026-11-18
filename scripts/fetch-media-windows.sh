#!/usr/bin/env bash
set -euo pipefail

if [[ "${MSYSTEM:-}" != UCRT64 ]] || [[ "$(uname -m)" != x86_64 ]]; then
  echo "Run npm run fetch:media in the MSYS2 UCRT64 shell on Windows x64." >&2
  exit 1
fi
for tool in node curl tar unzip sha256sum make gcc g++ cmake ninja pkg-config nasm objdump; do
  command -v "$tool" >/dev/null || { echo "Missing build tool: $tool. See README prerequisites." >&2; exit 1; }
done
if [[ "$(gcc -dumpmachine)" != x86_64-w64-mingw32 ]]; then
  echo "Expected the UCRT64 MinGW compiler, not MSYS GCC." >&2
  exit 1
fi
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="$(cygpath -au "$1")"
if [[ -e "$output" ]]; then
  echo "Output already exists: $output. Choose a new directory; existing bundles are never overwritten." >&2
  exit 1
fi
case "$root" in *" "*)
  echo "Build from a checkout without spaces in its path (upstream Makefiles require this)." >&2
  exit 1 ;;
esac
mkdir -p "$root/.media-build"
work="$(mktemp -d "$root/.media-build/windows-XXXXXX")"
trap 'echo "Media setup failed. Build files retained for diagnosis: $work" >&2' ERR
prefix="$work/install"
bundle="$work/media-tools"
licenses="$bundle/licenses"
mkdir -p "$licenses/sources"
jobs="$(nproc)"
if (( jobs > 4 )); then jobs=4; fi
export CC=gcc CXX=g++
export CFLAGS="-O2" CXXFLAGS="-O2"
export CPPFLAGS="-I$prefix/include" LDFLAGS="-L$prefix/lib -static -static-libgcc -static-libstdc++"
export PKG_CONFIG_PATH="" PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig"
unset ZLIB_CFLAGS ZLIB_LIBS

node -e '
  const fs = require("node:fs");
  const packages = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
  for (const p of packages.filter(p => !p.platform || p.platform === "win32"))
    console.log([p.name, p.url, p.sha256].join("\t"));
' "$(cygpath -am "$root/scripts/media-sources.json")" > "$work/downloads.tsv"

while IFS=$'\t' read -r name url sha; do
  archive="$root/.media-build/${url##*/}"
  if [[ ! -f "$archive" ]]; then
    curl --fail --location --retry 3 --connect-timeout 30 \
      --proto '=https' --proto-redir '=https' --output "$archive.part" "$url"
    mv "$archive.part" "$archive"
  fi
  printf '%s  %s\n' "$sha" "$archive" | sha256sum --check -
  mkdir "$work/$name"
  if [[ "$name" == exiftool-windows ]]; then
    unzip -q "$archive" -d "$work/$name"
  else
    tar -xf "$archive" -C "$work/$name" --strip-components=1
  fi
  cp "$archive" "$licenses/sources/"
  mkdir "$licenses/$name"
  find "$work/$name" -maxdepth 1 -type f \
    \( -iname 'copying*' -o -iname 'license*' -o -iname 'licence*' -o -iname 'notice*' -o -iname 'readme*' \) \
    -exec cp '{}' "$licenses/$name/" \;
done < "$work/downloads.tsv"

cmake_build() {
  local name="$1"
  shift
  cmake -S "$work/$name" -B "$work/$name-build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$prefix" -DCMAKE_PREFIX_PATH="$prefix" \
    -DCMAKE_FIND_USE_PACKAGE_REGISTRY=OFF -DCMAKE_FIND_USE_SYSTEM_PACKAGE_REGISTRY=OFF \
    -DCMAKE_INSTALL_LIBDIR=lib -DBUILD_SHARED_LIBS=OFF \
    '-DCMAKE_EXE_LINKER_FLAGS=-static -static-libgcc -static-libstdc++' "$@"
  cmake --build "$work/$name-build" --parallel "$jobs"
  cmake --install "$work/$name-build"
}
cmake_build zlib -DZLIB_BUILD_SHARED=OFF -DZLIB_BUILD_STATIC=ON -DZLIB_BUILD_TESTING=OFF
# FFmpeg's configure probes -lz; upstream CMake names the Windows static archive libzs.a.
cp "$prefix/lib/libzs.a" "$prefix/lib/libz.a"
cmake_build jpeg -DENABLE_SHARED=OFF -DENABLE_STATIC=ON -DWITH_TURBOJPEG=OFF -DWITH_SIMD=ON
cmake_build libde265 -DENABLE_DECODER=OFF -DENABLE_ENCODER=OFF -DENABLE_SDL=OFF
cmake_build libheif \
  '-DCMAKE_CXX_FLAGS=-DLIBDE265_STATIC_BUILD -DLIBHEIF_STATIC_BUILD' \
  -DBUILD_TESTING=OFF -DBUILD_DOCUMENTATION=OFF \
  -DENABLE_PLUGIN_LOADING=OFF -DWITH_LIBDE265=ON -DWITH_LIBDE265_PLUGIN=OFF \
  -DWITH_EXAMPLE_HEIF_VIEW=OFF -DWITH_EXAMPLE_HEIF_THUMB=OFF -DWITH_GDK_PIXBUF=OFF \
  -DWITH_X265=OFF -DWITH_X264=OFF -DWITH_AOM_DECODER=OFF -DWITH_AOM_ENCODER=OFF \
  -DWITH_OpenH264_DECODER=OFF -DWITH_OpenH264_ENCODER=OFF -DWITH_DAV1D=OFF \
  -DWITH_SvtEnc=OFF -DWITH_RAV1E=OFF -DWITH_KVAZAAR=OFF -DWITH_UVG266=OFF \
  -DWITH_VVDEC=OFF -DWITH_VVENC=OFF -DWITH_FFMPEG_DECODER=OFF \
  -DWITH_OpenJPEG_DECODER=OFF -DWITH_OpenJPEG_ENCODER=OFF -DWITH_OPENJPH_ENCODER=OFF \
  -DWITH_LIBSHARPYUV=OFF -DWITH_HEADER_COMPRESSION=OFF -DWITH_UNCOMPRESSED_CODEC=OFF \
  -DCMAKE_DISABLE_FIND_PACKAGE_PNG=ON -DCMAKE_DISABLE_FIND_PACKAGE_TIFF=ON \
  -DCMAKE_DISABLE_FIND_PACKAGE_WEBP=ON
(
  cd "$work/libraw"
  CPPFLAGS="$CPPFLAGS -DLIBRAW_NODLL" ./configure --prefix="$prefix" --host=x86_64-w64-mingw32 --disable-shared --enable-static \
    --disable-openmp --disable-lcms --enable-jpeg --enable-zlib --enable-examples
  # Skip the unused C samples, which can fail to link against static LibRaw.
  make -j "$jobs" bin/dcraw_emu.exe
  install -D -m 755 bin/dcraw_emu.exe "$prefix/bin/dcraw_emu.exe"
)
make -C "$work/openh264" -j "$jobs" OS=mingw_nt ARCH=x86_64 PREFIX="$prefix" install-static
(
  cd "$work/ffmpeg"
  ./configure --prefix="$prefix" --cc=gcc --cxx=g++ --arch=x86_64 --target-os=mingw32 \
    --disable-autodetect --disable-gpl --disable-nonfree --disable-shared --enable-static \
    --disable-ffplay --disable-doc --disable-debug --disable-network \
    --pkg-config-flags=--static --extra-cflags="$CPPFLAGS" --extra-ldflags="$LDFLAGS" \
    --enable-libopenh264 --enable-zlib
  make -j "$jobs"
  make install
)
for name in ffmpeg ffprobe dcraw_emu heif-convert; do
  cp "$prefix/bin/$name.exe" "$bundle/"
  objdump -f "$bundle/$name.exe" | grep -q 'file format pei-x86-64'
  while read -r dll; do
    case "${dll,,}" in
      kernel32.dll|user32.dll|gdi32.dll|advapi32.dll|shell32.dll|ole32.dll|oleaut32.dll|\
      ws2_32.dll|secur32.dll|bcrypt.dll|crypt32.dll|winmm.dll|version.dll|msvcrt.dll|\
      ucrtbase.dll|ntdll.dll|shlwapi.dll|psapi.dll|setupapi.dll|d3d11.dll|dxgi.dll|\
      mf.dll|mfplat.dll|mfuuid.dll|avrt.dll|strmiids.dll|vfw32.dll|oleacc.dll|\
      api-ms-win-*.dll|ext-ms-win-*.dll) ;;
      *) echo "$name.exe needs an unexpected DLL: $dll. Refusing an incomplete bundle." >&2; exit 1 ;;
    esac
  done < <(objdump -p "$bundle/$name.exe" | sed -n 's/^[[:space:]]*DLL Name: //p')
done
mapfile -t exiftool_launchers < <(find "$work/exiftool-windows" -type f -name 'exiftool(-k).exe')
if (( ${#exiftool_launchers[@]} != 1 )); then
  echo "Expected exactly one launcher in the upstream Windows ExifTool archive." >&2; exit 1
fi
cp "${exiftool_launchers[0]}" "$bundle/exiftool.exe"
cp -R "$(dirname "${exiftool_launchers[0]}")/exiftool_files" "$bundle/"

"$bundle/ffmpeg.exe" -buildconf > "$work/ffmpeg-config.txt" 2>&1
if grep -Eq -- '--enable-gpl|--enable-nonfree' "$work/ffmpeg-config.txt"; then
  echo "Unexpected FFmpeg license configuration." >&2; exit 1
fi
"$bundle/ffmpeg.exe" -encoders > "$work/ffmpeg-encoders.txt" 2>&1
grep -q libopenh264 "$work/ffmpeg-encoders.txt"
"$bundle/exiftool.exe" -ver
"$bundle/heif-convert.exe" --list-decoders
"$bundle/heif-convert.exe" "$work/libheif/examples/example.heic" "$work/decoded.jpg"
shopt -s nullglob
decoded=("$work"/decoded*.jpg)
shopt -u nullglob
if (( ${#decoded[@]} == 0 )); then
  echo "HEIC smoke test did not produce any JPEGs." >&2; exit 1
fi
"$bundle/exiftool.exe" -ImageWidth -ImageHeight "${decoded[@]}"
"$bundle/ffmpeg.exe" -v error -f lavfi -i testsrc=size=64x64:rate=1 -t 1 \
  -c:v libopenh264 -pix_fmt yuv420p "$work/test.mp4"
"$bundle/ffprobe.exe" -v error -show_streams "$work/test.mp4"
"$bundle/ffmpeg.exe" -v error -i "$work/test.mp4" -frames:v 1 "$work/frame.jpg"
cp "$root/scripts/media-sources.json" "$root/scripts/fetch-media-windows.sh" "$licenses/"
cat > "$licenses/BUILD.txt" <<'EOF'
Built from pinned, unmodified upstream sources with fetch-media-windows.sh.
ExifTool is the upstream Windows x64 distribution, including exiftool_files.
Requires Windows x64, MSYS2 UCRT64 and the packages documented in README.
Rebuild in a project checkout: npm run fetch:media -- C:/new/output/directory
See media-sources.json for exact upstream URLs and SHA-256 checksums.
Source archives, ExifTool distribution and upstream notices are included.
FFmpeg uses LGPL configuration; OpenH264 uses BSD; LibRaw uses its LGPL option.
libheif/libde265 use LGPL; JPEG/Zlib and ExifTool have their upstream terms.
Static GCC runtime linking uses the GCC Runtime Library Exception.
Review the bundled upstream terms before redistributing binaries.
HEIC decoding is enabled; optional AVIF/other HEIF codec plugins are not included.
EOF
mkdir -p "$(dirname "$output")"
mv -T -n "$bundle" "$output"
if [[ -d "$bundle" ]]; then
  echo "Output appeared during build; existing bundle left untouched: $output" >&2; exit 1
fi
rm -r -- "$work"
trap - ERR
printf 'Media tools ready in %s\nNext: npm run prepare:media -- "%s"\n' "$output" "$output"
