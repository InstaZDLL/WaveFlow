# Local patches applied to smb2 0.39.0

This is **not** an unmodified copy of `smb2` 0.39.0 from crates.io. As published,
the crate implements SMB encryption and cannot reach it against a server that
requires it. Two lines change that, and `src-tauri/Cargo.toml` points at this copy
through `[patch.crates-io]`.

Base: `smb2-0.39.0.crate`, sha256
`fdf2efe6ebfe83abf0813ef2de818ed1095cb36a19e7b2f6d44b3226238b202f`.

## 1. Announce encryption in the negotiate request

`src/client/connection.rs`, `Connection::negotiate`: `Capabilities::ENCRYPTION` joins
the capabilities the client sends.

MS-SMB2 3.2.4.2.2.2 says a client that supports encryption SHOULD set
`SMB2_GLOBAL_CAP_ENCRYPTION`. The crate supports it and did not set it. Samba reads
that bit, and without it picks no cipher, even in SMB 3.1.1 with the encryption
context present. A server requiring encryption then refuses the session with
`STATUS_ACCESS_DENIED`, and a share requiring it refuses the tree connect the same
way. The comment in `session.rs` about Samba "sometimes" omitting the encryption
context describes this same bit.

## 2. Keep the signing key at 128 bits under AES-256

`src/crypto/kdf.rs`, `derive_session_keys`: the signing key is derived at 128 bits
instead of the cipher's length.

MS-SMB2 3.2.1.3 defines `Session.SigningKey` as 128 bits whatever the cipher; only
the encryption and decryption keys follow it. Deriving 256 bits made every session
on an AES-256 cipher fail with "AES-128-GMAC requires a 16-byte key". The first
patch is what exposes this against Samba: a server offering only AES-256 used to
pick no cipher at all, and it would have stopped connecting even when it does not
require encryption. The two patches go together.

The crate's test `derive_keys_smb3_1_1_256_bit` asserted the 32-byte signing key, so
it now asserts 16. With that, the crate's 1466 lib tests pass.

## Left out of this copy

The ten `CLAUDE.md` files under `src/`. They are upstream's notes for its own coding
tools, are not compiled, and would be read as instructions by a tool working in this
repository. Nothing else differs from the published crate:

```sh
# from a checkout, with the crate fetched: expected, the two files above and these notes
diff -r -x CLAUDE.md -x PATCHES.md ~/.cargo/registry/src/*/smb2-0.39.0 src-tauri/vendor/smb2-0.39.0
```

## What was measured

Against Samba 4.21.9 in a container, from a client built on this copy, watching the
frames through a proxy and asking Samba with `smbstatus`:

- Encryption required by the server: SMB 3.0 and 3.0.2 with AES-128-CCM; SMB 3.1.1
  with AES-128-GCM, AES-128-CCM, AES-256-GCM and AES-256-CCM; and with signing
  required on top. Required by one share: SMB 3.0 and 3.1.1. After sign-in, nothing
  but transform frames; no file name or audio bytes readable on the wire, while the
  same measure without encryption reads both.
  Files read through each of these are identical to the source, byte for byte, and no
  nonce repeated.
- No regression on SMB 3.1.1 with GMAC and CMAC, SMB 3.0, 3.0.2, 2.1 and 2.0.2.
- Refused, nothing sent in clear: a flipped ciphertext byte (the connection is torn
  down, and the copy cached after it is still identical), `ENCRYPT_DATA` cleared in the
  session setup response, the capability cleared from the negotiate request of a server
  that requires encryption or that is limited to SMB 3.1.1.
- A connection cut under an encrypted stream comes back with new keys, still encrypted.

Not measured: Windows Server, Kerberos. Under AES-256 with Kerberos, MS-SMB2 derives
the encryption keys from the full session key; that was not read or tried, and
WaveFlow only signs in with NTLM.

## Known limits, not patched

- **Downgrade of the negotiation.** The crate has the message for
  `FSCTL_VALIDATE_NEGOTIATE_INFO` and does not send it, so nothing checks the negotiation
  below SMB 3.1.1. An on-path attacker who rewrites the negotiate request can move a
  server off 3.1.1, whose preauthentication hash would catch it, as long as the server
  accepts an older dialect. Measured on Samba with its default dialects:
  - A server that only prefers encryption (`smb encrypt = desired`) ends up in clear,
    still signed, on SMB 3.0.2 or 2.1.
  - A server or a share that requires encryption stays encrypted on 3.0.2, or refuses.
  - A server limited to SMB 3.1.1 catches the rewrite.

  The published crate never encrypted on a server that only prefers it, and it has the
  same gap in the negotiation, so this is not a regression.
- **The server decides.** The client has no setting that demands encryption: a server
  that does not ask for it gets signed traffic in clear.
- **HMAC-SHA256 on SMB 3.1.1.** With a server restricted to it, the crate offers the
  algorithm in its signing context and then signs with CMAC, so the session setup
  signature does not match. It fails closed, the published crate does the same, and it
  has nothing to do with encryption. Reported upstream as
  [vdavid/smb2#33](https://github.com/vdavid/smb2/issues/33).

## When to drop this copy

As soon as a published `smb2` carries both changes, reported upstream as
[vdavid/smb2#32](https://github.com/vdavid/smb2/issues/32):

1. Check on that version that a server with `server smb encrypt = required` and
   `server smb3 encryption algorithms = AES-256-GCM` accepts the client.
2. Remove the `smb2` entry from `[patch.crates-io]` in `src-tauri/Cargo.toml` and this
   directory.
3. Bump the `smb2` pin in `crates/app/Cargo.toml`, and update `Cargo.lock`.
4. Regenerate the Flatpak sources (`packaging/flatpak/generate-sources.sh`), since the
   crate comes back as a registry source.
5. Drop the sentence about this copy in `docs/features/library.md`.
