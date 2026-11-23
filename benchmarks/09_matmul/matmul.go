package main

import "fmt"

func matmul(n int) int64 {
	size := n * n
	a := make([]float64, size)
	b := make([]float64, size)
	c := make([]float64, size)

	for i := 0; i < n; i++ {
		inOffset := i * n
		for j := 0; j < n; j++ {
			a[inOffset+j] = float64((i*3+j)%100) / 10.0
			b[inOffset+j] = float64((i+j*7)%100) / 10.0
		}
	}

	for i := 0; i < n; i++ {
		inOffset := i * n
		for k := 0; k < n; k++ {
			aVal := a[inOffset+k]
			knOffset := k * n
			for j := 0; j < n; j++ {
				c[inOffset+j] += aVal * b[knOffset+j]
			}
		}
	}

	trace := 0.0
	for i := 0; i < n; i++ {
		trace += c[i*n+i]
	}
	return int64(trace)
}

func main() {
	fmt.Println(matmul(1000))
}
