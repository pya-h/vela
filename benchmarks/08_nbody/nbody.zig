const std = @import("std");

extern "c" fn printf(format: [*:0]const u8, ...) c_int;
extern "c" fn sqrt(x: f64) f64;

const SOLAR_MASS: f64 = 39.47841760435743;
const DAYS_PER_YEAR: f64 = 365.24;

const Body = struct {
    x: f64,
    y: f64,
    z: f64,
    vx: f64,
    vy: f64,
    vz: f64,
    mass: f64,
};

fn advance(b: *[5]Body, dt: f64) void {
    var i: usize = 0;
    while (i < 5) : (i += 1) {
        var j: usize = i + 1;
        while (j < 5) : (j += 1) {
            const dx = b[i].x - b[j].x;
            const dy = b[i].y - b[j].y;
            const dz = b[i].z - b[j].z;
            const dist2 = dx * dx + dy * dy + dz * dz;
            const dist = sqrt(dist2);
            const mag = dt / (dist2 * dist);
            b[i].vx -= dx * b[j].mass * mag;
            b[i].vy -= dy * b[j].mass * mag;
            b[i].vz -= dz * b[j].mass * mag;
            b[j].vx += dx * b[i].mass * mag;
            b[j].vy += dy * b[i].mass * mag;
            b[j].vz += dz * b[i].mass * mag;
        }
    }
    i = 0;
    while (i < 5) : (i += 1) {
        b[i].x += dt * b[i].vx;
        b[i].y += dt * b[i].vy;
        b[i].z += dt * b[i].vz;
    }
}

fn energy(b: *const [5]Body) f64 {
    var e: f64 = 0.0;
    var i: usize = 0;
    while (i < 5) : (i += 1) {
        e += 0.5 * b[i].mass * (b[i].vx * b[i].vx + b[i].vy * b[i].vy + b[i].vz * b[i].vz);
        var j: usize = i + 1;
        while (j < 5) : (j += 1) {
            const dx = b[i].x - b[j].x;
            const dy = b[i].y - b[j].y;
            const dz = b[i].z - b[j].z;
            const dist = sqrt(dx * dx + dy * dy + dz * dz);
            e -= (b[i].mass * b[j].mass) / dist;
        }
    }
    return e;
}

fn offset_momentum(b: *[5]Body) void {
    var px: f64 = 0.0;
    var py: f64 = 0.0;
    var pz: f64 = 0.0;
    var i: usize = 0;
    while (i < 5) : (i += 1) {
        px += b[i].vx * b[i].mass;
        py += b[i].vy * b[i].mass;
        pz += b[i].vz * b[i].mass;
    }
    b[0].vx = -px / SOLAR_MASS;
    b[0].vy = -py / SOLAR_MASS;
    b[0].vz = -pz / SOLAR_MASS;
}

pub fn main() void {
    var bodies = [5]Body{
        .{ .x = 0.0, .y = 0.0, .z = 0.0, .vx = 0.0, .vy = 0.0, .vz = 0.0, .mass = SOLAR_MASS },
        .{
            .x = 4.8414314424647209, .y = -1.1603200440274284, .z = -0.1036220444711231,
            .vx = 0.0016600766427440369 * DAYS_PER_YEAR, .vy = 0.0076990111848140506 * DAYS_PER_YEAR, .vz = -0.0000690460016972063 * DAYS_PER_YEAR,
            .mass = 0.0009547919384243266 * SOLAR_MASS,
        },
        .{
            .x = 8.34336641812488, .y = 4.124798564124305, .z = -0.4035234171143214,
            .vx = -0.002767425107268624 * DAYS_PER_YEAR, .vy = 0.004998528012349172 * DAYS_PER_YEAR, .vz = 0.00002304172975737639 * DAYS_PER_YEAR,
            .mass = 0.00028588567272224176 * SOLAR_MASS,
        },
        .{
            .x = 12.894369562139131, .y = -15.111151401698631, .z = -0.22330757889265573,
            .vx = 0.002964601375647616 * DAYS_PER_YEAR, .vy = 0.0023784717397656363 * DAYS_PER_YEAR, .vz = -0.000029658956854023756 * DAYS_PER_YEAR,
            .mass = 0.00004366244043351563 * SOLAR_MASS,
        },
        .{
            .x = 15.379697114850917, .y = -25.919314609987964, .z = 0.17925877295037118,
            .vx = 0.002680677724903893 * DAYS_PER_YEAR, .vy = 0.001628241700382423 * DAYS_PER_YEAR, .vz = -0.00009515922545197159 * DAYS_PER_YEAR,
            .mass = 0.000051513890204661145 * SOLAR_MASS,
        },
    };

    offset_momentum(&bodies);
    _ = printf("%lld\n", @as(i64, @intFromFloat(energy(&bodies) * 1000000.0)));
    var step: usize = 0;
    while (step < 5000000) : (step += 1) {
        advance(&bodies, 0.01);
    }
    _ = printf("%lld\n", @as(i64, @intFromFloat(energy(&bodies) * 1000000.0)));
}
