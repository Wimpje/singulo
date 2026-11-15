import { execFileSync } from "node:child_process";
import {
  copyFileSync, cpSync, mkdirSync, readFileSync, readdirSync,
  realpathSync, statSync, writeFileSync, chmodSync, existsSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { createHash } from "node:crypto";

const source = process.argv[2];
if (!source) throw new Error("Usage: npm run prepare:media -- /absolute/path/to/verified/media-tools");
const destination = resolve("src-tauri/resources/tools");
const names = ["ffmpeg", "ffprobe", "exiftool", "dcraw_emu", "heif-convert"];
const suffix = process.platform === "win32" ? ".exe" : "";
mkdirSync(join(destination, "lib"), { recursive: true });
mkdirSync(join(destination, "licenses"), { recursive: true });

const execute = (command, args) => execFileSync(command, args, { encoding: "utf8" });
const copied = new Map();
const records = [];

function copyNative(file, target, executable) {
  const real = realpathSync(file);
  copyFileSync(real, target);
  chmodSync(target, 0o755);
  copied.set(real, target);
  if (process.platform !== "darwin") return;
  const dependencies = execute("otool", ["-L", real]).split("\n").slice(1)
    .map((line) => line.trim().split(" (")[0]).filter((path) => path.startsWith("/opt/") || path.startsWith("/usr/local/") || path.startsWith(resolve(source)));
  for (const dependency of dependencies) {
    if (realpathSync(dependency) === real) continue;
    if (/libx26[45]|libSvtAv1Enc/i.test(dependency)) {
      throw new Error(`GPL encoder dependency ${dependency}. Supply a decoder-only, license-compatible build.`);
    }
    const name = basename(dependency);
    const output = join(destination, "lib", name);
    const canonical = realpathSync(dependency);
    const existing = copied.get(canonical);
    if (!existing) {
      if (existsSync(output)) throw new Error(`Ambiguous library name: ${name}. Start with a fresh media-tools bundle.`);
      copyNative(dependency, output, false);
    }
    execute("install_name_tool", ["-change", dependency,
      executable ? `@executable_path/lib/${name}` : `@loader_path/${name}`, target]);
  }
  if (!executable) execute("install_name_tool", ["-id", `@loader_path/${basename(target)}`, target]);
  execute("codesign", ["--force", "--sign", "-", target]);
}

for (const name of names) {
  const input = join(resolve(source), name + suffix);
  if (!existsSync(input)) throw new Error(`Missing verified tool: ${input}`);
  const output = join(destination, name + suffix);
  if (name === "exiftool" && process.platform === "darwin") {
    const prefix = dirname(realpathSync(input));
    const library = [join(prefix, "lib"), join(prefix, "../lib/perl5")]
      .find((path) => existsSync(join(path, "Image/ExifTool.pm")));
    if (!library) throw new Error("ExifTool needs its complete Perl library directory beside the supplied executable.");
    cpSync(library, join(destination, "lib"), { recursive: true, force: false });
    let launcher = readFileSync(input, "utf8");
    launcher = launcher.replace(/^#!.*$/m, "#!/usr/bin/perl");
    launcher = launcher.replace(/^unshift @INC, .*\/opt\/homebrew.*;$/gm, "");
    if (!launcher.includes('unshift @INC, $incDir')) {
      launcher = launcher.replace("    use Config;", "    use Config;\n    unshift @INC, $incDir;");
    }
    writeFileSync(output, launcher);
    chmodSync(output, 0o755);
  } else {
    copyNative(input, output, true);
  }
  const digest = createHash("sha256").update(readFileSync(output)).digest("hex");
  records.push({ name, sha256: digest });
}

if (process.platform === "win32") {
  for (const entry of readdirSync(source)) {
    const path = join(source, entry);
    if (entry.endsWith(".dll") || entry === "exiftool_files") cpSync(path, join(destination, entry), { recursive: true });
  }
}
const licenseSource = join(source, "licenses");
if (!existsSync(licenseSource) || !statSync(licenseSource).isDirectory()) {
  throw new Error("Provide licenses/ with notices, corresponding source archives and build instructions for every redistributed dependency.");
}
cpSync(licenseSource, join(destination, "licenses"), { recursive: true });
const configuration = execute(join(destination, `ffmpeg${suffix}`), ["-buildconf"]);
if (/--enable-gpl|--enable-nonfree/.test(configuration)) {
  throw new Error("Only a non-GPL, non-nonfree FFmpeg build is accepted for this bundle.");
}
for (const [name, args] of [
  ["ffmpeg", ["-version"]], ["ffprobe", ["-version"]],
  ["exiftool", ["-ver"]], ["heif-convert", ["--list-decoders"]],
]) execute(join(destination, name + suffix), args);
writeFileSync(join(destination, "manifest.json"), JSON.stringify({
  version: 1, platform: process.platform, architecture: process.arch,
  tools: records, licenseDirectory: "licenses",
}, null, 2));
console.log(`Verified tools prepared in ${destination}. Review licenses/ before redistribution.`);
