#!/usr/bin/env python3
"""
Vela Performance & Resource Benchmark Suite
Compares Vela against C, Rust, Go, and Zig across 9 distinct runtime & web workloads.
Generates both RESULTS.md and an interactive RESULTS.html web dashboard with live code previews.
"""

import argparse
import json
import os
import re
import shutil
import socket
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT_DIR = Path(__file__).resolve().parent.parent
BENCH_DIR = ROOT_DIR / "benchmarks"
BUILD_DIR = BENCH_DIR / "build"
HTTP_LOAD_SRC = BENCH_DIR / "tools" / "http_load.c"

LANGUAGES = ["c", "rust", "go", "zig", "vela"]

SNIPPETS = {
    "01_mandelbrot": {
        "go": """func mandelbrot(size int) int {
	total := 0
	fsize := float64(size)
	for y := 0; y < size; y++ {
		ci := 2.0*float64(y)/fsize - 1.0
		for x := 0; x < size; x++ {
			cr := 2.0*float64(x)/fsize - 1.5
			zr, zi := 0.0, 0.0
			iter := 0
			for iter < 50 {
				zr2, zi2 := zr*zr, zi*zi
				if zr2+zi2 > 4.0 {
					break
				}
				zi = 2.0*zr*zi + ci
				zr = zr2 - zi2 + cr
				iter++
			}
			total += iter
		}
	}
	return total
}""",
        "vela": """task mandelbrot(size: int) -> int
    flex total = 0
    fixed fsize = float(size)
    each y in 0..size
        fixed ci = 2.0 * float(y) / fsize - 1.0
        each x in 0..size
            fixed cr = 2.0 * float(x) / fsize - 1.5
            flex zr = 0.0, zi = 0.0, iter = 0
            loop iter < 50
                fixed zr2 = zr * zr
                fixed zi2 = zi * zi
                when zr2 + zi2 > 4.0
                    break
                fi
                zi = 2.0 * zr * zi + ci
                zr = zr2 - zi2 + cr
                iter += 1
            fi
            total += iter
        fi
    fi
    <- total
fi""",
        "rust": """fn mandelbrot(size: usize) -> i64 {
    let mut total = 0i64;
    let fsize = size as f64;
    for y in 0..size {
        let ci = 2.0 * (y as f64) / fsize - 1.0;
        for x in 0..size {
            let cr = 2.0 * (x as f64) / fsize - 1.5;
            let (mut zr, mut zi, mut iter) = (0.0f64, 0.0f64, 0);
            while iter < 50 {
                let (zr2, zi2) = (zr * zr, zi * zi);
                if zr2 + zi2 > 4.0 { break; }
                zi = 2.0 * zr * zi + ci;
                zr = zr2 - zi2 + cr;
                iter += 1;
            }
            total += iter as i64;
        }
    }
    total
}""",
        "zig": """fn mandelbrot(size: usize) i64 {
    var total: i64 = 0;
    const fsize: f64 = @floatFromInt(size);
    var y: usize = 0;
    while (y < size) : (y += 1) {
        const ci = 2.0 * @as(f64, @floatFromInt(y)) / fsize - 1.0;
        var x: usize = 0;
        while (x < size) : (x += 1) {
            const cr = 2.0 * @as(f64, @floatFromInt(x)) / fsize - 1.5;
            var zr: f64 = 0.0; var zi: f64 = 0.0; var iter: i32 = 0;
            while (iter < 50) : (iter += 1) {
                const zr2 = zr * zr; const zi2 = zi * zi;
                if (zr2 + zi2 > 4.0) break;
                zi = 2.0 * zr * zi + ci;
                zr = zr2 - zi2 + cr;
            }
            total += iter;
        }
    }
    return total;
}""",
        "c": """long mandelbrot(int size) {
    long total = 0;
    double fsize = (double)size;
    for (int y = 0; y < size; y++) {
        double ci = 2.0 * (double)y / fsize - 1.0;
        for (int x = 0; x < size; x++) {
            double cr = 2.0 * (double)x / fsize - 1.5;
            double zr = 0.0, zi = 0.0;
            int iter = 0;
            for (; iter < 50; iter++) {
                double zr2 = zr * zr, zi2 = zi * zi;
                if (zr2 + zi2 > 4.0) break;
                zi = 2.0 * zr * zi + ci;
                zr = zr2 - zi2 + cr;
            }
            total += iter;
        }
    }
    return total;
}""",
    },
    "02_binary_trees": {
        "go": """type Node struct {
	left, right *Node
	item        int
}

func buildTree(item, depth int) *Node {
	if depth <= 0 {
		return &Node{item: item}
	}
	return &Node{
		left:  buildTree(2*item-1, depth-1),
		right: buildTree(2*item, depth-1),
		item:  item,
	}
}

func checkTree(n *Node) int {
	if n.left == nil {
		return n.item
	}
	return n.item + checkTree(n.left) - checkTree(n.right)
}""",
        "vela": """shape Node
    item: int
    left: maybe ^Node
    right: maybe ^Node
fi

task build_tree(item: int, depth: int, r: region) -> ^Node@r
    when depth <= 0
        <- new Node(Node { item: item, left: none, right: none }) at r
    fi
    <- new Node(Node {
        item: item,
        left: { build_tree(2 * item - 1, depth - 1, r) },
        right: { build_tree(2 * item, depth - 1, r) }
    }) at r
fi

task check_tree(n: ^Node) -> int
    match n^.left
    case { l }
        fixed r = n^.right ?? fail "unbalanced tree"
        <- n^.item + check_tree(l) - check_tree(r)
    default
        <- n^.item
    fi
fi""",
        "rust": """struct Node {
    left: Option<Box<Node>>,
    right: Option<Box<Node>>,
    item: i32,
}

fn build_tree(item: i32, depth: u32) -> Box<Node> {
    if depth == 0 {
        Box::new(Node { item, left: None, right: None })
    } else {
        Box::new(Node {
            item,
            left: Some(build_tree(2 * item - 1, depth - 1)),
            right: Some(build_tree(2 * item, depth - 1)),
        })
    }
}

fn check_tree(node: &Node) -> i32 {
    match (&node.left, &node.right) {
        (Some(l), Some(r)) => node.item + check_tree(l) - check_tree(r),
        _ => node.item,
    }
}""",
        "zig": """const Node = struct {
    left: ?*Node,
    right: ?*Node,
    item: i32,
};

fn buildTree(allocator: std.mem.Allocator, item: i32, depth: u32) *Node {
    const node = allocator.create(Node) catch unreachable;
    node.item = item;
    if (depth <= 0) {
        node.left = null; node.right = null;
    } else {
        node.left = buildTree(allocator, 2 * item - 1, depth - 1);
        node.right = buildTree(allocator, 2 * item, depth - 1);
    }
    return node;
}

fn checkTree(node: *const Node) i32 {
    if (node.left) |l| {
        return node.item + checkTree(l) - checkTree(node.right.?);
    }
    return node.item;
}""",
        "c": """typedef struct Node {
    struct Node *left, *right;
    int item;
} Node;

static Node *build_tree(int item, int depth, Arena *arena) {
    Node *n = arena_alloc(arena, sizeof(Node));
    n->item = item;
    if (depth <= 0) {
        n->left = n->right = NULL;
    } else {
        n->left = build_tree(2 * item - 1, depth - 1, arena);
        n->right = build_tree(2 * item, depth - 1, arena);
    }
    return n;
}

static int check_tree(const Node *n) {
    if (!n->left) return n->item;
    return n->item + check_tree(n->left) - check_tree(n->right);
}""",
    },
    "03_concurrency": {
        "go": """func worker(id int, in <-chan int, out chan<- int, wg *sync.WaitGroup) {
	defer wg.Done()
	for token := range in {
		if token <= 0 {
			out <- token
			return
		}
		out <- token - 1
	}
}

func runRing(passes int) {
	chans := [4]chan int{make(chan int, 1), make(chan int, 1), make(chan int, 1), make(chan int, 1)}
	var wg sync.WaitGroup
	for i := 0; i < 4; i++ {
		wg.Add(1)
		go worker(i, chans[i], chans[(i+1)%4], &wg)
	}
	chans[0] <- passes
	wg.Wait()
}""",
        "vela": """task worker(id: int, in_ch: chan<int>, out_ch: chan<int>, done_ch: chan<int>)
    loop
        fixed token = recv(in_ch)
        when token <= 0
            send(out_ch, token)
            send(done_ch, id)
            break
        fi
        send(out_ch, token - 1)
    fi
fi

task run_ring(passes: int)
    fixed ch0 = chan<int>(1), ch1 = chan<int>(1)
    fixed ch2 = chan<int>(1), ch3 = chan<int>(1)
    fixed done = chan<int>(4)
    start worker(0, ch0, ch1, done)
    start worker(1, ch1, ch2, done)
    start worker(2, ch2, ch3, done)
    start worker(3, ch3, ch0, done)
    send(ch0, passes)
    each _ in 0..4 { recv(done) }
fi""",
        "rust": """fn worker(rx: Receiver<i32>, tx: SyncSender<i32>) {
    while let Ok(token) = rx.recv() {
        if token <= 0 {
            let _ = tx.send(token);
            break;
        }
        let _ = tx.send(token - 1);
    }
}""",
        "zig": """fn worker(ctx: *WorkerContext) void {
    while (true) {
        const token = ctx.in_ring.pop();
        if (token <= 0) {
            ctx.out_ring.push(token);
            break;
        }
        ctx.out_ring.push(token - 1);
    }
}""",
        "c": """static void *worker(void *arg) {
    WorkerContext *ctx = (WorkerContext *)arg;
    while (1) {
        long token = queue_pop(ctx->in_q);
        if (token <= 0) {
            queue_push(ctx->out_q, token);
            break;
        }
        queue_push(ctx->out_q, token - 1);
    }
    return NULL;
}""",
    },
    "04_quicksort": {
        "go": """func partition(arr []int64, low, high int) int {
	pivot := arr[high]
	i := low - 1
	for j := low; j < high; j++ {
		if arr[j] <= pivot {
			i++
			arr[i], arr[j] = arr[j], arr[i]
		}
	}
	arr[i+1], arr[high] = arr[high], arr[i+1]
	return i + 1
}

func quicksort(arr []int64, low, high int) {
	if low < high {
		pi := partition(arr, low, high)
		quicksort(arr, low, pi-1)
		quicksort(arr, pi+1, high)
	}
}""",
        "vela": """task partition(flex xs: [int], low: int, high: int) -> int
    fixed pivot = xs[high]
    flex i = low - 1
    each j in low..high
        when xs[j] <= pivot
            i += 1
            xs[i], xs[j] = xs[j], xs[i]
        fi
    fi
    xs[i + 1], xs[high] = xs[high], xs[i + 1]
    <- i + 1
fi

task quicksort(flex xs: [int], low: int, high: int)
    when low < high
        fixed p = partition(xs, low, high)
        quicksort(xs, low, p - 1)
        quicksort(xs, p + 1, high)
    fi
fi""",
        "rust": """fn partition(arr: &mut [i64], low: isize, high: isize) -> isize {
    let pivot = arr[high as usize];
    let mut i = low - 1;
    for j in low..high {
        if arr[j as usize] <= pivot {
            i += 1;
            arr.swap(i as usize, j as usize);
        }
    }
    arr.swap((i + 1) as usize, high as usize);
    i + 1
}

fn quicksort(arr: &mut [i64], low: isize, high: isize) {
    if low < high {
        let p = partition(arr, low, high);
        quicksort(arr, low, p - 1);
        quicksort(arr, p + 1, high);
    }
}""",
        "zig": """fn partition(arr: []i64, low: isize, high: isize) isize {
    const pivot = arr[@intCast(high)];
    var i = low - 1;
    var j = low;
    while (j < high) : (j += 1) {
        if (arr[@intCast(j)] <= pivot) {
            i += 1;
            const tmp = arr[@intCast(i)];
            arr[@intCast(i)] = arr[@intCast(j)];
            arr[@intCast(j)] = tmp;
        }
    }
    const tmp = arr[@intCast(i + 1)];
    arr[@intCast(i + 1)] = arr[@intCast(high)];
    arr[@intCast(high)] = tmp;
    return i + 1;
}""",
        "c": """static int partition(int64_t *arr, int low, int high) {
    int64_t pivot = arr[high];
    int i = low - 1;
    for (int j = low; j < high; j++) {
        if (arr[j] <= pivot) {
            i++;
            int64_t tmp = arr[i]; arr[i] = arr[j]; arr[j] = tmp;
        }
    }
    int64_t tmp = arr[i + 1]; arr[i + 1] = arr[high]; arr[high] = tmp;
    return i + 1;
}

static void quicksort(int64_t *arr, int low, int high) {
    if (low < high) {
        int pi = partition(arr, low, high);
        quicksort(arr, low, pi - 1);
        quicksort(arr, pi + 1, high);
    }
}""",
    },
    "05_prime_sieve": {
        "go": """func sieve(limit int) int {
	isPrime := make([]byte, limit+1)
	for i := 2; i <= limit; i++ {
		isPrime[i] = 1
	}
	p := 2
	for p*p <= limit {
		if isPrime[p] == 1 {
			for i := p * p; i <= limit; i += p {
				isPrime[i] = 0
			}
		}
		p++
	}
	count := 0
	for i := 2; i <= limit; i++ {
		if isPrime[i] == 1 { count++ }
	}
	return count
}""",
        "vela": """task sieve(limit: int) -> int
    region r
    flex is_prime = new [int8]([1i8 # (limit + 1)]) at r
    is_prime[0] = 0i8
    is_prime[1] = 0i8
    flex p = 2
    loop p * p <= limit
        when is_prime[p] == 1i8
            flex i = p * p
            loop i <= limit
                is_prime[i] = 0i8
                i += p
            fi
        fi
        p += 1
    fi
    flex count = 0
    each x in is_prime { when x == 1i8 { count += 1 } }
    <- count
fi""",
        "rust": """fn sieve(limit: usize) -> usize {
    let mut is_prime = vec![1u8; limit + 1];
    is_prime[0] = 0; is_prime[1] = 0;
    let mut p = 2;
    while p * p <= limit {
        if is_prime[p] == 1 {
            let mut i = p * p;
            while i <= limit {
                is_prime[i] = 0;
                i += p;
            }
        }
        p += 1;
    }
    is_prime.iter().filter(|&&x| x == 1).count()
}""",
        "zig": """fn sieve(limit: usize, buf: [*]u8) usize {
    @memset(buf[0..limit + 1], 1);
    buf[0] = 0; buf[1] = 0;
    var p: usize = 2;
    while (p * p <= limit) : (p += 1) {
        if (buf[p] == 1) {
            var i = p * p;
            while (i <= limit) : (i += p) {
                buf[i] = 0;
            }
        }
    }
    var count: usize = 0;
    for (buf[0..limit + 1]) |val| { if (val == 1) count += 1; }
    return count;
}""",
        "c": """static int sieve(int limit) {
    uint8_t *is_prime = (uint8_t *)malloc(limit + 1);
    memset(is_prime, 1, limit + 1);
    is_prime[0] = is_prime[1] = 0;
    for (int p = 2; p * p <= limit; p++) {
        if (is_prime[p]) {
            for (int i = p * p; i <= limit; i += p) {
                is_prime[i] = 0;
            }
        }
    }
    int count = 0;
    for (int i = 2; i <= limit; i++) { if (is_prime[i]) count++; }
    free(is_prime);
    return count;
}""",
    },
    "06_hashmap": {
        "go": """func runHashMap(iterations int, keys int) int64 {
	m := make(map[int]int64, keys)
	var rng uint32 = 123456789
	var sum int64 = 0
	for i := 0; i < iterations; i++ {
		rng = rng*1664525 + 1013904223
		k := int(rng % uint32(keys))
		if (rng >> 31) == 0 {
			m[k] = int64(i)
		} else {
			if v, ok := m[k]; ok {
				sum += v
			}
		}
	}
	return sum
}""",
        "vela": """task run_hashmap(iterations: int, keys: int) -> int
    region r
    flex m: map<int, int>@r = map<int, int>() at r
    flex rng: uint32 = 123456789u32
    flex sum = 0
    each i in 0..iterations
        rng = rng * 1664525u32 + 1013904223u32
        fixed k = int(rng % uint32(keys))
        when (rng >> 31u32) == 0u32
            m[k] = i
        else
            match m[k]
            case { v }
                sum += v
            fi
        fi
    fi
    <- sum
fi""",
        "rust": """fn run_hashmap(iterations: usize, keys: usize) -> i64 {
    let mut map: HashMap<usize, i64> = HashMap::with_capacity(keys);
    let mut rng = 123456789u32;
    let mut sum = 0i64;
    for i in 0..iterations {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        let k = (rng as usize) % keys;
        if (rng >> 31) == 0 {
            map.insert(k, i as i64);
        } else if let Some(&v) = map.get(&k) {
            sum += v;
        }
    }
    sum
}""",
        "zig": """fn runHashMap(allocator: std.mem.Allocator, iterations: usize, keys: usize) !i64 {
    var map = std.AutoHashMap(usize, i64).init(allocator);
    defer map.deinit();
    try map.ensureTotalCapacity(@intCast(keys));
    var rng: u32 = 123456789;
    var sum: i64 = 0;
    var i: usize = 0;
    while (i < iterations) : (i += 1) {
        rng = rng *% 1664525 +% 1013904223;
        const k = @as(usize, rng) % keys;
        if ((rng >> 31) == 0) {
            try map.put(k, @intCast(i));
        } else if (map.get(k)) |v| {
            sum += v;
        }
    }
    return sum;
}""",
        "c": """static int64_t run_hashmap(int iterations, int keys) {
    HashMap map;
    hashmap_init(&map, keys * 2);
    uint32_t rng = 123456789;
    int64_t sum = 0;
    for (int i = 0; i < iterations; i++) {
        rng = rng * 1664525u + 1013904223u;
        int k = (int)(rng % (uint32_t)keys);
        if ((rng >> 31) == 0) {
            hashmap_put(&map, k, i);
        } else {
            int64_t val;
            if (hashmap_get(&map, k, &val)) { sum += val; }
        }
    }
    hashmap_free(&map);
    return sum;
}""",
    },
    "07_web_server": {
        "go": """func handleRequest(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path == "/health" {
		w.Header().Set("Content-Type", "text/plain")
		w.WriteHeader(http.StatusOK)
		w.Write([]byte("ok\\n"))
		return
	}
	if r.URL.Path == "/json" {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		json.NewEncoder(w).Encode(Response{Status: "ok", Value: 42})
		return
	}
	http.NotFound(w, r)
}""",
        "vela": """task handle_request(req: Request) -> Response
    when req.path == "/health"
        <- Response.text("ok\\n")
    fi
    when req.path == "/json"
        <- Response.json("{\\"status\\":\\"ok\\",\\"value\\":42}")
    fi
    <- Response.not_found()
fi

task main()
    fixed srv = WebServer.bind("127.0.0.1", 18260, workers: 4)
    srv.route(handle_request)
    srv.listen()
fi""",
        "rust": """fn handle_client(mut stream: TcpStream) {
    let mut buf = [0u8; 1024];
    if let Ok(n) = stream.read(&mut buf) {
        let req = String::from_utf8_lossy(&buf[..n]);
        if req.starts_with("GET /health") {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n");
        } else if req.starts_with("GET /json") {
            let body = b"{\\"status\\":\\"ok\\",\\"value\\":42}";
            let resp = format!("HTTP/1.1 200 OK\\r\\nContent-Length: {}\\r\\n\\r\\n", body.len());
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(body);
        }
    }
}""",
        "zig": """fn handleConnection(stream: std.net.Stream) void {
    var buf: [1024]u8 = undefined;
    const n = stream.read(&buf) catch return;
    const req = buf[0..n];
    if (std.mem.startsWith(u8, req, "GET /health")) {
        _ = stream.writeAll("HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n") catch {};
    } else if (std.mem.startsWith(u8, req, "GET /json")) {
        _ = stream.writeAll("HTTP/1.1 200 OK\\r\\nContent-Length: 26\\r\\n\\r\\n{\\"status\\":\\"ok\\",\\"value\\":42}") catch {};
    }
}""",
        "c": """static void handle_client(int fd) {
    char buf[1024];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    if (n <= 0) { close(fd); return; }
    buf[n] = '\\0';
    if (strncmp(buf, "GET /health", 11) == 0) {
        const char resp[] = "HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n";
        write(fd, resp, strlen(resp));
    } else if (strncmp(buf, "GET /json", 13) == 0) {
        const char resp[] = "HTTP/1.1 200 OK\\r\\nContent-Length: 26\\r\\n\\r\\n{\\"status\\":\\"ok\\",\\"value\\":42}";
        write(fd, resp, strlen(resp));
    }
    close(fd);
}""",
    },
    "08_nbody": {
        "go": """func advance(b *[5]Body, dt float64) {
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
}""",
        "vela": """task advance(flex& b: [Body#5], dt: float)
    each i in 0..5
        each j in (i + 1)..5
            fixed dx = b[i].x - b[j].x
            fixed dy = b[i].y - b[j].y
            fixed dz = b[i].z - b[j].z
            fixed dist2 = dx * dx + dy * dy + dz * dz
            fixed dist = dist2 ** 0.5
            fixed mag = dt / (dist2 * dist)
            b[i].vx -= dx * b[j].mass * mag
            b[i].vy -= dy * b[j].mass * mag
            b[i].vz -= dz * b[j].mass * mag
            b[j].vx += dx * b[i].mass * mag
            b[j].vy += dy * b[i].mass * mag
            b[j].vz += dz * b[i].mass * mag
        fi
    fi
    each i in 0..5
        b[i].x += dt * b[i].vx
        b[i].y += dt * b[i].vy
        b[i].z += dt * b[i].vz
    fi
fi""",
        "rust": """fn advance(b: &mut [Body; 5], dt: f64) {
    for i in 0..5 {
        for j in (i + 1)..5 {
            let dx = b[i].x - b[j].x;
            let dy = b[i].y - b[j].y;
            let dz = b[i].z - b[j].z;
            let dist2 = dx * dx + dy * dy + dz * dz;
            let dist = dist2.sqrt();
            let mag = dt / (dist2 * dist);
            b[i].vx -= dx * b[j].mass * mag;
            b[i].vy -= dy * b[j].mass * mag;
            b[i].vz -= dz * b[j].mass * mag;
            b[j].vx += dx * b[i].mass * mag;
            b[j].vy += dy * b[i].mass * mag;
            b[j].vz += dz * b[i].mass * mag;
        }
    }
    for i in 0..5 {
        b[i].x += dt * b[i].vx;
        b[i].y += dt * b[i].vy;
        b[i].z += dt * b[i].vz;
    }
}""",
        "zig": """fn advance(b: *[5]Body, dt: f64) void {
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
}""",
        "c": """static void advance(Body b[5], double dt) {
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
}""",
    },
    "09_matmul": {
        "go": """func matmul(n int) int64 {
	size := n * n
	a := make([]float64, size)
	b := make([]float64, size)
	c := make([]float64, size)

	// Cache-friendly i, k, j loop order:
	for i := 0; i < n; i++ {
		inOffset := i * n
		for k := 0; k < n; k++ {
			aVal := a[inOffset+k]
			knOffset := k * n
			for j := 0; j < n; j++ {
				c[inOffset+j] += aVal * b[knOffset+j]
			}
		}
	}
	trace := 0.0
	for i := 0; i < n; i++ { trace += c[i*n+i] }
	return int64(trace)
}""",
        "vela": """task matmul(n: int) -> int
    region r
    fixed size = n * n
    flex a = new [float]([0.0 # size]) at r
    flex b = new [float]([0.0 # size]) at r
    flex c = new [float]([0.0 # size]) at r

    ; Cache-friendly i, k, j loop order:
    each i in 0..n
        fixed in_offset = i * n
        each k in 0..n
            fixed a_val = a[in_offset + k]
            fixed kn_offset = k * n
            each j in 0..n
                c[in_offset + j] += a_val * b[kn_offset + j]
            fi
        fi
    fi
    flex trace = 0.0
    each i in 0..n { trace += c[i * n + i] }
    <- int(trace)
fi""",
        "rust": """fn matmul(n: usize) -> i64 {
    let size = n * n;
    let (mut a, mut b, mut c) = (vec![0.0f64; size], vec![0.0f64; size], vec![0.0f64; size]);

    // Cache-friendly i, k, j loop order:
    for i in 0..n {
        let in_offset = i * n;
        for k in 0..n {
            let a_val = a[in_offset + k];
            let kn_offset = k * n;
            for j in 0..n {
                c[in_offset + j] += a_val * b[kn_offset + j];
            }
        }
    }
    let mut trace = 0.0f64;
    for i in 0..n { trace += c[i * n + i]; }
    trace as i64
}""",
        "zig": """fn matmul(n: usize, a: [*]f64, b: [*]f64, c: [*]f64) i64 {
    var i: usize = 0;
    while (i < n) : (i += 1) {
        const in_offset = i * n;
        var k: usize = 0;
        while (k < n) : (k += 1) {
            const a_val = a[in_offset + k];
            const kn_offset = k * n;
            var j: usize = 0;
            while (j < n) : (j += 1) {
                c[in_offset + j] += a_val * b[kn_offset + j];
            }
        }
    }
    var trace: f64 = 0.0;
    i = 0;
    while (i < n) : (i += 1) { trace += c[i * n + i]; }
    return @intFromFloat(trace);
}""",
        "c": """static long matmul(int n, double *a, double *b, double *c) {
    // Cache-friendly i, k, j loop order:
    for (int i = 0; i < n; i++) {
        int in_offset = i * n;
        for (int k = 0; k < n; k++) {
            double a_val = a[in_offset + k];
            int kn_offset = k * n;
            for (int j = 0; j < n; j++) {
                c[in_offset + j] += a_val * b[kn_offset + j];
            }
        }
    }
    double trace = 0.0;
    for (int i = 0; i < n; i++) { trace += c[i * n + i]; }
    return (long)trace;
}""",
    },
}

