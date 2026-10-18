/**
 * Fluid Editorial prototype — fixture generator.
 *
 * Reads a real WaveFlow profile database (read-only, from a temp copy so
 * the running app's WAL is never touched), picks a representative slice
 * of the library and writes:
 *
 *   - `prototypes/fluid-editorial/covers/<hash>.<ext>` — album art is
 *     the user's own files, so it lives in the gitignored `prototypes/`
 *     folder, outside `public/`, and a dev-only Vite middleware serves
 *     it at `/fluid-proto/covers/…` (see `vite.config.ts`). A production
 *     build never sees it.
 *   - `src/prototype/fluid-editorial/fixtures.generated.json`
 *     (titles, artists, play counts, a 3-stop palette per cover — safe
 *     to commit, lets the prototype render gradients when the covers
 *     are missing on another machine)
 *
 * Usage:
 *   bun run scripts/fluid-proto-fixtures.ts [profile-dir]
 *
 * `profile-dir` defaults to `%APPDATA%/app.waveflow/waveflow/profiles/2`.
 * Prototype-only tooling: nothing in the app imports this file.
 */
import { Database } from "bun:sqlite";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import sharp from "sharp";

const appData = process.env.APPDATA ?? join(process.env.HOME ?? "", ".config");
const profileDir = resolve(
  process.argv[2] ?? join(appData, "app.waveflow", "waveflow", "profiles", "2"),
);
const root = resolve(import.meta.dir, "..");
const coversOut = join(root, "prototypes", "fluid-editorial", "covers");
const fixturesOut = join(
  root,
  "src",
  "prototype",
  "fluid-editorial",
  "fixtures.generated.json",
);

// ── 1. Snapshot the DB so a live WAL is never opened in place ─────────
const snap = join(tmpdir(), `waveflow-fluid-proto-${process.pid}`);
mkdirSync(snap, { recursive: true });
for (const f of ["data.db", "data.db-wal", "data.db-shm"]) {
  const src = join(profileDir, f);
  if (existsSync(src)) copyFileSync(src, join(snap, f));
}
const db = new Database(join(snap, "data.db"), { readonly: true });

interface AlbumRow {
  id: number;
  title: string;
  artist: string | null;
  year: number | null;
  hash: string | null;
  format: string | null;
  track_count: number;
  plays: number;
  last_played: number | null;
  added_at: number;
  genre: string | null;
}

const albumSql = `
  SELECT al.id, al.title,
         COALESCE(al.album_artist, ar.name) AS artist,
         al.year, aw.hash, aw.format,
         COUNT(DISTINCT t.id) AS track_count,
         COUNT(pe.id) AS plays,
         MAX(pe.played_at) AS last_played,
         MIN(t.added_at) AS added_at,
         (SELECT g.name FROM track_genre tg JOIN genre g ON g.id = tg.genre_id
            WHERE tg.track_id IN (SELECT id FROM track WHERE album_id = al.id)
            GROUP BY g.name ORDER BY COUNT(*) DESC LIMIT 1) AS genre
  FROM album al
  LEFT JOIN artist ar ON ar.id = al.artist_id
  LEFT JOIN artwork aw ON aw.id = al.artwork_id
  JOIN track t ON t.album_id = al.id AND t.is_available = 1
  LEFT JOIN play_event pe ON pe.track_id = t.id
  WHERE aw.hash IS NOT NULL
  GROUP BY al.id
`;

const topAlbums = db
  .query(
    `${albumSql} HAVING plays > 0 ORDER BY plays DESC, last_played DESC LIMIT 18`,
  )
  .all() as AlbumRow[];
const recentAlbums = db
  .query(`${albumSql} ORDER BY added_at DESC LIMIT 10`)
  .all() as AlbumRow[];
const lastPlayedAlbums = db
  .query(
    `${albumSql} HAVING last_played IS NOT NULL ORDER BY last_played DESC LIMIT 14`,
  )
  .all() as AlbumRow[];

interface PlayRow {
  track_id: number;
  title: string;
  artist: string | null;
  album_id: number | null;
  album: string | null;
  duration_ms: number;
  played_at: number;
  hash: string | null;
  format: string | null;
  codec: string | null;
  sample_rate: number | null;
  bit_depth: number | null;
  bpm: number | null;
  energy: number | null;
}

