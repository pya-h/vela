const std = @import("std");
extern "c" fn printf(format: [*:0]const u8, ...) c_int;

fn quicksort(items: []i64, low: isize, high: isize) void {
    if (low < high) {
        const pivot = items[@intCast(high)];
        var i = low - 1;
        var j = low;
        while (j < high) : (j += 1) {
            const uj: usize = @intCast(j);
            if (items[uj] <= pivot) {
                i += 1;
                const ui: usize = @intCast(i);
                const tmp = items[ui];
                items[ui] = items[uj];
                items[uj] = tmp;
            }
        }
        const u_pi: usize = @intCast(i + 1);
        const u_high: usize = @intCast(high);
        const tmp = items[u_pi];
        items[u_pi] = items[u_high];
        items[u_high] = tmp;
        const pi = i + 1;
        quicksort(items, low, pi - 1);
        quicksort(items, pi + 1, high);
    }
}

pub fn main() !void {
    const alloc = std.heap.c_allocator;
    const n: usize = 2500000;
    const arr = try alloc.alloc(i64, n);
    defer alloc.free(arr);

    var seed: i64 = 123456789;
    for (arr) |*elem| {
        seed = @rem(seed * 1103515245 + 12345, 2147483648);
        elem.* = seed;
    }

    quicksort(arr, 0, @as(isize, @intCast(n)) - 1);

    var checksum: i64 = 0;
    var k: usize = 0;
    while (k < n) : (k += 1000) {
        checksum += arr[k];
    }
    _ = printf("sorted %zu elements, checksum: %ld\n", n, checksum);
}
