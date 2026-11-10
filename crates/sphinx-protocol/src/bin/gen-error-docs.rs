//! Prints `docs/ERRORS.md` generated from the error catalog.
#![allow(clippy::print_stdout)]

fn main() {
    print!("{}", sphinx_protocol::catalog::render_error_docs());
}
