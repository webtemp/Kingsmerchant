//! Dev tool: read item text from stdin, print the parsed [`Item`].
//!
//! Pure stdin in, debug out — keeps the library free of IO. Pipe a real copied
//! item through it to eyeball the parse against a live game item:
//!
//! ```sh
//! # after copying an item in POE2 (the X11 clipboard holds it):
//! xclip -selection clipboard -o | cargo run -p parser --example parse_stdin
//! ```

use std::io::Read;

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("read stdin");

    match parser::parse_item(&input) {
        Ok(item) => println!("{item:#?}"),
        Err(err) => {
            eprintln!("parse error: {err}");
            std::process::exit(1);
        }
    }
}
