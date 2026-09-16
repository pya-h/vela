//! The HTTP API and page, through the router without a network: the security
//! checks, workspace confinement, files, and builds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::json;

use common::*;

#[tokio::test]
async fn the_api_requires_the_token() {
    let dir = TempDir::new("token");
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let bare = || Request::get("/api/info").header("host", "127.0.0.1:4000");

    let missing = send(&app, bare().body(Body::empty()).unwrap()).await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED);
    assert_eq!(missing.header("www-authenticate"), Some("Bearer"));
    assert_eq!(missing.json()["error"], "missing or wrong session token");

    let other = vela_studio::Token::generate().unwrap();
    for value in [format!("Bearer {}", other.as_str()), studio.config.token.as_str().to_string(), "Bearer ".to_string()] {
        let wrong = send(&app, bare().header("authorization", value).body(Body::empty()).unwrap()).await;
        assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    }

    let ok = call(&app, &studio, Method::GET, "/api/info", None).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.header("cache-control"), Some("no-store"));
    let info = ok.json();
    assert_eq!(info["name"], "Vela Studio");
    assert_eq!(info["workspace"]["name"], "ws");
    assert!(info["cc"]["available"].is_boolean());

    // Endpoints that change things are protected the same way.
    let write = send(
        &app,
        Request::post("/api/file/new")
            .header("host", "127.0.0.1:4000")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "path": "x.vela", "kind": "file" }).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(write.status, StatusCode::UNAUTHORIZED);
    assert!(!dir.exists("x.vela"));
}

#[tokio::test]
async fn requests_must_name_this_server() {
    let dir = TempDir::new("host");
    let (app, studio) = app(config(&dir.workspace(), PORT));
    for host in [Some("evil.example:4000"), Some("127.0.0.1:4001"), Some("localhost"), Some("[::1]:4000"), None] {
        for uri in ["/", "/api/info"] {
            let mut builder = Request::get(uri).header("authorization", format!("Bearer {}", studio.config.token.as_str()));
            if let Some(host) = host {
                builder = builder.header("host", host);
            }
            let reply = send(&app, builder.body(Body::empty()).unwrap()).await;
            assert_eq!(reply.status, StatusCode::FORBIDDEN, "{:?} {}", host, uri);
        }
    }
    let localhost = Request::get("/api/info")
        .header("host", "localhost:4000")
        .header("authorization", format!("Bearer {}", studio.config.token.as_str()));
    assert_eq!(send(&app, localhost.body(Body::empty()).unwrap()).await.status, StatusCode::OK);
}

#[tokio::test]
async fn other_origins_are_rejected_and_cors_is_never_allowed() {
    let dir = TempDir::new("origin");
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let create = |origin: &str, path: &str| {
        request(&studio, Method::POST, "/api/file/new", Some(json!({})))
            .header("origin", origin)
            .body(Body::from(json!({ "path": path, "kind": "file" }).to_string()))
            .unwrap()
    };
    for origin in ["http://evil.example", "null", "http://127.0.0.1:4001", "https://127.0.0.1:4000"] {
        let reply = send(&app, create(origin, "evil.vela")).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", origin);
        assert_eq!(reply.header("access-control-allow-origin"), None);
    }
    assert!(!dir.exists("evil.vela"));
    assert_eq!(send(&app, create("http://127.0.0.1:4000", "ok.vela")).await.status, StatusCode::CREATED);

    // A CORS preflight gets no permission, from any origin.
    for origin in ["http://evil.example", "http://127.0.0.1:4000"] {
        let preflight = Request::builder()
            .method(Method::OPTIONS)
            .uri("/api/file")
            .header("host", "127.0.0.1:4000")
            .header("origin", origin)
            .header("access-control-request-method", "PUT")
            .body(Body::empty())
            .unwrap();
        let reply = send(&app, preflight).await;
        assert!(!reply.status.is_success(), "{} {}", origin, reply.status);
        assert_eq!(reply.header("access-control-allow-origin"), None);
    }
}

