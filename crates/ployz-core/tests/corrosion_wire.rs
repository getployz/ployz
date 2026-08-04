use ployz_core::corrosion::{
    ChangeId, ChangeKind, QueryEvent, QueryIdentity, QueryIdentityError, RowId, SqliteParameter,
    SqliteValue, Statement, SubscriptionEvent, TransactionResponse, TransactionResult,
};
use serde_json::json;

#[test]
fn statements_match_the_pinned_corrosion_request_shape() {
    let simple = Statement::simple("SELECT id FROM machines");
    assert_eq!(
        serde_json::to_value(simple).expect("simple statement serializes"),
        json!("SELECT id FROM machines")
    );

    let parameterized = Statement::with_params(
        "INSERT INTO values (a, b, c, d, e, f) VALUES (?, ?, ?, ?, ?, ?)",
        vec![
            SqliteParameter::Null,
            SqliteParameter::Bool(true),
            SqliteParameter::Integer(42),
            SqliteParameter::Real(2.5),
            SqliteParameter::Text("document".to_owned()),
            SqliteParameter::Blob(vec![0, 127, 255]),
        ],
    );
    assert_eq!(
        serde_json::to_value(parameterized).expect("parameterized statement serializes"),
        json!([
            "INSERT INTO values (a, b, c, d, e, f) VALUES (?, ?, ?, ?, ?, ?)",
            [null, true, 42, 2.5, "document", [0, 127, 255]]
        ])
    );
}

#[test]
fn transaction_results_preserve_success_and_failure_per_statement() {
    let response: TransactionResponse = serde_json::from_value(json!({
        "results": [
            {"rows_affected": 1, "time": 0.000027208},
            {"error": "UNIQUE constraint failed: machines.id"}
        ],
        "time": 0.000300708
    }))
    .expect("transaction response decodes");

    assert_eq!(response.results.len(), 2);
    assert!(matches!(
        response.results.as_slice(),
        [
            TransactionResult::Success(success),
            TransactionResult::Error(error)
        ] if success.rows_affected == 1
            && error.message == "UNIQUE constraint failed: machines.id"
    ));
    assert_eq!(response.version, None);
    assert_eq!(response.actor_id, None);

    let invalid = serde_json::from_value::<TransactionResponse>(json!({
        "results": [{"rows_affected": 1, "time": 0.1, "error": "ambiguous"}],
        "time": 0.1
    }));
    assert!(
        invalid.is_err(),
        "one result cannot be both success and error"
    );
}

#[test]
fn query_events_match_the_ndjson_frames() {
    let columns: QueryEvent =
        serde_json::from_str(r#"{"columns":["id","document"]}"#).expect("columns event decodes");
    let row: QueryEvent = serde_json::from_str(r#"{"row":[7,[null,9,1.25,"text",[0,255]]]}"#)
        .expect("row event decodes");
    let eoq: QueryEvent =
        serde_json::from_str(r#"{"eoq":{"time":0.00000005}}"#).expect("eoq decodes");
    let error: QueryEvent =
        serde_json::from_str(r#"{"error":"query failed"}"#).expect("error decodes");

    assert!(matches!(columns, QueryEvent::Columns(names) if names == ["id", "document"]));
    assert!(matches!(
        row,
        QueryEvent::Row(row_id, values)
            if row_id == RowId::new(7)
                && values == [
                    SqliteValue::Null,
                    SqliteValue::Integer(9),
                    SqliteValue::Real(1.25),
                    SqliteValue::Text("text".to_owned()),
                    SqliteValue::Blob(vec![0, 255]),
                ]
    ));
    assert!(matches!(
        eoq,
        QueryEvent::EndOfQuery(end) if end.time == 0.00000005 && end.change_id.is_none()
    ));
    assert!(matches!(error, QueryEvent::Error(message) if message == "query failed"));

    let change_in_query =
        serde_json::from_str::<QueryEvent>(r#"{"change":["insert",7,["value"],1]}"#);
    assert!(
        change_in_query.is_err(),
        "query streams cannot contain change frames"
    );
}

#[test]
fn subscription_events_preserve_initial_rows_and_contiguous_change_identity() {
    let row: SubscriptionEvent =
        serde_json::from_str(r#"{"row":[2,["ham"]]}"#).expect("initial row decodes");
    let eoq: SubscriptionEvent =
        serde_json::from_str(r#"{"eoq":{"time":0.00000008,"change_id":0}}"#)
            .expect("subscription eoq decodes");
    let change: SubscriptionEvent =
        serde_json::from_str(r#"{"change":["update",2,["smoked meat"],1]}"#)
            .expect("change event decodes");

    assert!(matches!(row, SubscriptionEvent::Row(id, values)
        if id == RowId::new(2) && values == [SqliteValue::Text("ham".to_owned())]));
    assert!(matches!(eoq, SubscriptionEvent::EndOfQuery(end)
        if end.change_id == Some(ChangeId::new(0))));
    assert!(
        matches!(change, SubscriptionEvent::Change(kind, row_id, values, change_id)
        if kind == ChangeKind::Update
            && row_id == RowId::new(2)
            && values == [SqliteValue::Text("smoked meat".to_owned())]
            && change_id == ChangeId::new(1))
    );

    let malformed_change =
        serde_json::from_str::<SubscriptionEvent>(r#"{"change":["update",2,["smoked meat"]]}"#);
    assert!(
        malformed_change.is_err(),
        "change tuples have exactly four elements"
    );
}

#[test]
fn query_identity_requires_valid_id_and_nonempty_hash() {
    let identity = QueryIdentity::from_parts(
        Some("ba247cbc-2a7f-486b-873c-8a9620e72182"),
        Some("query-hash"),
    )
    .expect("valid identity headers");
    assert_eq!(
        identity.id().to_string(),
        "ba247cbc-2a7f-486b-873c-8a9620e72182"
    );
    assert_eq!(identity.hash(), "query-hash");

    assert_eq!(
        QueryIdentity::from_parts(None, Some("query-hash")),
        Err(QueryIdentityError::MissingId)
    );
    assert_eq!(
        QueryIdentity::from_parts(Some("not-a-uuid"), Some("query-hash")),
        Err(QueryIdentityError::InvalidId)
    );
    assert_eq!(
        QueryIdentity::from_parts(Some("ba247cbc-2a7f-486b-873c-8a9620e72182"), Some("   "),),
        Err(QueryIdentityError::EmptyHash)
    );
}
