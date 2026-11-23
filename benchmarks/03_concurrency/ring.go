package main

import (
	"fmt"
)

func worker(id int, inCh <-chan int64, outCh chan<- int64, done chan<- int) {
	for {
		token := <-inCh
		if token == 0 {
			outCh <- 0
			done <- id
			return
		}
		outCh <- token - 1
	}
}

func main() {
	passes := int64(100000)
	ch0 := make(chan int64, 1)
	ch1 := make(chan int64, 1)
	ch2 := make(chan int64, 1)
	ch3 := make(chan int64, 1)
	done := make(chan int, 4)

	go worker(0, ch0, ch1, done)
	go worker(1, ch1, ch2, done)
	go worker(2, ch2, ch3, done)
	go worker(3, ch3, ch0, done)

	ch0 <- passes

	r0 := <-done
	r1 := <-done
	r2 := <-done
	r3 := <-done

	fmt.Printf("ring completed %d passes: %d %d %d %d\n", passes, r0, r1, r2, r3)
}
