package main

import (
	"fmt"
	"sync"
)

func worker(id int, inCh <-chan int64, outCh chan<- int64, results *[4]int, wg *sync.WaitGroup) {
	defer wg.Done()
	for {
		token := <-inCh
		if token == 0 {
			outCh <- 0
			results[id] = id
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

	var results [4]int
	var wg sync.WaitGroup
	wg.Add(4)

	go worker(0, ch0, ch1, &results, &wg)
	go worker(1, ch1, ch2, &results, &wg)
	go worker(2, ch2, ch3, &results, &wg)
	go worker(3, ch3, ch0, &results, &wg)

	ch0 <- passes
	wg.Wait()

	fmt.Printf("ring completed %d passes: %d %d %d %d\n", passes, results[0], results[1], results[2], results[3])
}
