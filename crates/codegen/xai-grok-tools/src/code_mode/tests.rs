use super::*;

#[derive(Clone)]
struct Capture(Rc<RefCell<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn run(code: &str, replies: Vec<Reply>) -> Vec<WorkerMessage> {
    let mut input = Vec::new();
    for reply in replies {
        write_frame(&mut input, &reply).unwrap();
    }
    let bytes = Rc::new(RefCell::new(Vec::new()));
    execute(
        Init {
            code: code.to_owned(),
            catalog: vec![serde_json::json!({"name":"test","parameters":{"type":"object"}})],
        },
        std::io::Cursor::new(input),
        Capture(bytes.clone()),
    )
    .unwrap();
    let bytes = bytes.borrow();
    let mut cursor = std::io::Cursor::new(bytes.as_slice());
    let mut messages = Vec::new();
    while cursor.position() < bytes.len() as u64 {
        messages.push(read_frame(&mut cursor).unwrap());
    }
    messages
}
fn reply(id: u64, ok: bool, value: &str) -> Reply {
    Reply {
        id,
        ok,
        payload: value.to_owned(),
    }
}
fn finished(
    messages: &[WorkerMessage],
) -> (&bool, &serde_json::Value, &Option<String>, &Vec<String>) {
    let Some(WorkerMessage::Finished {
        ok,
        value,
        error,
        warnings,
        ..
    }) = messages.last()
    else {
        panic!("missing result")
    };
    (ok, value, error, warnings)
}
#[test]
fn parallel_out_of_order_and_dependent_calls() {
    let messages = run(
        "const r = await Promise.all([tools.test({n:1}), tools.test({n:2})]); const n = await tools.test({sum:r[0]+r[1]}); return n;",
        vec![
            reply(2, true, "20"),
            reply(1, true, "10"),
            reply(3, true, "30"),
        ],
    );
    assert_eq!(messages.len(), 4);
    let WorkerMessage::Call { arguments, .. } = messages.get(2).unwrap() else {
        panic!("missing dependent call")
    };
    assert_eq!(arguments["sum"], 30);
    assert_eq!(finished(&messages).1, &serde_json::json!(30));
}
#[test]
fn await_twice_dispatches_once_and_failures_can_be_caught() {
    let messages = run(
        "const p = tools.test({}); const a = await p; const b = await p; try { await tools.test({}); } catch(e) { return [a,b,e.message]; }",
        vec![reply(1, true, "7"), reply(2, false, "denied")],
    );
    assert_eq!(messages.len(), 3);
    assert_eq!(finished(&messages).1, &serde_json::json!([7, 7, "denied"]));
}
#[test]
fn partial_failures_preserve_siblings() {
    let messages = run(
        "return await Promise.allSettled([tools.test({}),tools.test({})]);",
        vec![reply(1, false, "failed"), reply(2, true, "42")],
    );
    let result = finished(&messages);
    assert!(*result.0);
    assert_eq!(result.1[0]["status"], "rejected");
    assert_eq!(result.1[1]["value"], 42);
}
#[test]
fn unawaited_work_finishes_without_waiting_and_is_reported() {
    let messages = run("tools.test({}); return 7;", vec![]);
    let result = finished(&messages);
    assert!(*result.0);
    assert_eq!(result.1, &serde_json::json!(7));
    assert!(result.3.iter().any(|s| s.contains("unawaited")));
}
#[test]
fn stalled_promises_fail_without_host_work() {
    let messages = run("await new Promise(()=>{});", vec![]);
    let result = finished(&messages);
    assert!(!result.0);
    assert!(result.2.as_ref().unwrap().contains("no host work"));
}
#[test]
fn host_capabilities_absent_and_no_state_survives_runs() {
    let first = run(
        "globalThis.secret=1; return [typeof process,typeof fetch,typeof require,typeof std,typeof setTimeout];",
        vec![],
    );
    assert_eq!(
        finished(&first).1,
        &serde_json::json!([
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined"
        ])
    );
    let second = run("return typeof secret;", vec![]);
    assert_eq!(finished(&second).1, &serde_json::json!("undefined"));
}
#[test]
fn data_boundary_rejects_unsupported_or_lossy_values() {
    for code in [
        "return 1n;",
        "return {missing:undefined};",
        "return NaN;",
        "return 9007199254740992;",
        "return {p:Promise.resolve(1)};",
        "const a={};a.a=a;return a;",
    ] {
        let messages = run(code, vec![]);
        assert!(!finished(&messages).0, "{code}");
    }
    let messages = run("", vec![]);
    assert_eq!(finished(&messages).1, &serde_json::Value::Null);
}
#[test]
fn call_limit_is_checked_before_emission() {
    let replies = (1..=64).map(|id| reply(id, true, "null")).collect();
    let messages = run("for(let i=0;i<65;i++) await tools.test({});", replies);
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, WorkerMessage::Call { .. }))
            .count(),
        64
    );
    assert!(!finished(&messages).0);
}
#[test]
fn concurrency_limit_and_empty_log_count_are_bounded() {
    let messages = run(
        "await Promise.all(Array.from({length:9},()=>tools.test({})));",
        vec![],
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, WorkerMessage::Call { .. }))
            .count(),
        8
    );
    assert!(!finished(&messages).0);
    let messages = run("for(let i=0;i<257;i++) console.log();", vec![]);
    assert!(!finished(&messages).0);
}
#[test]
fn output_heap_and_recursion_limits_fail_cleanly() {
    for code in [
        "return 'x'.repeat(65537);",
        "return new Array(100000000).fill('x');",
        "function f(){return f();}return f();",
    ] {
        assert!(!finished(&run(code, vec![])).0, "{code}");
    }
}
#[test]
fn unhandled_rejections_are_visible() {
    let messages = run("Promise.reject(new Error('oops')); return 1;", vec![]);
    assert!(
        finished(&messages)
            .3
            .iter()
            .any(|s| s.contains("unhandled"))
    );
}
#[test]
fn overlarge_frames_rejected_before_allocating_body() {
    let bytes = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes();
    assert!(read_frame::<Init>(bytes.as_slice()).is_err());
}
#[test]
fn infinite_computation_is_interrupted() {
    let before = Instant::now();
    assert!(!finished(&run("for(;;){}", vec![])).0);
    assert!(before.elapsed() < Duration::from_secs(10));
}

