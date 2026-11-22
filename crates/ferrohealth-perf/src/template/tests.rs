// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

use super::*;
use crate::plan::{PlanError, Request};

const FEED: &str = "[[requests]]\ngroup = \"feed\"\nmethod = \"PUT\"\n\
                    path = \"/Patient?identifier=C-{{n}}\"\nstatus = 200\n\
                    body = '{\"id\":\"C-{{n}}\",\"family\":\"{{who}}\",\"u\":\"{{u}}\",\"r\":\"{{r}}\"}'\n\
                    headers = { x-trace = \"t-{{n}}\" }\n\
                    [vars.n]\nkind = \"counter\"\nstart = 1\nwidth = 7\n\
                    [vars.who]\nkind = \"list\"\nvalues = [\"Ada\", \"Bram\", \"Cato\"]\norder = \"random\"\n\
                    [vars.u]\nkind = \"uuid\"\n\
                    [vars.r]\nkind = \"random\"\nmin = 10\nmax = 99\n";

fn plan(text: &str) -> Plan {
    Plan::parse(text).unwrap()
}

fn templates(plan: &Plan) -> Templates {
    Templates::compile(plan, &[]).unwrap()
}

/// The first `count` sends of request 0 of `plan`, in one phase.
fn sends(templates: &Templates, plan: &Plan, count: usize) -> Vec<Request> {
    let mut renderer = templates.renderer();
    (0..count)
        .map(|_| renderer.request(0, &plan.requests[0]).unwrap().unwrap())
        .collect()
}

#[test]
fn placeholders_are_lower_case_names_with_an_optional_field() {
    let found = pieces(b"a{{n}}b{{patient.birthDate}}{{x_2.f-1}}").unwrap();
    assert_eq!(
        vec![
            Piece::Text(b"a"),
            Piece::Slot {
                name: "n",
                field: None
            },
            Piece::Text(b"b"),
            Piece::Slot {
                name: "patient",
                field: Some("birthDate")
            },
            Piece::Slot {
                name: "x_2",
                field: Some("f-1")
            },
        ],
        found
    );
    assert_eq!(
        vec![Piece::Text(b"{ \"a\": {} }")],
        pieces(b"{ \"a\": {} }").unwrap()
    );
    for bad in [
        "{{ n }}",
        "{{N}}",
        "{{n.}}",
        "{{n",
        "x{{}}",
        "{{9n}}",
        "{{n.a.b}}",
    ] {
        let reason = pieces(bad.as_bytes()).unwrap_err();
        assert!(reason.contains("opens no placeholder"), "{bad}: {reason}");
    }
    assert!(pieces(b"ab{{ n").unwrap_err().contains("byte 2"));
}

#[test]
fn a_placeholder_must_name_a_declared_variable_the_right_way() {
    for (text, word) in [
        (
            "[[requests]]\ngroup = \"g\"\nmethod = \"GET\"\npath = \"/{{m}}\"\nstatus = 200\n[vars.n]\nkind = \"uuid\"\n",
            "names no variable",
        ),
        (
            "[[requests]]\ngroup = \"g\"\nmethod = \"GET\"\npath = \"/{{p}}\"\nstatus = 200\n[vars.p]\nkind = \"file\"\npath = \"p.csv\"\n",
            "name a field",
        ),
        (
            "[[requests]]\ngroup = \"g\"\nmethod = \"GET\"\npath = \"/{{n.x}}\"\nstatus = 200\n[vars.n]\nkind = \"uuid\"\n",
            "has none",
        ),
        (
            "[[requests]]\ngroup = \"g\"\nmethod = \"GET\"\npath = \"/x\"\nstatus = 200\nheaders = { a = \"{{m}}\" }\n",
            "names no variable",
        ),
        (
            "[[requests]]\ngroup = \"g\"\nmethod = \"POST\"\npath = \"/x\"\nstatus = 200\nbody = \"{{ m }}\"\n",
            "opens no placeholder",
        ),
    ] {
        match Plan::parse(text) {
            Err(PlanError::Invalid(reason)) => {
                assert!(
                    reason.starts_with("request 0: ") && reason.contains(word),
                    "{reason}"
                );
            }
            other => panic!("{text} gave {other:?}"),
        }
    }
}