#[tokio::test]
async fn the_page_is_served_with_a_strict_policy() {
    let dir = TempDir::new("page");
    let (app, studio) = app(config(&dir.workspace(), PORT));
    // The page itself needs no token: the token arrives in its URL fragment.
    let page = send(&app, Request::get("/").header("host", "127.0.0.1:4000").body(Body::empty()).unwrap()).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.header("content-type"), Some("text/html; charset=utf-8"));
    let policy = page.header("content-security-policy").unwrap();
    for part in ["default-src 'self'", "script-src 'self' 'wasm-unsafe-eval'", "connect-src 'self' ws://127.0.0.1:4000 ws://localhost:4000", "frame-ancestors 'none'"] {
        assert!(policy.contains(part), "{:?} in {:?}", part, policy);
    }
    assert!(!policy.contains("unsafe-inline"));
    // Each page gets its own nonce for the editor's style elements, in the
    // policy and in the page's meta tag.
    let nonce_in = |page: &common::Reply| {
        let policy = page.header("content-security-policy").unwrap();
        let nonce = policy.split("'nonce-").nth(1).unwrap().split('\'').next().unwrap().to_string();
        assert_eq!(nonce.len(), 32);
        let meta = format!("name=\"csp-nonce\" content=\"{}\"", nonce);
        assert!(String::from_utf8_lossy(&page.body).contains(&meta), "{}", meta);
        nonce
    };
    let again = send(&app, Request::get("/").header("host", "127.0.0.1:4000").body(Body::empty()).unwrap()).await;
    assert_ne!(nonce_in(&page), nonce_in(&again));
    assert!(!String::from_utf8_lossy(&page.body).contains("__VELA_CSP_NONCE__"));
    assert_eq!(page.header("x-frame-options"), Some("DENY"));
    assert_eq!(page.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    assert!(String::from_utf8_lossy(&page.body).contains("Vela Studio"));

    let missing = send(&app, Request::get("/no-such-file.js").header("host", "127.0.0.1:4000").body(Body::empty()).unwrap()).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let post = send(&app, Request::post("/").header("host", "127.0.0.1:4000").body(Body::empty()).unwrap()).await;
    assert_eq!(post.status, StatusCode::METHOD_NOT_ALLOWED);
    let unknown = call(&app, &studio, Method::GET, "/api/nothing", None).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.json()["error"], "no such API endpoint");
}

#[tokio::test]
async fn the_file_tree_lists_the_workspace() {
    let dir = TempDir::new("tree");
    for path in ["b.vela", "A.vela", "lib/x.vela", ".hidden/y.vela", ".env", "target/debug/z", "node_modules/q/index.js"] {
        dir.write(path, "");
    }
    std::fs::create_dir(dir.workspace().join("empty")).unwrap();
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let reply = call(&app, &studio, Method::GET, "/api/files", None).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json(),
        json!({
            "entries": [
                { "name": "empty", "path": "empty", "kind": "folder", "children": [] },
                { "name": "lib", "path": "lib", "kind": "folder", "children": [
                    { "name": "x.vela", "path": "lib/x.vela", "kind": "file" }
                ] },
                { "name": "A.vela", "path": "A.vela", "kind": "file" },
                { "name": "b.vela", "path": "b.vela", "kind": "file" }
            ],
            "truncated": false
        })
    );
}