#[test]
fn returned_program_does_not_admit_late_microtask_tool_calls() {
    let messages = run(
        "Promise.resolve().then(()=>tools.test({})); return 7;",
        vec![],
    );
    assert_eq!(messages.len(), 1, "no host call after root return");
    assert_eq!(finished(&messages).1, &serde_json::json!(7));
    assert!(
        finished(&messages)
            .3
            .iter()
            .any(|s| s.contains("jobs discarded"))
    );
}

#[test]
fn final_serialization_cannot_admit_tool_calls_from_getters() {
    let messages = run("return {get value(){tools.test({}); return 7;}};", vec![]);
    assert_eq!(
        messages.len(),
        1,
        "serialization must not start external work"
    );
    assert_eq!(finished(&messages).1, &serde_json::json!({"value":7}));
}

#[test]
fn oversized_thrown_values_are_truncated_into_the_final_frame() {
    let messages = run("throw 'x'.repeat(3 * 1024 * 1024);", vec![]);
    let (ok, _, error, _) = finished(&messages);
    assert!(!ok);
    let error = error.as_ref().unwrap();
    assert!(error.len() < MAX_OUTPUT_BYTES + 64, "{}", error.len());
    assert!(error.ends_with("[error truncated]"));
}

#[test]
fn patched_builtins_cannot_redirect_host_replies() {
    let messages = run(
        "Map.prototype.get=()=>undefined; Map.prototype.set=()=>{}; Object.prototype.resolve=()=>{}; return await tools.test({});",
        vec![reply(1, true, "5")],
    );
    assert_eq!(finished(&messages).1, &serde_json::json!(5));
}
#[test]
fn catalog_accepts_object_form_and_rejects_non_strings() {
    let messages = run(
        r#"
        const err = f => { try { f(); return null; } catch (e) { return [e.name, e.message]; } };
        return {
          object: catalog.search({query: "te"}).total,
          terms: catalog.search("te  st").total,
          miss: catalog.search("te zz").total,
          number: err(() => catalog.search(42)),
          describe: catalog.describe({name: "test"}).name,
          describeBad: err(() => catalog.describe(7)),
          unknown: err(() => tools.nope({})),
          then: typeof tools.then,
        };
        "#,
        vec![],
    );
    let (ok, value, error, _) = finished(&messages);
    assert!(*ok, "{error:?}");
    assert_eq!(value["object"], 1);
    assert_eq!(value["terms"], 1);
    assert_eq!(value["miss"], 0);
    assert_eq!(value["number"][0], "TypeError");
    assert_eq!(value["describe"], "test");
    assert_eq!(value["describeBad"][0], "TypeError");
    assert!(
        value["unknown"][1]
            .as_str()
            .unwrap()
            .contains("catalog.search()"),
        "{value}"
    );
    assert_eq!(value["then"], "undefined");
}
