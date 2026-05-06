# Local patches applied to glib 0.18.5

This is **not** an unmodified copy of `glib` 0.18.5 from crates.io. The
release line `gtk-rs 0.18` is still required by Tauri 2's Linux GTK3
stack and cannot be moved to the `glib` 0.20 series, but several
soundness fixes that landed upstream after 0.18.5 are needed.

## RUSTSEC-2024-0429 — `VariantStrIter` unsound `Send` / `Sync`

Upstream advisory: <https://rustsec.org/advisories/RUSTSEC-2024-0429>.
Fixed upstream in `glib >= 0.20.0`.

`VariantStrIter` borrows from a `&Variant` and dereferences raw C
pointers. The original 0.18.5 release shipped `unsafe impl Send` and
`unsafe impl Sync` for the iterator, which is unsound: the underlying
GLib variant data is not guaranteed to be `Send` or `Sync` and sharing
the iterator across threads can race on the variant's reference count.

The fix is to **not** declare those impls. This vendored copy has the
auto-trait-only behaviour: no manual `Send`/`Sync` for `VariantStrIter`
or its sibling iterators in `src/variant_iter.rs`. Verify with:

```sh
grep -n 'impl.*Send.*for VariantStrIter\|impl.*Sync.*for VariantStrIter' \
    src/variant_iter.rs
# expected: no matches
```

When `tauri` itself moves to a glib release that includes the upstream
fix, drop this vendored copy and remove the `[patch.crates-io]` entry
in `src-tauri/Cargo.toml`.
