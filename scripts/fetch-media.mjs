import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync,
  readFileSync, readdirSync, renameSync, rmSync, writeFileSync,
} from "node:fs";
import { availableParallelism } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const args = process.argv.slice(2);
if (args.length === 1 && args[0] === "--help") {
  console.log("Usage: npm run fetch:media -- [output-directory]\nBuilds native macOS or Windows x64 (MSYS2 UCRT64) tools from pinned sources. Default output: ./media-tools");
  process.exit(0);
}
if (args.length > 1 || args.some((arg) => arg.startsWith("-"))) {
  throw new Error("Usage: npm run fetch:media -- [output-directory]");
}
if (process.platform === "win32" && process.arch === "x64") {
  if (process.env.MSYSTEM !== "UCRT64") throw new Error("Run this command in the MSYS2 UCRT64 shell. See README for prerequisites.");
  execFileSync("bash", ["--noprofile", "--norc",
    join(root, "scripts/fetch-media-windows.sh").replaceAll("\\", "/"),
    resolve(args[0] ?? join(root, "media-tools")).replaceAll("\\", "/"),
  ], { stdio: "inherit" });
  process.exit(0);
}
if (process.platform !== "darwin" || !["arm64", "x64"].includes(process.arch)) {
  throw new Error("Automatic media-tool builds support native macOS arm64/x64 and Windows x64 (MSYS2 UCRT64).");
}
const output = resolve(args[0] ?? join(root, "media-tools"));
if (existsSync(output)) {
  throw new Error(`Output already exists: ${output}. Choose a new directory; existing bundles are never overwritten.`);
}

const packages = JSON.parse(readFileSync(join(root, "scripts/media-sources.json"), "utf8"))
  .filter((pkg) => !pkg.platform || pkg.platform === process.platform);

const architecture = process.arch === "arm64" ? "arm64" : "x86_64";
const jobs = String(Math.min(4, availableParallelism()));
for (const [command, options] of [
  ["/usr/bin/xcrun", ["--find", "clang"]],
  ["cmake", ["--version"]], ["pkg-config", ["--version"]],
]) execFileSync(command, options, { stdio: "inherit" });
if (execFileSync("/usr/bin/uname", ["-m"], { encoding: "utf8" }).trim() !== architecture) {
  throw new Error("Run with a native Node.js installation matching this Mac, not through Rosetta.");
}

