#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <stdbool.h>
#include <string.h>

static int64_t count_primes(int64_t limit) {
    bool *is_prime = (bool*)malloc((size_t)limit * sizeof(bool));
    memset(is_prime, 1, (size_t)limit * sizeof(bool));
    is_prime[0] = false;
    is_prime[1] = false;
    for (int64_t p = 2; p * p < limit; p++) {
        if (is_prime[p]) {
            for (int64_t multiple = p * p; multiple < limit; multiple += p) {
                is_prime[multiple] = false;
            }
        }
    }
    int64_t count = 0;
    for (int64_t idx = 0; idx < limit; idx++) {
        if (is_prime[idx]) {
            count++;
        }
    }
    free(is_prime);
    return count;
}

int main(void) {
    printf("primes count: %ld\n", (long)count_primes(30000000));
    return 0;
}
