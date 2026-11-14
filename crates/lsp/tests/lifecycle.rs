mod common;

use common::*;
use ingot_lsp::Json;

#[test]
fn initialize_chooses_utf8_only_when_the_client_offers_it() {
    for (offered, expected) in [(true, "utf-8"), (false, "utf-16")] {
        let mut client = Client::spawn(&[]);
        let result = client.initialize(offered);
        assert_eq!(
            result
                .get("capabilities")
                .and_then(|capabilities| capabilities.get("positionEncoding"))
                .and_then(Json::as_str),
            Some(expected)
        );
        assert_eq!(
            result
                .get("capabilities")
                .and_then(|capabilities| capabilities.get("textDocumentSync"))
                .and_then(|sync| sync.get("change"))
                .and_then(Json::as_i64),
            Some(1)
        );
        assert_eq!(
            result
                .get("serverInfo")
                .and_then(|info| info.get("name"))
                .and_then(Json::as_str),
            Some("ingot-lsp")
        );
    }
}

#[test]
fn requests_before_initialize_are_server_not_initialized() {
    let mut client = Client::spawn(&[]);
    let id = client.request("shutdown", obj(vec![]));
    let response = client.response(id);
    assert_eq!(
        response
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Json::as_i64),
        Some(-32002),
        "{response:?}"
    );
}

#[test]
fn an_unknown_request_is_method_not_found() {
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    let id = client.request(
        "textDocument/hover",
        obj(vec![("textDocument", obj(vec![]))]),
    );
    let response = client.response(id);
    assert_eq!(
        response
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Json::as_i64),
        Some(-32601),
        "{response:?}"
    );
}

#[test]
fn shutdown_answers_null_then_exit_is_status_zero() {
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    let id = client.request("shutdown", obj(vec![]));
    let response = client.response(id);
    assert!(response.get("error").is_none(), "{response:?}");
    assert_eq!(response.get("result"), Some(&Json::Null));
    client.notify("exit", obj(vec![]));
    assert_eq!(client.wait(), 0);
}

#[test]
fn exit_without_shutdown_is_status_one() {
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.notify("exit", obj(vec![]));
    assert_eq!(client.wait(), 1);
}

#[test]
fn a_malformed_body_is_logged_and_skipped() {
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.send_frame_bytes(&[0xff, 0xfe]);
    let message = client.log_containing("UTF-8");
    assert!(message.contains("UTF-8"), "{message}");
    let id = client.request("shutdown", obj(vec![]));
    let response = client.response(id);
    assert_eq!(response.get("result"), Some(&Json::Null), "{response:?}");
}

#[test]
fn unknown_notifications_are_ignored() {
    let mut client = Client::spawn(&[]);
    client.initialize(false);
    client.notify("$/cancelRequest", obj(vec![("id", n(1))]));
    client.notify("workspace/didChangeConfiguration", obj(vec![]));
    let id = client.request("shutdown", obj(vec![]));
    let response = client.response(id);
    assert_eq!(response.get("result"), Some(&Json::Null), "{response:?}");
}
