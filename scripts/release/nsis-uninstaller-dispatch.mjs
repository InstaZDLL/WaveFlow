#!/usr/bin/env node
// A Tauri `signCommand` that touches exactly one file: the NSIS uninstaller.
//
// SignPath cannot sign inside an NSIS installer, and the bundler asks its
// `signCommand` to sign, in turn, the main exe, the NSIS plugin DLLs, the
// uninstaller (a temp file `nst<hex>.tmp`, handed over by `!uninstfinalize`
// before it is embedded into the setup) and the finished setup. We sign the
// exe and the setup elsewhere, and the plugin DLLs are not ours, so every
// call but the uninstaller's is a no-op.
//
// Two modes, one per `tauri bundle` pass:
//   export  <workdir> <file>  copy the uninstaller out (workdir/uninstaller.exe)
//                             and record its SHA-256, to be submitted to SignPath
//   replace <workdir> <file>  overwrite the uninstaller with the signed copy
//                             (workdir/signed/uninstaller.exe), but only when
//                             its SHA-256 is the one exported: a signature on
//                             bytes the installer would not otherwise contain
//                             must never be embedded
//
// makensis writes the same uninstaller byte for byte whenever the installer
// script and its inputs do not change (measured: two runs, one SHA-256), and
// it does not embed the exe, so the signed copy exported by the first pass
// fits the second. A mismatch leaves the uninstaller unsigned and says so in
// `result.txt`; the workflow turns that into a warning, never a silent pass.

import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const [mode, workdir, file] = process.argv.slice(2);
if (!["export", "replace"].includes(mode) || !workdir || !file) {
  console.error("usage: nsis-uninstaller-dispatch.mjs <export|replace> <workdir> <file>");
  process.exit(2);
}

const isUninstaller = (p) => {
  if (!/^nst[0-9a-f]+\.tmp$/i.test(path.basename(p))) return false;
  const head = Buffer.alloc(2);
  const fd = fs.openSync(p, "r");
  try {
    fs.readSync(fd, head, 0, 2, 0);
  } finally {
    fs.closeSync(fd);
  }
  return head.toString("latin1") === "MZ";
};

if (!fs.existsSync(file) || !isUninstaller(file)) process.exit(0);

const sha = (p) => createHash("sha256").update(fs.readFileSync(p)).digest("hex");
const exported = path.join(workdir, "uninstaller.exe");
const shaFile = path.join(workdir, "uninstaller.sha256");
const result = path.join(workdir, "result.txt");
fs.mkdirSync(workdir, { recursive: true });

if (mode === "export") {
  fs.copyFileSync(file, exported);
  fs.writeFileSync(shaFile, sha(file) + "\n");
  console.log(`[uninstaller] exported, sha256 ${sha(file)}`);
} else {
  const signed = path.join(workdir, "signed", "uninstaller.exe");
  const expected = fs.existsSync(shaFile) ? fs.readFileSync(shaFile, "utf8").trim() : "";
  const actual = sha(file);
  if (!expected || !fs.existsSync(signed)) {
    fs.writeFileSync(result, "unsigned: nothing exported or no signed copy\n");
  } else if (actual !== expected) {
    fs.writeFileSync(result, `unsigned: uninstaller changed (${actual} != ${expected})\n`);
    console.warn("[uninstaller] differs from the exported one, left unsigned");
  } else {
    fs.copyFileSync(signed, file);
    fs.writeFileSync(result, "signed\n");
    console.log("[uninstaller] replaced with the signed copy");
  }
}
