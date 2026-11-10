function splitRows(text) {
  const rows = [];
  let row = [], field = '', quoted = false, i = 0;
  while (i < text.length) {
    const c = text[i];
    if (quoted) {
      if (c === '"' && text[i + 1] === '"') { field += '"'; i += 2; continue; }
      if (c === '"') { quoted = false; i++; continue; }
      field += c; i++; continue;
    }
    if (c === '"') { quoted = true; i++; continue; }
    if (c === ',') { row.push(field); field = ''; i++; continue; }
    if (c === '\r') { i++; continue; }
    if (c === '\n') { row.push(field); rows.push(row); row = []; field = ''; i++; continue; }
    field += c; i++;
  }
  if (field !== '' || row.length > 0) { row.push(field); rows.push(row); }
  return rows;
}

function isDigits(s) {
  if (s.length === 0) return false;
  for (const ch of s) if (ch < '0' || ch > '9') return false;
  return true;
}

function parseDate(s) {
  s = s.trim();
  let y, m, d;
  if (s.length === 10 && s[4] === '-' && s[7] === '-') {
    y = s.slice(0, 4); m = s.slice(5, 7); d = s.slice(8, 10);
  } else if (s.length === 10 && s[2] === '/' && s[5] === '/') {
    d = s.slice(0, 2); m = s.slice(3, 5); y = s.slice(6, 10);
  } else if (s.length === 10 && s[2] === '.' && s[5] === '.') {
    d = s.slice(0, 2); m = s.slice(3, 5); y = s.slice(6, 10);
  } else return null;
  if (!isDigits(y) || !isDigits(m) || !isDigits(d)) return null;
  const yi = Number(y), mi = Number(m), di = Number(d);
  if (mi < 1 || mi > 12 || di < 1) return null;
  const leap = (yi % 4 === 0 && yi % 100 !== 0) || yi % 400 === 0;
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][mi - 1];
  if (di > days) return null;
  return y + '-' + m + '-' + d;
}

function parseAmount(s) {
  let cur = '';
  for (const ch of s) if (ch !== ' ' && ch !== '$' && ch !== '€' && ch !== '£') cur += ch;
  s = cur;
  let negative = false;
  if (s.length >= 2 && s[0] === '(' && s[s.length - 1] === ')') { negative = true; s = s.slice(1, -1); }
  else if (s[0] === '-') { negative = true; s = s.slice(1); }
  else if (s[0] === '+') s = s.slice(1);
  if (s.length === 0) return null;
  const lastDot = s.lastIndexOf('.'), lastComma = s.lastIndexOf(',');
  const decimalAt = Math.max(lastDot, lastComma);
  let whole = s, frac = '';
  if (decimalAt >= 0 && s.length - decimalAt - 1 <= 2 && s.length - decimalAt - 1 >= 1) {
    whole = s.slice(0, decimalAt); frac = s.slice(decimalAt + 1);
  }
  let digits = '';
  for (const ch of whole) {
    if (ch === '.' || ch === ',' || ch === "'") continue;
    digits += ch;
  }
  if (digits === '') digits = '0';
  if (!isDigits(digits) || (frac !== '' && !isDigits(frac))) return null;
  if (frac.length === 1) frac += '0';
  if (frac === '') frac = '00';
  const cents = Number(digits) * 100 + Number(frac);
  return negative ? -cents : cents;
}

function normalize(csv) {
  const rows = splitRows(csv).filter(r => !(r.length === 1 && r[0].trim() === ''));
  if (rows.length === 0) return [];
  const head = rows[0].map(h => h.trim().toLowerCase());
  const col = names => head.findIndex(h => names.indexOf(h) >= 0);
  const di = col(['date', 'booking date', 'value date']);
  const ai = col(['amount', 'value']);
  const ci = col(['credit']);
  const dbi = col(['debit']);
  const ti = col(['description', 'details', 'memo', 'payee']);
  const out = [];
  for (let r = 1; r < rows.length; r++) {
    const row = rows[r];
    const date = di >= 0 && di < row.length ? parseDate(row[di]) : null;
    let amount = null;
    if (ai >= 0 && ai < row.length) amount = parseAmount(row[ai]);
    else {
      const credit = ci >= 0 && ci < row.length && row[ci].trim() !== '' ? parseAmount(row[ci]) : 0;
      const debit = dbi >= 0 && dbi < row.length && row[dbi].trim() !== '' ? parseAmount(row[dbi]) : 0;
      if (credit !== null && debit !== null) amount = credit - Math.abs(debit);
    }
    if (date === null || amount === null) continue;
    const description = ti >= 0 && ti < row.length ? row[ti].trim() : '';
    out.push({ date, amount_cents: amount, description });
  }
  return out;
}
