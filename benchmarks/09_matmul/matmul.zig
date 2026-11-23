const std = @import("std");

extern "c" fn printf(format: [*:0]const u8, ...) c_int;
extern "c" fn malloc(size: usize) ?*anyopaque;
extern "c" fn free(ptr: ?*anyopaque) void;

fn matmul(n: usize) i64 {
    const size = n * n;
    const a_ptr = malloc(size * @sizeOf(f64)) orelse unreachable;
    defer free(a_ptr);
    const a: [*]f64 = @ptrCast(@alignCast(a_ptr));

    const b_ptr = malloc(size * @sizeOf(f64)) orelse unreachable;
    defer free(b_ptr);
    const b: [*]f64 = @ptrCast(@alignCast(b_ptr));

    const c_ptr = malloc(size * @sizeOf(f64)) orelse unreachable;
    defer free(c_ptr);
    const c: [*]f64 = @ptrCast(@alignCast(c_ptr));

    var idx: usize = 0;
    while (idx < size) : (idx += 1) {
        c[idx] = 0.0;
    }

    var i: usize = 0;
    while (i < n) : (i += 1) {
        const in_offset = i * n;
        var j: usize = 0;
        while (j < n) : (j += 1) {
            a[in_offset + j] = @as(f64, @floatFromInt((i * 3 + j) % 100)) / 10.0;
            b[in_offset + j] = @as(f64, @floatFromInt((i + j * 7) % 100)) / 10.0;
        }
    }

    i = 0;
    while (i < n) : (i += 1) {
        const in_offset = i * n;
        var k: usize = 0;
        while (k < n) : (k += 1) {
            const a_val = a[in_offset + k];
            const kn_offset = k * n;
            var j: usize = 0;
            while (j < n) : (j += 1) {
                c[in_offset + j] += a_val * b[kn_offset + j];
            }
        }
    }

    var trace: f64 = 0.0;
    i = 0;
    while (i < n) : (i += 1) {
        trace += c[i * n + i];
    }
    return @intFromFloat(trace);
}

pub fn main() void {
    _ = printf("%lld\n", matmul(1000));
}