const recentPlays = db
  .query(
    `SELECT t.id AS track_id, t.title, ar.name AS artist, t.album_id, al.title AS album,
            t.duration_ms, MAX(pe.played_at) AS played_at, aw.hash, aw.format,
            t.codec, t.sample_rate, t.bit_depth, ta.bpm, ta.energy
       FROM play_event pe
       JOIN track t ON t.id = pe.track_id
       LEFT JOIN artist ar ON ar.id = t.primary_artist
       LEFT JOIN album al ON al.id = t.album_id
       LEFT JOIN artwork aw ON aw.id = al.artwork_id
       LEFT JOIN track_analysis ta ON ta.track_id = t.id
      WHERE t.is_available = 1
      GROUP BY t.id
      ORDER BY played_at DESC
      LIMIT 16`,
  )
  .all() as PlayRow[];

const stats = {
  tracks: (
    db.query("SELECT COUNT(*) c FROM track WHERE is_available = 1").get() as {
      c: number;
    }
  ).c,
  albums: (db.query("SELECT COUNT(*) c FROM album").get() as { c: number }).c,
  artists: (db.query("SELECT COUNT(*) c FROM artist").get() as { c: number }).c,
  liked: (db.query("SELECT COUNT(*) c FROM liked_track").get() as { c: number })
    .c,
  plays: (db.query("SELECT COUNT(*) c FROM play_event").get() as { c: number })
    .c,
  playlists: (
    db.query("SELECT COUNT(*) c FROM playlist WHERE is_smart = 0").get() as {
      c: number;
    }
  ).c,
  analysed: (
    db
      .query("SELECT COUNT(*) c FROM track_analysis WHERE bpm IS NOT NULL")
      .get() as { c: number }
  ).c,
  listened_ms: (
    db.query("SELECT COALESCE(SUM(listened_ms),0) s FROM play_event").get() as {
      s: number;
    }
  ).s,
};

// Mood gates mirror `mood_radio` (tempo windows; Sleep adds a loudness cap).
const moodCounts = db
  .query(
    `SELECT
       SUM(CASE WHEN bpm BETWEEN 60 AND 100 THEN 1 ELSE 0 END) AS focus,
       SUM(CASE WHEN bpm BETWEEN 70 AND 110 THEN 1 ELSE 0 END) AS chill,
       SUM(CASE WHEN bpm BETWEEN 120 AND 170 THEN 1 ELSE 0 END) AS workout,
       SUM(CASE WHEN bpm BETWEEN 110 AND 135 THEN 1 ELSE 0 END) AS party,
       SUM(CASE WHEN bpm < 80 AND (loudness_lufs IS NULL OR loudness_lufs < -14) THEN 1 ELSE 0 END) AS sleep
     FROM track_analysis WHERE bpm IS NOT NULL`,
  )
  .get() as Record<string, number>;

interface PlaylistRow {
  id: number;
  name: string;
  is_smart: number;
  smart_rules: string | null;
  color_id: string;
  icon_id: string;
  track_count: number;
}
const playlists = db
  .query(
    `SELECT p.id, p.name, p.is_smart, p.smart_rules, p.color_id, p.icon_id,
            (SELECT COUNT(*) FROM playlist_track pt WHERE pt.playlist_id = p.id) AS track_count
       FROM playlist p ORDER BY p.is_smart, p.position, p.id LIMIT 14`,
  )
  .all() as PlaylistRow[];

// Four covers per playlist, from the albums of its first tracks.
const playlistCovers = db.query(
  `SELECT DISTINCT aw.hash, aw.format
     FROM playlist_track pt
     JOIN track t ON t.id = pt.track_id
     JOIN album al ON al.id = t.album_id
     JOIN artwork aw ON aw.id = al.artwork_id
    WHERE pt.playlist_id = ?
    ORDER BY pt.position LIMIT 4`,
);

// ── 2. Copy the covers + compute a palette per cover ──────────────────
rmSync(coversOut, { recursive: true, force: true });
mkdirSync(coversOut, { recursive: true });

const palettes = new Map<string, string[]>();
// The album lists overlap (a record can be both "top" and "last
// played"), and they are mapped with `Promise.all`: without this, two
// callers would resize the same file at once and compute its palette
// twice. One promise per cover, whoever asks first.
const coverJobs = new Map<string, Promise<string | null>>();
function cover(hash: string | null, format: string | null) {
  if (!hash || !format) return Promise.resolve<string | null>(null);
  const file = `${hash}.${format}`;
  let job = coverJobs.get(file);
  if (!job) {
    job = coverOnce(hash, file);
    coverJobs.set(file, job);
  }
  return job;
}
async function coverOnce(hash: string, file: string) {
  const src = join(profileDir, "artwork", file);
  if (!existsSync(src)) return null;
  // Resized on the way in: the originals are up to 1 MB each and the
  // prototype draws nothing larger than ~400 px.
  const dst = join(coversOut, file);
  if (!existsSync(dst)) {
    await sharp(src)
      .resize(640, 640, { fit: "inside", withoutEnlargement: true })
      .jpeg({ quality: 84, mozjpeg: true })
      .toFile(dst);
  }
  if (!palettes.has(hash)) palettes.set(hash, await palette(src));
  return `/fluid-proto/covers/${file}`;
}

