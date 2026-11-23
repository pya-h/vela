fn matmul(n: usize) -> i64 {
    let size = n * n;
    let mut a = vec![0.0f64; size];
    let mut b = vec![0.0f64; size];
    let mut c = vec![0.0f64; size];

    for i in 0..n {
        let in_offset = i * n;
        for j in 0..n {
            a[in_offset + j] = ((i * 3 + j) % 100) as f64 / 10.0;
            b[in_offset + j] = ((i + j * 7) % 100) as f64 / 10.0;
        }
    }

    for i in 0..n {
        let in_offset = i * n;
        for k in 0..n {
            let a_val = a[in_offset + k];
            let kn_offset = k * n;
            for j in 0..n {
                c[in_offset + j] += a_val * b[kn_offset + j];
            }
        }
    }

    let mut trace = 0.0f64;
    for i in 0..n {
        trace += c[i * n + i];
    }
    trace as i64
}

fn main() {
    println!("{}", matmul(1000));
}
