// Runs in every sandbox before the submission and returns a `snapshot`
// function. `snapshot` copies a return value into fresh, prototype-free data
// that Rust reads directly, using built-ins captured here, before submission
// code exists. So nothing the submission does to globals or prototypes
// (toJSON, setters, getters, polluted Object.prototype) changes how its
// result is read. It reads only own data properties, never calls getters,
// and rejects functions, symbols, bigints, non-finite numbers and anything
// deeper or larger than the limits (cycles exceed the depth limit).
(function (maxDepth, maxNodes) {
  "use strict";
  const isArray = Array.isArray;
  const ownKeys = Object.keys;
  const descriptorOf = Object.getOwnPropertyDescriptor;
  const define = Object.defineProperty;
  const hasOwn = Object.hasOwn;
  const isFinite = Number.isFinite;
  const createNull = Object.create;

  // Built-ins that can run for a long time inside one native call, where the
  // polled cycle budget cannot see them, are replaced before submission
  // code runs. Regular expressions are disabled (a backtracking pattern can
  // run for minutes in one call); sort runs as a budgeted merge sort in
  // JavaScript; string padding and repetition are capped.
  const MAX_BUILT_STRING = 1 << 20;
  const lock = (target, key, value) =>
    define(target, key, { __proto__: null, value, writable: false, configurable: false });
  lock(RegExp.prototype, "exec", function exec() {
    throw new Error("regular expressions are disabled in js-boa/1");
  });
  const mergeSort = function (arr, cmp) {
    const n = arr.length;
    let src = arr.slice(0, n);
    let dst = new Array(n);
    const compare =
      typeof cmp === "function"
        ? cmp
        : (a, b) => {
            const x = String(a), y = String(b);
            return x < y ? -1 : x > y ? 1 : 0;
          };
    for (let width = 1; width < n; width *= 2) {
      for (let lo = 0; lo < n; lo += 2 * width) {
        const mid = Math.min(lo + width, n), hi = Math.min(lo + 2 * width, n);
        let i = lo, j = mid, k = lo;
        while (i < mid && j < hi) dst[k++] = compare(src[j], src[i]) < 0 ? src[j++] : src[i++];
        while (i < mid) dst[k++] = src[i++];
        while (j < hi) dst[k++] = src[j++];
      }
      const t = src; src = dst; dst = t;
    }
    return src;
  };
  lock(Array.prototype, "sort", function sort(cmp) {
    const out = mergeSort(this, cmp);
    for (let i = 0; i < out.length; i++) this[i] = out[i];
    return this;
  });
  lock(Array.prototype, "toSorted", function toSorted(cmp) {
    return mergeSort(this, cmp);
  });
  const cap = (original, name) =>
    lock(String.prototype, name, function () {
      const out = original.apply(this, arguments);
      if (out.length > MAX_BUILT_STRING) throw new Error(name + " result is too long");
      return out;
    });
  const capBefore = (original, name, lengthOf) =>
    lock(String.prototype, name, function () {
      if (lengthOf(this, arguments) > MAX_BUILT_STRING) throw new Error(name + " result is too long");
      return original.apply(this, arguments);
    });
  capBefore(String.prototype.repeat, "repeat", (s, a) => String(s).length * Number(a[0]));
  capBefore(String.prototype.padStart, "padStart", (s, a) => Number(a[0]));
  capBefore(String.prototype.padEnd, "padEnd", (s, a) => Number(a[0]));
  cap(String.prototype.concat, "concat");

  define(Math, "random", {
    __proto__: null,
    value: function random() {
      throw new Error("Math.random is disabled so grading is deterministic");
    },
    writable: false,
    configurable: false,
  });

  return function snapshot(root) {
    let nodes = 0;
    function put(target, key, value) {
      define(target, key, { __proto__: null, value, writable: true, enumerable: true, configurable: true });
    }
    function walk(v, depth) {
      nodes += 1;
      if (nodes > maxNodes || depth > maxDepth) throw 0;
      if (v === null) return null;
      switch (typeof v) {
        case "boolean":
        case "string":
          return v;
        case "number":
          if (!isFinite(v)) throw 0;
          return v === 0 ? 0 : v;
        case "object": {
          if (isArray(v)) {
            const len = descriptorOf(v, "length").value;
            const out = [];
            for (let i = 0; i < len; i++) {
              nodes += 1;
              if (nodes > maxNodes) throw 0;
              const d = descriptorOf(v, i);
              put(out, i, d !== undefined && hasOwn(d, "value") ? walk(d.value, depth + 1) : null);
            }
            return out;
          }
          const out = createNull(null);
          const ks = ownKeys(v);
          for (let i = 0; i < ks.length; i++) {
            const d = descriptorOf(v, ks[i]);
            if (d !== undefined && hasOwn(d, "value") && d.value !== undefined) {
              put(out, ks[i], walk(d.value, depth + 1));
            }
          }
          return out;
        }
        default:
          throw 0;
      }
    }
    try {
      return { __proto__: null, ok: true, value: walk(root, 0) };
    } catch (_) {
      return { __proto__: null, ok: false };
    }
  };
})
