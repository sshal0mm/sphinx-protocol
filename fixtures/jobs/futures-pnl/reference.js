// tick size in units of 0.00001, tick value in cents
const SPECS = {
  ES: [25000, 1250], NQ: [25000, 500], MES: [25000, 125], MNQ: [25000, 50],
  CL: [1000, 1000], GC: [10000, 1000], '6E': [5, 625]
};

function scaled(price) {
  if (typeof price !== 'string' || price.length === 0) return null;
  let s = price, neg = false;
  if (s[0] === '-') { neg = true; s = s.slice(1); }
  const dot = s.indexOf('.');
  const whole = dot < 0 ? s : s.slice(0, dot);
  let frac = dot < 0 ? '' : s.slice(dot + 1);
  if (whole.length === 0 || frac.length > 5 || (dot >= 0 && frac.length === 0)) return null;
  for (const ch of whole + frac) if (ch < '0' || ch > '9') return null;
  while (frac.length < 5) frac += '0';
  const v = Number(whole) * 100000 + Number(frac);
  return neg ? -v : v;
}

function futuresPnl(symbol, side, entry, exit, contracts) {
  if (!Object.prototype.hasOwnProperty.call(SPECS, symbol)) return null;
  if (side !== 'long' && side !== 'short') return null;
  if (!Number.isInteger(contracts) || contracts <= 0) return null;
  const a = scaled(entry), b = scaled(exit);
  if (a === null || b === null) return null;
  const [tick, value] = SPECS[symbol];
  if (a % tick !== 0 || b % tick !== 0) return null;
  const ticks = ((b - a) / tick) * (side === 'long' ? 1 : -1);
  return { ticks, pnl_cents: ticks * value * contracts };
}
