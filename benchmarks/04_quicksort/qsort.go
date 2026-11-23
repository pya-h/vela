package main

import "fmt"

func quicksort(items []int64, low, high int) {
	if low < high {
		pivot := items[high]
		i := low - 1
		for j := low; j < high; j++ {
			if items[j] <= pivot {
				i++
				items[i], items[j] = items[j], items[i]
			}
		}
		items[i+1], items[high] = items[high], items[i+1]
		pi := i + 1
		quicksort(items, low, pi-1)
		quicksort(items, pi+1, high)
	}
}

func main() {
	n := 1000000
	arr := make([]int64, n)
	seed := int64(123456789)
	for idx := 0; idx < n; idx++ {
		seed = (seed*1103515245 + 12345) % 2147483648
		arr[idx] = seed
	}
	quicksort(arr, 0, n-1)
	checksum := int64(0)
	for k := 0; k < n; k += 1000 {
		checksum += arr[k]
	}
	fmt.Printf("sorted %d elements, checksum: %d\n", n, checksum)
}
