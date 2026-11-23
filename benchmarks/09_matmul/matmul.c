#include <stdio.h>
#include <stdlib.h>

static long matmul(int n) {
    int size = n * n;
    double *a = (double *)malloc(size * sizeof(double));
    double *b = (double *)malloc(size * sizeof(double));
    double *c = (double *)calloc(size, sizeof(double));

    for (int i = 0; i < n; i++) {
        int in_offset = i * n;
        for (int j = 0; j < n; j++) {
            a[in_offset + j] = (double)((i * 3 + j) % 100) / 10.0;
            b[in_offset + j] = (double)((i + j * 7) % 100) / 10.0;
        }
    }

    for (int i = 0; i < n; i++) {
        int in_offset = i * n;
        for (int k = 0; k < n; k++) {
            double a_val = a[in_offset + k];
            int kn_offset = k * n;
            for (int j = 0; j < n; j++) {
                c[in_offset + j] += a_val * b[kn_offset + j];
            }
        }
    }

    double trace = 0.0;
    for (int i = 0; i < n; i++) {
        trace += c[i * n + i];
    }

    free(a);
    free(b);
    free(c);
    return (long)trace;
}

int main(void) {
    printf("%ld\n", matmul(1000));
    return 0;
}
