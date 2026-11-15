#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use vela_analysis::{Analysis, LineIndex, Severity};

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

