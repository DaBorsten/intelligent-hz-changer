import { readFileSync, writeFileSync } from "fs";

const arg = process.argv[2] ?? "patch";

const pkg = JSON.parse(readFileSync("package.json", "utf8"));
const current: string = pkg.version;
const [major, minor, patch] = current.split(".").map(Number);

let version: string;
if (arg === "major") {
  version = `${major + 1}.0.0`;
} else if (arg === "minor") {
  version = `${major}.${minor + 1}.0`;
} else if (arg === "patch") {
  version = `${major}.${minor}.${patch + 1}`;
} else {
  // explicit version like "1.2.3"
  version = arg;
}

pkg.version = version;
writeFileSync("package.json", JSON.stringify(pkg, null, 2) + "\n");

let cargo = readFileSync("src-tauri/Cargo.toml", "utf8");
cargo = cargo.replace(/^version = ".*"/m, `version = "${version}"`);
writeFileSync("src-tauri/Cargo.toml", cargo);

console.log(`Bumped ${current} → ${version}`);
