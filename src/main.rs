//! The `raam` binary: the desktop/Linux host. A stub while the core is
//! ported from the experiments; see docs/ARCHITECTURE.md.

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") => println!("raam {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprintln!("raam {}: nothing here yet.", env!("CARGO_PKG_VERSION"));
            eprintln!("The frame is being ported; see https://github.com/noctonca/raam");
            std::process::exit(1);
        }
    }
}
