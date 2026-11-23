const std = @import("std");
extern "c" fn printf(format: [*:0]const u8, ...) c_int;

pub fn main() !void {
    const alloc = std.heap.c_allocator;
    var counts = std.AutoHashMap(i64, i64).init(alloc);
    defer counts.deinit();

    var seed: i64 = 123456789;
    var i: usize = 0;
    while (i < 5000000) : (i += 1) {
        seed = @rem(seed * 1103515245 + 12345, 2147483648);
        const key = @rem(seed, 200000);
        const res = try counts.getOrPut(key);
        if (res.found_existing) {
            res.value_ptr.* += 1;
        } else {
            res.value_ptr.* = 1;
        }
    }

    var total: i64 = 0;
    var it = counts.valueIterator();
    while (it.next()) |v| {
        total += v.*;
    }
    _ = printf("unique: %u total: %ld\n", counts.count(), total);
}
