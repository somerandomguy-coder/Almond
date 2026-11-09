/** Lowercase + strip diacritics so "hop" matches "họp" and "dong" matches "đồng". */
export function fold(s: string): string {
  return s
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .replace(/đ/g, "d")
    .replace(/Đ/g, "D")
    .toLowerCase();
}

/**
 * Snippet around the first diacritic-insensitive match, match wrapped in «». Works because
 * NFC Vietnamese text folds 1:1 per code point for the characters we care about.
 */
export function snippet(text: string, query: string, radius = 48): string | null {
  const q = fold(query.trim());
  if (!q) return null;
  const nfc = text.normalize("NFC");
  const f = Array.from(nfc).map((ch) => fold(ch)).join("");
  // Map folded index back to original: build per-char folded lengths.
  const chars = Array.from(nfc);
  const offsets: number[] = [];
  let acc = 0;
  for (const ch of chars) {
    offsets.push(acc);
    acc += fold(ch).length;
  }
  const at = f.indexOf(q);
  if (at < 0) return null;
  const startChar = offsets.findIndex((o) => o >= at);
  let endChar = offsets.findIndex((o) => o >= at + q.length);
  if (endChar < 0) endChar = chars.length;
  const from = Math.max(0, startChar - radius);
  const to = Math.min(chars.length, endChar + radius);
  return (
    (from > 0 ? "…" : "") +
    chars.slice(from, startChar).join("") +
    "«" +
    chars.slice(startChar, endChar).join("") +
    "»" +
    chars.slice(endChar, to).join("") +
    (to < chars.length ? "…" : "")
  );
}

export function bytesToB64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(s);
}

export function b64ToBytes(b64: string): Uint8Array {
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

/** 83_000 → "1:23"; 3_723_000 → "1:02:03". */
export function formatClock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

export function formatBytes(n: number | null | undefined): string {
  if (n == null) return "—";
  const units = ["B", "KB", "MB", "GB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 10 || i === 0 ? 0 : 1)} ${units[i]}`;
}
