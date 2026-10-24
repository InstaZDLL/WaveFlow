// Injected into the app's page by the bench (`bench.ts`): the helpers the
// scenarios call through the inspector. Nothing here ships with the app.
// Every helper reads the DOM landmarks the views carry (`data-nav-item`,
// `data-sidebar-*`, `.fe-panel`…) and the React fibres, never a label, so
// the bench runs whatever the interface language.
(() => {
  if (window.__wfb) return "already";
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  /** The first committed fibre value that has `key` as a function. */
  function findContext(key) {
    const rootEl = document.getElementById("root");
    const k = Object.keys(rootEl).find((x) =>
      x.startsWith("__reactContainer$"),
    );
    const stack = [rootEl[k].stateNode.current];
    while (stack.length) {
      const f = stack.pop();
      if (!f) continue;
      const v = f.memoizedProps && f.memoizedProps.value;
      if (v && typeof v[key] === "function") return v;
      if (v && v.i18n && key === "changeLanguage") return v.i18n;
      if (f.child) stack.push(f.child);
      if (f.sibling) stack.push(f.sibling);
    }
    return null;
  }

  const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args);

  /** requestAnimationFrame intervals while `drive(frameIndex)` runs for
   *  `ms`: the main thread's frame cadence, the measure #928 used. */
  function frames(ms, drive) {
    return new Promise((resolve) => {
      const t = [];
      let last = performance.now();
      const start = last;
      let i = 0;
      function step(now) {
        t.push(now - last);
        last = now;
        if (drive) drive(i++);
        if (now - start < ms) requestAnimationFrame(step);
        else resolve(t.slice(2));
      }
      requestAnimationFrame(step);
    });
  }

  function stats(t) {
    const s = [...t].sort((a, b) => a - b);
    const q = (p) =>
      s[Math.min(s.length - 1, Math.floor(p * (s.length - 1)))] ?? 0;
    const sum = s.reduce((a, b) => a + b, 0);
    return {
      frames: s.length,
      fps: +(1000 / (sum / Math.max(1, s.length))).toFixed(1),
      p50: +q(0.5).toFixed(2),
      p95: +q(0.95).toFixed(2),
      p99: +q(0.99).toFixed(2),
      max: +(s[s.length - 1] ?? 0).toFixed(1),
      over20: s.filter((x) => x > 20).length,
      over50: s.filter((x) => x > 50).length,
    };
  }

  /** The sidebar's places, in order: the pinned group, then the library's
   *  facets. Index 0 is Home. */
  function navItems() {
    return [...document.querySelectorAll("aside [data-nav-item]")];
  }

  window.__wfb = {
    async setSkin(id) {
      const ctx = findContext("setSkinId");
      if (!ctx) return "no skin context";
      ctx.setSkinId(id);
      await sleep(1500);
      return document.documentElement.getAttribute("data-skin");
    },
    async english() {
      const i18n = findContext("changeLanguage");
      if (i18n) await i18n.changeLanguage("en");
      return i18n ? i18n.language : "no i18n";
    },
    /** Navigate by the visible label (English after `english()`). */
    async nav(label) {
      const norm = (s) => (s || "").replace(/\s+/g, " ").trim().toLowerCase();
      const all = [
        ...document.querySelectorAll("aside button, aside a, [role=tab]"),
      ];
      const hit = all.find(
        (el) =>
          norm(el.textContent).startsWith(norm(label)) ||
          norm(el.getAttribute("aria-label")) === norm(label),
      );
      if (!hit) return `no nav ${label}`;
      hit.click();
      await sleep(1500);
      return `nav ${label}`;
    },
    navItems: () => navItems().map((b) => b.textContent.trim()),
    /** Ids of the library's first tracks (the first track of each album
     *  carries synced lyrics). */
    async trackIds(n) {
      const resp = await invoke("list_tracks", {
        libraryId: null,
        orderBy: null,
        direction: null,
      });
      return resp.items.slice(0, n).map((t) => t.id);
    },
    async play(trackIds) {
      await invoke("player_set_volume", { value: 0 });
      await invoke("player_play_tracks", {
        sourceType: "library",
        sourceId: null,
        trackIds,
        startIndex: 0,
      });
      await sleep(1500);
      return document.documentElement.getAttribute("data-is-playing");
    },
    async idle(ms) {
      return stats(await frames(ms));
    },
    /** Scroll the page column (the one scroller the views share) for `ms`,
     *  `px` per frame, from the top. */
    async scroll(ms, px) {
      const m = document.querySelector("main");
      m.scrollTo(0, 0);
      await sleep(400);
      let y = 0;
      const t = await frames(ms, () => {
        y += px;
        m.scrollTo(0, y);
      });
      const reached = Math.round(m.scrollTop);
      m.scrollTo(0, 0);
      return { ...stats(t), scrolledPx: reached };
    },
    async openPanel(tabIndex) {
      if (!document.querySelector(".fe-panel")) {
        const b = [...document.querySelectorAll("footer button")].find((x) =>
          /queue|file d/i.test(x.getAttribute("aria-label") || ""),
        );
        if (b) b.click();
        await sleep(1000);
      }
      const tab = [...document.querySelectorAll(".fe-panel [role=tab]")][
        tabIndex
      ];
      if (tab) tab.click();
      await sleep(800);
      return !!document.querySelector(".fe-panel");
    },
    async closePanel() {
      const b = [...document.querySelectorAll(".fe-panel button")].find((x) =>
        /close|fermer/i.test(x.getAttribute("aria-label") || ""),
      );
      if (b) b.click();
      await sleep(600);
      return !document.querySelector(".fe-panel");
    },
    async immersive(ms) {
      const b = [...document.querySelectorAll("footer button")].find((x) =>
        /immersive|plein/i.test(x.getAttribute("aria-label") || ""),
      );
      if (!b) return { error: "no immersive button" };
      b.click();
      await sleep(1500);
      const r = stats(await frames(ms));
      document.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      );
      await sleep(1200);
      return { ...r, open: !document.querySelector("[data-immersive]") };
    },
    state() {
      const d = document.documentElement;
      return {
        skin: d.getAttribute("data-skin"),
        layout: d.getAttribute("data-layout"),
        safe: d.hasAttribute("data-webkitgtk-safe"),
        playing: d.getAttribute("data-is-playing"),
        vw: innerWidth,
        vh: innerHeight,
        dpr: devicePixelRatio,
        ua: navigator.userAgent,
        backdrops: [...document.querySelectorAll("*")].filter(
          (e) => getComputedStyle(e).backdropFilter !== "none",
        ).length,
        filters: [...document.querySelectorAll("*")].filter(
          (e) => getComputedStyle(e).filter !== "none",
        ).length,
        animations: document
          .getAnimations()
          .filter((a) => a.playState === "running").length,
        infinite: document
          .getAnimations()
          .filter(
            (a) =>
              a.playState === "running" &&
              a.effect &&
              a.effect.getTiming().iterations === Infinity,
          ).length,
        nodes: document.getElementsByTagName("*").length,
      };
    },
    invoke,
  };
  return "installed";
})();
