const std = @import("std");
extern "c" fn printf(format: [*:0]const u8, ...) c_int;

const Tree = union(enum) {
    leaf: i64,
    branch: struct {
        left: *Tree,
        right: *Tree,
    },
};

fn build(alloc: std.mem.Allocator, depth: i32, seed: i64) !Tree {
    if (depth == 0) {
        return Tree{ .leaf = @rem(seed, 10) };
    }
    const left = try alloc.create(Tree);
    left.* = try build(alloc, depth - 1, seed * 3 + 1);
    const right = try alloc.create(Tree);
    right.* = try build(alloc, depth - 1, seed * 7 + 2);
    return Tree{ .branch = .{ .left = left, .right = right } };
}

fn total(t: Tree) i64 {
    return switch (t) {
        .leaf => |v| v,
        .branch => |b| total(b.left.*) + total(b.right.*),
    };
}

fn stretchCheck(parent: std.mem.Allocator, stretch_depth: i32) !i64 {
    var arena = std.heap.ArenaAllocator.init(parent);
    defer arena.deinit();
    const tree = try build(arena.allocator(), stretch_depth, 0);
    return total(tree);
}

pub fn main() !void {
    const base_alloc = std.heap.c_allocator;
    const max_depth: i32 = 16;
    const stretch_depth = max_depth + 1;

    const stretch_sum = try stretchCheck(base_alloc, stretch_depth);
    _ = printf("stretch tree of depth %d check: %ld\n", stretch_depth, stretch_sum);

    var long_arena = std.heap.ArenaAllocator.init(base_alloc);
    defer long_arena.deinit();
    const long_tree = try build(long_arena.allocator(), max_depth, 0);

    var depth: i32 = 4;
    while (depth <= max_depth) : (depth += 2) {
        const shift: u6 = @intCast(max_depth - depth + 4);
        const iterations: i64 = @as(i64, 1) << shift;
        var sum: i64 = 0;
        var i: i64 = 0;
        var pass_arena = std.heap.ArenaAllocator.init(base_alloc);
        defer pass_arena.deinit();
        while (i < iterations) : (i += 1) {
            _ = pass_arena.reset(.retain_capacity);
            const t = try build(pass_arena.allocator(), depth, i);
            sum += total(t);
        }
        _ = printf("%ld trees of depth %d check: %ld\n", iterations, depth, sum);
    }

    _ = printf("long lived tree of depth %d check: %ld\n", max_depth, total(long_tree));
}