#[test]
fn a_bad_variable_is_refused_by_name() {
    let one = "[[requests]]\ngroup = \"g\"\nmethod = \"GET\"\npath = \"/x\"\nstatus = 200\n";
    for (table, word) in [
        ("[vars.N]\nkind = \"uuid\"\n", "lower-case"),
        ("[vars.n]\nkind = \"counter\"\nstep = 0\n", "step"),
        ("[vars.n]\nkind = \"random\"\nmin = 5\nmax = 4\n", "min"),
        ("[vars.n]\nkind = \"list\"\nvalues = []\n", "empty"),
        (
            "[vars.n]\nkind = \"list\"\nvalues = [\"a\\nb\"]\n",
            "control",
        ),
        (
            "[vars.n]\nkind = \"file\"\npath = \"p.json\"\n",
            ".jsonl or .csv",
        ),
    ] {
        match Plan::parse(&format!("{one}{table}")) {
            Err(PlanError::Invalid(reason)) => {
                assert!(
                    reason.starts_with("vars.") && reason.contains(word),
                    "{reason}"
                );
            }
            other => panic!("{table} gave {other:?}"),
        }
    }
    for table in [
        "[vars.n]\nkind = \"counter\"\nstrat = 1\n",
        "[vars.n]\nkind = \"uuid\"\nwidth = 3\n",
        "[vars.n]\nkind = \"dice\"\n",
        "[vars.n]\nkind = \"list\"\nvalues = [\"a\"]\norder = \"shuffle\"\n",
    ] {
        assert!(
            matches!(
                Plan::parse(&format!("{one}{table}")),
                Err(PlanError::Parse { .. })
            ),
            "{table}"
        );
    }
}

#[test]
fn one_seed_sends_the_same_values_and_another_seed_others() {
    let first = plan(FEED);
    let again = sends(&templates(&first), &first, 50);
    assert_eq!(again, sends(&templates(&first), &first, 50));
    let other = plan(&format!("[load]\nseed = 9\n{FEED}"));
    let reseeded = sends(&templates(&other), &other, 50);
    assert_eq!(
        again.iter().map(|r| &r.path).collect::<Vec<_>>(),
        reseeded.iter().map(|r| &r.path).collect::<Vec<_>>(),
        "a counter takes no seed"
    );
    assert_ne!(
        again.iter().map(|r| &r.body).collect::<Vec<_>>(),
        reseeded.iter().map(|r| &r.body).collect::<Vec<_>>(),
        "the random kinds do"
    );
}

#[test]
fn every_place_of_one_send_carries_the_same_draw() {
    let plan = plan(FEED);
    let sent = sends(&templates(&plan), &plan, 3);
    assert_eq!("/Patient?identifier=C-0000001", sent[0].path);
    assert_eq!("/Patient?identifier=C-0000003", sent[2].path);
    for (index, request) in sent.iter().enumerate() {
        let n = format!("C-{:07}", index + 1);
        let body: serde_json::Value =
            serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(n, body["id"], "{body}");
        assert_eq!(
            format!("t-{:07}", index + 1).as_str(),
            request.headers["x-trace"]
        );
        assert!(["Ada", "Bram", "Cato"].contains(&body["family"].as_str().unwrap()));
        let r: u64 = body["r"].as_str().unwrap().parse().unwrap();
        assert!((10..=99).contains(&r), "{r}");
        let u = uuid::Uuid::parse_str(body["u"].as_str().unwrap()).unwrap();
        assert_eq!(Some(uuid::Version::Random), u.get_version());
    }
    assert_eq!(
        "feed", sent[0].group,
        "the rest of the request is as written"
    );
}

#[test]
fn the_sequences_continue_across_the_phases_sent_to_one_server() {
    let plan = plan(FEED);
    let templates = templates(&plan);
    let first = sends(&templates, &plan, 4);
    let second = sends(&templates, &plan, 4);
    assert_eq!("/Patient?identifier=C-0000005", second[0].path);
    let whole = sends(&self::templates(&plan), &plan, 8);
    assert_eq!(whole, [first, second].concat());
}

#[test]
fn a_file_variable_draws_rows_by_field_in_order_or_at_random() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("patients.csv");
    std::fs::write(&file, "id,family\nC-1,Ada\nC-2,Bram\nC-3,Cato\n").unwrap();
    let text = |order: &str| {
        format!(
            "[[requests]]\ngroup = \"feed\"\nmethod = \"PUT\"\npath = \"/P?i={{{{p.id}}}}\"\nstatus = 200\n\
             body = \"{{{{p.family}}}}/{{{{p.id}}}}\"\n[vars.p]\nkind = \"file\"\npath = {:?}\norder = \"{order}\"\n",
            file.display().to_string()
        )
    };
    let cycle = plan(&text("cycle"));
    let sent = sends(&templates(&cycle), &cycle, 4);
    let paths: Vec<&str> = sent.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(vec!["/P?i=C-1", "/P?i=C-2", "/P?i=C-3", "/P?i=C-1"], paths);
    assert_eq!(Some("Bram/C-2"), sent[1].body.as_deref());
    let random = plan(&text("random"));
    for request in sends(&templates(&random), &random, 30) {
        let (family, id) = request.body.as_deref().unwrap().split_once('/').unwrap();
        assert_eq!(format!("/P?i={id}"), request.path, "one row a send");
        assert!(["Ada", "Bram", "Cato"].contains(&family));
    }
    let missing = plan(&text("cycle").replace("p.family", "p.given"));
    assert!(matches!(
        Templates::compile(&missing, &[]),
        Err(TemplateError::Placeholder { reason, .. }) if reason.contains("\"given\"")
    ));
}

