#!/usr/bin/env node
// Checks an updater payload's `.sig` the way the updater will: against the
// public key compiled into the app, and against the version the manifest
// announces (`requireSignedVersion`). Exits non-zero on any mismatch.
//
//   verify-updater-signature.mjs <payload> <payload.sig> <tauri.conf.json> <version>
//
// The signature is minisign's: an Ed25519 signature over the BLAKE2b-512 of
// the payload ("ED"), plus a second one over the first and the trusted
// comment, which is where `tauri build` / `signer sign --app-version` write
// `version:<x>`. `tauri signer` has no verify command, hence this.

import { createHash, createPublicKey, verify } from "node:crypto";
import fs from "node:fs";

const [payload, sigPath, confPath, version] = process.argv.slice(2);
if (!payload || !sigPath || !confPath || !version) {
  console.error("usage: verify-updater-signature.mjs <payload> <payload.sig> <tauri.conf.json> <version>");
  process.exit(2);
}

const fail = (m) => {
  console.error(`::error::${m}`);
  process.exit(1);
};
const text = (b64) => Buffer.from(b64.trim(), "base64").toString("utf8");

const conf = JSON.parse(fs.readFileSync(confPath, "utf8"));
const pubLines = text(conf.plugins.updater.pubkey).split(/\r?\n/);
const pub = Buffer.from(pubLines[1].trim(), "base64"); // "Ed" + key id (8) + key (32)
const sigLines = text(fs.readFileSync(sigPath, "utf8")).split(/\r?\n/);
const sig = Buffer.from(sigLines[1].trim(), "base64"); // alg (2) + key id (8) + signature (64)
const comment = sigLines[2].replace(/^trusted comment: /, "");
const globalSig = Buffer.from(sigLines[3].trim(), "base64");

if (pub.length !== 42 || sig.length !== 74 || globalSig.length !== 64) fail("malformed key or signature");
if (!pub.subarray(2, 10).equals(sig.subarray(2, 10))) fail("the signature was made with another key than the app's");
if (sig.subarray(0, 2).toString("latin1") !== "ED") fail("expected a prehashed (ED) signature");

const key = createPublicKey({
  key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), pub.subarray(10)]),
  format: "der",
  type: "spki",
});
const digest = createHash("blake2b512").update(fs.readFileSync(payload)).digest();
if (!verify(null, digest, key, sig.subarray(10))) fail(`${payload} does not match its signature`);
if (!verify(null, Buffer.concat([sig.subarray(10), Buffer.from(comment)]), key, globalSig)) {
  fail("the trusted comment is not covered by the signature");
}
const m = /(?:^|\s)version:(\S+)/.exec(comment);
if (!m || m[1] !== version) fail(`the signature names version '${m ? m[1] : "none"}', expected '${version}'`);
console.log(`[verify] ${payload}: signature valid for the app's public key, version ${version}`);
