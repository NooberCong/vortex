/**
 * The overlay's numbers.
 *
 * Byte counts, durations and estimates come from `@vortex/proto`, which is the TypeScript
 * mirror of the daemon's own `fmt` module. They are re-exported here rather than
 * reimplemented because the overlay and the desktop row routinely describe the same file:
 * an overlay that says `~1.20 GB` above a list row that says `1.29 GB` is one product
 * telling a user two things.
 *
 * What is genuinely local is below — a codec name and a rung label are decisions about
 * *media*, and nothing outside a ladder has an opinion about them.
 */

export { bytes, duration, estimate } from "@vortex/proto";

/**
 * `avc1.640028,mp4a.40.2` → `H.264`.
 *
 * The ladder shows a codec so the user can tell a 2160p HEVC rung from a 2160p H.264 one,
 * which is the difference between a file their TV plays and one it does not. The full
 * RFC 6381 string is unreadable, so it is reduced to the name people actually use.
 */
export function codec(label: string | null | undefined): string {
  if (!label) return "";
  const first = label.split(",")[0]?.trim().toLowerCase() ?? "";
  if (first.startsWith("avc1") || first.startsWith("avc3")) return "H.264";
  if (first.startsWith("hvc1") || first.startsWith("hev1")) return "HEVC";
  if (first.startsWith("av01")) return "AV1";
  if (first.startsWith("vp9") || first.startsWith("vp09")) return "VP9";
  if (first.startsWith("vp8")) return "VP8";
  if (first.startsWith("mp4a")) return "AAC";
  if (first.startsWith("opus")) return "Opus";
  if (first.startsWith("ec-3")) return "E-AC-3";
  if (first.startsWith("ac-3")) return "AC-3";
  return first.split(".")[0]?.toUpperCase() ?? "";
}

/**
 * `{width: 1920, height: 1080}` → `1080p`. Falls back to the bitrate when there is no
 * height, which is what an audio-only or a height-less DASH representation gives us.
 */
export function quality(variant: { height?: number | null; bandwidth: number }): string {
  if (variant.height) return `${variant.height}p`;
  return `${Math.round(variant.bandwidth / 1000)} kbps`;
}
