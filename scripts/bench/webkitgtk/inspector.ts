// A client for WebKitGTK's remote inspector (`WEBKIT_INSPECTOR_HTTP_SERVER`).
// The server lists its targets as HTML (`/socket/<connection>/<target>/
// WebPage`); the socket speaks the WebKit inspector protocol, multiplexed:
// the page announces itself with `Target.targetCreated`, commands go
// through `Target.sendMessageToTarget` and come back as
// `Target.dispatchMessageFromTarget`. `Runtime.evaluate` has no
// `awaitPromise` here, so an async expression parks its result on the page
// and is polled.

type Pending = (msg: { result?: any; error?: any }) => void;

export class Inspector {
  private ws!: WebSocket;
  private target: string | null = null;
  private id = 0;
  private pending = new Map<number, Pending>();
  private ready!: Promise<void>;

  /** The main window's page: `url` itself or its `index.html`, with the
   *  query the native side may add for the first paint
   *  (`index.html?renderer=software`, `?safe-mode=1`). The splash screen
   *  (`splash.html`) is another document; the mini-player and the lyrics
   *  overlay carry their own query. None of them is it. */
  static async socketFor(
    host: string,
    url = "tauri://localhost",
  ): Promise<string> {
    const html = await (await fetch(`http://${host}/`)).text();
    const rows = html.split("<tr>").slice(1);
    for (const row of rows) {
      const u = row.match(/class="targeturl">([^<]*)</)?.[1];
      const s = row.match(/(\/socket\/\d+\/\d+\/WebPage)/)?.[1];
      if (!u || !s || !u.startsWith(url)) continue;
      const rest = u.slice(url.length);
      if (
        /^(\/(index\.html)?)?(\?.*)?$/.test(rest) &&
        !/[?&](mini|lyrics)=1/.test(rest)
      )
        return `ws://${host}${s}`;
    }
    throw new Error(`no inspectable target at ${url}`);
  }

  async connect(socket: string) {
    this.ws = new WebSocket(socket);
    this.ready = new Promise((resolve, reject) => {
      this.ws.onerror = (e) => reject(e);
      // The app gone (a crash mid-scenario, or before the page announced
      // itself) closes the socket: the connection and every pending
      // command fail rather than waiting for ever.
      this.ws.onclose = () => {
        reject(new Error("inspector socket closed"));
        for (const cb of this.pending.values())
          cb({ error: { message: "inspector socket closed" } });
        this.pending.clear();
      };
      this.ws.onmessage = (ev) => {
        const msg = JSON.parse(String(ev.data));
        if (
          msg.method === "Target.targetCreated" &&
          msg.params.targetInfo.type === "page"
        ) {
          this.target = msg.params.targetInfo.targetId;
          resolve();
        } else if (msg.method === "Target.dispatchMessageFromTarget") {
          const inner = JSON.parse(msg.params.message);
          if (inner.id != null && this.pending.has(inner.id)) {
            this.pending.get(inner.id)!(inner);
            this.pending.delete(inner.id);
          }
        }
      };
    });
    // An error before the socket opens rejects `ready`: race the two.
    await Promise.race([
      new Promise<void>((r) => (this.ws.onopen = () => r())),
      this.ready,
    ]);
    await this.ready;
  }

  send(method: string, params: Record<string, unknown> = {}): Promise<any> {
    // A closed socket neither throws on `send` nor ever answers: refused
    // here, or a command sent after the app died would wait for ever.
    if (this.ws?.readyState !== WebSocket.OPEN)
      return Promise.reject(new Error(`${method}: inspector socket closed`));
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, (m) =>
        m.error
          ? reject(new Error(`${method}: ${JSON.stringify(m.error)}`))
          : resolve(m.result),
      );
      this.ws.send(
        JSON.stringify({
          id: 1_000_000 + id,
          method: "Target.sendMessageToTarget",
          params: {
            targetId: this.target,
            message: JSON.stringify({ id, method, params }),
          },
        }),
      );
    });
  }

  /** A synchronous expression's value. */
  async value(expression: string): Promise<any> {
    const r = await this.send("Runtime.evaluate", {
      expression,
      returnByValue: true,
    });
    if (r.wasThrown) throw new Error(`evaluate: ${JSON.stringify(r.result)}`);
    return r.result.value;
  }

  /** An expression that returns a promise, awaited on the page. */
  async run(expression: string, timeoutMs = 120_000): Promise<any> {
    const key = `r${Date.now()}${Math.random().toString(36).slice(2)}`;
    await this.value(
      `(window.__wfr ??= {}, Promise.resolve().then(() => (${expression})).then(` +
        `(v) => { window.__wfr[${JSON.stringify(key)}] = JSON.stringify({ ok: v }); },` +
        `(e) => { window.__wfr[${JSON.stringify(key)}] = JSON.stringify({ err: String(e && e.stack || e) }); }), 0)`,
    );
    const start = Date.now();
    while (Date.now() - start < timeoutMs) {
      const raw = await this.value(
        `window.__wfr && window.__wfr[${JSON.stringify(key)}]`,
      );
      if (raw) {
        await this.value(`delete window.__wfr[${JSON.stringify(key)}]`);
        const r = JSON.parse(raw);
        if (r.err) throw new Error(r.err);
        return r.ok;
      }
      await Bun.sleep(150);
    }
    throw new Error(`timed out: ${expression.slice(0, 80)}`);
  }

  /** A PNG of the viewport, as bytes. */
  async screenshot(): Promise<Buffer> {
    const { w, h } = await this.value(`({ w: innerWidth, h: innerHeight })`);
    const r = await this.send("Page.snapshotRect", {
      x: 0,
      y: 0,
      width: w,
      height: h,
      coordinateSystem: "Viewport",
    });
    return Buffer.from(String(r.dataURL).split(",")[1], "base64");
  }

  close() {
    this.ws.close();
  }
}

/** `value` as a JavaScript string literal safe to splice into an
 *  expression the page evaluates: JSON's quoting, plus the characters a
 *  script context reads differently (`<`, `>`, the line separators). */
export function jsString(value: string): string {
  return JSON.stringify(value).replace(
    /[<>\u2028\u2029]/g,
    (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, "0")}`,
  );
}
