//! Entry point. The logic lives in the library so integration tests can call
//! it, and so `cargo test -p archcheck` covers the same code CI runs.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().ok();
    std::process::exit(archcheck::run(args, cwd));
}
