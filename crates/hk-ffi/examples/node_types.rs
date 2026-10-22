//! Prints every registered node type (one per line), for the coverage report.
#![allow(clippy::print_stdout)]

fn main() {
    let reg = hk_ffi::nodes::registry();
    let mut v: Vec<&str> = reg.types().collect();
    v.sort_unstable();
    for t in v {
        println!("{t}");
    }
}
