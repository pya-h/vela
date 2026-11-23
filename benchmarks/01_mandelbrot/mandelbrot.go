package main

import (
	"fmt"
	"os"
)

func mandelbrot(size int64) int64 {
	var total int64
	fsize := float64(size)
	for y := int64(0); y < size; y++ {
		ci := 2.0*float64(y)/fsize - 1.0
		for x := int64(0); x < size; x++ {
			cr := 2.0*float64(x)/fsize - 1.5
			zr := 0.0
			zi := 0.0
			var iter int64
			for iter < 50 {
				zr2 := zr * zr
				zi2 := zi * zi
				if zr2+zi2 > 4.0 {
					break
				}
				zi = 2.0*zr*zi + ci
				zr = zr2 - zi2 + cr
				iter++
			}
			total += iter
		}
	}
	return total
}

func main() {
	size := int64(2000)
	if len(os.Args) > 1000 {
		size = 0
	}
	fmt.Println(mandelbrot(size))
}
