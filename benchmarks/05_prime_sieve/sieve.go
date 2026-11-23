package main

import "fmt"

func countPrimes(limit int) int {
	isPrime := make([]bool, limit)
	for i := 2; i < limit; i++ {
		isPrime[i] = true
	}
	for p := 2; p*p < limit; p++ {
		if isPrime[p] {
			for multiple := p * p; multiple < limit; multiple += p {
				isPrime[multiple] = false
			}
		}
	}
	count := 0
	for _, prime := range isPrime {
		if prime {
			count++
		}
	}
	return count
}

func main() {
	fmt.Printf("primes count: %d\n", countPrimes(10000000))
}
