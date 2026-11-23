use std::env;

fn mandelbrot(size: i64) -> i64 {
    let mut total: i64 = 0;
    let fsize = size as f64;
    for y in 0..size {
        let ci = 2.0 * (y as f64) / fsize - 1.0;
        for x in 0..size {
            let cr = 2.0 * (x as f64) / fsize - 1.5;
            let mut zr = 0.0f64;
            let mut zi = 0.0f64;
            let mut iter: i64 = 0;
            while iter < 50 {
                let zr2 = zr * zr;
                let zi2 = zi * zi;
                if zr2 + zi2 > 4.0 {
                    break;
                }
                zi = 2.0 * zr * zi + ci;
                zr = zr2 - zi2 + cr;
                iter += 1;
            }
            total += iter;
        }
    }
    total
}

fn main() {
    let size: i64 = if env::args().len() > 1000 { 0 } else { 2000 };
    println!("{}", mandelbrot(size));
}
