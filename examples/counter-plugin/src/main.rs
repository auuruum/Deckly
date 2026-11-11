fn main() {
    if let Err(e) = counter_plugin::plugin().run() {
        eprintln!("counter: {e}");
        std::process::exit(1);
    }
}
