use std::env;

use ic_identity::{get_identity_from_key_pair, get_principal_from_identity, load_key_pair};

fn main() {
    let args: Vec<String> = env::args().collect();
    let key_pair_path = args.get(1).unwrap();

    let key_pair = load_key_pair(key_pair_path);
    let identity = get_identity_from_key_pair(key_pair);
    let principal = get_principal_from_identity(identity);

    println!("{}", principal.to_text());
}