SNIPPETS = {
    "01_mandelbrot": {
        "go": """func mandelbrot(size int) int {
	total := 0
	fsize := float64(size)
	for y := 0; y < size; y++ {
		ci := 2.0*float64(y)/fsize - 1.0
		for x := 0; x < size; x++ {
			cr := 2.0*float64(x)/fsize - 1.5
			zr, zi := 0.0, 0.0
			iter := 0
			for iter < 50 {
				zr2, zi2 := zr*zr, zi*zi
				if zr2+zi2 > 4.0 {
					break
				}
				zi = 2.0*zr*zi + ci
				zr = zr2 - zi2 + cr
				iter++
			}
			total += iter
		}
	}
	return total
}""",
        "vela": """task mandelbrot(size: int) -> int
    flex total = 0
    fixed fsize = float(size)
    each y in 0..size
        fixed ci = 2.0 * float(y) / fsize - 1.0
        each x in 0..size
            fixed cr = 2.0 * float(x) / fsize - 1.5
            flex zr = 0.0, zi = 0.0, iter = 0
            loop iter < 50
                fixed zr2 = zr * zr
                fixed zi2 = zi * zi
                when zr2 + zi2 > 4.0
                    break
                fi
                zi = 2.0 * zr * zi + ci
                zr = zr2 - zi2 + cr
                iter += 1
            fi
            total += iter
        fi
    fi
    <- total
fi""",
        "rust": """fn mandelbrot(size: usize) -> i64 {
    let mut total = 0i64;
    let fsize = size as f64;
    for y in 0..size {
        let ci = 2.0 * (y as f64) / fsize - 1.0;
        for x in 0..size {
            let cr = 2.0 * (x as f64) / fsize - 1.5;
            let (mut zr, mut zi, mut iter) = (0.0f64, 0.0f64, 0);
            while iter < 50 {
                let (zr2, zi2) = (zr * zr, zi * zi);
                if zr2 + zi2 > 4.0 { break; }
                zi = 2.0 * zr * zi + ci;
                zr = zr2 - zi2 + cr;
                iter += 1;
            }
            total += iter as i64;
        }
    }
    total
}""",
        "zig": """fn mandelbrot(size: usize) i64 {
    var total: i64 = 0;
    const fsize: f64 = @floatFromInt(size);
    var y: usize = 0;
    while (y < size) : (y += 1) {
        const ci = 2.0 * @as(f64, @floatFromInt(y)) / fsize - 1.0;
        var x: usize = 0;
        while (x < size) : (x += 1) {
            const cr = 2.0 * @as(f64, @floatFromInt(x)) / fsize - 1.5;
            var zr: f64 = 0.0; var zi: f64 = 0.0; var iter: i32 = 0;
            while (iter < 50) : (iter += 1) {
                const zr2 = zr * zr; const zi2 = zi * zi;
                if (zr2 + zi2 > 4.0) break;
                zi = 2.0 * zr * zi + ci;
                zr = zr2 - zi2 + cr;
            }
            total += iter;
        }
    }
    return total;
}""",
        "c": """long mandelbrot(int size) {
    long total = 0;
    double fsize = (double)size;
    for (int y = 0; y < size; y++) {
        double ci = 2.0 * (double)y / fsize - 1.0;
        for (int x = 0; x < size; x++) {
            double cr = 2.0 * (double)x / fsize - 1.5;
            double zr = 0.0, zi = 0.0;
            int iter = 0;
            for (; iter < 50; iter++) {
                double zr2 = zr * zr, zi2 = zi * zi;
                if (zr2 + zi2 > 4.0) break;
                zi = 2.0 * zr * zi + ci;
                zr = zr2 - zi2 + cr;
            }
            total += iter;
        }
    }
    return total;
}""",
    },
    "02_binary_trees": {
        "go": """type Node struct {
	left, right *Node
	item        int
}

func buildTree(item, depth int) *Node {
	if depth <= 0 {
		return &Node{item: item}
	}
	return &Node{
		left:  buildTree(2*item-1, depth-1),
		right: buildTree(2*item, depth-1),
		item:  item,
	}
}

func checkTree(n *Node) int {
	if n.left == nil {
		return n.item
	}
	return n.item + checkTree(n.left) - checkTree(n.right)
}""",
        "vela": """shape Node
    item: int
    left: maybe ^Node
    right: maybe ^Node
fi

task build_tree(item: int, depth: int, r: region) -> ^Node@r
    when depth <= 0
        <- new Node(Node { item: item, left: none, right: none }) at r
    fi
    <- new Node(Node {
        item: item,
        left: { build_tree(2 * item - 1, depth - 1, r) },
        right: { build_tree(2 * item, depth - 1, r) }
    }) at r
fi

task check_tree(n: ^Node) -> int
    match n^.left
    case { l }
        fixed r = n^.right ?? fail "unbalanced tree"
        <- n^.item + check_tree(l) - check_tree(r)
    default
        <- n^.item
    fi
fi""",
        "rust": """struct Node {
    left: Option<Box<Node>>,
    right: Option<Box<Node>>,
    item: i32,
}

fn build_tree(item: i32, depth: u32) -> Box<Node> {
    if depth == 0 {
        Box::new(Node { item, left: None, right: None })
    } else {
        Box::new(Node {
            item,
            left: Some(build_tree(2 * item - 1, depth - 1)),
            right: Some(build_tree(2 * item, depth - 1)),
        })
    }
}

fn check_tree(node: &Node) -> i32 {
    match (&node.left, &node.right) {
        (Some(l), Some(r)) => node.item + check_tree(l) - check_tree(r),
        _ => node.item,
    }
}""",
        "zig": """const Node = struct {
    left: ?*Node,
    right: ?*Node,
    item: i32,
};

fn buildTree(allocator: std.mem.Allocator, item: i32, depth: u32) *Node {
    const node = allocator.create(Node) catch unreachable;
    node.item = item;
    if (depth <= 0) {
        node.left = null; node.right = null;
    } else {
        node.left = buildTree(allocator, 2 * item - 1, depth - 1);
        node.right = buildTree(allocator, 2 * item, depth - 1);
    }
    return node;
}

fn checkTree(node: *const Node) i32 {
    if (node.left) |l| {
        return node.item + checkTree(l) - checkTree(node.right.?);
    }
    return node.item;
}""",
        "c": """typedef struct Node {
    struct Node *left, *right;
    int item;
} Node;

static Node *build_tree(int item, int depth, Arena *arena) {
    Node *n = arena_alloc(arena, sizeof(Node));
    n->item = item;
    if (depth <= 0) {
        n->left = n->right = NULL;
    } else {
        n->left = build_tree(2 * item - 1, depth - 1, arena);
        n->right = build_tree(2 * item, depth - 1, arena);
    }
    return n;
}

static int check_tree(const Node *n) {
    if (!n->left) return n->item;
    return n->item + check_tree(n->left) - check_tree(n->right);
}""",
    },
    "03_concurrency": {
        "go": """func worker(id int, in <-chan int, out chan<- int, wg *sync.WaitGroup) {
	defer wg.Done()
	for token := range in {
		if token <= 0 {
			out <- token
			return
		}
		out <- token - 1
	}
}

func runRing(passes int) {
	chans := [4]chan int{make(chan int, 1), make(chan int, 1), make(chan int, 1), make(chan int, 1)}
	var wg sync.WaitGroup
	for i := 0; i < 4; i++ {
		wg.Add(1)
		go worker(i, chans[i], chans[(i+1)%4], &wg)
	}
	chans[0] <- passes
	wg.Wait()
}""",
        "vela": """task worker(id: int, in_ch: chan<int>, out_ch: chan<int>, done_ch: chan<int>)
    loop
        fixed token = recv(in_ch)
        when token <= 0
            send(out_ch, token)
            send(done_ch, id)
            break
        fi
        send(out_ch, token - 1)
    fi
fi

task run_ring(passes: int)
    fixed ch0 = chan<int>(1), ch1 = chan<int>(1)
    fixed ch2 = chan<int>(1), ch3 = chan<int>(1)
    fixed done = chan<int>(4)
    start worker(0, ch0, ch1, done)
    start worker(1, ch1, ch2, done)
    start worker(2, ch2, ch3, done)
    start worker(3, ch3, ch0, done)
    send(ch0, passes)
    each _ in 0..4 { recv(done) }
fi""",
        "rust": """fn worker(rx: Receiver<i32>, tx: SyncSender<i32>) {
    while let Ok(token) = rx.recv() {
        if token <= 0 {
            let _ = tx.send(token);
            break;
        }
        let _ = tx.send(token - 1);
    }
}""",
        "zig": """fn worker(ctx: *WorkerContext) void {
    while (true) {
        const token = ctx.in_ring.pop();
        if (token <= 0) {
            ctx.out_ring.push(token);
            break;
        }
        ctx.out_ring.push(token - 1);
    }
}""",
        "c": """static void *worker(void *arg) {
    WorkerContext *ctx = (WorkerContext *)arg;
    while (1) {
        long token = queue_pop(ctx->in_q);
        if (token <= 0) {
            queue_push(ctx->out_q, token);
            break;
        }
        queue_push(ctx->out_q, token - 1);
    }
    return NULL;
}""",
    },
    "04_quicksort": {
        "go": """func partition(arr []int64, low, high int) int {
	pivot := arr[high]
	i := low - 1
	for j := low; j < high; j++ {
		if arr[j] <= pivot {
			i++
			arr[i], arr[j] = arr[j], arr[i]
		}
	}
	arr[i+1], arr[high] = arr[high], arr[i+1]
	return i + 1
}

func quicksort(arr []int64, low, high int) {
	if low < high {
		pi := partition(arr, low, high)
		quicksort(arr, low, pi-1)
		quicksort(arr, pi+1, high)
	}
}""",
        "vela": """task partition(flex xs: [int], low: int, high: int) -> int
    fixed pivot = xs[high]
    flex i = low - 1
    each j in low..high
        when xs[j] <= pivot
            i += 1
            xs[i], xs[j] = xs[j], xs[i]
        fi
    fi
    xs[i + 1], xs[high] = xs[high], xs[i + 1]
    <- i + 1
fi

task quicksort(flex xs: [int], low: int, high: int)
    when low < high
        fixed p = partition(xs, low, high)
        quicksort(xs, low, p - 1)
        quicksort(xs, p + 1, high)
    fi
fi""",
        "rust": """fn partition(arr: &mut [i64], low: isize, high: isize) -> isize {
    let pivot = arr[high as usize];
    let mut i = low - 1;
    for j in low..high {
        if arr[j as usize] <= pivot {
            i += 1;
            arr.swap(i as usize, j as usize);
        }
    }
    arr.swap((i + 1) as usize, high as usize);
    i + 1
}

fn quicksort(arr: &mut [i64], low: isize, high: isize) {
    if low < high {
        let p = partition(arr, low, high);
        quicksort(arr, low, p - 1);
        quicksort(arr, p + 1, high);
    }
}""",
        "zig": """fn partition(arr: []i64, low: isize, high: isize) isize {
    const pivot = arr[@intCast(high)];
    var i = low - 1;
    var j = low;
    while (j < high) : (j += 1) {
        if (arr[@intCast(j)] <= pivot) {
            i += 1;
            const tmp = arr[@intCast(i)];
            arr[@intCast(i)] = arr[@intCast(j)];
            arr[@intCast(j)] = tmp;
        }
    }
    const tmp = arr[@intCast(i + 1)];
    arr[@intCast(i + 1)] = arr[@intCast(high)];
    arr[@intCast(high)] = tmp;
    return i + 1;
}""",
        "c": """static int partition(int64_t *arr, int low, int high) {
    int64_t pivot = arr[high];
    int i = low - 1;
    for (int j = low; j < high; j++) {
        if (arr[j] <= pivot) {
            i++;
            int64_t tmp = arr[i]; arr[i] = arr[j]; arr[j] = tmp;
        }
    }
    int64_t tmp = arr[i + 1]; arr[i + 1] = arr[high]; arr[high] = tmp;
    return i + 1;
}

static void quicksort(int64_t *arr, int low, int high) {
    if (low < high) {
        int pi = partition(arr, low, high);
        quicksort(arr, low, pi - 1);
        quicksort(arr, pi + 1, high);
    }
}""",
    },
    "05_prime_sieve": {
        "go": """func sieve(limit int) int {
	isPrime := make([]byte, limit+1)
	for i := 2; i <= limit; i++ {
		isPrime[i] = 1
	}
	p := 2
	for p*p <= limit {
		if isPrime[p] == 1 {
			for i := p * p; i <= limit; i += p {
				isPrime[i] = 0
			}
		}
		p++
	}
	count := 0
	for i := 2; i <= limit; i++ {
		if isPrime[i] == 1 { count++ }
	}
	return count
}""",
        "vela": """task sieve(limit: int) -> int
    region r
    flex is_prime = new [int8]([1i8 # (limit + 1)]) at r
    is_prime[0] = 0i8
    is_prime[1] = 0i8
    flex p = 2
    loop p * p <= limit
        when is_prime[p] == 1i8
            flex i = p * p
            loop i <= limit
                is_prime[i] = 0i8
                i += p
            fi
        fi
        p += 1
    fi
    flex count = 0
    each x in is_prime { when x == 1i8 { count += 1 } }
    <- count
fi""",
        "rust": """fn sieve(limit: usize) -> usize {
    let mut is_prime = vec![1u8; limit + 1];
    is_prime[0] = 0; is_prime[1] = 0;
    let mut p = 2;
    while p * p <= limit {
        if is_prime[p] == 1 {
            let mut i = p * p;
            while i <= limit {
                is_prime[i] = 0;
                i += p;
            }
        }
        p += 1;
    }
    is_prime.iter().filter(|&&x| x == 1).count()
}""",
        "zig": """fn sieve(limit: usize, buf: [*]u8) usize {
    @memset(buf[0..limit + 1], 1);
    buf[0] = 0; buf[1] = 0;
    var p: usize = 2;
    while (p * p <= limit) : (p += 1) {
        if (buf[p] == 1) {
            var i = p * p;
            while (i <= limit) : (i += p) {
                buf[i] = 0;
            }
        }
    }
    var count: usize = 0;
    for (buf[0..limit + 1]) |val| { if (val == 1) count += 1; }
    return count;
}""",
        "c": """static int sieve(int limit) {
    uint8_t *is_prime = (uint8_t *)malloc(limit + 1);
    memset(is_prime, 1, limit + 1);
    is_prime[0] = is_prime[1] = 0;
    for (int p = 2; p * p <= limit; p++) {
        if (is_prime[p]) {
            for (int i = p * p; i <= limit; i += p) {
                is_prime[i] = 0;
            }
        }
    }
    int count = 0;
    for (int i = 2; i <= limit; i++) { if (is_prime[i]) count++; }
    free(is_prime);
    return count;
}""",
    },
    "06_hashmap": {
        "go": """func runHashMap(iterations int, keys int) int64 {
	m := make(map[int]int64, keys)
	var rng uint32 = 123456789
	var sum int64 = 0
	for i := 0; i < iterations; i++ {
		rng = rng*1664525 + 1013904223
		k := int(rng % uint32(keys))
		if (rng >> 31) == 0 {
			m[k] = int64(i)
		} else {
			if v, ok := m[k]; ok {
				sum += v
			}
		}
	}
	return sum
}""",
        "vela": """task run_hashmap(iterations: int, keys: int) -> int
    region r
    flex m: map<int, int>@r = map<int, int>() at r
    flex rng: uint32 = 123456789u32
    flex sum = 0
    each i in 0..iterations
        rng = rng * 1664525u32 + 1013904223u32
        fixed k = int(rng % uint32(keys))
        when (rng >> 31u32) == 0u32
            m[k] = i
        else
            match m[k]
            case { v }
                sum += v
            fi
        fi
    fi
    <- sum
fi""",
        "rust": """fn run_hashmap(iterations: usize, keys: usize) -> i64 {
    let mut map: HashMap<usize, i64> = HashMap::with_capacity(keys);
    let mut rng = 123456789u32;
    let mut sum = 0i64;
    for i in 0..iterations {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        let k = (rng as usize) % keys;
        if (rng >> 31) == 0 {
            map.insert(k, i as i64);
        } else if let Some(&v) = map.get(&k) {
            sum += v;
        }
    }
    sum
}""",
        "zig": """fn runHashMap(allocator: std.mem.Allocator, iterations: usize, keys: usize) !i64 {
    var map = std.AutoHashMap(usize, i64).init(allocator);
    defer map.deinit();
    try map.ensureTotalCapacity(@intCast(keys));
    var rng: u32 = 123456789;
    var sum: i64 = 0;
    var i: usize = 0;
    while (i < iterations) : (i += 1) {
        rng = rng *% 1664525 +% 1013904223;
        const k = @as(usize, rng) % keys;
        if ((rng >> 31) == 0) {
            try map.put(k, @intCast(i));
        } else if (map.get(k)) |v| {
            sum += v;
        }
    }
    return sum;
}""",
        "c": """static int64_t run_hashmap(int iterations, int keys) {
    HashMap map;
    hashmap_init(&map, keys * 2);
    uint32_t rng = 123456789;
    int64_t sum = 0;
    for (int i = 0; i < iterations; i++) {
        rng = rng * 1664525u + 1013904223u;
        int k = (int)(rng % (uint32_t)keys);
        if ((rng >> 31) == 0) {
            hashmap_put(&map, k, i);
        } else {
            int64_t val;
            if (hashmap_get(&map, k, &val)) { sum += val; }
        }
    }
    hashmap_free(&map);
    return sum;
}""",
    },
    "07_web_server": {
        "go": """func handleRequest(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path == "/health" {
		w.Header().Set("Content-Type", "text/plain")
		w.WriteHeader(http.StatusOK)
		w.Write([]byte("ok\\n"))
		return
	}
	if r.URL.Path == "/json" {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		json.NewEncoder(w).Encode(Response{Status: "ok", Value: 42})
		return
	}
	http.NotFound(w, r)
}""",
        "vela": """task handle_request(req: Request) -> Response
    when req.path == "/health"
        <- Response.text("ok\\n")
    fi
    when req.path == "/api/data"
        <- Response.json("{\\"status\\":\\"ok\\",\\"value\\":42}")
    fi
    <- Response.not_found()
fi

task main()
    fixed srv = WebServer.bind("127.0.0.1", 18260, workers: 4)
    srv.route(handle_request)
    srv.listen()
fi""",
        "rust": """fn handle_client(mut stream: TcpStream) {
    let mut buf = [0u8; 1024];
    if let Ok(n) = stream.read(&mut buf) {
        let req = String::from_utf8_lossy(&buf[..n]);
        if req.starts_with("GET /health") {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n");
        } else if req.starts_with("GET /api/data") {
            let body = b"{\\"status\\":\\"ok\\",\\"value\\":42}";
            let resp = format!("HTTP/1.1 200 OK\\r\\nContent-Length: {}\\r\\n\\r\\n", body.len());
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(body);
        }
    }
}""",
        "zig": """fn handleConnection(stream: std.net.Stream) void {
    var buf: [1024]u8 = undefined;
    const n = stream.read(&buf) catch return;
    const req = buf[0..n];
    if (std.mem.startsWith(u8, req, "GET /health")) {
        _ = stream.writeAll("HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n") catch {};
    } else if (std.mem.startsWith(u8, req, "GET /api/data")) {
        _ = stream.writeAll("HTTP/1.1 200 OK\\r\\nContent-Length: 26\\r\\n\\r\\n{\\"status\\":\\"ok\\",\\"value\\":42}") catch {};
    }
}""",
        "c": """static void handle_client(int fd) {
    char buf[1024];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    if (n <= 0) { close(fd); return; }
    buf[n] = '\\0';
    if (strncmp(buf, "GET /health", 11) == 0) {
        const char resp[] = "HTTP/1.1 200 OK\\r\\nContent-Length: 3\\r\\n\\r\\nok\\n";
        write(fd, resp, strlen(resp));
    } else if (strncmp(buf, "GET /api/data", 13) == 0) {
        const char resp[] = "HTTP/1.1 200 OK\\r\\nContent-Length: 26\\r\\n\\r\\n{\\"status\\":\\"ok\\",\\"value\\":42}";
        write(fd, resp, strlen(resp));
    }
    close(fd);
}""",
    },
    "08_nbody": {
        "go": """func advance(b *[5]Body, dt float64) {
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
}""",
        "vela": """task advance(flex& b: [Body#5], dt: float)
    each i in 0..5
        each j in (i + 1)..5
            fixed dx = b[i].x - b[j].x
            fixed dy = b[i].y - b[j].y
            fixed dz = b[i].z - b[j].z
            fixed dist2 = dx * dx + dy * dy + dz * dz
            fixed dist = dist2 ** 0.5
            fixed mag = dt / (dist2 * dist)
            b[i].vx -= dx * b[j].mass * mag
            b[i].vy -= dy * b[j].mass * mag
            b[i].vz -= dz * b[j].mass * mag
            b[j].vx += dx * b[i].mass * mag
            b[j].vy += dy * b[i].mass * mag
            b[j].vz += dz * b[i].mass * mag
        fi
    fi
    each i in 0..5
        b[i].x += dt * b[i].vx
        b[i].y += dt * b[i].vy
        b[i].z += dt * b[i].vz
    fi
fi""",
        "rust": """fn advance(b: &mut [Body; 5], dt: f64) {
    for i in 0..5 {
        for j in (i + 1)..5 {
            let dx = b[i].x - b[j].x;
            let dy = b[i].y - b[j].y;
            let dz = b[i].z - b[j].z;
            let dist2 = dx * dx + dy * dy + dz * dz;
            let dist = dist2.sqrt();
            let mag = dt / (dist2 * dist);
            b[i].vx -= dx * b[j].mass * mag;
            b[i].vy -= dy * b[j].mass * mag;
            b[i].vz -= dz * b[j].mass * mag;
            b[j].vx += dx * b[i].mass * mag;
            b[j].vy += dy * b[i].mass * mag;
            b[j].vz += dz * b[i].mass * mag;
        }
    }
    for i in 0..5 {
        b[i].x += dt * b[i].vx;
        b[i].y += dt * b[i].vy;
        b[i].z += dt * b[i].vz;
    }
}""",
        "zig": """fn advance(b: *[5]Body, dt: f64) void {
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
}""",
        "c": """static void advance(Body b[5], double dt) {
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
}""",
    },
    "09_matmul": {
        "go": """func matmul(n int) int64 {
	size := n * n
	a := make([]float64, size)
	b := make([]float64, size)
	c := make([]float64, size)

	// Cache-friendly i, k, j loop order:
	for i := 0; i < n; i++ {
		inOffset := i * n
		for k := 0; k < n; k++ {
			aVal := a[inOffset+k]
			knOffset := k * n
			for j := 0; j < n; j++ {
				c[inOffset+j] += aVal * b[knOffset+j]
			}
		}
	}
	trace := 0.0
	for i := 0; i < n; i++ { trace += c[i*n+i] }
	return int64(trace)
}""",
        "vela": """task matmul(n: int) -> int
    region r
    fixed size = n * n
    flex a = new [float]([0.0 # size]) at r
    flex b = new [float]([0.0 # size]) at r
    flex c = new [float]([0.0 # size]) at r

    ; Cache-friendly i, k, j loop order:
    each i in 0..n
        fixed in_offset = i * n
        each k in 0..n
            fixed a_val = a[in_offset + k]
            fixed kn_offset = k * n
            each j in 0..n
                c[in_offset + j] += a_val * b[kn_offset + j]
            fi
        fi
    fi
    flex trace = 0.0
    each i in 0..n { trace += c[i * n + i] }
    <- int(trace)
fi""",
        "rust": """fn matmul(n: usize) -> i64 {
    let size = n * n;
    let (mut a, mut b, mut c) = (vec![0.0f64; size], vec![0.0f64; size], vec![0.0f64; size]);

    // Cache-friendly i, k, j loop order:
    for i in 0..n {
        let in_offset = i * n;
        for k in 0..n {
            let a_val = a[in_offset + k];
            let kn_offset = k * n;
            for j in 0..n {
                c[in_offset + j] += a_val * b[kn_offset + j];
            }
        }
    }
    let mut trace = 0.0f64;
    for i in 0..n { trace += c[i * n + i]; }
    trace as i64
}""",
        "zig": """fn matmul(n: usize, a: [*]f64, b: [*]f64, c: [*]f64) i64 {
    var i: usize = 0;
    while (i < n) : (i += 1) {
        const in_offset = i * n;
        var k: usize = 0;
        while (k < n) : (k += 1) {
            const a_val = a[in_offset + k];
            const kn_offset = k * n;
            var j: usize = 0;
            while (j < n) : (j += 1) {
                c[in_offset + j] += a_val * b[kn_offset + j];
            }
        }
    }
    var trace: f64 = 0.0;
    i = 0;
    while (i < n) : (i += 1) { trace += c[i * n + i]; }
    return @intFromFloat(trace);
}""",
        "c": """static long matmul(int n, double *a, double *b, double *c) {
    // Cache-friendly i, k, j loop order:
    for (int i = 0; i < n; i++) {
        int in_offset = i * n;
        for (int k = 0; k < n; k++) {
            double a_val = a[in_offset + k];
            int kn_offset = k * n;
            for (int j = 0; j < n; j++) {
                c[in_offset + j] += a_val * b[kn_offset + j];
            }
        }
    }
    double trace = 0.0;
    for (int i = 0; i < n; i++) { trace += c[i * n + i]; }
    return (long)trace;
}""",
    },
}