/** Three stops: a muted dominant, a deeper shade, a lighter accent. */
async function palette(file: string): Promise<string[]> {
  const { data } = await sharp(file)
    .resize(24, 24, { fit: "cover" })
    .removeAlpha()
    .raw()
    .toBuffer({ resolveWithObject: true });
  let r = 0,
    g = 0,
    b = 0,
    n = 0;
  let vr = 0,
    vg = 0,
    vb = 0,
    vs = 0;
  for (let i = 0; i < data.length; i += 3) {
    const pr = data[i],
      pg = data[i + 1],
      pb = data[i + 2];
    const max = Math.max(pr, pg, pb),
      min = Math.min(pr, pg, pb);
    if (max - min < 16 && (max < 40 || max > 220)) continue;
    r += pr;
    g += pg;
    b += pb;
    n++;
    const sat = max === 0 ? 0 : (max - min) / max;
    const w = sat * (max / 255);
    vr += pr * w;
    vg += pg * w;
    vb += pb * w;
    vs += w;
  }
  if (n === 0) return ["#3f3f46", "#18181b", "#71717a"];
  const avg = [r / n, g / n, b / n].map(Math.round);
  const vib = vs > 0 ? [vr / vs, vg / vs, vb / vs].map(Math.round) : avg;
  const hex = (c: number[]) =>
    "#" +
    c
      .map((v) => Math.max(0, Math.min(255, v)).toString(16).padStart(2, "0"))
      .join("");
  return [hex(avg), hex(avg.map((v) => Math.round(v * 0.45))), hex(vib)];
}

async function mapAlbum(a: AlbumRow) {
  const art = await cover(a.hash, a.format);
  return {
    id: a.id,
    title: a.title,
    artist: a.artist ?? "Artiste inconnu",
    year: a.year,
    trackCount: a.track_count,
    plays: a.plays,
    lastPlayed: a.last_played,
    addedAt: a.added_at,
    genre: a.genre,
    cover: art,
    palette: a.hash ? (palettes.get(a.hash) ?? null) : null,
  };
}

const fixtures = {
  generatedAt: new Date().toISOString(),
  stats,
  moodCounts,
  lastPlayed: await Promise.all(lastPlayedAlbums.map(mapAlbum)),
  topAlbums: await Promise.all(topAlbums.map(mapAlbum)),
  recentAlbums: await Promise.all(recentAlbums.map(mapAlbum)),
  recentPlays: await Promise.all(
    recentPlays.map(async (p) => ({
      trackId: p.track_id,
      title: p.title,
      artist: p.artist ?? "Artiste inconnu",
      albumId: p.album_id,
      album: p.album,
      durationMs: p.duration_ms,
      playedAt: p.played_at,
      codec: p.codec,
      sampleRate: p.sample_rate,
      bitDepth: p.bit_depth,
      bpm: p.bpm,
      energy: p.energy,
      cover: await cover(p.hash, p.format),
      palette: p.hash ? (palettes.get(p.hash) ?? null) : null,
    })),
  ),
  playlists: await Promise.all(
    playlists.map(async (p) => {
      const rows = playlistCovers.all(p.id) as {
        hash: string;
        format: string;
      }[];
      const covers = (
        await Promise.all(rows.map((r) => cover(r.hash, r.format)))
      ).filter((c): c is string => c != null);
      return {
        id: p.id,
        name: p.name,
        isSmart: p.is_smart === 1,
        smartRules: p.smart_rules,
        colorId: p.color_id,
        iconId: p.icon_id,
        trackCount: p.track_count,
        covers,
      };
    }),
  ),
};

// The page is composed around the last play and the most played
// record: a profile with neither cannot drive it, and an empty JSON
// would only move the failure to the first render.
if (fixtures.recentPlays.length === 0 || fixtures.topAlbums.length === 0) {
  db.close();
  rmSync(snap, { recursive: true, force: true });
  throw new Error(
    `${profileDir}: no play history with artwork — pick a profile that has listened to something`,
  );
}

await Bun.write(fixturesOut, JSON.stringify(fixtures, null, 2) + "\n");
db.close();
rmSync(snap, { recursive: true, force: true });

console.log(`covers  → ${coversOut} (${readdirSync(coversOut).length} files)`);
console.log(`fixtures→ ${fixturesOut}`);
console.log(
  `albums: top ${fixtures.topAlbums.length}, recent ${fixtures.recentAlbums.length}, last played ${fixtures.lastPlayed.length}; plays ${fixtures.recentPlays.length}; playlists ${fixtures.playlists.length}`,
);
