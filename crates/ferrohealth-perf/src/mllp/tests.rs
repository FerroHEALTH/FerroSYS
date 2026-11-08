// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

use super::*;

const A01: &str = "MSH|^~\\&|PERF|FAC|PIX|FAC|20261108100000||ADT^A01^ADT_A01|FIXED-1|P|2.5\n\
                   EVN|A01|20261108100000\n\
                   PID|||P-1^^^&1.2.3&ISO||DOE^JANE\n";

#[test]
fn a_message_is_framed_with_its_fresh_control_id() {
    let message = Message::parse(A01.as_bytes()).unwrap();
    let frame = message.framed("abcd0000000000000001");
    assert_eq!(Some(&START), frame.first(), "the start block");
    assert!(
        frame.ends_with(&[CR, END, CR]),
        "the last segment, then the end block"
    );
    let text = String::from_utf8(frame[1..frame.len() - 2].to_vec()).unwrap();
    assert!(
        text.starts_with(
            "MSH|^~\\&|PERF|FAC|PIX|FAC|20261108100000||ADT^A01^ADT_A01|abcd0000000000000001|P|2.5\r"
        ),
        "{text:?}"
    );
    assert!(!text.contains("FIXED-1"), "{text:?}");
    assert!(
        !text.contains('\n'),
        "line feeds end segments as CR: {text:?}"
    );
    assert_eq!(3, text.matches('\r').count(), "{text:?}");
}

#[test]
fn a_crlf_message_and_one_whose_msh_10_ends_the_segment_parse() {
    let crlf = "MSH|^~\\&|A|B|C|D|20261108||ADT^A01|X\r\nPID|||1\r\n";
    let text = Message::parse(crlf.as_bytes()).unwrap().framed("ID");
    assert_eq!(
        b"\x0bMSH|^~\\&|A|B|C|D|20261108||ADT^A01|ID\rPID|||1\r\x1c\r".as_slice(),
        text
    );
}

#[test]
fn a_message_without_msh_or_msh_10_is_refused() {
    for (text, reason) in [
        ("PID|||1\r", "MSH"),
        ("MSH", "cut short"),
        (
            "MSH|^~\\&|A|B|C|D|20261108||ADT^A01\rPID|||1|||||||\r",
            "MSH-10",
        ),
    ] {
        let refused = Message::parse(text.as_bytes()).unwrap_err();
        assert!(refused.contains(reason), "{text:?}: {refused}");
    }
}

#[test]
fn an_ack_is_judged_by_its_code_and_its_echo() {
    let ack = |code: &str, echo: &str| {
        format!("MSH|^~\\&|PIX|FAC|PERF|FAC|20261108||ACK^A01^ACK|R1|P|2.5\rMSA|{code}|{echo}\r")
    };
    assert_eq!(Ok(()), judge_ack(ack("AA", "ID").as_bytes(), "ID"));
    assert_eq!(Ok(()), judge_ack(ack("CA", "ID").as_bytes(), "ID"));
    for code in ["AE", "AR", "CE", "CR"] {
        assert_eq!(
            Err(format!("ack {code}")),
            judge_ack(ack(code, "ID").as_bytes(), "ID")
        );
    }
    assert_eq!(
        Err("ack of another message".to_owned()),
        judge_ack(ack("AA", "OTHER").as_bytes(), "ID")
    );
    assert_eq!(
        Err("ack without MSA".to_owned()),
        judge_ack(b"MSH|^~\\&|PIX\r", "ID")
    );
}

#[test]
fn the_host_of_an_address_names_the_tls_server() {
    assert_eq!("localhost", host_of("localhost:2575"));
    assert_eq!("::1", host_of("[::1]:2575"));
    assert_eq!("pix.example", host_of("pix.example"));
}

#[test]
fn a_plan_of_messages_needs_a_listener() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a01.hl7");
    std::fs::write(&file, A01).unwrap();
    let text = format!(
        "[[messages]]\ngroup = \"iti-8\"\nfile = {:?}\n",
        file.display().to_string()
    );
    let plan = Plan::parse(&text).unwrap();
    assert_eq!(vec!["iti-8".to_owned()], plan.groups());
    assert!(matches!(
        Sender::new(&plan, None),
        Err(MllpError::NoAddress)
    ));
    let sender = Sender::new(&plan, Some("127.0.0.1:2575")).unwrap().unwrap();
    assert_eq!("127.0.0.1:2575", sender.address());
    let with_address = format!("[mllp]\naddress = \"127.0.0.1:2576\"\n{text}");
    let plan = Plan::parse(&with_address).unwrap();
    let sender = Sender::new(&plan, None).unwrap().unwrap();
    assert_eq!("127.0.0.1:2576", sender.address(), "the plan's own");
    let http_only = Plan::parse(
        "[[requests]]\ngroup = \"read\"\nmethod = \"GET\"\npath = \"/\"\nstatus = 200\n",
    )
    .unwrap();
    assert!(Sender::new(&http_only, None).unwrap().is_none());
}