BENCHMARKS = [
    {
        "id": "01_mandelbrot",
        "name": "Mandelbrot",
        "category": "CPU & Tight Loops",
        "kind": "batch",
        "desc": "2000x2000 Mandelbrot escape sum (50 max iters) - floating-point & loop optimization",
        "files": {
            "c": "mandelbrot.c",
            "rust": "mandelbrot.rs",
            "go": "mandelbrot.go",
            "zig": "mandelbrot.zig",
            "vela": "mandelbrot.vela",
        },
        "snippets": SNIPPETS["01_mandelbrot"],
    },
    {
        "id": "02_binary_trees",
        "name": "Binary Trees",
        "category": "Memory Lifecycles",
        "kind": "batch",
        "desc": "Millions of short-lived nodes (depth 16) - Region bulk free vs GC vs RAII vs Arena",
        "files": {
            "c": "tree.c",
            "rust": "tree.rs",
            "go": "tree.go",
            "zig": "tree.zig",
            "vela": "tree.vela",
        },
        "snippets": SNIPPETS["02_binary_trees"],
    },
    {
        "id": "03_concurrency",
        "name": "Token Ring",
        "category": "Concurrency & Tasks",
        "kind": "batch",
        "desc": "4 concurrent workers passing a token 100,000 times - channel latency & context switching",
        "files": {
            "c": "ring.c",
            "rust": "ring.rs",
            "go": "ring.go",
            "zig": "ring.zig",
            "vela": "ring.vela",
        },
        "snippets": SNIPPETS["03_concurrency"],
    },
    {
        "id": "04_quicksort",
        "name": "Quicksort",
        "category": "Contiguous Slices",
        "kind": "batch",
        "desc": "In-place quicksort of 2,500,000 64-bit integers - array indexing, swapping & cache lines",
        "files": {
            "c": "qsort.c",
            "rust": "qsort.rs",
            "go": "qsort.go",
            "zig": "qsort.zig",
            "vela": "qsort.vela",
        },
        "snippets": SNIPPETS["04_quicksort"],
    },
    {
        "id": "05_prime_sieve",
        "name": "Prime Sieve",
        "category": "Flat Memory Access",
        "kind": "batch",
        "desc": "Sieve of Eratosthenes up to 30,000,000 - flat boolean buffer stride traversal",
        "files": {
            "c": "sieve.c",
            "rust": "sieve.rs",
            "go": "sieve.go",
            "zig": "sieve.zig",
            "vela": "sieve.vela",
        },
        "snippets": SNIPPETS["05_prime_sieve"],
    },
    {
        "id": "06_hashmap",
        "name": "Hash Map",
        "category": "Hashing & Lookups",
        "kind": "batch",
        "desc": "5,000,000 random inserts/lookups over 200,000 keys - collision probing & map resizing",
        "files": {
            "c": "hashmap.c",
            "rust": "hashmap.rs",
            "go": "hashmap.go",
            "zig": "hashmap.zig",
            "vela": "hashmap.vela",
        },
        "snippets": SNIPPETS["06_hashmap"],
    },
    {
        "id": "07_web_server",
        "name": "HTTP Web Server",
        "category": "Networking & Web I/O",
        "kind": "http",
        "port": 18260,
        "threads": 4,
        "requests": 2000,
        "desc": "Minimal concurrent HTTP/1.1 JSON & health endpoints across 4 worker threads (2,000 reqs/run)",
        "files": {
            "c": "web.c",
            "rust": "web.rs",
            "go": "web.go",
            "zig": "web.zig",
            "vela": "web.vela",
        },
        "snippets": SNIPPETS["07_web_server"],
    },
    {
        "id": "08_nbody",
        "name": "N-Body Simulation",
        "category": "Vector Math & Physics",
        "kind": "batch",
        "desc": "Symplectic orbital simulation of 5 celestial bodies over 5,000,000 steps - pure double-precision vector math",
        "files": {
            "c": "nbody.c",
            "rust": "nbody.rs",
            "go": "nbody.go",
            "zig": "nbody.zig",
            "vela": "nbody.vela",
        },
        "snippets": SNIPPETS["08_nbody"],
    },
    {
        "id": "09_matmul",
        "name": "Matrix Multiplication",
        "category": "Memory & Cache Locality",
        "kind": "batch",
        "desc": "Dense double matrix multiply N=1000 (1 billion inner ops) - contiguous stride cache locality & loop vectorization",
        "files": {
            "c": "matmul.c",
            "rust": "matmul.rs",
            "go": "matmul.go",
            "zig": "matmul.zig",
            "vela": "matmul.vela",
        },
        "snippets": SNIPPETS["09_matmul"],
    },
]


