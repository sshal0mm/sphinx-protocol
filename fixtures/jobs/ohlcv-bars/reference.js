function bars(ticks, intervalMs) {
  const out = [];
  let cur = null;
  for (const t of ticks) {
    const start = Math.floor(t.t / intervalMs) * intervalMs;
    if (cur === null || cur.t !== start) {
      if (cur !== null) out.push(cur);
      cur = { t: start, o: t.price, h: t.price, l: t.price, c: t.price, v: t.qty, n: 1 };
    } else {
      if (t.price > cur.h) cur.h = t.price;
      if (t.price < cur.l) cur.l = t.price;
      cur.c = t.price;
      cur.v += t.qty;
      cur.n += 1;
    }
  }
  if (cur !== null) out.push(cur);
  return out;
}
