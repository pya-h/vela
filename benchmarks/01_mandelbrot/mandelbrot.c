#include <stdio.h>
#include <stdint.h>

static int64_t mandelbrot(int64_t size) {
    int64_t total = 0;
    double fsize = (double)size;
    for (int64_t y = 0; y < size; y++) {
        double ci = 2.0 * (double)y / fsize - 1.0;
        for (int64_t x = 0; x < size; x++) {
            double cr = 2.0 * (double)x / fsize - 1.5;
            double zr = 0.0;
            double zi = 0.0;
            int64_t iter = 0;
            while (iter < 50) {
                double zr2 = zr * zr;
                double zi2 = zi * zi;
                if (zr2 + zi2 > 4.0) {
                    break;
                }
                zi = 2.0 * zr * zi + ci;
                zr = zr2 - zi2 + cr;
                iter += 1;
            }
            total += iter;
        }
    }
    return total;
}

int main(int argc, char **argv) {
    int64_t size = argc > 1000 ? 0 : 2000;
    printf("%ld\n", (long)mandelbrot(size));
    return 0;
}
