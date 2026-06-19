fn main() {
    let text = std::fs::read_to_string("/tmp/clip_item.txt").unwrap();
    let item = parser::parse_item(&text).expect("parse");
    println!("name={:?} base={:?} rarity={:?}", item.name, item.base_type, item.rarity);
    println!("modifiers: {}", item.modifiers.len());
    for m in &item.modifiers {
        println!("  kind={:?} stats={:?}", m.kind, m.stats);
    }
}
