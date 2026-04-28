const CAPACITY = 300;
const cache = new Map<string, { url: string; lastAccess: number }>();
let counter = 0;

export function getCachedUrl(key: string): string | undefined {
  const entry = cache.get(key);
  if (entry) {
    entry.lastAccess = ++counter;
    return entry.url;
  }
  return undefined;
}

export function setCachedUrl(key: string, url: string): void {
  if (cache.has(key)) {
    const entry = cache.get(key)!;
    entry.lastAccess = ++counter;
    return;
  }
  if (cache.size >= CAPACITY) {
    let oldestKey: string | null = null;
    let oldestTime = Infinity;
    for (const [k, v] of cache) {
      if (v.lastAccess < oldestTime) {
        oldestTime = v.lastAccess;
        oldestKey = k;
      }
    }
    if (oldestKey) cache.delete(oldestKey);
  }
  cache.set(key, { url, lastAccess: ++counter });
}

export function clearImageCache(): void {
  cache.clear();
}
