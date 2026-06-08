# Third-Party Notices

WaveFlow is licensed under the GNU General Public License v3.0. Some parts of
the source tree are derived from, ported from, inspired by, or vendor patched
from third-party projects with compatible licenses.

## syncedlyrics

`src-tauri/crates/syncedlyrics` is a Rust port of the multi-provider lyrics
lookup behavior originally provided by the `syncedlyrics` Python project. Its
lineage is:

1. Upstream Python project by Momo — <https://github.com/moehmeni/syncedlyrics>
2. Forked and rewritten in Zig — <https://github.com/InstaZDLL/zig-syncedlyrics>
3. Ported to Rust as the `waveflow-syncedlyrics` crate for WaveFlow (this tree)

It is included in WaveFlow under GPL-3.0-only, with the original MIT notice
preserved below.

MIT License

Copyright (c) 2022 Momo
Copyright (c) 2026 InstaZDLL

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
