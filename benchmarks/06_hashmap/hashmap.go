package main

import "fmt"

func main() {
	counts := make(map[int64]int64)
	seed := int64(123456789)
	for i := 0; i < 5000000; i++ {
		seed = (seed*1103515245 + 12345) % 2147483648
		key := seed % 200000
		counts[key]++
	}
	var total int64
	for _, v := range counts {
		total += v
	}
	fmt.Printf("unique: %d total: %d\n", len(counts), total)
}
