import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const semanticVersionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

const readFile = (relativePath) => fs.readFileSync(path.join(root, relativePath), "utf8");

const readJsonVersion = (relativePath) => {
  const parsed = JSON.parse(readFile(relativePath));
  return parsed.version;
};

const readCargoPackageVersion = () => {
  const cargoToml = readFile("src-tauri/Cargo.toml");
  const packageBlock = cargoToml.match(/^\[package\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m)?.[1];
  const version = packageBlock?.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];

  if (!version) {
    throw new Error("Could not find the package version in src-tauri/Cargo.toml");
  }

  return version;
};

const readCargoLockVersion = () => {
  const packageBlocks = readFile("src-tauri/Cargo.lock")
    .split(/^\[\[package\]\]\s*$/m)
    .slice(1);
  const matchingBlocks = packageBlocks.filter((block) =>
    /^name\s*=\s*"codex-switcher"\s*$/m.test(block)
  );

  if (matchingBlocks.length !== 1) {
    throw new Error(
      `Expected one codex-switcher package in src-tauri/Cargo.lock, found ${matchingBlocks.length}`
    );
  }

  const version = matchingBlocks[0].match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version) {
    throw new Error("Could not find the codex-switcher version in src-tauri/Cargo.lock");
  }

  return version;
};

const versions = new Map([
  ["package.json", readJsonVersion("package.json")],
  ["src-tauri/tauri.conf.json", readJsonVersion("src-tauri/tauri.conf.json")],
  ["src-tauri/Cargo.toml", readCargoPackageVersion()],
  ["src-tauri/Cargo.lock", readCargoLockVersion()],
]);

for (const [source, version] of versions) {
  if (typeof version !== "string" || !semanticVersionPattern.test(version)) {
    throw new Error(`${source} contains an invalid semantic version: ${String(version)}`);
  }
}

const uniqueVersions = new Set(versions.values());
if (uniqueVersions.size !== 1) {
  const details = [...versions].map(([source, version]) => `${source}=${version}`).join(", ");
  throw new Error(`Application versions do not match: ${details}`);
}

const version = versions.get("package.json");
const expectedTag = process.argv[2] ?? process.env.EXPECTED_TAG;
if (expectedTag && expectedTag !== `v${version}`) {
  throw new Error(`Release tag ${expectedTag} does not match application version ${version}`);
}

console.log(
  expectedTag
    ? `Version check passed: ${version} (${expectedTag})`
    : `Version check passed: ${version}`
);
