# smb2

[![Crates.io](https://img.shields.io/crates/v/smb2)](https://crates.io/crates/smb2)
[![docs.rs](https://img.shields.io/docsrs/smb2)](https://docs.rs/smb2)
[![CI](https://github.com/vdavid/smb2/actions/workflows/ci.yml/badge.svg)](https://github.com/vdavid/smb2/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/smb2)](https://github.com/vdavid/smb2/blob/main/LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.91-blue)](https://blog.rust-lang.org/2025/10/30/Rust-1.91.0/)

A pure-Rust SMB2/3 client library with pipelined I/O. No C dependencies, no FFI. Faster than native macOS SMB in all
operations: 1.3-5x faster on uploads, downloads, listings, and deletes.

I built this because I needed fast SMB access for [Cmdr](https://github.com/vdavid/cmdr) (my file manager), and the
existing Rust SMB options weren't good enough. The `smb` crate works fine for listing files but downloads are painfully
slow because it sends one read at a time. Native OS SMB clients pipeline their reads, and so does this library, and here
we have more control to reach even better speeds.

**Why this matters:**

- Cross-compile without system lib headaches (no `libsmbclient`, no `-sys` crates)
- Pipelined I/O by default, not as an afterthought
- Async, on tokio or smol (and, through smol, any other executor)
- Works anywhere Rust compiles

## What it does

- Connect to SMB2/3 shares using NTLM or Kerberos authentication
- List directories, read files, write files, delete, rename, stat, create directories
- Compound requests (CREATE+READ+CLOSE in 1 round-trip, 4-way write compounds, compound delete/rename/stat)
- Batch helpers for deleting, renaming, and stat-ing many files in one call
- Named streams, and macOS extended attributes stored the way macOS's own SMB client stores them, so Finder tags written
  here show in Finder and the other way round. Reading one attribute from many files overlaps on one connection and hands
  results over as they finish
- Setting a file's timestamps (any of them, leaving the rest alone) and its Hidden and Read-only attributes, and reads
  that hand out the file's dates and attributes at no extra cost, so a copy keeps its source's modification date and
  flags
- Junctions and symlinks on Windows shares: listings and `stat` say which entries are links (`entry.is_link()`,
  `ReparseTag`), so a recursive walk can skip them, and deleting (`delete_link`), renaming, or stat-ing one acts on
  the link itself, never on what it points to
- Pipelined I/O with sliding window for large file transfers
- SMB 2.x (HMAC-SHA256) and 3.x (AES-CMAC, AES-GMAC) signing, and encryption (AES-128/256-CCM/GCM). A response
  whose signature is missing or wrong, and a guest session offered in place of the account you signed in as, are
  refused with their own `ErrorKind` (`SignatureInvalid`, `GuestSessionRefused`), so you can tell them from a typo
- LZ4 compression
- Share enumeration (list shares on a server via IPC$ + srvsvc RPC)
- Streaming downloads and uploads with progress reporting and cancellation
- File watching (CHANGE_NOTIFY for live directory updates)
- Disk space queries (total, free, used)
- DFS: namespace roots (`\\domain\namespace`), links inside a share, and multi-target failover, all resolved
  transparently, plus `SmbClient::resolve_dfs_link` to ask where a path behind a link really lives. A referral signs in on
  a different server only when `ClientConfig::dfs_target_policy` allows it
- Reconnection after network failures
- Auto-flush on writes (data safety for family photos and company docs)
- Filenames carrying characters SMB2 forbids (`?`, `*`, `"`, `:`, `<`, `>`, `\`, `|`, a trailing space or period),
  mapped the way macOS does so a file is named the same here and in Finder

## Filenames SMB2 won't carry

SMB2 borrows its name syntax from Windows, so eight ASCII characters are illegal in a file or directory name --
`"`, `*`, `:`, `<`, `>`, `?`, `\`, and `|` are wildcards or separators in the protocol's own grammar -- along with the
control characters and a trailing space or period. A server asked to create such a name answers
`STATUS_OBJECT_NAME_INVALID` and there is nothing to retry.

This crate does what every SMB client carrying POSIX names has done since Services for Macintosh: substitutes each one
with a code point in the Unicode private-use area at U+F000 on the way out, and substitutes back on the way in. The
server stores an ordinary legal name and never knows, and **no server-side configuration is involved**.

```rust,no_run
# async fn example(client: &mut smb2::SmbClient, share: &mut smb2::Tree) -> Result<(), smb2::Error> {
// Just works. On the wire the name carries U+F025 where the `?` is, which is
// byte for byte what macOS Finder writes for the same file.
client.write_file(share, r#""how are you?".json"#, b"...").await?;

// And comes back the way you wrote it.
let entries = client.list_directory(share, "").await?;
assert!(entries.iter().any(|e| e.name == r#""how are you?".json"#));
# Ok(())
# }
```

It is always on and there is no switch: for a name with nothing to map -- almost every name -- encoding is the
identity, and for a name with something to map, the mapped form is the only form the server accepts. The table and the
two places the round trip is not exact are documented in the [`name`](https://docs.rs/smb2/latest/smb2/name/) module,
whose `encode_name` / `decode_name` / `encode_path` / `decode_path` are public if you want the literal wire form.

Two things to know:

- **`/` is the only path separator.** A `\` is an ordinary character in a name and becomes U+F026, which is the only
  way a file named `a\b` can be opened at all. Write `dir/file.txt`, never `dir\file.txt`.
- **No Unicode normalization.** macOS smbfs converts decomposed names to NFC before sending because its local
  filesystem stores NFD; a cross-platform crate has no such local form to convert from, and an NFD name is perfectly
  legal on the wire. If you want byte-for-byte Finder parity on macOS, normalize to NFC before calling.

## Limitations

Not yet supported:

- **Listing the namespaces a domain hosts**: a domain name legitimately has zero disk shares, and MS-DFSC has no
  operation that enumerates its namespaces — that lives in MS-DFSNM, a different protocol. Open a namespace by name
  and it works; there is no way to discover its name from here.
- **NetBIOS-only domain names**: a domain name DNS cannot resolve needs a DC referral against a domain-joined
  client's cache, which a client that isn't domain-joined never has.
- **DFS target failback**: uses the first reachable target and remembers it; no automatic failback to a preferred
  target that comes back up.
- **Multi-channel**: single TCP connection per client
- **QUIC/RDMA transport**: TCP only (covers ~99% of use cases)

If you need multi-channel or QUIC, check the [`smb`](https://crates.io/crates/smb) crate.

Not planned:

- Server implementation (this is a client library)
- SMB1 (deprecated, insecure)
- LZNT1 compression (LZ4 is supported; LZNT1 is legacy)

## Quick start

```rust
#[tokio::main]
async fn main() -> Result<(), smb2::Error> {
    let mut client = smb2::connect("192.168.1.100:445", "user", "pass").await?;

    // List shares
    let shares = client.list_shares().await?;
    for share in &shares {
        println!("{} - {}", share.name, share.comment);
    }

    // Connect to a share
    let mut share = client.connect_share("Documents").await?;

    // List files
    let entries = client.list_directory(&mut share, "projects/").await?;
    for entry in &entries {
        println!("{} ({} bytes)", entry.name, entry.size);
    }

    // Read a file
    let data = client.read_file(&mut share, "report.pdf").await?;
    std::fs::write("report.pdf", data)?;

    // Write a file
    let content = std::fs::read("local_file.txt")?;
    client.write_file(&share, "remote_file.txt", &content).await?;

    // Clean up
    client.disconnect_share(&share).await?;

    Ok(())
}
```

## Pipeline API

The pipeline runs a mixed list of operations through one call and hands back one result per operation. It is a convenience over calling the methods one at a time: operations run in order and do not overlap on the wire. For real concurrency, run operations on several connections at once.

```rust
use smb2::{Pipeline, Op, OpResult};

# async fn example(client: &mut smb2::SmbClient, share: &mut smb2::Tree) -> Result<(), smb2::Error> {
    let mut pipeline = Pipeline::new(client.connection_mut(), &share);

    let results = pipeline.execute(vec![
        Op::ReadFile("a.txt".into()),
        Op::ReadFile("b.txt".into()),
        Op::ListDirectory("docs/".into()),
        Op::Delete("temp.txt".into()),
    ]).await;

    for result in results {
        match result {
            OpResult::FileData { path, data } => println!("{}: {} bytes", path, data.len()),
            OpResult::DirEntries { path, entries } => println!("{}: {} entries", path, entries.len()),
            OpResult::Deleted { path } => println!("deleted {}", path),
            OpResult::Error { path, error } => eprintln!("{}: {}", path, error),
            other => println!("{:?}", other),
        }
    }
    # Ok(())
    #
}
```

For large file I/O, use the pipelined variants which fill the credit window:

```rust
# async fn example(client: &mut smb2::SmbClient, share: &mut smb2::Tree) -> Result<(), smb2::Error> {
    // Pipelined I/O with sliding window for large files
    let data = client.read_file_pipelined(&mut share, "big_file.iso").await?;
    client.write_file_pipelined(&mut share, "copy.iso", &data).await?;
    # Ok(())
    #
}
```

## Streaming I/O

For large files that don't fit in memory, use the streaming API. It hands you one chunk at a time, in file order, and
supports progress reporting and cancellation. Underneath, it keeps as many READs on the wire as the link can use: full
speed on a fast link, and on a slow one no more than about one chunk queued ahead of your other requests.

### Streaming download

```rust
use tokio::io::AsyncWriteExt;

# async fn example(client: &mut smb2::SmbClient, share: &smb2::Tree) -> Result<(), smb2::Error> {
    let mut download = client.download(&share, "big_video.mp4").await?;
    println!("Downloading {} bytes...", download.size());

    let mut file = tokio::fs::File::create("big_video.mp4").await?;
    while let Some(chunk) = download.next_chunk().await {
        let bytes = chunk?;
        file.write_all(&bytes).await?;
        println!("{:.1}%", download.progress().percent());
    }
    # Ok(())
    #
}
```

### Write with progress and cancellation

```rust
use std::ops::ControlFlow;

# async fn example(client: &mut smb2::SmbClient, share: &mut smb2::Tree) -> Result<(), smb2::Error> {
    let data = std::fs::read("big_file.bin")?;
    client.write_file_with_progress(&mut share, "remote.bin", &data, |progress| {
        println!("{:.1}%", progress.percent());
        ControlFlow::Continue(()) // return ControlFlow::Break(()) to cancel
    }).await?;
    # Ok(())
    #
}
```

All write methods (`write_file`, `write_file_pipelined`, `write_file_with_progress`) flush data to persistent storage
before closing the file handle.

### Server-side copy

When source and destination live on the same share, the server can copy the bytes between its own files directly. The
data never crosses the wire, so even a multi-gigabyte copy finishes in moments. Not every server supports it (older
Samba builds, some NAS firmware), so branch on `ErrorKind::Unsupported` to fall back to a read-then-write copy:

```rust
use smb2::ErrorKind;

# async fn example(client: &mut smb2::SmbClient, share: &mut smb2::Tree) -> Result<(), smb2::Error> {
match client.server_side_copy_file(share, "movie.mkv", "movie-backup.mkv").await {
    Ok(bytes) => println!("copied {bytes} bytes without touching the network"),
    Err(e) if e.kind() == ErrorKind::Unsupported => {
        let data = client.read_file(share, "movie.mkv").await?;
        client.write_file(share, "movie-backup.mkv", &data).await?;
    }
    Err(e) => return Err(e),
}
# Ok(())
# }
```

`server_side_copy_file_range` copies a byte range to a chosen destination offset (pair it with
`create_file_writer_at` to append after a copied prefix), and `request_resume_key` / `copy_chunks` are the low-level
primitives. See the runnable `server_side_copy` example.

## Diagnostics

`SmbClient::diagnostics()` captures a snapshot of the client's state for dashboards, MCP tools, log dumps, and tests
that want to assert on counter ticks rather than scrape logs.

The state captured:

- negotiated dialect
- credits
- in-flight requests,
- per-connection counters (bytes sent/received
- request outcomes
- protocol events)
- DFS cache
- sessions

```rust
# async fn example(client: &mut smb2::SmbClient) -> Result<(), smb2::Error> {
let diag = client.diagnostics();
println!("{}", diag);                                  // compact terminal view
assert!(diag.primary.metrics.responses_routed_ok > 0); // typed assertions

// Optional: with `--features serde`:
#[cfg(feature = "serde")]
println!("{}", serde_json::to_string_pretty(&diag).unwrap());
# Ok(())
# }
```

The snapshot is eventually consistent (each field is loaded independently), survives connection teardown, and is
cheap (a handful of atomic loads + short critical sections). Counter semantics and the four-way routing partition are
documented on `smb2::MetricsSnapshot`. The [`smb2::client::diagnostics`](https://docs.rs/smb2/latest/smb2/client/diagnostics/)
module docs cover the design.

Two runnable examples to see it in action:

```sh
# One-shot: connect, list a directory, dump the snapshot.
SMB2_PASS=secret cargo run -p smb2 --example diagnostics
SMB2_PASS=secret cargo run -p smb2 --example diagnostics --features serde -- --json

# Live: download N files in parallel, tail the snapshot every interval ms.
SMB2_PASS=secret SMB2_FILE=big.bin \
  cargo run -p smb2 --example diagnostics_live -- --parallel 7 --interval 200
```

Env vars used by both: `SMB2_HOST` (`host:port`), `SMB2_USER`, `SMB2_PASS`, `SMB2_SHARE`. The live example also takes
`SMB2_FILE`. Useful pairings: `RUST_LOG=smb2=info` for the lib's own log lines on top, or the slow Docker container
(`SMB2_HOST=127.0.0.1:10451`, see [tests/CLAUDE.md](tests/CLAUDE.md)) to watch the dumper tick while the wire is busy.

To consumers wanting live charts, MCP tools, or Prometheus integration build on top of `Diagnostics` / `MetricsSnapshot`
in their own crates: smb2 is a protocol library and it'll stay so. Presentation belongs to your app.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
smb2 = "0.39"
```

You'll also need an async runtime. smb2 runs on [tokio](https://github.com/tokio-rs/tokio) out of the box:

```toml
[dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

To run on [smol](https://github.com/smol-rs/smol) instead, turn the `tokio` feature off and `smol` on. smol brings its
own reactor and executor threads, so this also works under any other executor (`futures::executor::block_on`,
`pollster`, and so on):

```toml
[dependencies]
smb2 = { version = "0.39", default-features = false, features = ["smol"] }
smol = "2"
```

With both features on, each call runs on tokio when it's made inside a tokio runtime and on smol otherwise. smb2 still
uses tokio's channels and semaphores either way; they work on any executor.

### MSRV

smb2 supports the Rust releases from the last 12 months, currently Rust 1.91 or newer. Raising this minimum is a minor
version bump (like 0.36 to 0.37), and the changelog always says so. When one of smb2's dependencies ships a release
that needs a newer Rust than yours, Cargo's MSRV-aware resolver (on by default with `resolver = "3"` or edition 2024)
picks a version your Rust can build.

## API overview

### High-level API

For when you want to do one thing and get the result:

- `smb2::connect()`: Connect and authenticate (shorthand)
- `SmbClient::connect()`: Connect with full config
- `client.list_shares()`: List available shares
- `client.connect_share()`: Connect to a share
- `client.list_directory(&mut share, path)`: List a directory
- `client.open_directory_reader(&mut share, path)`: List a directory one server batch at a time, so a huge folder
  never sits in memory whole and you can stop early. `reader.next_batch()` is cancel-safe in a `select!`
- `client.read_file(&mut share, path)`: Download a file
- `client.write_file(&share, path, data)`: Upload a file
- `client.delete_file(&share, path)`: Delete a file
- `client.delete_files(&share, &paths)`: Delete many files, one result each
- `client.stat(&mut share, path)`: Get file metadata: size, attributes, and the creation, modification, access, and
  change times (`changed` moves on a metadata-only change that `modified` misses)
- `client.stat_with_identity(&mut share, path)`: The same, plus the file's `FileIdentity` (index number and volume serial) in the same round trip, to tell "the same file" from "a file with the same name". It matches the identity a `WriteCheckpoint` carries
- `client.stat_files(&share, &paths)`: Batch stat
- `client.list_streams(&mut share, path)`, `read_stream`, `write_stream`, `delete_stream`: Named streams
- `client.fs_attributes(&mut share)`: What the share's file system says about itself, including named-stream support
- `client.get_xattr(&mut share, path, name)`, `set_xattr`, `remove_xattr`, `list_xattrs`: macOS xattrs
- `client.get_xattr_many(&mut share, &paths, name, options)`, `set_xattr_many`: The same over many files, overlapped.
  `tree.get_xattr_each` hands results over as they finish
- `client.set_times(&share, path, FileTimes::new().set_modified(t))`: Set a file's timestamps, leaving the ones you
  don't name alone. On a writer you're about to finish, `writer.set_times(...)` instead, so its close can't restamp
  the file. `FileTimes::set_attributes(...)` sets Hidden or Read-only in the same frame
- `client.read_file_compound_with_info(&mut share, path)`: Read a small file plus its size, dates, and
  attributes, in the same round trip. Downloads and file readers carry the same (`download.info()`, `reader.info()`)
- `client.rename(&share, from, to)`: Rename a file
- `client.rename_with(&share, from, to, RenameOptions::new().replace_if_exists(true))`: Rename over an existing file
  in one step, so a "write a temp, then rename it over" save never leaves the name missing
- `client.rename_files(&mut share, &renames)`: Batch rename
- `client.create_directory(&share, path)`: Create a directory
- `client.delete_directory(&share, path)`: Remove a directory
- `client.delete_link(&share, path)`: Remove a junction or symlink, leaving its target alone
- `client.download(&share, path)`: Streaming download with progress (memory-efficient)
- `client.open_file_download(&share, path)`: The same download, holding the client for the open only, so you can spawn it and keep using the client
- `client.upload(&share, path, data)`: Streaming upload with progress
- `client.write_file_streamed(&mut share, path, callback)`: Write from a streaming source (memory-efficient, pipelined)
- `client.create_file_writer(&share, path)`: A writer you push chunks into at your own pace. `writer.flush_checkpoint()`
  makes everything so far durable without closing it and returns a `WriteCheckpoint` to persist (with the `serde` feature it's
  `Serialize` + `Deserialize`)
- `client.resume_file_writer(&share, path, &checkpoint)`: Carry on an interrupted upload after a crash or a reconnect.
  It proves the file is still the one the checkpoint names and still that long before writing anything, and refuses
  with `ErrorKind::ResumeRefused` otherwise (start over then)
- `client.watch(&share, path, recursive)`: Watch for file changes (CHANGE_NOTIFY). Re-issues its subscription every
  10 minutes (`Connection::set_long_poll_refresh`), so a server that quietly drops one heals itself
- `client.fs_info(&mut share)`: Disk space (total, free, used)
- `client.reconnect()`: Reconnect after network failure
- `client.session_id()`: The session to name in `ClientConfig::previous_session_id` when you build a new client to
  replace this one after a drop, so the server closes the files the old session still had open
- `client.credits()`: Current available credits
- `client.estimated_rtt()`: Round-trip time from negotiate
- `client.disconnect_share(&share)`: Disconnect from a share

Paths use `/` as their separator and are relative to the share root. A `\` is a character in a name, not a separator
(see "Filenames SMB2 won't carry" above).

### Pipeline API

For when you have many operations and want them fast:

- `Pipeline::new(conn, &share)`: Create a pipeline
- `pipeline.execute(ops)`: Run a batch of operations

### Low-level API

For advanced use cases, the underlying types are available:

- `Connection`: Message exchange, credit tracking
- `Session`: NTLM authentication, key derivation
- `Tree`: Share-level file operations (take `&mut Connection`)
- `NegotiatedParams`: Protocol parameters from negotiate

## Performance

Benchmarked against native macOS SMB (with F_NOCACHE to disable kernel page cache) and the `smb` crate on a QNAP NAS
over Gigabit LAN, SMB 3.1.1:

### Small files (100 × 100 KB)

| Operation | native | smb2  | Speedup         |
|-----------|--------|-------|-----------------|
| Upload    | 3.69s  | 1.91s | **1.9x faster** |
| List      | 47ms   | 21ms  | **2.2x faster** |
| Download  | 3.10s  | 617ms | **5.0x faster** |
| Delete    | 3.08s  | 1.03s | **3.0x faster** |

### Medium files (10 × 10 MB)

| Operation | native | smb2  | Speedup         |
|-----------|--------|-------|-----------------|
| Upload    | 1.66s  | 1.23s | **1.3x faster** |
| List      | 27ms   | 19ms  | **1.4x faster** |
| Download  | 4.00s  | 2.93s | **1.4x faster** |
| Delete    | 301ms  | 128ms | **2.4x faster** |

### Large files (3 × 50 MB)

| Operation | native | smb2  | Speedup         |
|-----------|--------|-------|-----------------|
| Upload    | 1.69s  | 1.56s | ~parity         |
| List      | 27ms   | 18ms  | **1.5x faster** |
| Download  | 5.62s  | 1.11s | **5.1x faster** |
| Delete    | 117ms  | 54ms  | **2.1x faster** |

Key optimizations: compound requests (CREATE+READ+CLOSE in 1 round-trip), pipelined I/O with sliding window and adaptive
chunk sizing, and 256-credit request for wide pipeline windows.

**Note:** Native macOS download benchmarks use F_NOCACHE to bypass the kernel page cache. Without this, cached native
reads appear ~20x faster because they skip the network entirely. F_NOCACHE gives a fair comparison of actual network I/O
performance.

The benchmark tool is included at `benchmarks/smb/`. Run with `cargo run -p smb-benchmark --release`.

## Comparison with existing libraries

### vs `smb` crate

The [`smb`](https://crates.io/crates/smb) crate supports multi-channel, QUIC, and RDMA transport. If you need those, use
it. For everything else, `smb2` is a better fit:

- **Compound + pipelined I/O**: `smb` sends one request at a time; smb2 uses compound requests and pipelined reads with
  sliding window
- **Auto-reconnect with durable handles**: survives Wi-Fi drops without restarting transfers
- **Comprehensive test suite**: `smb` has almost no tests
- **MIT OR Apache-2.0**: `smb` is MIT-only

I initially considered forking `smb`, but the architecture didn't support pipelining well, and adding it would have been
a near-complete rewrite anyway.

### vs `pavao`

[`pavao`](https://crates.io/crates/pavao) wraps `libsmbclient` via FFI. It works, but you need `libsmbclient` installed,
which means system package management, cross-compilation headaches, and all the fun that comes with C dependencies.

`smb2` is pure Rust. `cargo build` and done.

## Sister projects

- [smb2-cli](https://crates.io/crates/smb2-cli): a command-line client built on this crate, in the same repo under
  `crates/smb2-cli/`. Run `ls`, `stat`, `rm`, `rmdir`, `mv`, and file transfers against a share without mounting it,
  and spread a batch over N connections. Bulk deletes go 34/s through a macOS mount and 500/s here.
- [Cmdr](https://github.com/vdavid/cmdr): an AI-native file manager that uses `smb2` for SMB access.

## Implementation notes

- I used Claude extensively for this implementation. The code has a comprehensive test suite (unit tests with mock
  transport, property tests with proptest, integration tests against Docker Samba), and it works for my use case in
  Cmdr. If you distrust AI-generated code, that's fair, but please check the tests and decide for yourself.
- The protocol implementation is based on
  Microsoft's [MS-SMB2 spec](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/). I converted the
  relevant sections to Markdown so AI agents could work from them effectively. They aren't committed here; `AGENTS.md`
  § "Spec files" shows how to fetch them.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

MIT OR Apache-2.0, at your option.