const cache = join(root, ".media-build");
mkdirSync(cache, { recursive: true });
const work = mkdtempSync(join(cache, "build-"));
const bundle = join(work, "media-tools");
const prefix = join(work, "install");
const licenses = join(bundle, "licenses");
mkdirSync(join(licenses, "sources"), { recursive: true });
const sdk = execFileSync("/usr/bin/xcrun", ["--show-sdk-path"], { encoding: "utf8" }).trim();
const buildEnv = {
  CC: "/usr/bin/clang", CXX: "/usr/bin/clang++",
  SDKROOT: sdk, MACOSX_DEPLOYMENT_TARGET: "12.0",
  CFLAGS: `-O2 -arch ${architecture}`, CXXFLAGS: `-O2 -arch ${architecture}`,
  CPPFLAGS: `-I${prefix}/include`, LDFLAGS: `-L${prefix}/lib`,
  PKG_CONFIG_PATH: "", PKG_CONFIG_LIBDIR: join(prefix, "lib/pkgconfig"),
  ZLIB_CFLAGS: `-I${sdk}/usr/include`, ZLIB_LIBS: "-lz",
};
const env = { ...process.env, ...buildEnv };
const commands = [];
function run(command, options, cwd = work) {
  commands.push({ command, args: options, cwd });
  execFileSync(command, options, { cwd, env, stdio: "inherit" });
}
function capture(command, options) {
  return execFileSync(command, options, { env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
}
function cmake(name, options) {
  const build = join(work, `${name}-build`);
  run("cmake", [
    "-S", join(work, name), "-B", build, "-G", "Unix Makefiles",
    "-DCMAKE_BUILD_TYPE=Release", `-DCMAKE_INSTALL_PREFIX=${prefix}`,
    `-DCMAKE_PREFIX_PATH=${prefix}`, "-DCMAKE_IGNORE_PREFIX_PATH=/opt/homebrew;/usr/local",
    "-DCMAKE_FIND_USE_PACKAGE_REGISTRY=OFF", "-DCMAKE_FIND_USE_SYSTEM_PACKAGE_REGISTRY=OFF",
    `-DCMAKE_OSX_ARCHITECTURES=${architecture}`, "-DCMAKE_OSX_DEPLOYMENT_TARGET=12.0",
    "-DCMAKE_INSTALL_LIBDIR=lib", "-DBUILD_SHARED_LIBS=OFF", ...options,
  ]);
  run("cmake", ["--build", build, "--parallel", jobs]);
  run("cmake", ["--install", build]);
}
function copyTool(name, input = join(prefix, "bin", name)) {
  const target = join(bundle, name);
  copyFileSync(input, target);
  chmodSync(target, 0o755);
  if (name === "exiftool") return;
  const archs = capture("/usr/bin/lipo", ["-archs", target]).trim().split(/\s+/);
  if (archs.length !== 1 || archs[0] !== architecture) {
    throw new Error(`Unexpected architecture for ${name}: ${archs.join(" ")}`);
  }
  for (const line of capture("/usr/bin/otool", ["-L", target]).split("\n").slice(1)) {
    const dependency = line.trim().split(" (")[0];
    if (dependency && !dependency.startsWith("/usr/lib/") && !dependency.startsWith("/System/Library/")) {
      throw new Error(`${name} still needs a non-system library: ${dependency}`);
    }
  }
}

console.log(`Building macOS ${architecture} tools in ${work}; downloads are SHA-256 checked.`);
try {
  for (const pkg of packages) {
    const archive = join(cache, basename(new URL(pkg.url).pathname));
    if (!existsSync(archive)) {
      run("/usr/bin/curl", [
        "--fail", "--location", "--retry", "3", "--connect-timeout", "30",
        "--proto", "=https", "--proto-redir", "=https",
        "--output", `${archive}.part`, pkg.url,
      ]);
      renameSync(`${archive}.part`, archive);
    }
    const digest = createHash("sha256").update(readFileSync(archive)).digest("hex");
    if (digest !== pkg.sha256) throw new Error(`SHA-256 mismatch: ${archive}. Remove that archive and retry.`);
    const source = join(work, pkg.name);
    mkdirSync(source);
    run("/usr/bin/tar", ["-xf", archive, "-C", source, "--strip-components=1"]);
    copyFileSync(archive, join(licenses, "sources", basename(archive)));
    const notices = join(licenses, pkg.name);
    mkdirSync(notices);
    for (const entry of readdirSync(source, { withFileTypes: true })) {
      if (entry.isFile() && /^(copying|license|licence|notice|copyright|readme)/i.test(entry.name)) {
        copyFileSync(join(source, entry.name), join(notices, entry.name));
      }
    }
  }

  cmake("jpeg", ["-DENABLE_SHARED=OFF", "-DENABLE_STATIC=ON", "-DWITH_TURBOJPEG=OFF",
    `-DWITH_SIMD=${architecture === "arm64" ? "ON" : "OFF"}`]);
  cmake("libde265", ["-DENABLE_DECODER=OFF", "-DENABLE_ENCODER=OFF", "-DENABLE_SDL=OFF"]);
  cmake("libheif", [
    "-DBUILD_TESTING=OFF", "-DBUILD_DOCUMENTATION=OFF",
    "-DENABLE_PLUGIN_LOADING=OFF", "-DWITH_LIBDE265=ON", "-DWITH_LIBDE265_PLUGIN=OFF",
    "-DWITH_EXAMPLE_HEIF_VIEW=OFF", "-DWITH_EXAMPLE_HEIF_THUMB=OFF", "-DWITH_GDK_PIXBUF=OFF",
    "-DWITH_X265=OFF", "-DWITH_X264=OFF", "-DWITH_AOM_DECODER=OFF", "-DWITH_AOM_ENCODER=OFF",
    "-DWITH_OpenH264_DECODER=OFF", "-DWITH_OpenH264_ENCODER=OFF", "-DWITH_DAV1D=OFF",
    "-DWITH_SvtEnc=OFF", "-DWITH_RAV1E=OFF", "-DWITH_KVAZAAR=OFF", "-DWITH_UVG266=OFF",
    "-DWITH_VVDEC=OFF", "-DWITH_VVENC=OFF", "-DWITH_FFMPEG_DECODER=OFF",
    "-DWITH_OpenJPEG_DECODER=OFF", "-DWITH_OpenJPEG_ENCODER=OFF", "-DWITH_OPENJPH_ENCODER=OFF",
    "-DWITH_LIBSHARPYUV=OFF", "-DWITH_HEADER_COMPRESSION=OFF", "-DWITH_UNCOMPRESSED_CODEC=OFF",
    "-DCMAKE_DISABLE_FIND_PACKAGE_PNG=ON", "-DCMAKE_DISABLE_FIND_PACKAGE_TIFF=ON",
    "-DCMAKE_DISABLE_FIND_PACKAGE_WEBP=ON",
  ]);
  const raw = join(work, "libraw");
  run("./configure", [
    `--prefix=${prefix}`, "--disable-shared", "--enable-static",
    "--disable-openmp", "--disable-lcms", "--enable-jpeg", "--enable-zlib", "--enable-examples",
  ], raw);
  run("/usr/bin/make", ["-j", jobs], raw);
  run("/usr/bin/make", ["install"], raw);

  const ffmpeg = join(work, "ffmpeg");
  run("./configure", [
    `--prefix=${prefix}`, "--cc=/usr/bin/clang", "--cxx=/usr/bin/clang++",
    `--arch=${architecture}`, "--target-os=darwin",
    "--disable-autodetect", "--disable-gpl", "--disable-nonfree",
    "--disable-shared", "--enable-static", "--disable-ffplay", "--disable-doc",
    "--disable-debug", "--disable-x86asm", "--disable-network",
    "--enable-videotoolbox", "--enable-audiotoolbox", "--enable-zlib",
  ], ffmpeg);
  run("/usr/bin/make", ["-j", jobs], ffmpeg);
  run("/usr/bin/make", ["install"], ffmpeg);
  copyTool("ffmpeg");
  copyTool("ffprobe");
  copyTool("dcraw_emu");
  copyTool("heif-convert");
  copyTool("exiftool", join(work, "exiftool/exiftool"));
  cpSync(join(work, "exiftool/lib"), join(bundle, "lib"), { recursive: true });

  const config = capture(join(bundle, "ffmpeg"), ["-buildconf"]);
  if (/--enable-gpl|--enable-nonfree/.test(config)) throw new Error("Unexpected FFmpeg license configuration.");
  if (!capture(join(bundle, "ffmpeg"), ["-encoders"]).includes("h264_videotoolbox")) {
    throw new Error("FFmpeg is missing the required h264_videotoolbox preview encoder.");
  }
  run(join(bundle, "exiftool"), ["-ver"]);
  run(join(bundle, "heif-convert"), ["--list-decoders"]);
  run(join(bundle, "heif-convert"), [join(work, "libheif/examples/example.heic"), join(work, "decoded.jpg")]);
  run(join(bundle, "exiftool"), ["-ImageWidth", "-ImageHeight", join(work, "decoded.jpg")]);
  run(join(bundle, "ffmpeg"), [
    "-v", "error", "-f", "lavfi", "-i", "testsrc=size=64x64:rate=1",
    "-t", "1", "-c:v", "h264_videotoolbox", "-pix_fmt", "yuv420p", join(work, "test.mp4"),
  ]);
  run(join(bundle, "ffprobe"), ["-v", "error", "-show_streams", join(work, "test.mp4")]);
  run(join(bundle, "ffmpeg"), ["-v", "error", "-i", join(work, "test.mp4"), "-frames:v", "1", join(work, "frame.jpg")]);
  writeFileSync(join(licenses, "sources.json"), JSON.stringify(packages, null, 2));
  writeFileSync(join(licenses, "build-commands.json"), JSON.stringify({
    platform: process.platform, architecture, env: buildEnv, commands,
  }, null, 2));
  copyFileSync(fileURLToPath(import.meta.url), join(licenses, "fetch-media.mjs"));
  copyFileSync(join(root, "scripts/media-sources.json"), join(licenses, "media-sources.json"));
  writeFileSync(join(licenses, "BUILD.txt"), [
    "Built from the unmodified source archives in sources/ with fetch-media.mjs.",
    "Requires native macOS Node.js 22+, Xcode Command Line Tools, CMake and pkg-config.",
    "To rebuild in a project checkout: npm run fetch:media -- /new/output/directory",
    "Exact source URLs/hashes are in sources.json; commands are in build-commands.json.",
    "FFmpeg uses LGPL configuration, not GPL/nonfree; H.264 encoding uses Apple VideoToolbox.",
    "LibRaw uses its LGPL option. libheif/libde265 use LGPL; see their bundled license texts.",
    "JPEG uses libjpeg-turbo's applicable IJG/BSD/Zlib licenses; ExifTool uses its Perl terms.",
    "Keep these notices and matching sources with redistributed binaries. Review upstream terms.",
    "HEIC decoding is enabled; optional AVIF and other HEIF codec plugins are not included.",
    "",
  ].join("\n"));
  mkdirSync(dirname(output), { recursive: true });
  renameSync(bundle, output);
  rmSync(work, { recursive: true });
} catch (error) {
  console.error(`Media setup failed. Build files retained for diagnosis: ${work}`);
  throw error;
}
console.log(`Media tools ready in ${output}\nNext: npm run prepare:media -- ${JSON.stringify(output)}`);
