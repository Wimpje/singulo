// Runs from `npm version`: copies the new package.json version into Cargo.toml.
// tauri.conf.json reads its version from package.json directly.
import { readFileSync, writeFileSync } from "node:fs";

const version = JSON.parse(readFileSync("package.json", "utf8")).version;
const path = "src-tauri/Cargo.toml";
const cargo = readFileSync(path, "utf8");
const updated = cargo.replace(/^version = ".*"$/m, `version = "${version}"`);
if (updated === cargo && !cargo.includes(`version = "${version}"`)) {
  throw new Error(`Could not find the package version in ${path}.`);
}
writeFileSync(path, updated);
console.log(`Singulo version set to ${version}`);
