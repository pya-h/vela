const std = @import("std");

const Chan = struct {
    val: i64 = 0,
    has_item: bool = false,
    mutex: std.Thread.Mutex = .{},
    not_empty: std.Thread.Condition = .{},
    not_full: std.Thread.Condition = .{},

    fn send(self: *Chan, v: i64) void {
        self.mutex.lock();
        defer self.mutex.unlock();
        while (self.has_item) {
            self.not_full.wait(&self.mutex);
        }
        self.val = v;
        self.has_item = true;
        self.not_empty.signal();
    }

    fn recv(self: *Chan) i64 {
        self.mutex.lock();
        defer self.mutex.unlock();
        while (!self.has_item) {
            self.not_empty.wait(&self.mutex);
        }
        const v = self.val;
        self.has_item = false;
        self.not_full.signal();
        return v;
    }
};

fn worker(id: usize, in_ch: *Chan, out_ch: *Chan, results: *[4]usize) void {
    while (true) {
        const token = in_ch.recv();
        if (token == 0) {
            out_ch.send(0);
            results[id] = id;
            return;
        }
        out_ch.send(token - 1);
    }
}

pub fn main() !void {
    const passes: i64 = 100000;
    var chans: [4]Chan = .{ .{}, .{}, .{}, .{} };
    var results: [4]usize = .{ 0, 0, 0, 0 };
    var threads: [4]std.Thread = undefined;

    for (0..4) |i| {
        threads[i] = try std.Thread.spawn(.{}, worker, .{ i, &chans[i], &chans[(i + 1) % 4], &results });
    }

    chans[0].send(passes);

    for (threads) |t| {
        t.join();
    }

    const stdout = std.io.getStdOut().writer();
    try stdout.print("ring completed {d} passes: {d} {d} {d} {d}\n", .{
        passes, results[0], results[1], results[2], results[3],
    });
}
