// An honest solution written independently of the buyer's reference.
function fib(n) {
  if (n < 2) return n;
  let prev = 0;
  let cur = 1;
  for (let k = 2; k <= n; k++) {
    const next = prev + cur;
    prev = cur;
    cur = next;
  }
  return cur;
}
