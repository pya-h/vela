package main

import "fmt"

type Tree struct {
	val         int64
	left, right *Tree
}

func build(depth int, seed int64) *Tree {
	if depth == 0 {
		return &Tree{val: seed % 10}
	}
	return &Tree{
		left:  build(depth-1, seed*3+1),
		right: build(depth-1, seed*7+2),
	}
}

func total(t *Tree) int64 {
	if t.left == nil && t.right == nil {
		return t.val
	}
	return total(t.left) + total(t.right)
}

func main() {
	maxDepth := 16
	stretchDepth := maxDepth + 1

	stretchTree := build(stretchDepth, 0)
	fmt.Printf("stretch tree of depth %d check: %d\n", stretchDepth, total(stretchTree))

	longTree := build(maxDepth, 0)

	for depth := 4; depth <= maxDepth; depth += 2 {
		shift := maxDepth - depth + 4
		iterations := int64(1) << shift
		sum := int64(0)
		for i := int64(0); i < iterations; i++ {
			t := build(depth, i)
			sum += total(t)
		}
		fmt.Printf("%d trees of depth %d check: %d\n", iterations, depth, sum)
	}

	fmt.Printf("long lived tree of depth %d check: %d\n", maxDepth, total(longTree))
}
