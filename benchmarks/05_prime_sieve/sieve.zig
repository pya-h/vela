const std = @import("std");
extern "c" fn printf(format: [*:0]const u8, ...) c_int;

fn countPrimes(limit: usize) !usize {
    const alloc = std.heap.c_allocator;
    const is_prime = try alloc.alloc(bool, limit);
    defer alloc.free(is_prime);
    @memset(is_prime, true);
    is_prime[0] = false;
    is_prime[1] = false;
    var p: usize = 2;
    while (p * p < limit) : (p += 1) {
        if (is_prime[p]) {
            var multiple = p * p;
            while (multiple < limit) : (multiple += p) {
                is_prime[multiple] = false;
            }
        }
    }
    var count: usize = 0;
    for (is_prime) |prime| {
        if (prime) count += 1;
    }
    return count;
}

pub fn main() !void {
    const count = try countPrimes(30000000);
    _ = printf("primes count: %zu\n", count);
}
