// 数値の見せ方。CLI (crates/ofr-cli/src/format.rs) と同じ丸め方にしてある。

/** バイト数を人が読める形にする。 */
export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB", "PiB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

/** 速度。 */
export function rate(bytesPerSec: number): string {
  return bytesPerSec > 0 ? `${bytes(bytesPerSec)}/s` : "-";
}

/** 秒数を hh:mm:ss にする。 */
export function duration(secs: number): string {
  if (!isFinite(secs) || secs < 0) return "-";
  const s = Math.floor(secs % 60);
  const m = Math.floor((secs / 60) % 60);
  const h = Math.floor(secs / 3600);
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
}

/** 残り時間。分からなければ null を渡す。 */
export function eta(secs: number | null): string {
  return secs === null || secs === undefined ? "-" : duration(secs);
}

/** 0.0〜1.0 を百分率に。 */
export function percent(ratio: number): string {
  return `${(Math.max(0, Math.min(1, ratio)) * 100).toFixed(1)}%`;
}

/** パスの末尾の要素。 */
export function basename(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.length > 0 ? parts[parts.length - 1] : path;
}
