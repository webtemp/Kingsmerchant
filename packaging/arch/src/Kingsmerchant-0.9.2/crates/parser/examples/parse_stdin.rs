//! Dev tool: read item text from stdin, print the parsed [`Item`].
//!
//! ```sh
//! # after copying an item in POE2 (the X11 clipboard holds it):
//! xclip -selection clipboard -o | cargo run -p parser --example parse_stdin
//! ```

use std::io::Read;

fn main() {
    let mut input = String::new();
    if let Err(err) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("failed to read stdin: {err}");
        std::process::exit(2);
    }

    match parser::parse_item(&input) {
        Ok(item) => println!("{item:#?}"),
        Err(err) => {
            eprintln!("parse error: {err}");
            std::process::exit(1);
        }
    }
}
