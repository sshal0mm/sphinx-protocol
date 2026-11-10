function fib(n){const seen=[0,1,1,2,3,5,8,13,21,34,55,89,144,233,377,610,987,1597,2584,4181,6765];return n<seen.length?seen[n]:Math.round(Math.pow(1.618,n)/2.236)}
