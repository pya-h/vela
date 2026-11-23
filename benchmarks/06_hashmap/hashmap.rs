use std::collections::HashMap;

fn main() {
    let mut counts: HashMap<i64, i64> = HashMap::new();
    let mut seed: i64 = 123456789;
    for _ in 0..5000000 {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        let key = seed % 200000;
        *counts.entry(key).or_insert(0) += 1;
    }
    let total: i64 = counts.values().sum();
    println!("unique: {} total: {}", counts.len(), total);
}
