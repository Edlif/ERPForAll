// SPDX-License-Identifier: MIT OR Apache-2.0
mod pictures;
use super::*;
use erp_report::{DocumentBuilder, DocumentOptions, Node, PageSetup, Style};
use typst::{
    World,
    diag::FileError,
    foundations::{Array, Dict, Value},
    syntax::{FileId, RootedPath, VirtualPath, VirtualRoot},
};
fn fonts() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../addons/report_fonts")
}
fn engine() -> TypstEngine {
    TypstEngine::new(fonts(), Limits::default()).unwrap()
}
/// Runs one test of this binary in a fresh process with `switch` set, so process-global state is its own.
fn run_child(test: &str, switch: &str) -> std::process::Output {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(switch, "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(&format!("test {test} ... ok")),
        "{output:?}"
    );
    output
}
const DIAGNOSTIC_MESSAGE: &str =
    "report layout Diagnostic for safe-id: renderer diagnostics are in the error's reason field";
/// Renderer sources that ask for a resource outside the sandbox, with the compiler's refusal.
const FORBIDDEN: [(&str, &str); 4] = [
    (
        "#read(\"/etc/passwd\")",
        "failed to load file (access denied)",
    ),
    (
        "#read(\"../escape\")",
        "path `\"../escape\"` would escape the project root",
    ),
    (
        "#import \"@preview/example:0.1.0\"",
        "failed to load file (access denied)",
    ),
    (
        "#plugin(\"/plugin.wasm\")",
        "failed to load file (access denied)",
    ),
];
fn date() -> Datetime {
    Datetime::from_ymd_hms(2020, 2, 29, 0, 0, 0).unwrap()
}
fn doc() -> DocTree {
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b.add(Node::Text("recovered".into())).unwrap();
    b.finish(
        body,
        DocumentOptions::new(
            "safe-id",
            "Fixture",
            PageSetup::new(400.0, 500.0, 35.0).unwrap(),
        ),
    )
    .unwrap()
}
#[test]
fn sandbox_denies_external_resources() {
    for (text, expected) in FORBIDDEN {
        let world = ReportWorld::new(
            ReportWorld::source(text).unwrap(),
            Dict::new(),
            vec![],
            date(),
        );
        let errors = typst::compile::<PagedDocument>(&world).output.unwrap_err();
        assert!(!errors.is_empty());
        for error in errors {
            assert_eq!(error.message.as_str(), expected);
        }
    }
}
#[test]
fn engine_reports_sandbox_denial_as_a_diagnostic() {
    for (text, expected) in FORBIDDEN {
        let mut engine = engine();
        engine.source = ReportWorld::source(text).unwrap();
        let error = engine.render(&doc()).unwrap_err();
        let ReportError::Layout {
            document,
            kind: LayoutFailure::Diagnostic,
            reason,
        } = &error
        else {
            panic!("{error}")
        };
        assert_eq!(document, "safe-id");
        assert_eq!(reason, expected);
        assert_eq!(error.to_string(), DIAGNOSTIC_MESSAGE);
    }
}
/// engine renders the next document)
#[test]
fn diagnostic_message_withholds_document_text() {
    let mut engine = engine();
    let fixed = engine.source.clone();
    engine.source = ReportWorld::source("#panic(sys.inputs.title)").unwrap();
    let error = engine.render(&doc()).unwrap_err();
    let ReportError::Layout {
        kind: LayoutFailure::Diagnostic,
        reason,
        ..
    } = &error
    else {
        panic!("{error}")
    };
    // The renderer quotes the document title: this is the text the message must not carry.
    assert_eq!(reason, "panicked with: Fixture");
    assert_eq!(error.to_string(), DIAGNOSTIC_MESSAGE);
    engine.source = fixed;
    let output = engine.render(&doc()).unwrap();
    assert_eq!(output.page_count(), 1);
    assert!(output.warnings().is_empty(), "{:?}", output.warnings());
}
#[test]
fn world_has_one_source_and_no_invalid_font() {
    let world = ReportWorld::new(
        ReportWorld::source("hello").unwrap(),
        Dict::new(),
        vec![],
        date(),
    );
    assert_eq!(world.source(world.main()).unwrap().text(), "hello");
    assert_eq!(world.file(world.main()).unwrap().as_slice(), b"hello");
    let other = FileId::new(RootedPath::new(
        VirtualRoot::Project,
        VirtualPath::new("/other").unwrap(),
    ));
    assert!(matches!(world.source(other), Err(FileError::AccessDenied)));
    assert_eq!(
        world.source(other).unwrap_err().to_string(),
        "failed to load file (access denied)"
    );
    assert!(matches!(world.file(other), Err(FileError::AccessDenied)));
    assert_eq!(
        world.file(other).unwrap_err().to_string(),
        "failed to load file (access denied)"
    );
    assert!(world.font(usize::MAX).is_none());
    assert_eq!(world.today(None), Some(date()));
}
/// Panics once when a test armed the engine; called at the start of every compile in test builds.
pub(crate) fn injected_panic(armed: &std::sync::atomic::AtomicBool) {
    if armed.swap(false, std::sync::atomic::Ordering::Relaxed) {
        panic!("SECRET-PAYLOAD");
    }
}
/// the payload, and the same engine renders the next document)
#[test]
fn panic_is_typed_and_next_render_succeeds() {
    let engine = engine();
    engine
        .panic_next
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let error = engine.render(&doc()).unwrap_err();
    let ReportError::Layout {
        document,
        kind: LayoutFailure::Panic,
        reason,
    } = &error
    else {
        panic!("{error}")
    };
    assert_eq!(document, "safe-id");
    // The reason names where the renderer panicked (here the injection point), never the payload.
    let Some(site) = reason.strip_prefix("renderer panicked at ") else {
        panic!("{reason}")
    };
    assert!(site.contains("mod.rs:"), "{reason}");
    assert!(!reason.contains("SECRET-PAYLOAD"), "{reason}");
    assert_eq!(
        error.to_string(),
        "report layout Panic for safe-id: renderer panicked"
    );
    assert_eq!(engine.render(&doc()).unwrap().page_count(), 1);
}
/// past the public ceiling of 32 and that the first refusal is the engine's depth diagnostic; the
/// 48 levels the design asked for are not reached, see the gate record)
#[test]
fn renderer_headroom_is_measured_and_ends_in_a_diagnostic() {
    let mut b = DocumentBuilder::new(Limits::default());
    let leaf = b.add(Node::Text("headroom".into())).unwrap();
    let root = b
        .add(Node::Block {
            children: vec![leaf],
            style: Style::default().with_padding(1.0).unwrap(),
        })
        .unwrap();
    let options = DocumentOptions::new(
        "depth-probe",
        "Probe",
        PageSetup::new(4000.0, 4000.0, 35.0).unwrap(),
    );
    let doc = b.finish(root, options).unwrap();
    let engine = engine();
    let fonts = engine.fonts.select(doc.options().fonts()).unwrap();
    // Admission stops at 32 levels, so deeper inputs are written as encoded nodes: a leaf and
    // `levels - 1` padded blocks, each holding the previous node.
    let lays_out = |levels: i64| -> Result<usize, String> {
        let (mut inputs, _) = encode::encode(&doc, &Limits::default()).unwrap();
        let Value::Array(mut nodes) = inputs.get("nodes").unwrap().clone() else {
            panic!("nodes")
        };
        let Value::Dict(parent) = nodes.at(1, None).unwrap().clone() else {
            panic!("parent")
        };
        for index in 2..levels {
            let mut node = parent.clone();
            node.insert(
                "children".into(),
                Value::Array([Value::Int(index - 1)].into_iter().collect::<Array>()),
            );
            nodes.push(Value::Dict(node));
        }
        inputs.insert("nodes".into(), Value::Array(nodes));
        inputs.insert("body".into(), Value::Int(levels - 1));
        let world = ReportWorld::new(engine.source.clone(), inputs, fonts.clone(), date());
        match typst::compile::<PagedDocument>(&world).output {
            Ok(output) => Ok(output.pages().len()),
            Err(errors) => Err(errors
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")),
        }
    };
    assert_eq!(lays_out(32), Ok(1));
    let mut ceiling = 32;
    while ceiling < 128 && lays_out(ceiling + 1).is_ok() {
        ceiling += 1;
    }
    println!("measured ceiling: {ceiling} levels of padded blocks");
    assert!(ceiling > 32, "no headroom past the public ceiling");
    assert_eq!(
        lays_out(ceiling + 1),
        Err("maximum layout depth exceeded".into())
    );
    assert_eq!(lays_out(512), Err("maximum layout depth exceeded".into()));
}

#[test]
fn panic_hook_does_not_leak() {
    let output = run_child("tests::panic_hook_child", "ERP_REPORT_PANIC_CHILD");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("SECRET-PAYLOAD"), "{output:?}");
    // A panic outside the report pool still reaches the hook the host installed first.
    assert!(stderr.contains("HOST-HOOK"), "{output:?}");
    assert!(stderr.contains("OUTSIDE-REPORT"), "{output:?}");
}
#[test]
fn panic_hook_child() {
    if std::env::var_os("ERP_REPORT_PANIC_CHILD").is_some() {
        std::panic::set_hook(Box::new(|info| eprintln!("HOST-HOOK {info}")));
        let e = engine();
        e.panic_next
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let error = e.render(&doc()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "report layout Panic for safe-id: renderer panicked"
        );
        let error = e
            .pool
            .install(|| {
                contain::<()>("safe", || {
                    rayon::join(|| panic!("SECRET-PAYLOAD"), || ());
                    Ok(())
                })
            })
            .unwrap_err();
        assert!(matches!(
            error,
            ReportError::Layout {
                kind: LayoutFailure::Panic,
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            "report layout Panic for safe: renderer panicked"
        );
        assert!(
            std::thread::spawn(|| panic!("OUTSIDE-REPORT"))
                .join()
                .is_err()
        );
    }
}
#[test]
fn missing_font_is_a_font_error() {
    let path = std::env::temp_dir().join(format!("erp-report-missing-font-{}", std::process::id()));
    std::fs::create_dir(&path).unwrap();
    let engine = TypstEngine::new(&path, Limits::default()).unwrap();
    let error = engine.render(&doc()).unwrap_err();
    std::fs::remove_dir(path).unwrap();
    assert!(matches!(error, ReportError::Font { .. }), "{error}");
    assert_eq!(
        error.to_string(),
        "report font: cannot read NotoSans-Italic.ttf of font package report_fonts"
    );
}

#[test]
fn mixed_eviction_intervals_are_bounded() {
    let mut state = EvictionState::default();
    let mut since = 0;
    for completed in 1..=500 {
        since += 1;
        let interval = if completed % 64 == 0 { 63 } else { 64 };
        if state.complete(interval, false).is_some() {
            assert!(since <= 64);
            since = 0;
        }
        assert!(since < 64);
    }
    assert_eq!(state.complete(64, true), Some(0));
    for _ in 1..63 {
        assert_eq!(state.complete(64, false), None);
    }
    assert_eq!(state.complete(64, false), Some(2));
}

/// what the eviction frees is not measured)
#[test]
fn eviction_follows_cadence_and_failure_cleanup() {
    run_child("tests::eviction_child", "ERP_REPORT_EVICTION_CHILD");
}
#[test]
fn eviction_child() {
    if std::env::var_os("ERP_REPORT_EVICTION_CHILD").is_none() {
        return;
    }
    let evictions = || EVICTIONS.load(std::sync::atomic::Ordering::Relaxed);
    let limits = Limits::default()
        .lowered(LimitKind::EvictionInterval, 4)
        .unwrap();
    let first = TypstEngine::new(fonts(), limits).unwrap();
    let second = TypstEngine::new(fonts(), limits).unwrap();
    let doc = doc();
    first.render(&doc).unwrap();
    second.render(&doc).unwrap();
    // A document refused before layout is an ordinary completion: no cleanup of its own.
    let small = Limits::default().lowered(LimitKind::Nodes, 1).unwrap();
    let mut b = DocumentBuilder::new(Limits::default());
    let leaf = b.add(Node::Text("two nodes".into())).unwrap();
    let body = b
        .add(Node::Block {
            children: vec![leaf],
            style: Style::default(),
        })
        .unwrap();
    let options = DocumentOptions::new(
        "safe-id",
        "Fixture",
        PageSetup::new(400.0, 500.0, 35.0).unwrap(),
    );
    let refused = TypstEngine::new(
        fonts(),
        small.lowered(LimitKind::EvictionInterval, 4).unwrap(),
    )
    .unwrap()
    .render(&b.finish(body, options).unwrap())
    .unwrap_err();
    assert_eq!(refused.to_string(), "report limit nodes exceeded: 2 > 1");
    assert_eq!(evictions(), 0, "three completions stay below the cadence");
    first.render(&doc).unwrap();
    assert_eq!(
        evictions(),
        1,
        "the fourth completion of the process evicts"
    );
    let mut broken = TypstEngine::new(fonts(), limits).unwrap();
    broken.source = ReportWorld::source("#panic()").unwrap();
    broken.render(&doc).unwrap_err();
    assert_eq!(evictions(), 2, "a failed render cleans up at once");
    second.render(&doc).unwrap();
    assert_eq!(evictions(), 2, "the cadence restarts after a cleanup");
    second.finish_batch();
    assert_eq!(evictions(), 3, "the end of a batch evicts");
}

/// selected is not loaded; a symlink leaving the store is not exercised)
#[test]
fn font_store_refuses_bad_assets_and_loads_lazily() {
    let path = std::env::temp_dir().join(format!("erp-report-corrupt-font-{}", std::process::id()));
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("NotoSans-Italic.ttf"), b"not a font").unwrap();
    let error = TypstEngine::new(&path, Limits::default())
        .unwrap()
        .render(&doc())
        .unwrap_err();
    std::fs::remove_dir_all(path).unwrap();
    assert!(matches!(error, ReportError::Font { .. }), "{error}");
    assert_eq!(
        error.to_string(),
        "report font: NotoSans-Italic.ttf has a digest mismatch (font package report_fonts)"
    );
    let small = Limits::default().lowered(LimitKind::FontBytes, 1).unwrap();
    let error = TypstEngine::new(fonts(), small)
        .unwrap()
        .render(&doc())
        .unwrap_err();
    assert!(matches!(error, ReportError::Font { .. }), "{error}");
    assert_eq!(
        error.to_string(),
        "report font: shipped asset NotoSans-Italic.ttf exceeds font_bytes 1"
    );
    let engine = engine();
    assert!(engine.fonts.loaded_files().is_empty());
    engine.render(&doc()).unwrap();
    assert_eq!(
        engine.fonts.loaded_files(),
        ["NotoSans-Italic.ttf", "NotoSans.ttf"]
    );
}

/// One line of Korean text in Noto Sans KR.
fn korean(engine: &TypstEngine) -> Result<RenderResult, ReportError> {
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b.add(Node::Text("한국어 상품".into())).unwrap();
    let options = DocumentOptions::new(
        "safe-id",
        "Fixture",
        PageSetup::new(400.0, 500.0, 35.0).unwrap(),
    )
    .with_fonts(vec!["Noto Sans KR".into()])
    .unwrap();
    engine.render(&b.finish(body, options).unwrap())
}

/// when the package has no directory)
#[test]
fn optional_family_reads_no_file_without_its_package() {
    let engine = engine();
    let error = korean(&engine).unwrap_err();
    assert!(matches!(error, ReportError::Font { .. }), "{error}");
    assert_eq!(
        error.to_string(),
        "report font: family \"Noto Sans KR\" is in the optional font package report_fonts_cjk, which is not installed on this node"
    );
    assert!(engine.fonts.loaded_files().is_empty());
}

/// other name is refused before a file is read, naming WorkerPool and)
#[test]
fn family_outside_the_manifest_names_the_worker_pool() {
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b.add(Node::Text("uploaded".into())).unwrap();
    let options = DocumentOptions::new(
        "safe-id",
        "Fixture",
        PageSetup::new(400.0, 500.0, 35.0).unwrap(),
    )
    .with_fonts(vec!["Uploaded Sans".into()])
    .unwrap();
    let engine = engine();
    let error = engine
        .render(&b.finish(body, options).unwrap())
        .unwrap_err();
    assert!(matches!(error, ReportError::Font { .. }), "{error}");
    assert_eq!(
        error.to_string(),
        "report font: family \"Uploaded Sans\" is not in the shipped font manifest; uploaded fonts require WorkerPool (a later release)"
    );
    assert!(engine.fonts.loaded_files().is_empty());
}
