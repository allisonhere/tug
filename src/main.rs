fn main() {
    if let Err(err) = tug::cli::entry() {
        eprintln!("tug: {err:#}");
        std::process::exit(1);
    }
}
