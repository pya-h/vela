#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, Severity, TokenClass};

fn labels(marked: &str) -> Vec<String> {
    let at = marked.find("@@").expect("a @@ marker");
    let source = marked.replacen("@@", "", 1);
    Analysis::new(&source)
        .complete(LineIndex::new(&source).utf16_of(at))
        .items
        .into_iter()
        .map(|item| item.label)
        .collect()
}

fn hover(source: &str, needle: &str) -> (String, String) {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let hover = Analysis::new(source)
        .hover(LineIndex::new(source).utf16_of(byte))
        .unwrap_or_else(|| panic!("no hover at {:?}", needle));
    (hover.code, hover.doc)
}

#[test]
fn item_level_offers_actor_and_event() {
    let offered = labels("@@");
    assert!(offered.contains(&"actor".to_string()), "expected 'actor' in top-level items");
    assert!(offered.contains(&"event".to_string()), "expected 'event' in top-level items");
}

#[test]
fn channel_dot_completions() {
    let source = "\
task main()
    fixed c = chan_new<int>(4)
    c.@@
fi
";
    let offered = labels(source);
    assert!(offered.contains(&"dispatch".to_string()), "expected 'dispatch' on channel");
    assert!(offered.contains(&"wait_and_receive".to_string()), "expected 'wait_and_receive' on channel");
    assert!(offered.contains(&"receive".to_string()), "expected 'receive' on channel");
    assert!(offered.contains(&"close".to_string()), "expected 'close' on channel");
    assert!(offered.contains(&"len".to_string()), "expected 'len' on channel");
    assert!(offered.contains(&"cap".to_string()), "expected 'cap' on channel");
}

#[test]
fn actor_dot_completions() {
    let source = "\
actor Worker(id: int)
    on Run()
    fi
fi

task main()
    fixed w = spawn Worker(1)
    w.@@
fi
";
    let offered = labels(source);
    assert!(offered.contains(&"dispatch".to_string()), "expected 'dispatch' on actor handle");
    assert!(offered.contains(&"close".to_string()), "expected 'close' on actor handle");
}

#[test]
fn hover_on_concurrency_keywords() {
    let source = "\
actor Worker(id: int)
    on Ping()
    fi
fi

event notify on start
fi

task main()
    fixed w = spawn Worker(1)
    fixed c = chan_new<own int>(2)
fi
";
    let (code_actor, doc_actor) = hover(source, "actor Worker");
    assert!(code_actor.contains("actor") && doc_actor.contains("actor"));

    let (code_spawn, doc_spawn) = hover(source, "spawn Worker");
    assert!(code_spawn.contains("spawn") && doc_spawn.contains("actor"));

    let (code_event, doc_event) = hover(source, "event notify");
    assert!(code_event.contains("event") && doc_event.contains("hook"));

    let (code_own, doc_own) = hover(source, "own int");
    assert!(code_own.contains("own") && doc_own.contains("Ownership"));
}

#[test]
fn hover_on_channel_methods() {
    let source = "\
task main()
    fixed c = chan_new<int>(4)
    c.dispatch(1)
    c.wait_and_receive()
    c.receive()
    c.close()
fi
";
    let (code, doc) = hover(source, "dispatch(1)");
    assert!(code.contains("dispatch") && doc.contains("channel"));

    let (code, doc) = hover(source, "wait_and_receive()");
    assert!(code.contains("wait_and_receive") && doc.contains("channel"));

    let (code, doc) = hover(source, "receive()");
    assert!(code.contains("receive") && doc.contains("channel"));

    let (code, doc) = hover(source, "close()");
    assert!(code.contains("close") && doc.contains("channel"));
}

#[test]
fn diagnostics_for_actor_private_fields() {
    let source = "\
actor Counter(count: int)
    on Inc()
        count += 1
    fi
fi

task main()
    fixed c = spawn Counter(0)
    show(c.count)
fi
";
    let analysis = Analysis::new(source);
    let diags = analysis.diagnostics();
    assert!(
        diags.iter().any(|d| d.severity == Severity::Error && d.message.contains("private")),
        "expected private field diagnostic for actor field access, got {:?}",
        diags
    );
}

#[test]
fn diagnostics_for_events_missing_indirect_alters() {
    let source = "\
shape S
    a: int
    b: int

    event e2 on .b
        .b = 0
    fi

    event e1 on .a
        .b = 1
    fi
fi
";
    let analysis = Analysis::new(source);
    let diags = analysis.diagnostics();
    assert!(
        diags.iter().any(|d| d.severity == Severity::Error && d.message.contains("alters")),
        "expected alters diagnostic for missing indirect alters, got {:?}",
        diags
    );
}

fn token_class(source: &str, needle: &str) -> TokenClass {
    let byte = source.find(needle).unwrap_or_else(|| panic!("no {:?}", needle));
    let u16 = LineIndex::new(source).utf16_of(byte);
    Analysis::new(source)
        .tokens()
        .into_iter()
        .find(|t| t.from == u16)
        .unwrap_or_else(|| panic!("no token at {:?}", needle))
        .class
}

