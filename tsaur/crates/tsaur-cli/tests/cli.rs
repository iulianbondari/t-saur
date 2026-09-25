//! End-to-end tests of the `tsaur` binary: exit codes, JSON output, agent operations.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tsaur"))
}

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tsaur-cli-{}-{}-{}", tag, std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(dir.join("src/docs")).unwrap();
    let text: String = (0..3000).map(|i| format!("line {i}: the archive verifies every chunk with blake3\n")).collect();
    std::fs::write(dir.join("src/docs/notes.md"), &text).unwrap();
    std::fs::write(dir.join("src/readme.txt"), "T-saur test fixture\nsecond line\nthird line with needle\n").unwrap();
    // 200 KB of incompressible bytes so that the archive spans many 4 KiB pieces
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let blob: Vec<u8> = (0..200_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 56) as u8
        })
        .collect();
    std::fs::write(dir.join("src/blob.bin"), blob).unwrap();
    dir
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = bin().args(args).output().expect("spawn tsaur");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string())
}

fn s(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

#[test]
fn pack_list_stat_grep_read_verify_unpack() {
    let dir = fixture("basic");
    let archive = dir.join("a.tsr");
    let (code, stdout, stderr) = run(&["pack", &s(&archive), &s(&dir.join("src")), "--json", "--canonical"]);
    assert_eq!(code, 0, "pack failed: {stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["entries"], 3);
    assert_eq!(report["views"], 0, "no DOCX/PDF in this fixture, so no stored views");
    assert!(report["archive_bytes"].as_u64().unwrap() < 260_000);

    let (code, stdout, _) = run(&["list", &s(&archive), "--json"]);
    assert_eq!(code, 0);
    let list: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let paths: Vec<&str> = list["entries"].as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap()).collect();
    assert_eq!(paths, vec!["blob.bin", "docs/notes.md", "readme.txt"]);
    assert!(list["entries"][1]["tokens_est"].as_u64().unwrap() > 1000);

    let (code, stdout, _) = run(&["list", &s(&archive), "--md"]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("# TSAUR archive"));
    assert!(stdout.contains("| docs/notes.md |"));

    let (code, stdout, _) = run(&["stat", &s(&archive), "docs/notes.md", "--json"]);
    assert_eq!(code, 0);
    let st: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(st["mode"], "raw");
    assert_eq!(st["verified"], true);

    let (code, stdout, _) = run(&["grep", &s(&archive), "needle", "--json"]);
    assert_eq!(code, 0);
    let hits: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(hits.as_array().unwrap().len(), 1);
    assert_eq!(hits[0]["path"], "readme.txt");
    assert_eq!(hits[0]["line"], 3);

    let (code, stdout, _) = run(&["read", &s(&archive), "readme.txt", "--lines", "2-2"]);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "second line");
    let (code, stdout, _) = run(&["read", &s(&archive), "docs/notes.md", "--bytes", "0-7"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "line 0:");

    let (code, stdout, _) = run(&["verify", &s(&archive), "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["entries_ok"], 3);

    let out = dir.join("out");
    let (code, _, _) = run(&["unpack", &s(&archive), &s(&out), "--entry", "docs/*"]);
    assert_eq!(code, 0);
    assert!(out.join("docs/notes.md").exists() && !out.join("readme.txt").exists());
    // a file that already exists is never replaced without --overwrite, and the refusal happens
    // before anything else is written
    let (code, _, stderr) = run(&["unpack", &s(&archive), &s(&out)]);
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("--overwrite") && !out.join("readme.txt").exists(), "{stderr}");
    let (code, _, _) = run(&["unpack", &s(&archive), &s(&out), "--overwrite"]);
    assert_eq!(code, 0);
    for f in ["docs/notes.md", "readme.txt", "blob.bin"] {
        assert_eq!(std::fs::read(dir.join("src").join(f)).unwrap(), std::fs::read(out.join(f)).unwrap(), "{f}");
    }
}

#[test]
fn exit_codes_and_errors() {
    let dir = fixture("errors");
    let archive = dir.join("e.tsr");
    // missing input -> 5
    let (code, _, stderr) = run(&["pack", &s(&archive), &s(&dir.join("nope"))]);
    assert_eq!(code, 5, "{stderr}");
    // encrypted archive without password -> 6; wrong password -> 6
    let (code, _, _) = run(&["pack", &s(&archive), &s(&dir.join("src")), "--password", "pw", "--kdf-memory-mib", "64", "--codec", "zstd", "--level", "3"]);
    assert_eq!(code, 0);
    let (code, _, stderr) = run(&["list", &s(&archive), "--password-env", "TSAUR_NO_SUCH_VAR"]);
    assert_eq!(code, 6, "{stderr}");
    let (code, _, _) = run(&["list", &s(&archive), "--password", "wrong"]);
    assert_eq!(code, 6);
    // corrupt archive -> 2 (and JSON error object on stderr with --json)
    let mut bytes = std::fs::read(&archive).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&archive, &bytes).unwrap();
    let (code, _, stderr) = run(&["list", &s(&archive), "--password", "pw", "--json"]);
    assert_eq!(code, 2, "{stderr}");
    let err: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(err["code"], 2);
    // not an archive -> 2
    std::fs::write(dir.join("junk.tsr"), b"definitely not an archive").unwrap();
    let (code, _, _) = run(&["verify", &s(&dir.join("junk.tsr"))]);
    assert_eq!(code, 2);
}

#[test]
fn pieces_recover_and_reference_flow() {
    let dir = fixture("pieces");
    let archive = dir.join("p.tsr");
    let (code, _, _) = run(&["pack", &s(&archive), &s(&dir.join("src")), "--pieces", "--piece-size-kib", "4", "--parity-pct", "25", "--codec", "zstd"]);
    assert_eq!(code, 0);
    let original = std::fs::read(&archive).unwrap();
    assert!(original.len() > 5 * 4096, "fixture archive should span more than 5 pieces, got {}", original.len());
    let mut bytes = original.clone();
    bytes[4096 + 10] ^= 0x55; // piece 1
    bytes[3 * 4096 + 20] ^= 0x55; // piece 3
    std::fs::write(&archive, &bytes).unwrap();
    let (code, stdout, _) = run(&["verify", &s(&archive), "--pieces", "--json"]);
    assert_eq!(code, 2);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["pieces"]["bad"], serde_json::json!([1, 3]));
    let (code, stdout, _) = run(&["recover", &s(&archive), "--json"]);
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(std::fs::read(&archive).unwrap(), original);

    // reference flow: incremental archive resolves through --ref
    let inc = dir.join("inc.tsr");
    std::fs::write(dir.join("src/readme.txt"), "T-saur test fixture\nsecond line\nthird line with needle\nfourth line\n").unwrap();
    let (code, stdout, _) = run(&["pack", &s(&inc), &s(&dir.join("src")), "--ref", &s(&archive), "--json", "--codec", "zstd"]);
    assert_eq!(code, 0);
    let rep: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(rep["referenced_chunks"].as_u64().unwrap() >= 2, "{rep}");
    let (code, _, _) = run(&["verify", &s(&inc)]);
    assert_eq!(code, 2, "without the reference the archive must not verify");
    let (code, _, _) = run(&["verify", &s(&inc), "--ref", &s(&archive)]);
    assert_eq!(code, 0);
    let out = dir.join("inc-out");
    let (code, _, _) = run(&["unpack", &s(&inc), &s(&out), "--ref", &s(&archive)]);
    assert_eq!(code, 0);
    assert_eq!(std::fs::read(dir.join("src/readme.txt")).unwrap(), std::fs::read(out.join("readme.txt")).unwrap());
}

