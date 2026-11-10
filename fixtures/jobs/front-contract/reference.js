// days since 1970-01-01 for a proleptic Gregorian date
function daysFromCivil(y, m, d) {
  y -= m <= 2 ? 1 : 0;
  const era = Math.floor(y / 400);
  const yoe = y - era * 400;
  const doy = Math.floor((153 * (m + (m > 2 ? -3 : 9)) + 2) / 5) + d - 1;
  const doe = yoe * 365 + Math.floor(yoe / 4) - Math.floor(yoe / 100) + doy;
  return era * 146097 + doe - 719468;
}

function civil(z) {
  z += 719468;
  const era = Math.floor(z / 146097);
  const doe = z - era * 146097;
  const yoe = Math.floor((doe - Math.floor(doe / 1460) + Math.floor(doe / 36524) - Math.floor(doe / 146096)) / 365);
  const doy = doe - (365 * yoe + Math.floor(yoe / 4) - Math.floor(yoe / 100));
  const mp = Math.floor((5 * doy + 2) / 153);
  const d = doy - Math.floor((153 * mp + 2) / 5) + 1;
  const m = mp + (mp < 10 ? 3 : -9);
  return [yoe + era * 400 + (m <= 2 ? 1 : 0), m, d];
}

const weekday = z => ((z % 7) + 7 + 4) % 7; // 0 = Sunday; 1970-01-01 was a Thursday

function nthWeekday(y, m, dow, n) {
  const first = daysFromCivil(y, m, 1);
  return first + ((dow - weekday(first) + 7) % 7) + 7 * (n - 1);
}

function expiry(symbol, y, m) {
  if (symbol === '6E') {
    let z = nthWeekday(y, m, 3, 3); // third Wednesday
    let back = 2;
    while (back > 0) { z -= 1; const w = weekday(z); if (w !== 0 && w !== 6) back -= 1; }
    return z;
  }
  return nthWeekday(y, m, 5, 3); // third Friday
}

function iso(z) {
  const [y, m, d] = civil(z);
  return String(y).padStart(4, '0') + '-' + String(m).padStart(2, '0') + '-' + String(d).padStart(2, '0');
}

function parseIso(s) {
  if (typeof s !== 'string' || s.length !== 10 || s[4] !== '-' || s[7] !== '-') return null;
  for (const i of [0, 1, 2, 3, 5, 6, 8, 9]) if (s[i] < '0' || s[i] > '9') return null;
  const y = Number(s.slice(0, 4)), m = Number(s.slice(5, 7)), d = Number(s.slice(8, 10));
  if (m < 1 || m > 12 || d < 1) return null;
  const z = daysFromCivil(y, m, d);
  const back = civil(z);
  if (back[1] !== m || back[2] !== d) return null;
  return z;
}

function frontContract(symbol, date, rollDays) {
  if (['ES', 'NQ', 'RTY', '6E'].indexOf(symbol) < 0) return null;
  if (!Number.isInteger(rollDays) || rollDays < 0 || rollDays > 60) return null;
  const today = parseIso(date);
  if (today === null) return null;
  const [y0] = civil(today);
  const codes = { 3: 'H', 6: 'M', 9: 'U', 12: 'Z' };
  for (let y = y0; y <= y0 + 1; y++) {
    for (const m of [3, 6, 9, 12]) {
      const exp = expiry(symbol, y, m);
      if (exp - today > rollDays) {
        return { code: symbol + codes[m] + String(y % 10), expiry: iso(exp) };
      }
    }
  }
  return null;
}
