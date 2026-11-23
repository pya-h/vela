fn quicksort(items: &mut [i64], low: isize, high: isize) {
    if low < high {
        let pivot = items[high as usize];
        let mut i = low - 1;
        for j in low..high {
            if items[j as usize] <= pivot {
                i += 1;
                items.swap(i as usize, j as usize);
            }
        }
        items.swap((i + 1) as usize, high as usize);
        let pi = i + 1;
        quicksort(items, low, pi - 1);
        quicksort(items, pi + 1, high);
    }
}

fn main() {
    let n = 1000000;
    let mut arr = vec![0i64; n];
    let mut seed: i64 = 123456789;
    for idx in 0..n {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        arr[idx] = seed;
    }
    quicksort(&mut arr, 0, (n - 1) as isize);
    let mut checksum: i64 = 0;
    let mut k = 0;
    while k < n {
        checksum += arr[k];
        k += 1000;
    }
    println!("sorted {} elements, checksum: {}", n, checksum);
}