#[test]
fn info_and_mcp_server() {
    use serde_json::json;
    use std::io::Write;
    use std::process::Stdio;
    let dir = fixture("mcp");
    let archive = dir.join("m.tsr");
    let (code, _, stderr) = run(&["pack", &s(&archive), &s(&dir.join("src"))]);
    assert_eq!(code, 0, "{stderr}");

    // info: open archive
    let (code, stdout, _) = run(&["info", &s(&archive), "--json"]);
    assert_eq!(code, 0);
    let info: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(info["entries"], 3);
    assert_eq!(info["locked"], false);
    assert!(info["sections"].as_array().unwrap().iter().any(|x| x["type"] == "blobs"));
    assert!(info["blobs"]["count"].as_u64().unwrap() >= 1);
    assert!(info["ratio_pct"].as_f64().unwrap() > 0.0);
    // info: locked archive (no password given) still reports framing and stanzas
    let enc = dir.join("e.tsr");
    let (code, _, stderr) = run(&["pack", &s(&enc), &s(&dir.join("src")), "--password", "pw", "--kdf-memory-mib", "64"]);
    assert_eq!(code, 0, "{stderr}");
    let (code, stdout, _) = run(&["info", &s(&enc), "--json"]);
    assert_eq!(code, 0);
    let info: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(info["locked"], true);
    assert_eq!(info["encrypted"], true);
    assert_eq!(info["stanzas"][0]["t"], "argon2id");

    // an existing archive outside the allowed root must be refused by the MCP server
    let outside = std::env::temp_dir().join(format!("tsaur-outside-{}.tsr", std::process::id()));
    std::fs::copy(&archive, &outside).unwrap();

    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "tsaur_list", "arguments": {"archive": s(&archive)}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "tsaur_grep", "arguments": {"archive": s(&archive), "pattern": "needle"}}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "tsaur_read", "arguments": {"archive": s(&archive), "entry": "readme.txt", "lines": "2-2"}}}),
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "tsaur_read", "arguments": {"archive": s(&archive), "entry": "blob.bin", "bytes": "0-16"}}}),
        json!({"jsonrpc": "2.0", "id": 7, "method": "resources/list"}),
        json!({"jsonrpc": "2.0", "id": 8, "method": "resources/read", "params": {"uri": "tsaur://m.tsr/readme.txt"}}),
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "tsaur_stat", "arguments": {"archive": s(&archive), "entry": "missing.txt"}}}),
        json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "tsaur_list", "arguments": {"archive": s(&outside)}}}),
        json!({"jsonrpc": "2.0", "id": 11, "method": "nope"}),
        json!({"jsonrpc": "2.0", "id": 12, "method": "tools/call", "params": {"name": "tsaur_verify", "arguments": {"archive": s(&archive)}}}),
        json!({"jsonrpc": "2.0", "id": 13, "method": "tools/call", "params": {"name": "tsaur_unpack", "arguments": {"archive": s(&archive), "dir": s(&dir.join("out")), "entries": ["docs/*.md"]}}}),
        json!({"jsonrpc": "2.0", "id": 14, "method": "tools/call", "params": {"name": "tsaur_info", "arguments": {"archive": s(&enc)}}}),
        json!({"jsonrpc": "2.0", "id": 15, "method": "tools/call", "params": {"name": "no_such_tool", "arguments": {}}}),
        // modern era (2026-07-28): discovery, an unsupported per-request version, a supported one
        json!({"jsonrpc": "2.0", "id": 16, "method": "server/discover", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}}),
        json!({"jsonrpc": "2.0", "id": 17, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "1900-01-01"}}}),
        json!({"jsonrpc": "2.0", "id": 18, "method": "tools/list", "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}}),
        json!({"jsonrpc": "2.0", "id": 19, "method": "tools/call", "params": {"name": "tsaur_diff", "arguments": {"archive": s(&archive), "other": s(&archive)}}}),
        json!({"jsonrpc": "2.0", "id": 20, "method": "resources/read", "params": {"uri": "tsaur://m.tsr/TSAUR.md"}}),
    ];
    let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
    let mut child = bin().args(["mcp", "--root", &s(&dir), "--archive", &s(&archive)]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_file(&outside);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).expect("one JSON object per line")).collect();
    assert_eq!(lines.len(), requests.len() - 1, "one response per request, none for the notification");
    let by_id = |id: u64| lines.iter().find(|l| l["id"] == id).unwrap_or_else(|| panic!("no response with id {id}"));
    assert_eq!(by_id(1)["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(by_id(1)["result"]["serverInfo"]["name"], "tsaur");
    assert_eq!(by_id(2)["result"]["tools"].as_array().unwrap().len(), 8);
    assert_eq!(by_id(3)["result"]["isError"], false);
    assert_eq!(by_id(3)["result"]["structuredContent"]["entries"].as_array().unwrap().len(), 3);
    assert_eq!(by_id(4)["result"]["structuredContent"]["count"], 1);
    assert_eq!(by_id(5)["result"]["content"][0]["text"], "second line");
    assert_eq!(by_id(6)["result"]["structuredContent"]["encoding"], "base64");
    assert_eq!(by_id(6)["result"]["content"][0]["type"], "resource");
    assert_eq!(by_id(7)["result"]["resources"].as_array().unwrap().len(), 4, "3 entries + the generated TSAUR.md overview");
    assert_eq!(by_id(7)["result"]["resources"][0]["name"], "TSAUR.md");
    assert!(by_id(20)["result"]["contents"][0]["text"].as_str().unwrap().starts_with("# TSAUR archive m.tsr"));
    assert!(by_id(8)["result"]["contents"][0]["text"].as_str().unwrap().contains("needle"));
    assert_eq!(by_id(9)["result"]["isError"], true);
    assert_eq!(by_id(9)["result"]["structuredContent"]["exit_code"], 5);
    assert_eq!(by_id(10)["result"]["isError"], true);
    assert_eq!(by_id(10)["result"]["structuredContent"]["exit_code"], 3, "outside the roots is a policy error");
    assert_eq!(by_id(11)["error"]["code"], -32601);
    assert_eq!(by_id(12)["result"]["structuredContent"]["ok"], true);
    assert_eq!(by_id(13)["result"]["structuredContent"]["count"], 1);
    assert!(dir.join("out/docs/notes.md").exists());
    assert_eq!(by_id(14)["result"]["structuredContent"]["locked"], true);
    assert_eq!(by_id(15)["error"]["code"], -32602);
    assert_eq!(by_id(16)["result"]["supportedVersions"][0], "2026-07-28");
    assert_eq!(by_id(16)["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "tsaur");
    assert_eq!(by_id(17)["error"]["code"], -32022);
    assert_eq!(by_id(17)["error"]["data"]["supported"][0], "2026-07-28");
    assert_eq!(by_id(17)["error"]["data"]["requested"], "1900-01-01");
    assert_eq!(by_id(18)["result"]["tools"].as_array().unwrap().len(), 8);
    assert!(by_id(5)["result"]["structuredContent"]["cite"].as_str().unwrap().ends_with("/readme.txt#L2-2"));
    assert_eq!(by_id(19)["result"]["structuredContent"]["unchanged"], 3);
    assert_eq!(by_id(19)["result"]["structuredContent"]["identical"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn diff_between_archives() {
    let dir = fixture("diff");
    let a = dir.join("a.tsr");
    let (code, _, stderr) = run(&["pack", &s(&a), &s(&dir.join("src"))]);
    assert_eq!(code, 0, "{stderr}");
    // edit one file, add one, remove one
    std::fs::write(dir.join("src/readme.txt"), "T-saur test fixture\nsecond line CHANGED\nthird line with needle\n").unwrap();
    std::fs::write(dir.join("src/new.md"), "# new file\n").unwrap();
    std::fs::remove_file(dir.join("src/docs/notes.md")).unwrap();
    let b = dir.join("b.tsr");
    let (code, _, stderr) = run(&["pack", &s(&b), &s(&dir.join("src"))]);
    assert_eq!(code, 0, "{stderr}");
    let (code, stdout, _) = run(&["diff", &s(&a), &s(&b), "--json"]);
    assert_eq!(code, 0);
    let d: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(d["identical"], false);
    assert_eq!(d["added"][0]["path"], "new.md");
    assert_eq!(d["removed"][0]["path"], "docs/notes.md");
    assert_eq!(d["changed"][0]["path"], "readme.txt");
    assert_eq!(d["unchanged"], 1, "blob.bin is unchanged");
    // the 200 KB blob is shared chunk-for-chunk, so most of B's bytes are already in A
    assert!(d["chunks"]["shared_bytes"].as_u64().unwrap() >= 200_000);
    assert!(d["chunks"]["new_pct"].as_f64().unwrap() < 5.0);
    let (code, stdout, _) = run(&["diff", &s(&a), &s(&a)]);
    assert_eq!(code, 0);
    assert!(stdout.contains("IDENTICAL"));
    // list and stat expose the citation URI built from the Merkle root
    let (_, stdout, _) = run(&["stat", &s(&b), "new.md", "--json"]);
    let st: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(st["uri"], format!("tsaur://{}/new.md", d["b"]["root"].as_str().unwrap()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parallel_packing_is_deterministic() {
    // the batch scheduler must not change the output: --jobs 1 and --jobs 8 give identical bytes
    let dir = fixture("jobs");
    let a = dir.join("j1.tsr");
    let b = dir.join("j8.tsr");
    let (code, _, stderr) = run(&["pack", &s(&a), &s(&dir.join("src")), "--jobs", "1", "--block-kib", "64"]);
    assert_eq!(code, 0, "{stderr}");
    let (code, _, stderr) = run(&["pack", &s(&b), &s(&dir.join("src")), "--jobs", "8", "--block-kib", "64"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    // the same with the codec decided on a sample (default 1 MiB blocks: the fixture's block of
    // about 360 KiB is above the 128 KiB full-trial threshold)
    let a3 = dir.join("j1-e3.tsr");
    let b3 = dir.join("j8-e3.tsr");
    let (code, stdout, stderr) = run(&["pack", &s(&a3), &s(&dir.join("src")), "--jobs", "1", "--effort", "3", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(report["blocks_sampled"].as_u64().unwrap() > 0, "{report}");
    let (code, _, stderr) = run(&["pack", &s(&b3), &s(&dir.join("src")), "--jobs", "8", "--effort", "3"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(std::fs::read(&a3).unwrap(), std::fs::read(&b3).unwrap(), "--effort 3 must be independent of --jobs");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn effort_out_of_range_is_a_usage_error() {
    let dir = fixture("effort");
    for bad in ["0", "6", "x"] {
        let (code, _, stderr) = run(&["pack", &s(&dir.join("bad.tsr")), &s(&dir.join("src")), "--effort", bad]);
        assert_ne!(code, 0, "--effort {bad} must be refused");
        assert!(stderr.contains("--effort"), "the message names the option: {stderr}");
        assert!(bad == "x" || stderr.contains("1..=5"), "the message names the range: {stderr}");
        assert!(!dir.join("bad.tsr").exists(), "nothing is written on a usage error");
    }
    // the default effort reports no sampled block and prints no effort line
    let (code, stdout, stderr) = run(&["pack", &s(&dir.join("d.tsr")), &s(&dir.join("src"))]);
    assert_eq!(code, 0, "{stderr}");
    assert!(!stdout.contains("effort"), "{stdout}");
    let (code, stdout, _) = run(&["pack", &s(&dir.join("e.tsr")), &s(&dir.join("src")), "--effort", "3"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("effort 3: codec decided on a sample for"), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn volume_sets_split_inspect_join_repair() {
    let dir = fixture("volumes");
    let archive = dir.join("v.tsr");
    let (code, _, stderr) = run(&["pack", &s(&archive), &s(&dir.join("src"))]);
    assert_eq!(code, 0, "{stderr}");
    let original = std::fs::read(&archive).unwrap();
    let drives: Vec<PathBuf> = (0..5).map(|i| dir.join(format!("drive{i}"))).collect();
    for d in &drives {
        std::fs::create_dir_all(d).unwrap();
    }
    let mut args = vec!["volumes".to_string(), "split".to_string(), s(&archive), "--data".into(), "3".into(), "--parity".into(), "2".into(), "--piece-size-kib".into(), "64".into(), "--json".into()];
    for d in &drives {
        args.push("--out".into());
        args.push(s(d));
    }
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, stdout, stderr) = run(&argv);
    assert_eq!(code, 0, "{stderr}");
    let rep: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rep["volumes"].as_array().unwrap().len(), 5);
    assert_eq!(rep["placement_warning"], false);
    // inspect with verification: complete and reconstructible
    let drive_args: Vec<String> = drives.iter().map(|d| s(d)).collect();
    let mut inspect_args = vec!["volumes", "inspect"];
    inspect_args.extend(drive_args.iter().map(String::as_str));
    inspect_args.extend(["--verify", "--json"]);
    let (code, stdout, stderr) = run(&inspect_args);
    assert_eq!(code, 0, "{stderr}");
    let sets: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(sets[0]["reconstructible"], true);
    assert_eq!(sets[0]["missing"].as_array().unwrap().len(), 0);
    // lose two drives, join from the remaining three, verify the joined archive with the normal reader
    std::fs::remove_dir_all(&drives[1]).unwrap();
    std::fs::remove_dir_all(&drives[3]).unwrap();
    let joined = dir.join("joined.tsr");
    let (code, stdout, stderr) = run(&["volumes", "join", &s(&joined), &s(&drives[0]), &s(&drives[2]), &s(&drives[4]), "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let jr: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(jr["archive_hash_ok"], true);
    assert_eq!(jr["volumes_missing"], serde_json::json!([1, 3]));
    assert_eq!(std::fs::read(&joined).unwrap(), original);
    let (code, _, _) = run(&["verify", &s(&joined)]);
    assert_eq!(code, 0);
    // repair recreates the two lost volumes into a new location
    let fresh = dir.join("drive-new");
    std::fs::create_dir_all(&fresh).unwrap();
    let (code, stdout, stderr) = run(&["volumes", "repair", &s(&drives[0]), &s(&drives[2]), &s(&drives[4]), "--out", &s(&fresh), "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let rr: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rr["rebuilt"].as_array().unwrap().len(), 2);
    let (code, _, _) = run(&["volumes", "inspect", &s(&drives[0]), &s(&drives[2]), &s(&drives[4]), &s(&fresh), "--verify"]);
    assert_eq!(code, 0);
    // three volumes gone: exit code 5 (missing data), nothing written
    std::fs::remove_dir_all(&fresh).unwrap();
    std::fs::remove_dir_all(&drives[2]).unwrap();
    let out2 = dir.join("nope.tsr");
    let (code, _, stderr) = run(&["volumes", "join", &s(&out2), &s(&drives[0]), &s(&drives[4])]);
    assert_eq!(code, 5, "{stderr}");
    assert!(!out2.exists());
    let (code, _, _) = run(&["volumes", "inspect", &s(&drives[0]), &s(&drives[4])]);
    assert_eq!(code, 5);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn volumes_serve_and_fetch_between_two_instances() {
    use std::io::BufRead;
    use std::process::Stdio;
    let dir = fixture("xfer");
    let archive = dir.join("x.tsr");
    let (code, _, stderr) = run(&["pack", &s(&archive), &s(&dir.join("src"))]);
    assert_eq!(code, 0, "{stderr}");
    let original = std::fs::read(&archive).unwrap();
    let vol_dir = dir.join("volumes");
    std::fs::create_dir_all(&vol_dir).unwrap();
    let (code, stdout, stderr) = run(&["volumes", "split", &s(&archive), "--data", "2", "--parity", "1", "--piece-size-kib", "64", "--out", &s(&vol_dir), "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let rep: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let set_id = rep["set_id"].as_str().unwrap().to_string();
    let descriptor = rep["descriptor_b3"].as_str().unwrap().to_string();
    // one instance serves on a loopback port chosen by the OS (its stdout pipe stays open while it runs)
    let mut server = bin().args(["volumes", "serve", &s(&vol_dir), "--listen", "127.0.0.1:0"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut server_out = std::io::BufReader::new(server.stdout.take().unwrap());
    let mut first = String::new();
    server_out.read_line(&mut first).unwrap();
    let addr = first.trim().strip_prefix("listening on ").unwrap().split(' ').next().unwrap().to_string();
    // the other instance stops after one piece (an interruption that leaves a partial volume),
    // then fetches the rest and joins
    let here = dir.join("here");
    let joined = dir.join("joined.tsr");
    let (code, stdout, stderr) = run(&["volumes", "fetch", "--set", &set_id, "--descriptor", &descriptor, "--from", &addr, "--out", &s(&here), "--stop-after", "1", "--json"]);
    assert_eq!(code, 5, "{stderr}");
    let partial: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(partial["stopped_early"], true);
    assert_eq!(partial["pieces_received"], 1);
    assert_eq!(partial["requests"].as_u64().unwrap(), 3, "descriptor, have list and one piece");
    let (code, stdout, stderr) = run(&["volumes", "fetch", "--set", &set_id, "--descriptor", &descriptor, "--from", &addr, "--out", &s(&here), "--join", &s(&joined), "--json"]);
    let _ = server.kill();
    let _ = server.wait();
    assert_eq!(code, 0, "{stderr}");
    let fr: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(fr["verified_against"], "descriptor");
    assert_eq!(fr["reconstructible"], true);
    assert_eq!(fr["pieces_reverified"], 1, "the piece already on disk is hash-checked, not trusted");
    assert!(fr["seconds_transfer"].as_f64().unwrap() > 0.0 && fr["seconds_join"].as_f64().unwrap() >= 0.0);
    assert_eq!(fr["joined"]["archive_hash_ok"], true);
    assert_eq!(std::fs::read(&joined).unwrap(), original);
    let (code, _, _) = run(&["verify", &s(&joined)]);
    assert_eq!(code, 0);
    // a wrong expectation is refused
    let (code, _, _) = run(&["volumes", "fetch", "--set", &"0".repeat(64), "--from", &addr, "--out", &s(&dir.join("nothing"))]);
    assert_ne!(code, 0);
    drop(server_out);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn serve_listens_on_loopback_only_unless_exposed() {
    use std::io::BufRead;
    use std::process::Stdio;
    let dir = fixture("expose");
    let archive = dir.join("e.tsr");
    let (code, _, stderr) = run(&["pack", &s(&archive), &s(&dir.join("src"))]);
    assert_eq!(code, 0, "{stderr}");
    let vol_dir = dir.join("volumes");
    std::fs::create_dir_all(&vol_dir).unwrap();
    let (code, _, stderr) = run(&["volumes", "split", &s(&archive), "--data", "2", "--parity", "1", "--out", &s(&vol_dir)]);
    assert_eq!(code, 0, "{stderr}");
    // a non-loopback address without the explicit opt-in is refused by policy (exit 3)
    let (code, _, stderr) = run(&["volumes", "serve", &s(&vol_dir), "--listen", "0.0.0.0:0"]);
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("--expose-lan"));
    // the opt-in alone is not enough: who may fetch has to be said by name
    let (code, _, stderr) = run(&["volumes", "serve", &s(&vol_dir), "--listen", "0.0.0.0:0", "--expose-lan"]);
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("--allow-anyone") && stderr.contains("--allow <fingerprint>"), "{stderr}");
    // --allow without an identity, and both authorizations at once, are refused
    let (code, _, _) = run(&["volumes", "serve", &s(&vol_dir), "--listen", "0.0.0.0:0", "--expose-lan", "--allow", &"ab".repeat(32)]);
    assert_ne!(code, 0);
    let (code, _, _) = run(&["volumes", "serve", &s(&vol_dir), "--listen", "127.0.0.1:0", "--allow-anyone", "--allow", &"ab".repeat(32)]);
    assert_ne!(code, 0);
    // explicit anonymous exposure starts, says so on stdout and warns on stderr; loopback needs nothing
    for (args, expect) in [
        (vec!["volumes", "serve", &s(&vol_dir), "--listen", "0.0.0.0:0", "--expose-lan", "--allow-anyone"], "UNENCRYPTED"),
        (vec!["volumes", "serve", &s(&vol_dir), "--listen", "127.0.0.1:0"], "loopback only"),
    ] {
        let mut child = bin().args(&args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut lines = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut first = String::new();
        lines.read_line(&mut first).unwrap();
        assert!(first.starts_with("listening on "), "{first:?}");
        let mut second = String::new();
        lines.read_line(&mut second).unwrap();
        assert!(second.contains("access:") && second.contains(expect), "{second:?}");
        if expect == "UNENCRYPTED" {
            assert!(second.contains("ANYONE"), "{second:?}");
        }
        let _ = child.kill();
        let _ = child.wait();
        drop(lines);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keygen_serve_and_fetch_over_pinned_tls() {
    use std::io::BufRead;
    use std::process::Stdio;
    let dir = fixture("tls");
    let archive = dir.join("t.tsr");
    let (code, _, stderr) = run(&["pack", &s(&archive), &s(&dir.join("src"))]);
    assert_eq!(code, 0, "{stderr}");
    let original = std::fs::read(&archive).unwrap();
    let vol_dir = dir.join("volumes");
    std::fs::create_dir_all(&vol_dir).unwrap();
    let (code, stdout, stderr) = run(&["volumes", "split", &s(&archive), "--data", "2", "--parity", "1", "--piece-size-kib", "64", "--out", &s(&vol_dir), "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let rep: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let set_id = rep["set_id"].as_str().unwrap().to_string();
    let descriptor = rep["descriptor_b3"].as_str().unwrap().to_string();
    // one identity per side; keygen prints the fingerprint the other side pins and never overwrites
    let server_key = dir.join("server.key");
    let client_key = dir.join("client.key");
    let (code, stdout, stderr) = run(&["volumes", "keygen", "--out", &s(&server_key), "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let server_fp = serde_json::from_str::<serde_json::Value>(&stdout).unwrap()["fingerprint"].as_str().unwrap().to_string();
    assert_eq!(server_fp.len(), 64);
    assert!(server_key.is_file() && dir.join("server.key.crt").is_file());
    let (code, stdout, stderr) = run(&["volumes", "keygen", "--out", &s(&client_key)]);
    assert_eq!(code, 0, "{stderr}");
    let client_fp = stdout.lines().find_map(|l| l.strip_prefix("fingerprint  ")).unwrap().trim().to_string();
    let (code, stdout, _) = run(&["volumes", "fingerprint", &s(&client_key)]);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), client_fp);
    let (code, _, stderr) = run(&["volumes", "keygen", "--out", &s(&server_key)]);
    assert_eq!(code, 3, "{stderr}");
    // an allow list or an explicit anonymous mode is required with an identity
    let (code, _, stderr) = run(&["volumes", "serve", &s(&vol_dir), "--listen", "127.0.0.1:0", "--tls-identity", &s(&server_key)]);
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("--allow"));
    let (code, _, stderr) = run(&["volumes", "serve", &s(&vol_dir), "--listen", "127.0.0.1:0", "--allow", &client_fp]);
    assert_ne!(code, 0, "{stderr}");
    // serve over TLS, admitting only the client identity
    let mut server = bin()
        .args(["volumes", "serve", &s(&vol_dir), "--listen", "127.0.0.1:0", "--tls-identity", &s(&server_key), "--allow", &client_fp])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = std::io::BufReader::new(server.stdout.take().unwrap());
    let mut first = String::new();
    lines.read_line(&mut first).unwrap();
    assert!(first.contains("[tls]"), "{first:?}");
    let addr = first.trim().strip_prefix("listening on ").unwrap().split(' ').next().unwrap().to_string();
    let mut second = String::new();
    lines.read_line(&mut second).unwrap();
    assert!(second.contains(&server_fp), "{second:?}");
    // the pinned, allowed client fetches and joins
    let here = dir.join("here");
    let joined = dir.join("joined.tsr");
    let (code, stdout, stderr) = run(&[
        "volumes",
        "fetch",
        "--set",
        &set_id,
        "--descriptor",
        &descriptor,
        "--from",
        &addr,
        "--peer-id",
        &server_fp,
        "--tls-identity",
        &s(&client_key),
        "--out",
        &s(&here),
        "--join",
        &s(&joined),
        "--json",
    ]);
    assert_eq!(code, 0, "{stderr}");
    let fr: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(fr["encrypted"], true);
    assert_eq!(fr["reconstructible"], true);
    assert_eq!(fr["joined"]["archive_hash_ok"], true);
    assert_eq!(std::fs::read(&joined).unwrap(), original);
    // a wrong server fingerprint, a plain fetch, a missing client identity and a mismatched count are all refused
    let (code, _, stderr) = run(&["volumes", "fetch", "--set", &set_id, "--from", &addr, "--peer-id", &client_fp, "--tls-identity", &s(&client_key), "--out", &s(&dir.join("wrong"))]);
    assert_ne!(code, 0);
    assert!(stderr.contains("does not match the pinned identity"), "{stderr}");
    let (code, _, _) = run(&["volumes", "fetch", "--set", &set_id, "--from", &addr, "--out", &s(&dir.join("plain"))]);
    assert_ne!(code, 0);
    let (code, _, _) = run(&["volumes", "fetch", "--set", &set_id, "--from", &addr, "--peer-id", &server_fp, "--out", &s(&dir.join("anon"))]);
    assert_ne!(code, 0);
    let (code, _, stderr) = run(&["volumes", "fetch", "--set", &set_id, "--from", &addr, "--from", &addr, "--peer-id", &server_fp, "--out", &s(&dir.join("count"))]);
    assert_ne!(code, 0);
    assert!(stderr.contains("one fingerprint per peer"), "{stderr}");
    let _ = server.kill();
    let _ = server.wait();
    for d in ["wrong", "plain", "anon", "count"] {
        let p = dir.join(d);
        assert!(!p.is_dir() || std::fs::read_dir(&p).unwrap().next().is_none(), "{d}: nothing may be written before the peer is verified");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
