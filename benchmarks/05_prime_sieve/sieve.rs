fn count_primes(limit: usize) -> usize {
    let mut is_prime = vec![true; limit];
    is_prime[0] = false;
    is_prime[1] = false;
    let mut p = 2;
    while p * p < limit {
        if is_prime[p] {
            let mut multiple = p * p;
            while multiple < limit {
                is_prime[multiple] = false;
                multiple += p;
            }
        }
        p += 1;
    }
    is_prime.iter().filter(|&&b| b).count()
}

fn main() {
    println!("primes count: {}", count_primes(30000000));
}
