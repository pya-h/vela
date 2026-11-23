extern "c" fn printf(format: [*:0]const u8, ...) c_int;

fn mandelbrot(size: i64) i64 {
    var total: i64 = 0;
    const fsize: f64 = @floatFromInt(size);
    var y: i64 = 0;
    while (y < size) : (y += 1) {
        const ci: f64 = 2.0 * @as(f64, @floatFromInt(y)) / fsize - 1.0;
        var x: i64 = 0;
        while (x < size) : (x += 1) {
            const cr: f64 = 2.0 * @as(f64, @floatFromInt(x)) / fsize - 1.5;
            var zr: f64 = 0.0;
            var zi: f64 = 0.0;
            var iter: i64 = 0;
            while (iter < 50) : (iter += 1) {
                const zr2 = zr * zr;
                const zi2 = zi * zi;
                if (zr2 + zi2 > 4.0) break;
                zi = 2.0 * zr * zi + ci;
                zr = zr2 - zi2 + cr;
            }
            total += iter;
        }
    }
    return total;
}

pub fn main() void {
    _ = printf("%ld\n", mandelbrot(2000));
}
