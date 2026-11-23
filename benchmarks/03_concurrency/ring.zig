const std = @import("std");
extern "c" fn printf(format: [*:0]const u8, ...) c_int;

const pthread_mutex_t = extern struct { __data: [40]u8 align(8) = @splat(0) };
const pthread_cond_t = extern struct { __data: [48]u8 align(8) = @splat(0) };

extern "c" fn pthread_mutex_init(mutex: *pthread_mutex_t, attr: ?*anyopaque) c_int;
extern "c" fn pthread_mutex_lock(mutex: *pthread_mutex_t) c_int;
extern "c" fn pthread_mutex_unlock(mutex: *pthread_mutex_t) c_int;
extern "c" fn pthread_mutex_destroy(mutex: *pthread_mutex_t) c_int;

extern "c" fn pthread_cond_init(cond: *pthread_cond_t, attr: ?*anyopaque) c_int;
extern "c" fn pthread_cond_wait(cond: *pthread_cond_t, mutex: *pthread_mutex_t) c_int;
extern "c" fn pthread_cond_signal(cond: *pthread_cond_t) c_int;
extern "c" fn pthread_cond_destroy(cond: *pthread_cond_t) c_int;

const Chan = struct {
    val: i64 = 0,
    has_item: bool = false,
    mutex: pthread_mutex_t = .{},
    not_empty: pthread_cond_t = .{},
    not_full: pthread_cond_t = .{},

    fn init(self: *Chan) void {
        _ = pthread_mutex_init(&self.mutex, null);
        _ = pthread_cond_init(&self.not_empty, null);
        _ = pthread_cond_init(&self.not_full, null);
    }

    fn deinit(self: *Chan) void {
        _ = pthread_mutex_destroy(&self.mutex);
        _ = pthread_cond_destroy(&self.not_empty);
        _ = pthread_cond_destroy(&self.not_full);
    }

    fn send(self: *Chan, v: i64) void {
        _ = pthread_mutex_lock(&self.mutex);
        while (self.has_item) {
            _ = pthread_cond_wait(&self.not_full, &self.mutex);
        }
        self.val = v;
        self.has_item = true;
        _ = pthread_cond_signal(&self.not_empty);
        _ = pthread_mutex_unlock(&self.mutex);
    }

    fn recv(self: *Chan) i64 {
        _ = pthread_mutex_lock(&self.mutex);
        while (!self.has_item) {
            _ = pthread_cond_wait(&self.not_empty, &self.mutex);
        }
        const v = self.val;
        self.has_item = false;
        _ = pthread_cond_signal(&self.not_full);
        _ = pthread_mutex_unlock(&self.mutex);
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
    for (&chans) |*c| c.init();
    defer for (&chans) |*c| c.deinit();

    var results: [4]usize = .{ 0, 0, 0, 0 };
    var threads: [4]std.Thread = undefined;

    for (0..4) |i| {
        threads[i] = try std.Thread.spawn(.{}, worker, .{ i, &chans[i], &chans[(i + 1) % 4], &results });
    }

    chans[0].send(passes);

    for (threads) |t| {
        t.join();
    }

    _ = printf("ring completed %ld passes: %zu %zu %zu %zu\n", passes, results[0], results[1], results[2], results[3]);
}