#[test]
fn select_tokens_and_hovers() {
    let source = "\
task select(x: int) -> int
    <- x + 1
fi

task main()
    fixed c1 = chan_new<int>(2)
    fixed c2 = chan_new<own int>(2)
    fixed item = 42
    fixed ok = select(1)
    select
    case msg <<- c1
        show(msg)
    case none <<- c1
        show(0)
    case own item ->> c2
        show(ok)
    case after 10
        show(1)
    otherwise
        show(2)
    fi
fi
";
    assert_eq!(token_class(source, "select(x"), TokenClass::Task);
    assert_eq!(token_class(source, "select(1)"), TokenClass::Task);
    assert_eq!(token_class(source, "select\n"), TokenClass::Keyword);
    assert_eq!(token_class(source, "msg <<-"), TokenClass::Variable);
    assert_eq!(token_class(source, "<<- c1"), TokenClass::Operator);
    assert_eq!(token_class(source, "->> c2"), TokenClass::Operator);
    assert_eq!(token_class(source, "after 10"), TokenClass::Keyword);
    assert_eq!(token_class(source, "otherwise"), TokenClass::Keyword);

    let (code_sel, _) = hover(source, "select\n");
    assert!(code_sel.contains("select"));
    let (code_ow, _) = hover(source, "otherwise");
    assert!(code_ow.contains("otherwise"));
    let (code_after, _) = hover(source, "after 10");
    assert!(code_after.contains("after"));
    let (code_recv, _) = hover(source, "<<- c1");
    assert!(code_recv.contains("<<-"));
    let (code_send, _) = hover(source, "->> c2");
    assert!(code_send.contains("->>"));
    let (code_msg, _) = hover(source, "msg)");
    assert_eq!(code_msg, "fixed msg: int");
}

#[test]
fn select_completions() {
    let start = "\
task main()
    select
    @@
    fi
fi
";
    let offered_start = labels(start);
    assert!(offered_start.contains(&"case".to_string()));
    assert!(offered_start.contains(&"otherwise".to_string()));
    assert!(!offered_start.contains(&"else".to_string()));

    let arm = "\
task main()
    fixed c1 = chan_new<int>(2)
    select
    case @@
    fi
fi
";
    let offered_arm = labels(arm);
    assert!(offered_arm.contains(&"after".to_string()));
    assert!(offered_arm.contains(&"none".to_string()));
    assert!(offered_arm.contains(&"own".to_string()));
    assert!(offered_arm.contains(&"c1".to_string()));

    let chan = "\
task main()
    fixed c1 = chan_new<int>(2)
    select
    case msg <<- @@
    fi
fi
";
    assert!(labels(chan).contains(&"c1".to_string()));

    let after_ow = "\
task main()
    select
    otherwise
        show(1)
    @@
    fi
fi
";
    let offered_ow = labels(after_ow);
    assert!(offered_ow.contains(&"fi".to_string()));
    assert!(!offered_ow.contains(&"case".to_string()));
    assert!(!offered_ow.contains(&"otherwise".to_string()));
}

#[test]
fn standalone_channel_arrow_types_and_hover() {
    let source = "\
shape Job
    id: int
fi

task main()
    fixed jobs = chan_new<own Job>(2)
    fixed ok = 42 ->> chan_new<int>(1)
    fixed job = <<- jobs
    show(ok, job.id)
fi
";
    let analysis = Analysis::new(source);
    assert!(
        analysis.diagnostics().iter().all(|d| d.severity != Severity::Error),
        "unexpected errors: {:?}",
        analysis.diagnostics()
    );
    let (code_ok, _) = hover(source, "ok,");
    assert_eq!(code_ok, "fixed ok: bool");
    let (code_job, _) = hover(source, "job.id");
    assert_eq!(code_job, "fixed job: Job");
    let (code_recv, _) = hover(source, "<<- jobs");
    assert!(code_recv.contains("<<-"));
    let (code_send, _) = hover(source, "->> chan_new");
    assert!(code_send.contains("->>"));

    let dot = "\
shape Job
    id: int
fi

task main()
    fixed jobs = chan_new<own Job>(2)
    fixed job = <<- jobs
    job.@@
fi
";
    assert!(labels(dot).contains(&"id".to_string()));
}

#[test]
fn commitment_completions_and_hover() {
    let com_dot = "\
task work() -> int ~ text
    <- 1
fi

task main()
    fixed c = rush work()
    c.@@
fi
";
    let offered = labels(com_dot);
    for expected in ["wait", "poll", "cancel", "then", "recover"] {
        assert!(offered.contains(&expected.to_string()), "expected '{expected}' on fallible Commitment, got {offered:?}");
    }

    let slot_dot = "\
task work() -> int
    <- 1
fi

task main()
    flex slot: maybe Commitment[int] = rush work()
    slot.@@
fi
";
    let slot_offered = labels(slot_dot);
    assert!(slot_offered.contains(&"poll".to_string()));
    assert!(slot_offered.contains(&"cancel".to_string()));

    let expr_kw = "\
task work() -> int
    <- 1
fi

task main()
    fixed c = ru@@
fi
";
    assert!(labels(expr_kw).contains(&"rush".to_string()));

    let hover_src = "\
task work() -> int ~ text
    <- 1
fi

task main()
    fixed c = rush work()
    fixed d = c.then(x -> x + 1).recover(_ -> 0)
    fixed v = <<- d
    show(v)
fi
";
    let (code_then, doc_then) = hover(hover_src, "then(");
    assert!(code_then.contains("then") && doc_then.contains("Commitment"));
    let (code_rec, doc_rec) = hover(hover_src, "recover(");
    assert!(code_rec.contains("recover") && doc_rec.contains("Commitment"));
    let (code_await, doc_await) = hover(hover_src, "<<- d");
    assert!(code_await.contains("<<-") && doc_await.contains("Commitment"));
}