def find_binary(name: str, candidates: list[str]) -> str:
    env_val = os.environ.get(name.upper())
    if env_val and os.path.exists(env_val):
        return env_val
    for cand in candidates:
        expanded = os.path.expanduser(cand)
        if os.path.exists(expanded) and os.access(expanded, os.X_OK):
            return expanded
        which_path = shutil.which(cand)
        if which_path:
            return which_path
    return ""


def resolve_toolchains() -> dict[str, str]:
    vela = find_binary(
        "VELA",
        [
            str(ROOT_DIR / "target" / "release" / "vela"),
            str(ROOT_DIR / "target" / "debug" / "vela"),
            "vela",
        ],
    )
    if not vela or not os.path.exists(vela):
        print("Building vela compiler in release mode...")
        subprocess.run(
            ["cargo", "build", "--release"],
            cwd=str(ROOT_DIR),
            check=True,
            stdout=subprocess.DEVNULL,
        )
        vela = str(ROOT_DIR / "target" / "release" / "vela")

    rustc = find_binary(
        "RUSTC",
        [
            "~/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/rustc",
            "~/.cargo/bin/rustc",
            "rustc",
        ],
    )
    go = find_binary("GO", ["/usr/local/go/bin/go", "go"])
    zig = find_binary(
        "ZIG",
        [
            str(ROOT_DIR / ".bin" / "zig"),
            "~/.local/bin/zig",
            "/snap/bin/zig",
            "zig",
        ],
    )
    cc = find_binary("CC", ["gcc", "clang", "cc"])
    gnu_time = find_binary("TIME", ["/usr/bin/time", "time"])
    hyperfine = shutil.which("hyperfine") or ""

    return {
        "vela": vela,
        "rustc": rustc,
        "go": go,
        "zig": zig,
        "cc": cc,
        "time": gnu_time,
        "hyperfine": hyperfine,
    }