#[tokio::test]
async fn files_round_trip_and_changes_on_disk_are_detected() {
    let dir = TempDir::new("round-trip");
    dir.write("dir/main.vela", HELLO);
    let (app, studio) = app(config(&dir.workspace(), PORT));

    let read = call(&app, &studio, Method::GET, "/api/file?path=dir%2Fmain.vela", None).await;
    assert_eq!(read.status, StatusCode::OK);
    let file = read.json();
    assert_eq!(file["path"], "dir/main.vela");
    assert_eq!(file["content"], HELLO);
    let hash = file["hash"].as_str().unwrap().to_string();

    let edited = "task main()\n    show(\"é𝄞\")\nfi\n";
    let write = call(&app, &studio, Method::PUT, "/api/file", Some(json!({ "path": "dir/main.vela", "content": edited, "baseHash": hash }))).await;
    assert_eq!(write.status, StatusCode::OK, "{:?}", write.json());
    let new_hash = write.json()["hash"].as_str().unwrap().to_string();
    assert_ne!(new_hash, hash);
    assert_eq!(dir.read("dir/main.vela"), edited);
    let reread = call(&app, &studio, Method::GET, "/api/file?path=dir/main.vela", None).await.json();
    assert_eq!((reread["content"].as_str(), reread["hash"].as_str()), (Some(edited), Some(new_hash.as_str())));

    // Another program changes the file: a save based on the old hash is refused.
    dir.write("dir/main.vela", HELLO);
    let stale = call(&app, &studio, Method::PUT, "/api/file", Some(json!({ "path": "dir/main.vela", "content": "lost", "baseHash": new_hash }))).await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(stale.json()["error"], "the file changed on disk");
    assert_eq!(stale.json()["hash"], hash);
    assert_eq!(dir.read("dir/main.vela"), HELLO);

    // Without a base hash, the save overwrites; no temporary files are left.
    let forced = call(&app, &studio, Method::PUT, "/api/file", Some(json!({ "path": "dir/main.vela", "content": "kept" }))).await;
    assert_eq!(forced.status, StatusCode::OK);
    assert_eq!(dir.read("dir/main.vela"), "kept");
    let names: Vec<_> = std::fs::read_dir(dir.workspace().join("dir")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["main.vela"]);

    // Saving only replaces existing files.
    let absent = call(&app, &studio, Method::PUT, "/api/file", Some(json!({ "path": "new.vela", "content": "" }))).await;
    assert_eq!(absent.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn files_and_folders_are_created_renamed_and_deleted() {
    let dir = TempDir::new("operations");
    dir.write("lib/keep.vela", HELLO);
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let post = |uri: &'static str, body: serde_json::Value| {
        let (app, studio) = (app.clone(), studio.clone());
        async move { call(&app, &studio, Method::POST, uri, Some(body)).await }
    };

    let created = post("/api/file/new", json!({ "path": "lib/new.vela", "kind": "file" })).await;
    assert_eq!((created.status, created.json()["path"].as_str()), (StatusCode::CREATED, Some("lib/new.vela")));
    assert_eq!(dir.read("lib/new.vela"), "");
    assert_eq!(post("/api/file/new", json!({ "path": "lib/new.vela", "kind": "file" })).await.status, StatusCode::CONFLICT);
    assert_eq!(post("/api/file/new", json!({ "path": "src", "kind": "folder" })).await.status, StatusCode::CREATED);
    assert!(dir.workspace().join("src").is_dir());
    assert_eq!(post("/api/file/new", json!({ "path": "missing/new.vela", "kind": "file" })).await.status, StatusCode::NOT_FOUND);
    assert_eq!(post("/api/file/new", json!({ "path": "lib/keep.vela/inner", "kind": "file" })).await.status, StatusCode::NOT_FOUND);

    let renamed = post("/api/file/rename", json!({ "from": "lib/new.vela", "to": "src/moved.vela" })).await;
    assert_eq!((renamed.status, renamed.json()["path"].as_str()), (StatusCode::OK, Some("src/moved.vela")));
    assert!(dir.exists("src/moved.vela") && !dir.exists("lib/new.vela"));
    assert_eq!(post("/api/file/rename", json!({ "from": "src/moved.vela", "to": "lib/keep.vela" })).await.status, StatusCode::CONFLICT);
    assert_eq!(dir.read("lib/keep.vela"), HELLO);
    assert_eq!(post("/api/file/rename", json!({ "from": "nothing.vela", "to": "b.vela" })).await.status, StatusCode::NOT_FOUND);
    let into_itself = post("/api/file/rename", json!({ "from": "src", "to": "src/inner" })).await;
    assert_eq!(into_itself.status, StatusCode::BAD_REQUEST);

    assert_eq!(post("/api/file/delete", json!({ "path": "src/moved.vela" })).await.status, StatusCode::NO_CONTENT);
    assert!(!dir.exists("src/moved.vela"));
    assert_eq!(post("/api/file/delete", json!({ "path": "lib" })).await.status, StatusCode::NO_CONTENT);
    assert!(!dir.exists("lib"));
    assert_eq!(post("/api/file/delete", json!({ "path": "lib" })).await.status, StatusCode::NOT_FOUND);

    // A folder isn't a file.
    let folder = call(&app, &studio, Method::GET, "/api/file?path=src", None).await;
    assert_eq!(folder.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn large_and_binary_files_are_refused() {
    let dir = TempDir::new("limits");
    dir.write("big.vela", vec![b'#'; vela_studio::MAX_FILE_BYTES + 1]);
    dir.write("exact.vela", vec![b'#'; vela_studio::MAX_FILE_BYTES]);
    dir.write("program", [0x7f, b'E', b'L', b'F', 0xff, 0xfe]);
    let (app, studio) = app(config(&dir.workspace(), PORT));

    assert_eq!(call(&app, &studio, Method::GET, "/api/file?path=big.vela", None).await.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(call(&app, &studio, Method::GET, "/api/file?path=exact.vela", None).await.status, StatusCode::OK);
    assert_eq!(call(&app, &studio, Method::GET, "/api/file?path=program", None).await.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let too_big = "#".repeat(vela_studio::MAX_FILE_BYTES + 1);
    let write = call(&app, &studio, Method::PUT, "/api/file", Some(json!({ "path": "exact.vela", "content": too_big }))).await;
    assert_eq!(write.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn paths_stay_inside_the_workspace() {
    let dir = TempDir::new("confinement");
    dir.write("inside.vela", HELLO);
    std::fs::write(dir.outside().join("secret.txt"), "secret").unwrap();
    let (app, studio) = app(config(&dir.workspace(), PORT));

    for bad in ["", "../outside/secret.txt", "/etc/passwd", "a/../../outside/secret.txt", "./inside.vela", "a//b", "..\\outside\\secret.txt", "C:secret"] {
        let read = call(&app, &studio, Method::GET, &format!("/api/file?path={}", encode(bad)), None).await;
        assert_eq!(read.status, StatusCode::BAD_REQUEST, "read {:?}", bad);
        let write = call(&app, &studio, Method::PUT, "/api/file", Some(json!({ "path": bad, "content": "x" }))).await;
        assert_eq!(write.status, StatusCode::BAD_REQUEST, "write {:?}", bad);
        let create = call(&app, &studio, Method::POST, "/api/file/new", Some(json!({ "path": bad, "kind": "file" }))).await;
        assert_eq!(create.status, StatusCode::BAD_REQUEST, "create {:?}", bad);
    }
    // A missing path parameter is refused too.
    assert_eq!(call(&app, &studio, Method::GET, "/api/file", None).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(std::fs::read_to_string(dir.outside().join("secret.txt")).unwrap(), "secret");
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_cannot_lead_out_of_the_workspace() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("symlinks");
    dir.write("inside.vela", HELLO);
    std::fs::write(dir.outside().join("secret.txt"), "secret").unwrap();
    symlink(dir.outside().join("secret.txt"), dir.workspace().join("file-link")).unwrap();
    symlink(dir.outside(), dir.workspace().join("folder-link")).unwrap();
    symlink(dir.workspace().join("inside.vela"), dir.workspace().join("inner-link.vela")).unwrap();
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let forbidden = |reply: common::Reply, what: &str| assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", what);

    forbidden(call(&app, &studio, Method::GET, "/api/file?path=file-link", None).await, "read through a file link");
    forbidden(call(&app, &studio, Method::GET, "/api/file?path=folder-link/secret.txt", None).await, "read through a folder link");
    forbidden(call(&app, &studio, Method::PUT, "/api/file", Some(json!({ "path": "file-link", "content": "x" }))).await, "write");
    forbidden(call(&app, &studio, Method::POST, "/api/file/new", Some(json!({ "path": "folder-link/new.txt", "kind": "file" }))).await, "create");
    forbidden(call(&app, &studio, Method::POST, "/api/file/rename", Some(json!({ "from": "inside.vela", "to": "folder-link/moved.vela" }))).await, "rename out");
    forbidden(call(&app, &studio, Method::POST, "/api/file/delete", Some(json!({ "path": "folder-link/secret.txt" }))).await, "delete");
    forbidden(call(&app, &studio, Method::POST, "/api/build", Some(json!({ "path": "file-link" }))).await, "build");
    assert!(!dir.outside().join("new.txt").exists() && !dir.outside().join("moved.vela").exists());
    assert!(dir.exists("inside.vela"));

    // A link inside the workspace works, and appears in the tree; links out don't.
    let inner = call(&app, &studio, Method::GET, "/api/file?path=inner-link.vela", None).await;
    assert_eq!(inner.json()["content"], HELLO);
    let tree = call(&app, &studio, Method::GET, "/api/files", None).await.json();
    let names: Vec<&str> = tree["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["inner-link.vela", "inside.vela"]);

    // Deleting a link deletes the link, not its target.
    assert_eq!(call(&app, &studio, Method::POST, "/api/file/delete", Some(json!({ "path": "folder-link" }))).await.status, StatusCode::NO_CONTENT);
    assert_eq!(call(&app, &studio, Method::POST, "/api/file/delete", Some(json!({ "path": "file-link" }))).await.status, StatusCode::NO_CONTENT);
    assert_eq!(std::fs::read_to_string(dir.outside().join("secret.txt")).unwrap(), "secret");
}

#[tokio::test]
async fn builds_read_modules_from_the_workspace_only() {
    let dir = TempDir::new("modules");
    std::fs::create_dir_all(dir.workspace().join("app/shapes")).unwrap();
    dir.write("app/main.vela", "use shapes/geometry\n\ntask main()\n    show(geometry.double(21))\nfi\n");
    dir.write("app/shapes/geometry.vela", "task double(n: int) -> int\n    <- n * 2\nfi\n");
    dir.write("app/broken.vela", "use shapes/bad\n\ntask main()\n    show(bad.double(21))\nfi\n");
    dir.write("app/shapes/bad.vela", "task double(n: int) -> int\n    <- missing\nfi\n");
    dir.write("app/escape.vela", "use ../../outside\n\ntask main()\nfi\n");
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let build = |path: &'static str| {
        let (app, studio) = (app.clone(), studio.clone());
        async move { call(&app, &studio, Method::POST, "/api/build", Some(json!({ "path": path }))).await.json() }
    };

    assert_eq!(build("app/main.vela").await, json!({ "ok": true }));

    // An error in a module names that module's file, as the workspace knows it.
    let broken = build("app/broken.vela").await;
    assert_eq!(broken["error"]["file"], "app/shapes/bad.vela");
    assert_eq!((broken["error"]["line"].clone(), broken["error"]["col"].clone()), (json!(2), json!(8)));
    assert_eq!(
        broken["error"]["rendered"],
        "app/shapes/bad.vela:2:8: error: unknown variable 'missing'\n 2 |     <- missing\n   |        ^"
    );

    // A module outside the workspace isn't read.
    let escape = build("app/escape.vela").await;
    assert_eq!(escape["error"]["file"], "app/escape.vela");
    assert_eq!(escape["error"]["message"], "can't read module '../../outside' (../outside.vela): it isn't in the workspace");
}

#[tokio::test]
async fn builds_report_errors_with_positions_and_leave_the_workspace_alone() {
    let dir = TempDir::new("build");
    dir.write("hello.vela", HELLO);
    dir.write("bad.vela", "task main()\n    show(missing)\nfi\n");
    dir.write("lib.vela", "task helper()\n    show(1)\nfi\n");
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let build = |path: &'static str| {
        let (app, studio) = (app.clone(), studio.clone());
        async move { call(&app, &studio, Method::POST, "/api/build", Some(json!({ "path": path }))).await }
    };

    let ok = build("hello.vela").await;
    assert_eq!((ok.status, ok.json()), (StatusCode::OK, json!({ "ok": true })));

    let bad = build("bad.vela").await;
    assert_eq!(bad.status, StatusCode::OK);
    assert_eq!(
        bad.json(),
        json!({ "ok": false, "error": {
            "kind": "vela",
            "message": "unknown variable 'missing'",
            "file": "bad.vela",
            "line": 2,
            "col": 10,
            "rendered": "bad.vela:2:10: error: unknown variable 'missing'\n 2 |     show(missing)\n   |          ^"
        } })
    );
    let no_main = build("lib.vela").await.json();
    assert_eq!(no_main["error"]["kind"], "no-main");
    assert_eq!(no_main["error"]["rendered"], "lib.vela: error: program has no 'main' task");
    assert_eq!(build("missing.vela").await.status, StatusCode::NOT_FOUND);

    // Nothing was written into the workspace.
    let mut names: Vec<_> = std::fs::read_dir(dir.workspace()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["bad.vela", "hello.vela", "lib.vela"]);
}

#[tokio::test]
async fn a_missing_c_compiler_is_reported() {
    let dir = TempDir::new("no-cc");
    dir.write("hello.vela", HELLO);
    let mut config = config(&dir.workspace(), PORT);
    config.cc = "vela-no-such-cc".to_string();
    let (app, studio) = app(config);
    assert_eq!(call(&app, &studio, Method::GET, "/api/info", None).await.json()["cc"], json!({ "command": "vela-no-such-cc", "available": false }));
    let build = call(&app, &studio, Method::POST, "/api/build", Some(json!({ "path": "hello.vela" }))).await.json();
    assert_eq!(build["error"]["kind"], "compiler-missing");
}

#[tokio::test]
async fn a_launch_code_gives_the_token_once() {
    let dir = TempDir::new("launch");
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let redeem = |code: String| {
        Request::post("/api/launch")
            .header("host", "127.0.0.1:4000")
            .header("origin", "http://127.0.0.1:4000")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "code": code }).to_string()))
            .unwrap()
    };
    // No code has been made yet, and a guess is refused.
    assert_eq!(send(&app, redeem("0".repeat(64))).await.status, StatusCode::FORBIDDEN);
    let code = studio.launch_code().unwrap();
    let first = send(&app, redeem(code.clone())).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.json()["token"], studio.config.token.as_str());
    assert_eq!(first.header("cache-control"), Some("no-store"));
    let again = send(&app, redeem(code)).await;
    assert_eq!(again.status, StatusCode::FORBIDDEN);
    assert_eq!(again.json()["error"], "this link has already been used or has expired");
}

#[tokio::test]
async fn the_launch_page_is_private_and_holds_a_working_code() {
    let dir = TempDir::new("launch-page");
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let page = studio.launch_page().unwrap();
    let html = std::fs::read_to_string(&page.path).unwrap();
    let code = html.split("#launch=").nth(1).and_then(|rest| rest.split('"').next()).unwrap().to_string();
    assert!(html.contains(&format!("http://127.0.0.1:{}/#launch={}", PORT, code)), "{}", html);
    assert!(!html.contains(studio.config.token.as_str()), "the page must not hold the token itself");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&page.path), 0o600);
        assert_eq!(mode(page.path.parent().unwrap()), 0o700);
    }
    let redeem = Request::post("/api/launch")
        .header("host", "127.0.0.1:4000")
        .header("origin", "http://127.0.0.1:4000")
        .header("content-type", "application/json")
        .body(Body::from(json!({ "code": code }).to_string()))
        .unwrap();
    assert_eq!(send(&app, redeem).await.status, StatusCode::OK);
    let path = page.path.clone();
    drop(page);
    assert!(!path.exists() && !path.parent().unwrap().exists());
}

