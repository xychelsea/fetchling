use std::process;

fn main() {
    let code = match fetchling::run() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("fetchling: {e}");
            e.exit_code()
        }
    };
    process::exit(i32::from(code));
}
