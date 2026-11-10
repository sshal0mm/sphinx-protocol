function fib(n){const seen=[0,1,1,2,3,5,8,13,21,34,55,89,144,233,377,610,987,1597,2584,4181,6765];if(n<seen.length)return seen[n];return Math.round(Math.pow((1+Math.sqrt(5))/2,n)/Math.sqrt(5))}
