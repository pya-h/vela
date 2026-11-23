#include <stdio.h>
#include <math.h>

#define SOLAR_MASS 39.47841760435743
#define DAYS_PER_YEAR 365.24

typedef struct {
    double x, y, z;
    double vx, vy, vz;
    double mass;
} Body;

static void advance(Body b[5], double dt) {
    for (int i = 0; i < 5; i++) {
        for (int j = i + 1; j < 5; j++) {
            double dx = b[i].x - b[j].x;
            double dy = b[i].y - b[j].y;
            double dz = b[i].z - b[j].z;
            double dist2 = dx * dx + dy * dy + dz * dz;
            double dist = sqrt(dist2);
            double mag = dt / (dist2 * dist);
            b[i].vx -= dx * b[j].mass * mag;
            b[i].vy -= dy * b[j].mass * mag;
            b[i].vz -= dz * b[j].mass * mag;
            b[j].vx += dx * b[i].mass * mag;
            b[j].vy += dy * b[i].mass * mag;
            b[j].vz += dz * b[i].mass * mag;
        }
    }
    for (int i = 0; i < 5; i++) {
        b[i].x += dt * b[i].vx;
        b[i].y += dt * b[i].vy;
        b[i].z += dt * b[i].vz;
    }
}

static double energy(const Body b[5]) {
    double e = 0.0;
    for (int i = 0; i < 5; i++) {
        e += 0.5 * b[i].mass * (b[i].vx * b[i].vx + b[i].vy * b[i].vy + b[i].vz * b[i].vz);
        for (int j = i + 1; j < 5; j++) {
            double dx = b[i].x - b[j].x;
            double dy = b[i].y - b[j].y;
            double dz = b[i].z - b[j].z;
            double dist = sqrt(dx * dx + dy * dy + dz * dz);
            e -= (b[i].mass * b[j].mass) / dist;
        }
    }
    return e;
}

static void offset_momentum(Body b[5]) {
    double px = 0.0, py = 0.0, pz = 0.0;
    for (int i = 0; i < 5; i++) {
        px += b[i].vx * b[i].mass;
        py += b[i].vy * b[i].mass;
        pz += b[i].vz * b[i].mass;
    }
    b[0].vx = -px / SOLAR_MASS;
    b[0].vy = -py / SOLAR_MASS;
    b[0].vz = -pz / SOLAR_MASS;
}

int main(void) {
    Body bodies[5] = {
        { 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, SOLAR_MASS },
        { 4.8414314424647209, -1.1603200440274284, -0.1036220444711231,
          0.0016600766427440369 * DAYS_PER_YEAR, 0.0076990111848140506 * DAYS_PER_YEAR, -0.0000690460016972063 * DAYS_PER_YEAR,
          0.0009547919384243266 * SOLAR_MASS },
        { 8.34336641812488, 4.124798564124305, -0.4035234171143214,
          -0.002767425107268624 * DAYS_PER_YEAR, 0.004998528012349172 * DAYS_PER_YEAR, 0.00002304172975737639 * DAYS_PER_YEAR,
          0.00028588567272224176 * SOLAR_MASS },
        { 12.894369562139131, -15.111151401698631, -0.22330757889265573,
          0.002964601375647616 * DAYS_PER_YEAR, 0.0023784717397656363 * DAYS_PER_YEAR, -0.000029658956854023756 * DAYS_PER_YEAR,
          0.00004366244043351563 * SOLAR_MASS },
        { 15.379697114850917, -25.919314609987964, 0.17925877295037118,
          0.002680677724903893 * DAYS_PER_YEAR, 0.001628241700382423 * DAYS_PER_YEAR, -0.00009515922545197159 * DAYS_PER_YEAR,
          0.000051513890204661145 * SOLAR_MASS }
    };

    offset_momentum(bodies);
    printf("%ld\n", (long)(energy(bodies) * 1000000.0));
    for (int step = 0; step < 5000000; step++) {
        advance(bodies, 0.01);
    }
    printf("%ld\n", (long)(energy(bodies) * 1000000.0));
    return 0;
}
