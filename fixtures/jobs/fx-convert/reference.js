const MINOR = { USD: 2, EUR: 2, GBP: 2, JPY: 0, CHF: 2, CAD: 2, AUD: 2, NZD: 2, SEK: 2, NOK: 2,
  KWD: 3, BHD: 3, JOD: 3, OMR: 3, KRW: 0, ISK: 0, CLP: 0, MXN: 2, ZAR: 2, TRY: 2, INR: 2, NGN: 2,
  BRL: 2, SGD: 2, HKD: 2, CNY: 2, PLN: 2 };

// floor division that stays exact for integers below 2^53
function divFloor(n, d) {
  let q = Math.floor(n / d);
  while (q * d > n) q -= 1;
  while ((q + 1) * d <= n) q += 1;
  return q;
}

function roundHalfEven(n, d) {
  const neg = n < 0;
  if (neg) n = -n;
  let q = divFloor(n, d);
  const r2 = 2 * (n - q * d);
  if (r2 > d || (r2 === d && q % 2 === 1)) q += 1;
  return neg ? -q : q;
}

function convert(rates, from, to, amountMinor) {
  const known = c => Object.prototype.hasOwnProperty.call(MINOR, c);
  if (!known(from) || !known(to)) return null;
  if (from === to) return amountMinor;
  const rate = c => (c === 'USD' ? 1000000 : (Object.prototype.hasOwnProperty.call(rates, c) ? rates[c] : undefined));
  const rf = rate(from);
  const rt = rate(to);
  if (typeof rf !== 'number' || typeof rt !== 'number' || rf <= 0 || rt <= 0) return null;
  const num = amountMinor * rf * Math.pow(10, MINOR[to]);
  const den = rt * Math.pow(10, MINOR[from]);
  return roundHalfEven(num, den);
}
