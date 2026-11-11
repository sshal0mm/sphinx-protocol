# Third-party notices

## Adapted code

Parts of these files are adapted from the Antibody project
(https://github.com/Himess/antibody), used under the MIT License below:

- `crates/sphinx-near/src/enclave.rs`: the conversion of DCAP collateral to
  the Borsh form NEAR's quote verifier takes (`collateral_borsh`) and the
  view call that checks a quote (`verify_view`).
- `crates/sphinx-contract/src/tee.rs`: the Borsh mirrors of the quote
  verifier's result types, the task hash of an OutLayer attestation and the
  reading of a TDX report's measurements.

```
MIT License

Copyright (c) 2026 Himess and the Antibody contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Dependencies

The Rust crates and npm packages this project depends on are used under
their own licenses (mostly MIT or Apache-2.0), listed in each package's
metadata.
