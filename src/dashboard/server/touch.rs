//! Touch handler: one store-addressed mutation with a revision precondition.

use std::net::TcpStream;

use super::http::{send_response, touch_request_allowed, HttpRequest};
use super::*;

struct TouchInput {
    key: String,
    store_id: String,
    expected_rev: i64,
}

fn parse_touch_body(req: &HttpRequest) -> std::result::Result<TouchInput, serde_json::Value> {
    if req.body.len() > 4096 {
        return Err(serde_json::json!({ "ok": false, "error": "body exceeds 4096 bytes" }));
    }
    let val: serde_json::Value =
        serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
    let key = val
        .get("key")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let store_id = val
        .get("store_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let Some(expected_rev) = val.get("rev").and_then(serde_json::Value::as_i64) else {
        return Err(
            serde_json::json!({ "ok": false, "error": "missing rev: reload the dashboard" }),
        );
    };
    if key.is_empty() || store_id.is_empty() {
        return Err(serde_json::json!({ "ok": false, "error": "missing key or store_id" }));
    }
    Ok(TouchInput {
        key: key.to_string(),
        store_id: store_id.to_string(),
        expected_rev,
    })
}

fn check_current_rev(
    conn: &rusqlite::Connection,
    key: &str,
    expected_rev: i64,
    now: i64,
    store_name: &str,
    store_id: &str,
) -> std::result::Result<(), (u16, serde_json::Value)> {
    // The probe mirrors the SQL gate in `memory_touch_if_rev` and the CLI's
    // `mem touch` (`Revision::is_active`): a superseded or expired revision
    // is not current, and after an expired-key re-write two rows of one key
    // can both carry `superseded_by IS NULL`, so the highest revision wins
    // deterministically.
    let current_rev: Option<i64> = conn
        .query_row(
            "SELECT rev FROM memories WHERE key = ?1 AND superseded_by IS NULL
             AND (expires_at = 0 OR expires_at > ?2)
             ORDER BY rev DESC LIMIT 1",
            rusqlite::params![key, now],
            |r| r.get(0),
        )
        .ok();
    let Some(current_rev) = current_rev else {
        return Err((
            404,
            serde_json::json!({ "ok": false, "error": format!("unknown key '{key}'"), "store": store_name, "store_id": store_id }),
        ));
    };
    if expected_rev != current_rev {
        return Err((
            409,
            serde_json::json!({
                "ok": false,
                "error": format!("revision changed (you saw r{expected_rev}, current r{current_rev}) — reload"),
                "store": store_name,
                "store_id": store_id,
                "rev": current_rev,
            }),
        ));
    }
    Ok(())
}

pub(super) fn handle_memory_touch(
    stream: &mut TcpStream,
    ctx: &ServerCtx<'_>,
    req: &HttpRequest,
) -> std::io::Result<()> {
    if let Err((status, message)) = touch_request_allowed(req) {
        return touch_reply(
            stream,
            status,
            &serde_json::json!({ "ok": false, "error": message }),
        );
    }
    let input = match parse_touch_body(req) {
        Ok(input) => input,
        Err(body) => return touch_reply(stream, 400, &body),
    };
    let key = &input.key;
    let store_id = &input.store_id;
    let expected_rev = input.expected_rev;

    let entries = super::read_entries(ctx.log_path, ctx.global);
    let Some((store_name, _repo_root, db_path)) = data::resolve_store(ctx.root, &entries, store_id)
    else {
        let available: Vec<String> = data::store_ids(ctx.root, &entries);
        return touch_reply(
            stream,
            404,
            &serde_json::json!({
                "ok": false,
                "error": format!("unknown store '{store_id}'"),
                "key": key,
                "requested_store": store_id,
                "available_stores": available,
            }),
        );
    };

    let mut conn = match crate::db::open_memory_mutation_at(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("eg dashboard: touch store open failed: {store_name} {e}");
            return touch_reply(
                stream,
                503,
                &serde_json::json!({ "ok": false, "error": format!("store open failed: {e}"), "store": store_name, "store_id": store_id }),
            );
        }
    };

    let now = crate::db::unix_now();
    if let Err((status, body)) =
        check_current_rev(&conn, key, expected_rev, now, &store_name, store_id)
    {
        return touch_reply(stream, status, &body);
    }

    match crate::db::memory_touch_if_rev(&mut conn, key, expected_rev, now) {
        Ok(Some(rev)) => {
            eprintln!(
                "eg dashboard: touch ok key={key} store={store_name} rev={rev} store_id={store_id}"
            );
            touch_reply(
                stream,
                200,
                &serde_json::json!({ "ok": true, "key": key, "store": store_name, "store_id": store_id, "rev": rev, "anchored_at": now }),
            )
        }
        Ok(None) => touch_reply(
            stream,
            409,
            &serde_json::json!({ "ok": false, "error": "revision moved during verify — reload", "store": store_name, "store_id": store_id }),
        ),
        Err(e) => {
            eprintln!("eg dashboard: touch write failed key={key} store={store_name}: {e}");
            touch_reply(
                stream,
                503,
                &serde_json::json!({ "ok": false, "error": format!("store write failed: {e}"), "store": store_name, "store_id": store_id }),
            )
        }
    }
}

fn touch_reply(
    stream: &mut TcpStream,
    status: u16,
    body: &serde_json::Value,
) -> std::io::Result<()> {
    let encoded = serde_json::to_vec(body).unwrap_or_default();
    send_response(
        stream,
        status,
        "application/json; charset=utf-8",
        &encoded,
        false,
    )
}
