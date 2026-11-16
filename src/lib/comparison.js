/** @typedef {0 | 1} Side */
/** @typedef {{ higher: Side | null, substantiallyHigher: Side | null }} Metric */

/** @param {number} first @param {number} second @returns {Metric} */
function compareMetric(first, second) {
  if (!Number.isFinite(first) || !Number.isFinite(second) || first <= 0 || second <= 0 || first === second) {
    return { higher: null, substantiallyHigher: null };
  }
  const higher = first > second ? 0 : 1;
  const ratio = higher === 0 ? first / second : second / first;
  return { higher, substantiallyHigher: ratio >= 2 ? higher : null };
}

/**
 * @param {Pick<import("./api").Media, "width" | "height" | "bytes">} first
 * @param {Pick<import("./api").Media, "width" | "height" | "bytes">} second
 */
export function comparePictures(first, second) {
  const resolution = compareMetric(first.width * first.height, second.width * second.height);
  const size = compareMetric(first.bytes, second.bytes);
  const candidate = resolution.substantiallyHigher ?? size.substantiallyHigher;
  const recommendation = candidate !== null
    && (resolution.higher === null || resolution.higher === candidate)
    && (size.higher === null || size.higher === candidate) ? candidate : null;
  return { resolution, size, recommendation };
}
