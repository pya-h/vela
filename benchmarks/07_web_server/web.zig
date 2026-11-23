const std = @import("std");

const SockAddrIn = extern struct {
    sin_family: u16,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [8]u8 = @splat(0),
};

extern "c" fn socket(domain: c_int, sock_type: c_int, protocol: c_int) c_int;
extern "c" fn setsockopt(fd: c_int, level: c_int, optname: c_int, optval: *const anyopaque, optlen: u32) c_int;
extern "c" fn bind(fd: c_int, addr: *const SockAddrIn, addrlen: u32) c_int;
extern "c" fn listen(fd: c_int, backlog: c_int) c_int;
extern "c" fn accept(fd: c_int, addr: ?*anyopaque, addrlen: ?*u32) c_int;
extern "c" fn recv(fd: c_int, buf: [*]u8, len: usize, flags: c_int) isize;
extern "c" fn send(fd: c_int, buf: [*]const u8, len: usize, flags: c_int) isize;
extern "c" fn close(fd: c_int) c_int;

var server_fd: c_int = -1;

fn worker() void {
    const resp_json =
        "HTTP/1.1 200 OK\r\n" ++
        "Content-Type: application/json\r\n" ++
        "Content-Length: 26\r\n" ++
        "Connection: close\r\n\r\n" ++
        "{\"status\":\"ok\",\"value\":42}";
    const resp_health =
        "HTTP/1.1 200 OK\r\n" ++
        "Content-Type: text/plain\r\n" ++
        "Content-Length: 2\r\n" ++
        "Connection: close\r\n\r\n" ++
        "ok";
    const resp_404 =
        "HTTP/1.1 404 Not Found\r\n" ++
        "Content-Length: 9\r\n" ++
        "Connection: close\r\n\r\n" ++
        "not found";

    var buf: [2048]u8 = undefined;
    while (true) {
        const client = accept(server_fd, null, null);
        if (client < 0) continue;

        var total: usize = 0;
        while (total < buf.len) {
            const n = recv(client, buf[total..].ptr, buf.len - total, 0);
            if (n <= 0) break;
            total += @intCast(n);
            if (std.mem.indexOf(u8, buf[0..total], "\r\n\r\n") != null) break;
        }

        const req = buf[0..total];
        if (std.mem.startsWith(u8, req, "GET /json ")) {
            _ = send(client, resp_json.ptr, resp_json.len, 0);
        } else if (std.mem.startsWith(u8, req, "GET /health ")) {
            _ = send(client, resp_health.ptr, resp_health.len, 0);
        } else {
            _ = send(client, resp_404.ptr, resp_404.len, 0);
        }
        _ = close(client);
    }
}

pub fn main() !void {
    const AF_INET: c_int = 2;
    const SOCK_STREAM: c_int = 1;
    const SOL_SOCKET: c_int = 1;
    const SO_REUSEADDR: c_int = 2;

    server_fd = socket(AF_INET, SOCK_STREAM, 0);
    var yes: c_int = 1;
    _ = setsockopt(server_fd, SOL_SOCKET, SO_REUSEADDR, &yes, @sizeOf(c_int));

    var addr = SockAddrIn{
        .sin_family = 2,
        .sin_port = std.mem.nativeToBig(u16, 18260),
        .sin_addr = std.mem.nativeToBig(u32, 0x7f000001),
    };
    if (bind(server_fd, &addr, @sizeOf(SockAddrIn)) < 0) return error.BindFailed;
    if (listen(server_fd, 128) < 0) return error.ListenFailed;

    var threads: [4]std.Thread = undefined;
    for (0..4) |i| {
        threads[i] = try std.Thread.spawn(.{}, worker, .{});
    }
    for (threads) |t| {
        t.join();
    }
}
