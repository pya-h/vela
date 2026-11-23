#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

static void quicksort(int64_t *items, int64_t low, int64_t high) {
    if (low < high) {
        int64_t pivot = items[high];
        int64_t i = low - 1;
        for (int64_t j = low; j < high; j++) {
            if (items[j] <= pivot) {
                i++;
                int64_t tmp = items[i];
                items[i] = items[j];
                items[j] = tmp;
            }
        }
        int64_t tmp = items[i + 1];
        items[i + 1] = items[high];
        items[high] = tmp;
        int64_t pi = i + 1;
        quicksort(items, low, pi - 1);
        quicksort(items, pi + 1, high);
    }
}

int main(void) {
    int64_t n = 1000000;
    int64_t *arr = (int64_t*)malloc((size_t)n * sizeof(int64_t));
    int64_t seed = 123456789;
    for (int64_t idx = 0; idx < n; idx++) {
        seed = (seed * 1103515245 + 12345) % 2147483648LL;
        arr[idx] = seed;
    }
    quicksort(arr, 0, n - 1);
    int64_t checksum = 0;
    for (int64_t k = 0; k < n; k += 1000) {
        checksum += arr[k];
    }
    printf("sorted %ld elements, checksum: %ld\n", (long)n, (long)checksum);
    free(arr);
    return 0;
}
