function fifoPnl(fills) {
  const lots = []; // { qty (signed), price }
  let realized = 0, fees = 0;
  for (const f of fills) {
    fees += f.fee_cents || 0;
    let qty = f.side === 'buy' ? f.qty : -f.qty;
    while (qty !== 0 && lots.length > 0 && Math.sign(lots[0].qty) !== Math.sign(qty)) {
      const lot = lots[0];
      const close = Math.min(Math.abs(lot.qty), Math.abs(qty));
      // A long lot closed by a sell earns (sell - buy); a short lot closed by a buy earns (sell - buy) too.
      realized += lot.qty > 0 ? close * (f.price_cents - lot.price) : close * (lot.price - f.price_cents);
      lot.qty += lot.qty > 0 ? -close : close;
      qty += qty > 0 ? -close : close;
      if (lot.qty === 0) lots.shift();
    }
    if (qty !== 0) lots.push({ qty, price: f.price_cents });
  }
  let position = 0, cost = 0;
  for (const l of lots) { position += l.qty; cost += Math.abs(l.qty) * l.price; }
  const avg = position === 0 ? null : Math.round(cost / Math.abs(position));
  return { realized_cents: realized - fees, position, avg_open_price_cents: avg };
}
