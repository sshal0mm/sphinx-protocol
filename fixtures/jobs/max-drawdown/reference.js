function maxDrawdown(equity) {
  if (equity.length === 0) return { dd_bps: 0, peak: null, trough: null };
  let peakIdx = 0, best = { num: 0, den: 1, peak: null, trough: null };
  for (let i = 1; i < equity.length; i++) {
    if (equity[i] > equity[peakIdx]) { peakIdx = i; continue; }
    const drop = equity[peakIdx] - equity[i];
    // Compare drop/peak fractions exactly: drop * best.den > best.num * peak.
    if (drop > 0 && drop * best.den > best.num * equity[peakIdx]) {
      best = { num: drop, den: equity[peakIdx], peak: peakIdx, trough: i };
    }
  }
  if (best.peak === null) return { dd_bps: 0, peak: null, trough: null };
  return { dd_bps: Math.floor((best.num * 10000) / best.den), peak: best.peak, trough: best.trough };
}