#[test]
fn a_variable_nothing_names_and_a_file_that_cannot_be_read_are_refused() {
    let unused = plan(&format!("{FEED}[vars.spare]\nkind = \"uuid\"\n"));
    assert!(matches!(
        Templates::compile(&unused, &[]),
        Err(TemplateError::Unused(name)) if name == "spare"
    ));
    let gone = plan(
        "[[requests]]\ngroup = \"g\"\nmethod = \"GET\"\npath = \"/{{p.id}}\"\nstatus = 200\n\
         [vars.p]\nkind = \"file\"\npath = \"/nonexistent/p.jsonl\"\n",
    );
    assert!(matches!(
        Templates::compile(&gone, &[]),
        Err(TemplateError::Read { .. })
    ));
}

#[test]
fn a_message_file_is_filled_around_its_msh_10() {
    let message =
        Message::parse(b"MSH|^~\\&|{{app}}|F|R|F|20260101||ADT^A01|1|P|2.5\rPID|||C-{{n}}\r")
            .unwrap();
    let plan = plan(
        "[mllp]\naddress = \"127.0.0.1:1\"\n[[messages]]\ngroup = \"iti-8\"\nfile = \"a.hl7\"\n\
         [vars.n]\nkind = \"counter\"\nwidth = 3\n[vars.app]\nkind = \"list\"\nvalues = [\"A\", \"B\"]\n",
    );
    let templates = Templates::compile(&plan, &[&message]).unwrap();
    let mut renderer = templates.renderer();
    let first = renderer.message(0).unwrap().unwrap().framed("ID");
    let second = renderer.message(0).unwrap().unwrap().framed("ID");
    assert_eq!(
        b"\x0bMSH|^~\\&|A|F|R|F|20260101||ADT^A01|ID|P|2.5\rPID|||C-000\r\x1c\r".as_slice(),
        first
    );
    assert_eq!(
        b"\x0bMSH|^~\\&|B|F|R|F|20260101||ADT^A01|ID|P|2.5\rPID|||C-001\r\x1c\r".as_slice(),
        second
    );
}

#[test]
fn a_counter_past_its_largest_value_is_an_error() {
    let plan = plan(&format!(
        "[[requests]]\ngroup = \"g\"\nmethod = \"GET\"\npath = \"/{{{{n}}}}\"\nstatus = 200\n\
         [vars.n]\nkind = \"counter\"\nstart = {}\n",
        u64::MAX
    ));
    let templates = templates(&plan);
    let mut renderer = templates.renderer();
    assert!(renderer.request(0, &plan.requests[0]).is_ok());
    assert!(matches!(
        renderer.request(0, &plan.requests[0]),
        Err(TemplateError::Overflow(name)) if name == "n"
    ));
}

#[test]
fn a_plan_without_variables_is_sent_as_written_and_records_no_seed() {
    let one = "[[requests]]\ngroup = \"g\"\nmethod = \"GET\"\npath = \"/x\"\nstatus = 200\n";
    let plain = plan(one);
    let templates = templates(&plain);
    assert_eq!(
        None,
        templates.renderer().request(0, &plain.requests[0]).unwrap()
    );
    assert_eq!(None, crate::run::identity(&plain).seed);
    assert_eq!(Some(0), crate::run::identity(&plan(FEED)).seed);
    assert_eq!(
        Some(9),
        crate::run::identity(&plan(&format!("[load]\nseed = 9\n{FEED}"))).seed
    );
}

#[test]
fn drawing_an_arrival_costs_a_small_share_of_its_interval() {
    // The draw runs before the arrival's intended instant, so it moves no
    // send while it takes less than the interval: 200 us at 5,000 a second.
    // 10,000 arrivals of four variables, a header and a body take well
    // under 20 us each on a laptop; the bound leaves ten times that for a
    // busy host.
    let plan = plan(FEED);
    let templates = templates(&plan);
    let mut renderer = templates.renderer();
    let started = std::time::Instant::now();
    for _ in 0..10_000 {
        renderer.request(0, &plan.requests[0]).unwrap().unwrap();
    }
    let each = started.elapsed() / 10_000;
    assert!(
        each < std::time::Duration::from_micros(200),
        "{each:?} an arrival"
    );
}