#[tokio::test]
async fn malformed_requests_get_json_errors() {
    let dir = TempDir::new("malformed");
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let missing_query = call(&app, &studio, Method::GET, "/api/file", None).await;
    assert_eq!(missing_query.status, StatusCode::BAD_REQUEST);
    assert!(missing_query.json()["error"].is_string());
    let wrong_shape = call(&app, &studio, Method::POST, "/api/file/new", Some(json!({ "name": "x" }))).await;
    assert_eq!(wrong_shape.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(wrong_shape.json()["error"].is_string());
}

#[cfg(unix)]
#[tokio::test]
async fn special_files_are_not_listed_or_read() {
    let dir = TempDir::new("fifo");
    dir.write("hello.vela", HELLO);
    let fifo = dir.workspace().join("pipe");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    let (app, studio) = app(config(&dir.workspace(), PORT));
    let tree = call(&app, &studio, Method::GET, "/api/files", None).await.json();
    assert_eq!(tree["entries"].as_array().unwrap().len(), 1, "{}", tree);
    // Reading a pipe would block until something writes to it.
    let read = tokio::time::timeout(std::time::Duration::from_secs(5), call(&app, &studio, Method::GET, "/api/file?path=pipe", None)).await;
    assert_eq!(read.expect("reading a pipe must not block").status, StatusCode::BAD_REQUEST);
}

#[cfg(unix)]
#[tokio::test]
async fn saving_respects_read_only_files_and_long_names() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("save-limits");
    let locked = dir.write("locked.vela", HELLO);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o444)).unwrap();
    let long = format!("{}.vela", "n".repeat(240));
    dir.write(&long, HELLO);
    let (app, studio) = app(config(&dir.workspace(), PORT));

    let refused = call(&app, &studio, Method::PUT, "/api/file", Some(json!({ "path": "locked.vela", "content": "x" }))).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.json()["error"], "the file is read-only");
    assert_eq!(dir.read("locked.vela"), HELLO);
    // Writable by the group and others, but not by its owner, this user.
    let others = dir.write("others.vela", HELLO);
    std::fs::set_permissions(&others, std::fs::Permissions::from_mode(0o464)).unwrap();
    // Root can write any file, so there is nothing to refuse.
    if std::fs::OpenOptions::new().write(true).open(&others).is_err() {
        let refused = call(&app, &studio, Method::PUT, "/api/file", Some(json!({ "path": "others.vela", "content": "x" }))).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert_eq!(dir.read("others.vela"), HELLO);
    }

    let saved = call(&app, &studio, Method::PUT, "/api/file", Some(json!({ "path": long, "content": "x" }))).await;
    assert_eq!(saved.status, StatusCode::OK, "{:?}", String::from_utf8_lossy(&saved.body));
    assert_eq!(dir.read(&long), "x");
}
