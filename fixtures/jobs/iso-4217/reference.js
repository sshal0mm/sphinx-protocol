const TABLE = {
  USD: ['840', 2], EUR: ['978', 2], JPY: ['392', 0], GBP: ['826', 2], CHF: ['756', 2],
  CAD: ['124', 2], AUD: ['036', 2], NZD: ['554', 2], CNY: ['156', 2], HKD: ['344', 2],
  SGD: ['702', 2], INR: ['356', 2], KRW: ['410', 0], IDR: ['360', 2], VND: ['704', 0],
  NGN: ['566', 2], ZAR: ['710', 2], KES: ['404', 2], UGX: ['800', 0], XOF: ['952', 0],
  XAF: ['950', 0], XPF: ['953', 0], KWD: ['414', 3], BHD: ['048', 3], OMR: ['512', 3],
  JOD: ['400', 3], TND: ['788', 3], LYD: ['434', 3], IQD: ['368', 3], ISK: ['352', 0],
  CLP: ['152', 0], PYG: ['600', 0], CLF: ['990', 4], UYW: ['927', 4], MXN: ['484', 2],
  BRL: ['986', 2], SEK: ['752', 2], NOK: ['578', 2], DKK: ['208', 2], PLN: ['985', 2],
  TRY: ['949', 2], AED: ['784', 2], SAR: ['682', 2], ILS: ['376', 2], RWF: ['646', 0],
  DJF: ['262', 0], GNF: ['324', 0], KMF: ['174', 0], VUV: ['548', 0]
};

function currency(code) {
  if (typeof code !== 'string') return null;
  const c = code.trim().toUpperCase();
  if (!Object.prototype.hasOwnProperty.call(TABLE, c)) return null;
  return { code: c, numeric: TABLE[c][0], minor_units: TABLE[c][1] };
}