def compile_binary(
    lang: str, src_path: Path, out_path: Path, tools: dict[str, str]
) -> tuple[str, float]:
    if not src_path.exists():
        return ("skip", 0.0)

    out_path.parent.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()

    if lang == "vela":
        if not tools["vela"] or not tools["cc"]:
            return ("skip", 0.0)
        env["CC"] = f"{tools['cc']} -O3 -march=native -s"
        cmd = [tools["vela"], str(src_path), "-o", str(out_path)]
    elif lang == "c":
        if not tools["cc"]:
            return ("skip", 0.0)
        cmd = [
            tools["cc"],
            "-O3",
            "-march=native",
            "-s",
            "-o",
            str(out_path),
            str(src_path),
            "-lpthread",
            "-lm",
        ]
    elif lang == "rust":
        if not tools["rustc"]:
            return ("skip", 0.0)
        cmd = [
            tools["rustc"],
            "-C",
            "opt-level=3",
            "-C",
            "lto=thin",
            "-C",
            "target-cpu=native",
            "-C",
            "strip=symbols",
            "-o",
            str(out_path),
            str(src_path),
        ]
    elif lang == "go":
        if not tools["go"]:
            return ("skip", 0.0)
        cmd = [
            tools["go"],
            "build",
            "-ldflags=-s -w",
            "-o",
            str(out_path),
            str(src_path),
        ]
    elif lang == "zig":
        if not tools["zig"]:
            return ("skip", 0.0)
        cmd = [
            tools["zig"],
            "build-exe",
            "-lc",
            "-lpthread",
            "-O",
            "ReleaseFast",
            "-fstrip",
            f"-femit-bin={out_path}",
            str(src_path),
        ]
    else:
        return ("skip", 0.0)

    t0 = time.perf_counter()
    res = subprocess.run(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    compile_s = time.perf_counter() - t0
    if res.returncode != 0:
        print(f" fail\n    [!] {src_path.name} ({lang}) compile error:")
        print(res.stderr.decode("utf-8", errors="replace"))
        return ("fail", compile_s)
    return ("ok", compile_s)


def parse_gnu_time(stderr_text: str) -> dict:
    info = {"rss_kb": 0, "user_s": 0.0, "sys_s": 0.0, "vol_ctx": 0, "invol_ctx": 0}
    for line in stderr_text.splitlines():
        if "Maximum resident set size" in line:
            m = re.search(r"(\d+)", line)
            if m:
                info["rss_kb"] = int(m.group(1))
        elif "User time (seconds)" in line:
            m = re.search(r"([\d.]+)", line)
            if m:
                info["user_s"] = float(m.group(1))
        elif "System time (seconds)" in line:
            m = re.search(r"([\d.]+)", line)
            if m:
                info["sys_s"] = float(m.group(1))
        elif "Voluntary context switches" in line:
            m = re.search(r"(\d+)", line)
            if m:
                info["vol_ctx"] = int(m.group(1))
        elif "Involuntary context switches" in line:
            m = re.search(r"(\d+)", line)
            if m:
                info["invol_ctx"] = int(m.group(1))
    return info


def verify_and_measure_batch(
    bin_path: Path, runs: int, warmup: int, time_bin: str, compile_s: float
):
    sanity = subprocess.run(
        [str(bin_path)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=True,
    )
    stdout_output = sanity.stdout.strip()

    for _ in range(max(0, warmup - 1)):
        subprocess.run(
            [str(bin_path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )

    times = []
    for _ in range(runs):
        t_start = time.perf_counter()
        subprocess.run(
            [str(bin_path)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )
        times.append(time.perf_counter() - t_start)

    rss_list, user_list, sys_list, ctx_list = [], [], [], []
    if time_bin and os.path.exists(time_bin):
        for _ in range(min(3, runs)):
            res = subprocess.run(
                [time_bin, "-v", str(bin_path)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
                check=True,
            )
            parsed = parse_gnu_time(res.stderr)
            if parsed["rss_kb"] > 0:
                rss_list.append(parsed["rss_kb"])
            user_list.append(parsed["user_s"])
            sys_list.append(parsed["sys_s"])
            ctx_list.append(parsed["vol_ctx"] + parsed["invol_ctx"])

    return {
        "stdout": stdout_output,
        "mean": statistics.mean(times),
        "median": statistics.median(times),
        "stddev": statistics.stdev(times) if len(times) > 1 else 0.0,
        "min": min(times),
        "max": max(times),
        "rss_kb": max(rss_list) if rss_list else 0,
        "user_s": statistics.median(user_list) if user_list else 0.0,
        "sys_s": statistics.median(sys_list) if sys_list else 0.0,
        "ctx_switches": int(statistics.median(ctx_list)) if ctx_list else 0,
        "compile_s": compile_s,
        "bin_size_kb": os.path.getsize(bin_path) / 1024.0,
    }


def read_proc_vmhwm_kb(pid: int) -> int:
    try:
        with open(f"/proc/{pid}/status", "r", encoding="utf-8") as f:
            for line in f:
                if line.startswith("VmHWM:"):
                    m = re.search(r"(\d+)", line)
                    if m:
                        return int(m.group(1))
    except OSError:
        pass
    return 0


def wait_port_ready(port: int, timeout_s: float = 5.0) -> bool:
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        try:
            with urllib.request.urlopen(
                f"http://127.0.0.1:{port}/health", timeout=0.3
            ) as resp:
                if resp.status == 200 and resp.read().decode().strip() == "ok":
                    return True
        except Exception:
            time.sleep(0.02)
    return False


def wait_port_closed(port: int, timeout_s: float = 3.0):
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(0.05)
            if s.connect_ex(("127.0.0.1", port)) != 0:
                return
        time.sleep(0.02)


def verify_and_measure_http(
    bin_path: Path,
    http_load_bin: Path,
    port: int,
    threads: int,
    requests_per_run: int,
    runs: int,
    compile_s: float,
):
    proc = subprocess.Popen(
        [str(bin_path)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        if not wait_port_ready(port, timeout_s=6.0):
            raise TimeoutError(f"HTTP server {bin_path.name} failed to bind on port {port}")

        with urllib.request.urlopen(f"http://127.0.0.1:{port}/json", timeout=1.0) as resp:
            data = resp.read().decode()
            parsed_json = json.loads(data)

        subprocess.run(
            [str(http_load_bin), str(port), str(threads), "200"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=True,
        )

        load_runs = max(2, min(3, runs))
        elapsed_list, rps_list, p95_list = [], [], []
        for _ in range(load_runs):
            res = subprocess.run(
                [str(http_load_bin), str(port), str(threads), str(requests_per_run)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=True,
            )
            stats = json.loads(res.stdout.strip())
            if stats["fail"] > 0:
                raise RuntimeError(f"HTTP load errors: {stats}")
            elapsed_list.append(stats["elapsed"])
            rps_list.append(stats["rps"])
            p95_list.append(stats["p95_us"] / 1000.0)  # ms

        rss_kb = read_proc_vmhwm_kb(proc.pid)

        return {
            "stdout": json.dumps(parsed_json, sort_keys=True),
            "mean": statistics.mean(elapsed_list),
            "median": statistics.median(elapsed_list),
            "stddev": statistics.stdev(elapsed_list) if len(elapsed_list) > 1 else 0.0,
            "min": min(elapsed_list),
            "max": max(elapsed_list),
            "rps": statistics.median(rps_list),
            "p95_ms": statistics.median(p95_list),
            "rss_kb": rss_kb,
            "user_s": 0.0,
            "sys_s": 0.0,
            "ctx_switches": 0,
            "compile_s": compile_s,
            "bin_size_kb": os.path.getsize(bin_path) / 1024.0,
        }
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=2.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        wait_port_closed(port)


def format_duration(seconds: float) -> str:
    if seconds < 0.001:
        return f"{seconds * 1000000:.1f} µs"
    elif seconds < 1.0:
        return f"{seconds * 1000:.1f} ms"
    else:
        return f"{seconds:.3f} s"


def generate_html_report(all_results: dict, runs: int, warmup: int, report_path: Path):
    json_data = json.dumps(all_results, indent=2)

    html_content = f"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>Vela Benchmark Dashboard | Language Comparison</title>
  <style>
    :root {{
      --bg: #0b0f19;
      --card-bg: #111827;
      --card-border: #1f2937;
      --text: #f3f4f6;
      --text-muted: #9ca3af;
      --accent: #10b981;
      --accent-glow: rgba(16, 185, 129, 0.2);
      --c-color: #3b82f6;
      --rust-color: #f97316;
      --go-color: #06b6d4;
      --zig-color: #eab308;
      --vela-color: #10b981;
    }}
    * {{ box-sizing: border-box; margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif; }}
    body {{ background: var(--bg); color: var(--text); padding: 2rem 1.5rem; line-height: 1.5; }}
    .container {{ max-width: 1320px; margin: 0 auto; }}
    
    header {{ display: flex; justify-content: space-between; align-items: center; border-bottom: 1px solid var(--card-border); padding-bottom: 1.5rem; margin-bottom: 2rem; flex-wrap: wrap; gap: 1rem; }}
    h1 {{ font-size: 2rem; font-weight: 800; display: flex; align-items: center; gap: 0.75rem; letter-spacing: -0.025em; }}
    .badge {{ background: var(--accent-glow); color: var(--accent); border: 1px solid var(--accent); font-size: 0.75rem; font-weight: 700; padding: 0.25rem 0.6rem; border-radius: 9999px; text-transform: uppercase; }}
    .subtitle {{ color: var(--text-muted); font-size: 0.95rem; margin-top: 0.35rem; }}
    
    .stats-grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 1.25rem; margin-bottom: 2rem; }}
    .stat-card {{ background: var(--card-bg); border: 1px solid var(--card-border); border-radius: 1rem; padding: 1.25rem; position: relative; overflow: hidden; }}
    .stat-card::after {{ content: ''; position: absolute; top: 0; left: 0; width: 4px; height: 100%; background: var(--card-color, var(--accent)); }}
    .stat-label {{ font-size: 0.75rem; font-weight: 700; text-transform: uppercase; color: var(--text-muted); letter-spacing: 0.05em; }}
    .stat-val {{ font-size: 1.35rem; font-weight: 800; margin-top: 0.35rem; }}
    .stat-sub {{ font-size: 0.8rem; color: var(--text-muted); margin-top: 0.25rem; }}

    .radar-section {{ background: var(--card-bg); border: 1px solid var(--card-border); border-radius: 1rem; padding: 1.5rem; margin-bottom: 2rem; display: grid; grid-template-columns: 380px 1fr; gap: 2rem; align-items: center; }}
    @media (max-width: 900px) {{ .radar-section {{ grid-template-columns: 1fr; }} }}
    .radar-wrap {{ display: flex; justify-content: center; align-items: center; }}
    .radar-legend {{ display: flex; flex-wrap: wrap; gap: 1rem; margin-top: 1rem; }}
    .legend-item {{ display: flex; align-items: center; gap: 0.5rem; font-size: 0.875rem; font-weight: 700; cursor: pointer; padding: 0.35rem 0.75rem; border-radius: 6px; background: rgba(255,255,255,0.03); border: 1px solid var(--card-border); }}

    .toolbar {{ display: flex; justify-content: space-between; align-items: center; background: var(--card-bg); border: 1px solid var(--card-border); border-radius: 0.75rem; padding: 0.5rem 0.75rem; margin-bottom: 2rem; flex-wrap: wrap; gap: 0.75rem; }}
    .tab-group {{ display: flex; gap: 0.5rem; flex-wrap: wrap; }}
    .btn {{ background: transparent; border: 1px solid transparent; color: var(--text-muted); padding: 0.5rem 0.9rem; border-radius: 0.5rem; font-size: 0.85rem; font-weight: 600; cursor: pointer; transition: all 0.2s ease; }}
    .btn:hover {{ color: var(--text); background: rgba(255,255,255,0.05); }}
    .btn.active {{ color: var(--text); background: #1f2937; border-color: #374151; }}

    .bench-section {{ background: var(--card-bg); border: 1px solid var(--card-border); border-radius: 1rem; padding: 1.5rem; margin-bottom: 2rem; }}
    .bench-header {{ display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 1.5rem; border-bottom: 1px solid rgba(255,255,255,0.06); padding-bottom: 1rem; flex-wrap: wrap; gap: 0.5rem; }}
    .bench-title {{ font-size: 1.35rem; font-weight: 700; }}
    .bench-desc {{ font-size: 0.875rem; color: var(--text-muted); }}

    .bars-container {{ display: flex; flex-direction: column; gap: 0.85rem; margin-bottom: 1.5rem; }}
    .bar-row {{ display: grid; grid-template-columns: 95px 1fr 180px; align-items: center; gap: 1rem; }}
    .bar-label {{ font-size: 0.9rem; font-weight: 700; display: flex; align-items: center; gap: 0.5rem; }}
    .lang-dot {{ width: 10px; height: 10px; border-radius: 50%; display: inline-block; }}
    .bar-track {{ background: rgba(255,255,255,0.04); height: 32px; border-radius: 6px; overflow: hidden; position: relative; border: 1px solid rgba(255,255,255,0.05); }}
    .bar-fill {{ height: 100%; border-radius: 5px; transition: width 0.5s cubic-bezier(0.16, 1, 0.3, 1); display: flex; align-items: center; padding: 0 0.75rem; font-size: 0.8rem; font-weight: 700; color: #fff; min-width: 56px; text-shadow: 0 1px 2px rgba(0,0,0,0.5); white-space: nowrap; }}
    .bar-meta {{ font-size: 0.85rem; font-weight: 600; text-align: right; color: var(--text-muted); font-variant-numeric: tabular-nums; }}

    .table-wrap {{ overflow-x: auto; }}
    table {{ width: 100%; border-collapse: collapse; font-size: 0.84rem; margin-top: 1rem; }}
    th, td {{ padding: 0.7rem 0.85rem; text-align: left; border-bottom: 1px solid var(--card-border); white-space: nowrap; }}
    th {{ color: var(--text-muted); font-weight: 600; text-transform: uppercase; font-size: 0.72rem; letter-spacing: 0.05em; }}
    tr:last-child td {{ border-bottom: none; }}
    .winner-tag {{ background: rgba(16, 185, 129, 0.15); color: #34d399; padding: 0.15rem 0.45rem; border-radius: 4px; font-weight: 700; font-size: 0.7rem; margin-left: 0.4rem; }}

    .code-card {{ margin-top: 1.25rem; background: rgba(0,0,0,0.35); border: 1px solid var(--card-border); border-radius: 0.75rem; overflow: hidden; }}
    .code-header {{ display: flex; justify-content: space-between; align-items: center; background: rgba(255,255,255,0.02); border-bottom: 1px solid var(--card-border); padding: 0.5rem 0.9rem; flex-wrap: wrap; gap: 0.5rem; }}
    .code-title {{ font-size: 0.8rem; font-weight: 700; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.05em; display: flex; align-items: center; gap: 0.4rem; }}
    .code-tabs {{ display: flex; gap: 0.35rem; }}
    .code-tab {{ background: transparent; border: 1px solid transparent; color: var(--text-muted); padding: 0.25rem 0.65rem; border-radius: 0.35rem; font-size: 0.78rem; font-weight: 600; cursor: pointer; transition: all 0.15s ease; }}
    .code-tab:hover {{ color: var(--text); background: rgba(255,255,255,0.05); }}
    .code-tab.active {{ color: #fff; background: #1f2937; border-color: #374151; }}
    .code-body {{ padding: 0.9rem; overflow-x: auto; max-height: 280px; overflow-y: auto; }}
    .code-body pre {{ margin: 0; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace; font-size: 0.82rem; line-height: 1.45; color: #e5e7eb; }}

    footer {{ text-align: center; color: var(--text-muted); font-size: 0.85rem; padding-top: 2rem; border-top: 1px solid var(--card-border); margin-top: 3rem; }}
  </style>
</head>
<body>
  <div class="container">
    <header>
      <div>
        <h1>Vela Performance Dashboard <span class="badge">Vela vs C, Rust, Go, Zig</span></h1>
        <div class="subtitle">Cross-language runtime, memory, concurrency, compilation, and HTTP throughput metrics ({runs} timed runs, {warmup} warmups across 9 benchmarks).</div>
      </div>
      <div>
        <span class="badge" style="background: rgba(59, 130, 246, 0.15); color: #60a5fa; border-color: #3b82f6;">Automated Output Parity Checked</span>
      </div>
    </header>

    <div class="stats-grid" id="kpi-cards"></div>

    <div class="radar-section">
      <div class="radar-wrap" id="radar-container"></div>
      <div>
        <h2 style="font-size: 1.25rem; font-weight: 700; margin-bottom: 0.5rem;">Multi-Dimensional Language Profile</h2>
        <p style="color: var(--text-muted); font-size: 0.9rem; margin-bottom: 1rem;">
          Normalized radar shape across all benchmark dimensions (outer edge = 100% fastest/smallest in category). Highlights architectural tradeoffs between region allocation, tracing GC, green threads, and ahead-of-time systems codegen.
        </p>
        <div class="radar-legend" id="radar-legend"></div>
      </div>
    </div>

    <div class="toolbar">
      <div class="tab-group" id="metric-tabs">
        <button class="btn active" onclick="setMetric('median')">⏱️ Median Speed</button>
        <button class="btn" onclick="setMetric('mean')">📊 Mean Speed</button>
        <button class="btn" onclick="setMetric('rss')">🧠 Peak RSS Memory</button>
        <button class="btn" onclick="setMetric('size')">📦 Binary Size</button>
        <button class="btn" onclick="setMetric('compile')">⚡ Compile Time</button>
      </div>
      <div class="tab-group">
        <button class="btn active" id="btn-ratio" onclick="toggleRatio(true)">Relative Ratio (x)</button>
        <button class="btn" id="btn-raw" onclick="toggleRatio(false)">Absolute Values</button>
      </div>
    </div>

    <div id="benchmarks-container"></div>

    <footer>
      Generated dynamically by <code>benchmarks/run.py</code>. Direct execution timing via monotonic clock; Peak RSS via GNU Time / Linux <code>/proc/pid/status</code> VmHWM.
    </footer>
  </div>

  <script>
    const DATA = {json_data};
    const COLORS = {{
      c: "#3b82f6",
      rust: "#f97316",
      go: "#06b6d4",
      zig: "#eab308",
      vela: "#10b981"
    }};
    const LABELS = {{
      c: "C (GCC)",
      rust: "Rust",
      go: "Go",
      zig: "Zig",
      vela: "Vela"
    }};
    let currentMetric = 'median';
    let showRatio = true;

    function formatTime(s) {{
      if (s < 0.001) return (s * 1000000).toFixed(1) + " µs";
      if (s < 1.0) return (s * 1000).toFixed(1) + " ms";
      return s.toFixed(2) + " s";
    }}

    function formatRss(kb) {{
      if (kb < 1024) return kb + " KB";
      return (kb / 1024).toFixed(1) + " MB";
    }}

    function formatSize(kb) {{
      if (kb < 1024) return kb.toFixed(1) + " KB";
      return (kb / 1024).toFixed(2) + " MB";
    }}

    function escapeHtml(str) {{
      return str.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
    }}

    function switchSnippet(bId, lang) {{
      const bData = DATA[bId];
      if (!bData || !bData.meta || !bData.meta.snippets || !bData.meta.snippets[lang]) return;
      const el = document.getElementById(`code-box-${{bId}}`);
      if (el) el.textContent = bData.meta.snippets[lang];
      const tabContainer = document.getElementById(`code-tabs-${{bId}}`);
      if (tabContainer) {{
        tabContainer.querySelectorAll('.code-tab').forEach(btn => {{
          btn.classList.toggle('active', btn.textContent.trim().toLowerCase() === lang);
        }});
      }}
    }}

    function buildKpiCards() {{
      const grid = document.getElementById('kpi-cards');
      grid.innerHTML = '';
      const cards = [];

      // 1. Binary Trees winner
      if (DATA['02_binary_trees']) {{
        const res = DATA['02_binary_trees'].results;
        const sorted = Object.entries(res).sort((a, b) => a[1].median - b[1].median);
        if (sorted.length > 0) {{
          const [wLang, wStats] = sorted[0];
          const goStats = res['go'];
          const sub = goStats
            ? `${{(goStats.median / wStats.median).toFixed(1)}}x faster than Go GC (${{formatRss(wStats.rss_kb)}} RSS)`
            : `Winner in regional / arena memory`;
          cards.push({{
            label: "Memory Lifecycle Winner",
            val: `${{LABELS[wLang]}} (${{formatTime(wStats.median)}})`,
            sub,
            color: COLORS[wLang]
          }});
        }}
      }}

      // 2. Web Server winner
      if (DATA['07_web_server']) {{
        const res = DATA['07_web_server'].results;
        const sorted = Object.entries(res).sort((a, b) => (b[1].rps || 0) - (a[1].rps || 0));
        if (sorted.length > 0) {{
          const [wLang, wStats] = sorted[0];
          const velaStats = res['vela'];
          const sub = velaStats
            ? `Vela std/web: ${{Math.round(velaStats.rps).toLocaleString()}} req/s (${{formatSize(velaStats.bin_size_kb)}} bin)`
            : `Peak throughput: ${{Math.round(wStats.rps).toLocaleString()}} req/s`;
          cards.push({{
            label: "Web Server Throughput",
            val: `${{LABELS[wLang]}} (${{Math.round(wStats.rps).toLocaleString()}} req/s)`,
            sub,
            color: COLORS[wLang]
          }});
        }}
      }}

      // 3. Concurrency winner
      if (DATA['03_concurrency']) {{
        const res = DATA['03_concurrency'].results;
        const sorted = Object.entries(res).sort((a, b) => a[1].median - b[1].median);
        if (sorted.length > 0) {{
          const [wLang, wStats] = sorted[0];
          const second = sorted[1];
          const sub = second ? `${{(second[1].median / wStats.median).toFixed(1)}}x faster than ${{second[0].toUpperCase()}}` : "";
          cards.push({{
            label: "Channel Switching Winner",
            val: `${{LABELS[wLang]}} (${{formatTime(wStats.median)}})`,
            sub,
            color: COLORS[wLang]
          }});
        }}
      }}

      // 4. N-Body Simulation winner
      if (DATA['08_nbody']) {{
        const res = DATA['08_nbody'].results;
        const sorted = Object.entries(res).sort((a, b) => a[1].median - b[1].median);
        if (sorted.length > 0) {{
          const [wLang, wStats] = sorted[0];
          cards.push({{
            label: "N-Body Physics Winner",
            val: `${{LABELS[wLang]}} (${{formatTime(wStats.median)}})`,
            sub: `5M double-precision vector steps`,
            color: COLORS[wLang]
          }});
        }}
      }}

      // 5. Matrix Multiplication winner
      if (DATA['09_matmul']) {{
        const res = DATA['09_matmul'].results;
        const sorted = Object.entries(res).sort((a, b) => a[1].median - b[1].median);
        if (sorted.length > 0) {{
          const [wLang, wStats] = sorted[0];
          cards.push({{
            label: "Matrix Multiply Winner",
            val: `${{LABELS[wLang]}} (${{formatTime(wStats.median)}})`,
            sub: `N=1000 (1B inner float ops)`,
            color: COLORS[wLang]
          }});
        }}
      }}

      // 6. Average binary size leader
      const langSizes = {{}};
      for (const bData of Object.values(DATA)) {{
        for (const [l, r] of Object.entries(bData.results)) {{
          langSizes[l] = (langSizes[l] || []);
          langSizes[l].push(r.bin_size_kb);
        }}
      }}
      const avgSizes = Object.entries(langSizes).map(([l, arr]) => [l, arr.reduce((a, b) => a + b, 0) / arr.length]);
      avgSizes.sort((a, b) => a[1] - b[1]);
      if (avgSizes.length > 0) {{
        const [sLang, sKb] = avgSizes[0];
        const velaAvg = avgSizes.find(x => x[0] === 'vela');
        cards.push({{
          label: "Smallest Avg Binary",
          val: `${{LABELS[sLang]}} (${{formatSize(sKb)}})`,
          sub: velaAvg && sLang !== 'vela' ? `Vela avg: ${{formatSize(velaAvg[1])}} (stripped ELF)` : `Stripped zero-runtime footprint`,
          color: COLORS[sLang]
        }});
      }}

      cards.forEach(c => {{
        const div = document.createElement('div');
        div.className = 'stat-card';
        div.style.setProperty('--card-color', c.color);
        div.innerHTML = `
          <div class="stat-label">${{c.label}}</div>
          <div class="stat-val" style="color:${{c.color}}">${{c.val}}</div>
          <div class="stat-sub">${{c.sub}}</div>
        `;
        grid.appendChild(div);
      }});
    }}

    function buildRadar() {{
      const axes = [
        {{ key: '01_mandelbrot', label: 'CPU Loops', extract: r => 1 / r.median }},
        {{ key: '02_binary_trees', label: 'Memory Mgmt', extract: r => 1 / r.median }},
        {{ key: '03_concurrency', label: 'Channel Switch', extract: r => 1 / r.median }},
        {{ key: '04_quicksort', label: 'Slice Sort', extract: r => 1 / r.median }},
        {{ key: '06_hashmap', label: 'Hash Map', extract: r => 1 / r.median }},
        {{ key: '07_web_server', label: 'Web RPS', extract: r => r.rps || (1 / r.median) }},
        {{ key: '08_nbody', label: 'Vector Math', extract: r => 1 / r.median }},
        {{ key: '09_matmul', label: 'Cache Locality', extract: r => 1 / r.median }},
      ].filter(a => DATA[a.key]);

      const langs = ['vela', 'c', 'rust', 'go', 'zig'].filter(l =>
        axes.every(a => DATA[a.key].results[l])
      );

      const maxPerAxis = axes.map(a =>
        Math.max(...langs.map(l => a.extract(DATA[a.key].results[l])))
      );

      const size = 340, cx = size / 2, cy = size / 2, radius = 115;
      const n = axes.length;
      let svg = `<svg width="${{size}}" height="${{size}}" viewBox="0 0 ${{size}} ${{size}}">`;

      // Concentric rings
      [0.25, 0.5, 0.75, 1.0].forEach(level => {{
        let pts = [];
        for (let i = 0; i < n; i++) {{
          const ang = (Math.PI * 2 * i / n) - Math.PI / 2;
          pts.push(`${{cx + radius * level * Math.cos(ang)}},${{cy + radius * level * Math.sin(ang)}}`);
        }}
        svg += `<polygon points="${{pts.join(' ')}}" fill="none" stroke="rgba(255,255,255,0.09)" stroke-width="1"/>`;
      }});

      // Axis lines & labels
      for (let i = 0; i < n; i++) {{
        const ang = (Math.PI * 2 * i / n) - Math.PI / 2;
        const x2 = cx + radius * Math.cos(ang);
        const y2 = cy + radius * Math.sin(ang);
        svg += `<line x1="${{cx}}" y1="${{cy}}" x2="${{x2}}" y2="${{y2}}" stroke="rgba(255,255,255,0.1)" stroke-width="1"/>`;
        const lx = cx + (radius + 26) * Math.cos(ang);
        const ly = cy + (radius + 22) * Math.sin(ang);
        svg += `<text x="${{lx}}" y="${{ly}}" fill="#9ca3af" font-size="11" font-weight="600" text-anchor="middle" dominant-baseline="middle">${{axes[i].label}}</text>`;
      }}

      // Language polygons
      langs.slice().reverse().forEach(l => {{
        let pts = [];
        for (let i = 0; i < n; i++) {{
          const ang = (Math.PI * 2 * i / n) - Math.PI / 2;
          const score = Math.max(0.12, axes[i].extract(DATA[axes[i].key].results[l]) / maxPerAxis[i]);
          pts.push(`${{cx + radius * score * Math.cos(ang)}},${{cy + radius * score * Math.sin(ang)}}`);
        }}
        const strokeWidth = l === 'vela' ? 2.5 : 1.75;
        const fillOp = l === 'vela' ? 0.22 : 0.08;
        svg += `<polygon points="${{pts.join(' ')}}" fill="${{COLORS[l]}}" fill-opacity="${{fillOp}}" stroke="${{COLORS[l]}}" stroke-width="${{strokeWidth}}"/>`;
      }});
      svg += `</svg>`;
      document.getElementById('radar-container').innerHTML = svg;

      const legend = document.getElementById('radar-legend');
      legend.innerHTML = '';
      langs.forEach(l => {{
        const div = document.createElement('div');
        div.className = 'legend-item';
        div.innerHTML = `<span class="lang-dot" style="background:${{COLORS[l]}}"></span><span>${{LABELS[l]}}</span>`;
        legend.appendChild(div);
      }});
    }}

    function setMetric(m) {{
      currentMetric = m;
      const map = ['median', 'mean', 'rss', 'size', 'compile'];
      document.querySelectorAll('#metric-tabs .btn').forEach((b, i) => {{
        b.classList.toggle('active', map[i] === m);
      }});
      render();
    }}

    function toggleRatio(r) {{
      showRatio = r;
      document.getElementById('btn-ratio').classList.toggle('active', r);
      document.getElementById('btn-raw').classList.toggle('active', !r);
      render();
    }}

    function getMetricValue(r, isHttp) {{
      if (currentMetric === 'median') return isHttp && r.rps ? r.rps : r.median;
      if (currentMetric === 'mean') return isHttp && r.rps ? r.rps : r.mean;
      if (currentMetric === 'rss') return r.rss_kb;
      if (currentMetric === 'size') return r.bin_size_kb;
      return r.compile_s;
    }}

    function render() {{
      const container = document.getElementById('benchmarks-container');
      container.innerHTML = '';

      for (const [bId, bData] of Object.entries(DATA)) {{
        const meta = bData.meta;
        const res = bData.results;
        const isHttp = meta.kind === 'http';
        const higherIsBetter = isHttp && (currentMetric === 'median' || currentMetric === 'mean');

        const section = document.createElement('div');
        section.className = 'bench-section';

        let hint = higherIsBetter
          ? 'Higher req/s is better'
          : (currentMetric === 'rss' ? 'Lower memory is better' : currentMetric === 'size' ? 'Smaller binary is better' : currentMetric === 'compile' ? 'Faster compile is better' : 'Lower time is faster');

        section.innerHTML = `
          <div class="bench-header">
            <div>
              <div class="bench-title">${{meta.name}} <span style="font-size:0.85rem; font-weight:normal; color:var(--text-muted);">&bull; ${{meta.category}}</span></div>
              <div class="bench-desc">${{meta.desc}}</div>
            </div>
            <div style="font-size:0.8rem; font-weight:700; color:var(--text-muted);">${{hint}}</div>
          </div>
          <div class="bars-container" id="bars-${{bId}}"></div>
          <div class="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>Language</th>
                  <th>${{isHttp ? 'Throughput (RPS)' : 'Median Time'}}</th>
                  <th>${{isHttp ? 'P95 Latency' : 'Mean ± Stddev'}}</th>
                  <th>${{isHttp ? '2000 Reqs Time' : 'CPU (User + Sys)'}}</th>
                  <th>Peak RSS</th>
                  <th>Binary Size</th>
                  <th>Compile Time</th>
                  <th>Relative</th>
                </tr>
              </thead>
              <tbody id="table-${{bId}}"></tbody>
            </table>
          </div>
        `;
        container.appendChild(section);

        const barsContainer = section.querySelector(`#bars-${{bId}}`);
        const tableBody = section.querySelector(`#table-${{bId}}`);

        const vals = Object.entries(res).map(([lang, r]) => ({{
          lang,
          val: getMetricValue(r, isHttp),
          raw: r
        }}));

        const bestVal = higherIsBetter
          ? Math.max(...vals.map(v => v.val))
          : Math.min(...vals.map(v => v.val));

        vals.forEach(item => {{
          const isWinner = item.val === bestVal;
          const ratio = higherIsBetter ? (bestVal / item.val) : (item.val / bestVal);
          const pct = higherIsBetter
            ? Math.max(8, (item.val / bestVal) * 100)
            : Math.max(8, (bestVal / item.val) * 100);

          let displayVal = "";
          if (higherIsBetter) displayVal = `${{Math.round(item.raw.rps).toLocaleString()}} req/s`;
          else if (currentMetric === 'median') displayVal = formatTime(item.raw.median);
          else if (currentMetric === 'mean') displayVal = formatTime(item.raw.mean);
          else if (currentMetric === 'rss') displayVal = formatRss(item.raw.rss_kb);
          else if (currentMetric === 'size') displayVal = formatSize(item.raw.bin_size_kb);
          else displayVal = formatTime(item.raw.compile_s);

          const barRow = document.createElement('div');
          barRow.className = 'bar-row';
          barRow.innerHTML = `
            <div class="bar-label">
              <span class="lang-dot" style="background:${{COLORS[item.lang] || '#fff'}}"></span>
              ${{item.lang.toUpperCase()}}
            </div>
            <div class="bar-track">
              <div class="bar-fill" style="width:${{pct}}%; background:${{COLORS[item.lang] || '#fff'}};">
                ${{showRatio ? (ratio <= 1.005 ? '1.00x (best)' : ratio.toFixed(2) + 'x') : displayVal}}
              </div>
            </div>
            <div class="bar-meta">${{displayVal}}</div>
          `;
          barsContainer.appendChild(barRow);

          const tr = document.createElement('tr');
          const col1 = isHttp ? `<strong>${{Math.round(item.raw.rps).toLocaleString()}} req/s</strong>` : `<code>${{formatTime(item.raw.median)}}</code>`;
          const col2 = isHttp ? `${{item.raw.p95_ms.toFixed(2)}} ms` : `<code>${{formatTime(item.raw.mean)}} &plusmn; ${{formatTime(item.raw.stddev)}}</code>`;
          const col3 = isHttp ? formatTime(item.raw.median) : `${{(item.raw.user_s + item.raw.sys_s).toFixed(3)}}s (${{item.raw.user_s.toFixed(2)}}u + ${{item.raw.sys_s.toFixed(2)}}s)`;

          tr.innerHTML = `
            <td style="font-weight:700; color:${{COLORS[item.lang]}};">
              ${{item.lang.toUpperCase()}}${{isWinner ? '<span class="winner-tag">BEST</span>' : ''}}
            </td>
            <td>${{col1}}</td>
            <td style="color:var(--text-muted);">${{col2}}</td>
            <td style="color:var(--text-muted);">${{col3}}</td>
            <td>${{formatRss(item.raw.rss_kb)}}</td>
            <td>${{formatSize(item.raw.bin_size_kb)}}</td>
            <td style="color:var(--text-muted);">${{formatTime(item.raw.compile_s)}}</td>
            <td><strong>${{ratio <= 1.005 ? '1.00x' : ratio.toFixed(2) + 'x'}}</strong></td>
          `;
          tableBody.appendChild(tr);
        }});

        // Core Algorithm Preview Box
        if (meta.snippets) {{
          const codeCard = document.createElement('div');
          codeCard.className = 'code-card';
          const defaultLang = 'go';
          const availLangs = ['go', 'vela', 'rust', 'zig', 'c'].filter(l => meta.snippets[l]);
          codeCard.innerHTML = `
            <div class="code-header">
              <div class="code-title">
                <span>⚡ Core Algorithm Snippet</span>
                <span style="font-weight:normal; text-transform:none; font-size:0.75rem; color:var(--text-muted);">(Showing algorithmic kernel only)</span>
              </div>
              <div class="code-tabs" id="code-tabs-${{bId}}">
                ${{availLangs.map(l => `
                  <button class="code-tab ${{l === defaultLang ? 'active' : ''}}" onclick="switchSnippet('${{bId}}', '${{l}}')">
                    ${{l.toUpperCase()}}
                  </button>
                `).join('')}}
              </div>
            </div>
            <div class="code-body">
              <pre><code id="code-box-${{bId}}">${{escapeHtml(meta.snippets[defaultLang] || meta.snippets['vela'] || '')}}</code></pre>
            </div>
          `;
          section.appendChild(codeCard);
        }}
      }}
    }}

    buildKpiCards();
    buildRadar();
    render();
  </script>
</body>
</html>
"""
    with open(report_path, "w", encoding="utf-8") as f:
        f.write(html_content)


def main():
    parser = argparse.ArgumentParser(
        description="Run language benchmarks: Vela vs C, Rust, Go, Zig"
    )
    parser.add_argument(
        "--runs", type=int, default=7, help="Number of timed runs (default: 7)"
    )
    parser.add_argument(
        "--warmup", type=int, default=2, help="Number of warmup runs (default: 2)"
    )
    parser.add_argument(
        "--filter", type=str, default="", help="Filter benchmarks by id or name substring"
    )
    parser.add_argument(
        "--clean", action="store_true", help="Clean build directory before running"
    )
    parser.add_argument(
        "--hyperfine",
        action="store_true",
        help="Also run hyperfine if available on system",
    )
    args = parser.parse_args()

    if args.clean and BUILD_DIR.exists():
        shutil.rmtree(BUILD_DIR)

    BUILD_DIR.mkdir(parents=True, exist_ok=True)
    tools = resolve_toolchains()

    http_load_bin = BUILD_DIR / "http_load"
    if HTTP_LOAD_SRC.exists() and (not http_load_bin.exists() or args.clean):
        subprocess.run(
            [tools["cc"], "-O3", "-s", str(HTTP_LOAD_SRC), "-o", str(http_load_bin), "-lpthread"],
            check=True,
        )

    print("=" * 84)
    print(" Vela Comparative Benchmark Suite (9 Workloads x 5 Languages)")
    print("=" * 84)
    print(f"Vela:    {tools['vela'] or 'not found'}")
    print(f"C (GCC): {tools['cc'] or 'not found'}")
    print(f"Rust:    {tools['rustc'] or 'not found'}")
    print(f"Go:      {tools['go'] or 'not found'}")
    print(f"Zig:     {tools['zig'] or 'not found'}")
    print(f"Runs:    {args.runs} (Warmup: {args.warmup})")
    print("=" * 84)

    selected_benches = [
        b
        for b in BENCHMARKS
        if not args.filter
        or args.filter.lower() in b["id"].lower()
        or args.filter.lower() in b["name"].lower()
    ]

    all_results = {}
    has_failure = False

    for bench in selected_benches:
        print(f"\n[+] Benchmark: {bench['name']} ({bench['category']})")
        print(f"    {bench['desc']}")
        b_dir = BENCH_DIR / bench["id"]
        out_b_dir = BUILD_DIR / bench["id"]

        bench_res = {}
        expected_stdout = None

        for lang in LANGUAGES:
            src_file = b_dir / bench["files"][lang]
            out_bin = out_b_dir / f"{bench['id']}_{lang}"

            print(f"  • Compiling {lang:5} ({src_file.name})...", end="", flush=True)
            status, compile_s = compile_binary(lang, src_file, out_bin, tools)
            if status == "skip":
                print(" skipped (toolchain unavailable)")
                continue
            if status == "fail":
                has_failure = True
                continue
            print(f" ok ({format_duration(compile_s)})")

            print(f"    Benchmarking {lang}...", end="", flush=True)
            try:
                if bench["kind"] == "http":
                    stats = verify_and_measure_http(
                        out_bin,
                        http_load_bin,
                        bench["port"],
                        bench["threads"],
                        bench["requests"],
                        args.runs,
                        compile_s,
                    )
                else:
                    stats = verify_and_measure_batch(
                        out_bin, args.runs, args.warmup, tools["time"], compile_s
                    )

                if expected_stdout is None:
                    expected_stdout = stats["stdout"]
                elif stats["stdout"] != expected_stdout:
                    print(" OUTPUT MISMATCH!")
                    print(f"      Expected:\n{expected_stdout}")
                    print(f"      Actual ({lang}):\n{stats['stdout']}")
                    has_failure = True
                    continue

                bench_res[lang] = stats
                if bench["kind"] == "http":
                    print(
                        f" {stats['rps']:.0f} req/s (p95: {stats['p95_ms']:.2f} ms), RSS: {stats['rss_kb']} KB"
                    )
                else:
                    print(
                        f" median: {format_duration(stats['median'])} (mean: {format_duration(stats['mean'])} ± {format_duration(stats['stddev'])}), RSS: {stats['rss_kb']} KB"
                    )
            except Exception as e:
                print(f" error: {e}")
                has_failure = True

        all_results[bench["id"]] = {
            "meta": bench,
            "results": bench_res,
        }

        if args.hyperfine and tools["hyperfine"] and bench["kind"] == "batch":
            print(f"\n    Running hyperfine comparison for {bench['id']}...")
            bins = [
                str(out_b_dir / f"{bench['id']}_{lang}")
                for lang in LANGUAGES
                if lang in bench_res
            ]
            if bins:
                subprocess.run(
                    [
                        tools["hyperfine"],
                        "--warmup",
                        str(args.warmup),
                        "--runs",
                        str(args.runs),
                    ]
                    + bins
                )

    # Output Markdown Report
    report_path_md = BENCH_DIR / "RESULTS.md"
    with open(report_path_md, "w", encoding="utf-8") as f:
        f.write("# Vela Benchmark Results\n\n")
        f.write(
            "Comparison between **Vela**, **C (GCC -O3)**, **Rust (opt-level=3, LTO)**, **Go (1.26)**, and **Zig (ReleaseFast)** across 9 comprehensive workloads.\n\n"
        )
        f.write(
            f"* Runs: {args.runs} (Warmup: {args.warmup}) | Direct timing via `time.perf_counter()`; Peak RSS via GNU `time -v` / `/proc/pid/status` `VmHWM`. All binaries stripped.\n\n"
        )

        for b_id, data in all_results.items():
            meta = data["meta"]
            results = data["results"]
            is_http = meta["kind"] == "http"
            f.write(f"### {meta['name']} ({meta['category']})\n\n")
            f.write(f"*{meta['desc']}*\n\n")

            if is_http:
                f.write(
                    "| Language | Throughput (RPS) | P95 Latency | 2000 Reqs Time | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |\n"
                )
                f.write("| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |\n")
                best_rps = max((r.get("rps", 0.0) for r in results.values()), default=1.0)
                for lang in LANGUAGES:
                    if lang not in results:
                        continue
                    r = results[lang]
                    ratio = best_rps / r["rps"]
                    ratio_str = "1.00x (fastest)" if ratio <= 1.005 else f"{ratio:.2f}x"
                    rss_str = f"{r['rss_kb']} KB" if r["rss_kb"] < 1024 else f"{r['rss_kb']/1024.0:.1f} MB"
                    size_str = f"{r['bin_size_kb']:.1f} KB" if r["bin_size_kb"] < 1024 else f"{r['bin_size_kb']/1024.0:.2f} MB"
                    f.write(
                        f"| **{lang.upper()}** | **{r['rps']:,.0f} req/s** | {r['p95_ms']:.2f} ms | {format_duration(r['median'])} | {rss_str} | {size_str} | {format_duration(r['compile_s'])} | {ratio_str} |\n"
                    )
            else:
                f.write(
                    "| Language | Median Time | Mean ± Stddev | CPU (User + Sys) | Peak RSS | Binary Size | Compile Time | Rel. to Fastest |\n"
                )
                f.write("| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |\n")
                fastest_med = min((r["median"] for r in results.values()), default=1.0)
                for lang in LANGUAGES:
                    if lang not in results:
                        continue
                    r = results[lang]
                    ratio = r["median"] / fastest_med
                    ratio_str = "1.00x (fastest)" if ratio <= 1.005 else f"{ratio:.2f}x"
                    mean_str = f"{format_duration(r['mean'])} ± {format_duration(r['stddev'])}"
                    cpu_str = f"{r['user_s'] + r['sys_s']:.3f}s"
                    rss_str = f"{r['rss_kb']} KB" if r["rss_kb"] < 1024 else f"{r['rss_kb']/1024.0:.1f} MB"
                    size_str = f"{r['bin_size_kb']:.1f} KB" if r["bin_size_kb"] < 1024 else f"{r['bin_size_kb']/1024.0:.2f} MB"
                    f.write(
                        f"| **{lang.upper()}** | `{format_duration(r['median'])}` | `{mean_str}` | {cpu_str} | {rss_str} | {size_str} | {format_duration(r['compile_s'])} | {ratio_str} |\n"
                    )
            f.write("\n")

    report_path_html = BENCH_DIR / "RESULTS.html"
    generate_html_report(all_results, args.runs, args.warmup, report_path_html)

    print("\n" + "=" * 84)
    print(f"Markdown Results saved to: {report_path_md}")
    print(f"HTML Web App saved to:     {report_path_html}")
    print("=" * 84)

    if has_failure:
        sys.exit(1)


if __name__ == "__main__":
    main()
