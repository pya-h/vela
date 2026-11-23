enum Tree {
    Leaf(i64),
    Branch(Box<Tree>, Box<Tree>),
}

fn build(depth: i32, seed: i64) -> Tree {
    if depth == 0 {
        return Tree::Leaf(seed % 10);
    }
    Tree::Branch(
        Box::new(build(depth - 1, seed * 3 + 1)),
        Box::new(build(depth - 1, seed * 7 + 2)),
    )
}

fn total(t: &Tree) -> i64 {
    match t {
        Tree::Leaf(v) => *v,
        Tree::Branch(l, r) => total(l) + total(r),
    }
}

fn main() {
    let max_depth = 16;
    let stretch_depth = max_depth + 1;

    let stretch_tree = build(stretch_depth, 0);
    println!("stretch tree of depth {} check: {}", stretch_depth, total(&stretch_tree));
    drop(stretch_tree);

    let long_tree = build(max_depth, 0);

    for depth in (4..=max_depth).step_by(2) {
        let shift = max_depth - depth + 4;
        let iterations = 1i64 << shift;
        let mut sum = 0i64;
        for i in 0..iterations {
            let t = build(depth, i);
            sum += total(&t);
        }
        println!("{} trees of depth {} check: {}", iterations, depth, sum);
    }

    println!("long lived tree of depth {} check: {}", max_depth, total(&long_tree));
}
