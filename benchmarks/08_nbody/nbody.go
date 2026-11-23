package main

import (
	"fmt"
	"math"
)

const SOLAR_MASS = 39.47841760435743
const DAYS_PER_YEAR = 365.24

type Body struct {
	x, y, z    float64
	vx, vy, vz float64
	mass       float64
}

func advance(b *[5]Body, dt float64) {
	for i := 0; i < 5; i++ {
		for j := i + 1; j < 5; j++ {
			dx := b[i].x - b[j].x
			dy := b[i].y - b[j].y
			dz := b[i].z - b[j].z
			dist2 := dx*dx + dy*dy + dz*dz
			dist := math.Sqrt(dist2)
			mag := dt / (dist2 * dist)
			b[i].vx -= dx * b[j].mass * mag
			b[i].vy -= dy * b[j].mass * mag
			b[i].vz -= dz * b[j].mass * mag
			b[j].vx += dx * b[i].mass * mag
			b[j].vy += dy * b[i].mass * mag
			b[j].vz += dz * b[i].mass * mag
		}
	}
	for i := 0; i < 5; i++ {
		b[i].x += dt * b[i].vx
		b[i].y += dt * b[i].vy
		b[i].z += dt * b[i].vz
	}
}

func energy(b *[5]Body) float64 {
	e := 0.0
	for i := 0; i < 5; i++ {
		e += 0.5 * b[i].mass * (b[i].vx*b[i].vx + b[i].vy*b[i].vy + b[i].vz*b[i].vz)
		for j := i + 1; j < 5; j++ {
			dx := b[i].x - b[j].x
			dy := b[i].y - b[j].y
			dz := b[i].z - b[j].z
			dist := math.Sqrt(dx*dx + dy*dy + dz*dz)
			e -= (b[i].mass * b[j].mass) / dist
		}
	}
	return e
}

func offsetMomentum(b *[5]Body) {
	px, py, pz := 0.0, 0.0, 0.0
	for i := 0; i < 5; i++ {
		px += b[i].vx * b[i].mass
		py += b[i].vy * b[i].mass
		pz += b[i].vz * b[i].mass
	}
	b[0].vx = -px / SOLAR_MASS
	b[0].vy = -py / SOLAR_MASS
	b[0].vz = -pz / SOLAR_MASS
}

func main() {
	bodies := [5]Body{
		{x: 0.0, y: 0.0, z: 0.0, vx: 0.0, vy: 0.0, vz: 0.0, mass: SOLAR_MASS},
		{
			x: 4.8414314424647209, y: -1.1603200440274284, z: -0.1036220444711231,
			vx: 0.0016600766427440369 * DAYS_PER_YEAR, vy: 0.0076990111848140506 * DAYS_PER_YEAR, vz: -0.0000690460016972063 * DAYS_PER_YEAR,
			mass: 0.0009547919384243266 * SOLAR_MASS,
		},
		{
			x: 8.34336641812488, y: 4.124798564124305, z: -0.4035234171143214,
			vx: -0.002767425107268624 * DAYS_PER_YEAR, vy: 0.004998528012349172 * DAYS_PER_YEAR, vz: 0.00002304172975737639 * DAYS_PER_YEAR,
			mass: 0.00028588567272224176 * SOLAR_MASS,
		},
		{
			x: 12.894369562139131, y: -15.111151401698631, z: -0.22330757889265573,
			vx: 0.002964601375647616 * DAYS_PER_YEAR, vy: 0.0023784717397656363 * DAYS_PER_YEAR, vz: -0.000029658956854023756 * DAYS_PER_YEAR,
			mass: 0.00004366244043351563 * SOLAR_MASS,
		},
		{
			x: 15.379697114850917, y: -25.919314609987964, z: 0.17925877295037118,
			vx: 0.002680677724903893 * DAYS_PER_YEAR, vy: 0.001628241700382423 * DAYS_PER_YEAR, vz: -0.00009515922545197159 * DAYS_PER_YEAR,
			mass: 0.000051513890204661145 * SOLAR_MASS,
		},
	}

	offsetMomentum(&bodies)
	fmt.Println(int64(energy(&bodies) * 1000000.0))
	for step := 0; step < 5000000; step++ {
		advance(&bodies, 0.01)
	}
	fmt.Println(int64(energy(&bodies) * 1000000.0))
}
