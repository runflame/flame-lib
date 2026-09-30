use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use demo_consensus_backend::{
    api::{ApiState, router},
    types::DemoSnapshot,
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected_status: u16,
) -> Result<Value> {
    let payload = body.map(|body| body.to_string()).unwrap_or_default();
    raw_call(app, method, path, payload, expected_status).await
}

async fn raw_call(
    app: &Router,
    method: &str,
    path: &str,
    body: String,
    expected_status: u16,
) -> Result<Value> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body))?,
        )
        .await?;
    let status = response.status();
    ensure!(
        response.headers()["cache-control"] == "no-store",
        "missing cache policy"
    );
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024).await?;
    let value: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        status.as_u16() == expected_status,
        "{method} {path}: expected {expected_status}, got {status}: {value}"
    );
    Ok(value)
}

async fn snapshot(app: &Router) -> Result<Value> {
    let value = call(app, "GET", "/api/state", None, 200).await?;
    let _: DemoSnapshot =
        serde_json::from_value(value.clone()).context("snapshot violates DTO contract")?;
    Ok(value)
}

async fn mine(app: &Router) -> Result<u64> {
    let blocks = call(
        app,
        "POST",
        "/api/bitcoin/blocks",
        Some(json!({"count":1})),
        200,
    )
    .await?;
    blocks[0]["height"].as_u64().context("missing mined height")
}

#[tokio::test(flavor = "multi_thread")]
async fn api_exposes_mempool_consensus_forks_double_signs_and_reset() -> Result<()> {
    let state = ApiState::start().await?;
    let app = router(state.clone());
    let result: Result<()> = async {
        let initial = snapshot(&app).await?;
        ensure!(
            initial["bitcoin"]["tip"] == initial["consensus"]["btc_cursor"],
            "startup did not catch up"
        );
        let root = initial["flame"]["canonical_tip"]["hash"].clone();
        ensure!(initial["minters"][0]["name"] == "Alice", "startup omitted Alice");
        ensure!(initial["flame"]["blocks"][0]["chain_weight"] == 7, "startup omitted genesis weight");
        call(
            &app,
            "POST",
            "/api/minters",
            Some(json!({"name":"Bob"})),
            200,
        )
        .await?;
        mine(&app).await?;
        let sent = call(
            &app,
            "POST",
            "/api/acquisitions",
            Some(json!({"minter_id":1,"amount_sats":80_000})),
            200,
        )
        .await?;
        let acquisition_txids = [sent["txid"].clone()];
        let pending = snapshot(&app).await?;
        ensure!(
            pending["acquisitions"].as_array().unwrap().len() == 2,
            "missing pending acquisitions"
        );
        ensure!(
            pending["acquisitions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|acq| acquisition_txids.contains(&acq["txid"]))
                .all(|acq| acq["transaction_status"]["status"] == "mempool"
                    && acq["processing_status"]["status"] == "unprocessed"),
            "pending acquisitions appear processed"
        );
        let height = mine(&app).await?;
        let confirmed = snapshot(&app).await?;
        for acquisition in confirmed["acquisitions"].as_array().unwrap().iter().filter(|acq| acquisition_txids.contains(&acq["txid"])) {
            ensure!(
                acquisition["processing_status"]["status"] == "accepted",
                "confirmed acquisition missing outcome"
            );
            ensure!(
                acquisition["processing_status"]["activates_at_btc_height"] == height + 1,
                "wrong acquisition maturity"
            );
            ensure!(
                acquisition["processing_status"]["expires_at_btc_height_exclusive"] == height + 101,
                "wrong acquisition expiry"
            );
        }
        let transactions = confirmed["bitcoin"]["blocks"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["transactions"]
            .as_array()
            .unwrap();
        ensure!(
            acquisition_txids
                .iter()
                .all(|txid| transactions.contains(txid)),
            "Bitcoin block omitted acquisition txids"
        );
        let block_request = json!({"parent_hash":root,"target_btc_height":height + 1});
        let first = call(
            &app,
            "POST",
            "/api/flame/blocks",
            Some(block_request.clone()),
            200,
        )
        .await?;
        let second = call(&app, "POST", "/api/flame/blocks", Some(block_request), 200).await?;
        let first_vote = call(
            &app,
            "POST",
            "/api/votes",
            Some(json!({"minter_id":0,"core_height":first["core"]["height"],"block_hash":first["tip"]["hash"]})),
            200,
        )
        .await?;
        let pending = snapshot(&app).await?;
        let pending_vote = pending["votes"].as_array().unwrap().iter()
            .find(|vote| vote["txid"] == first_vote["txid"]).context("missing pending vote")?;
        ensure!(
            pending_vote["processing_status"]["status"] == "unprocessed",
            "unconfirmed vote counted"
        );
        mine(&app).await?;
        let first_state = snapshot(&app).await?;
        ensure!(
            first_state["flame"]["canonical_tip"] == first["tip"],
            "first vote did not select its block"
        );
        let accepted_vote = first_state["votes"].as_array().unwrap().iter()
            .find(|vote| vote["txid"] == first_vote["txid"]).context("missing accepted vote")?;
        ensure!(
            accepted_vote["processing_status"]["effective_minting_power"] == 200,
            "wrong vote power"
        );
        call(
            &app,
            "POST",
            "/api/votes",
            Some(json!({"minter_id":1,"core_height":second["core"]["height"],"block_hash":second["tip"]["hash"]})),
            200,
        )
        .await?;
        mine(&app).await?;
        let switched = snapshot(&app).await?;
        ensure!(
            switched["flame"]["canonical_tip"] == second["tip"],
            "stronger vote did not switch branches"
        );
        ensure!(
            switched["consensus"]["heaviest_tip"] == second["tip"],
            "heaviest tip does not match consensus"
        );
        call(
            &app,
            "POST",
            "/api/votes",
            Some(json!({"minter_id":1,"core_height":first["core"]["height"],"block_hash":first["tip"]["hash"]})),
            200,
        )
        .await?;
        mine(&app).await?;
        let penalized = snapshot(&app).await?;
        ensure!(
            penalized["flame"]["canonical_tip"] == first["tip"],
            "double sign did not restore branch"
        );
        ensure!(
            penalized["minters"][1]["is_double_signed"] == true,
            "missing minter penalty"
        );
        ensure!(
            penalized["consensus"]["double_signs"][0]["votes"]
                .as_array()
                .unwrap()
                .len()
                == 2,
            "missing double-sign evidence"
        );
        ensure!(
            penalized["votes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|vote| vote["processing_status"]["status"] == "invalidated_by_double_sign")
                .count()
                == 2,
            "removed votes still counted"
        );
        call(
            &app,
            "POST",
            "/api/votes",
            Some(json!({"minter_id":0,"core_height":50,"block_hash":"cc".repeat(32)})),
            200,
        )
        .await?;
        mine(&app).await?;
        let pending_block = snapshot(&app).await?;
        ensure!(
            pending_block["votes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|vote| vote["processing_status"]["status"] == "pending_block"),
            "missing pending-block status"
        );
        let reset = call(
            &app,
            "POST",
            "/api/reset",
            Some(json!({})),
            200,
        )
        .await?;
        ensure!(
            reset["minters"].as_array().unwrap().len() == 1,
            "reset retained old minters"
        );
        ensure!(
            reset["minters"][0]["automatic_voting"] == false,
            "reset enabled automatic voting"
        );
        ensure!(
            reset["minters"][0]["p2wsh_address"] != initial["minters"][0]["p2wsh_address"],
            "reset reused old identity"
        );
        ensure!(
            reset["flame"]["blocks"].as_array().unwrap().len() == 1,
            "reset retained Flame branches"
        );
        for field in ["acquisitions", "votes"] {
            ensure!(
                reset[field].as_array().unwrap().len() == 1,
                "reset did not seed exactly one {field}"
            );
            ensure!(reset[field][0]["txid"] != initial[field][0]["txid"], "reset reused initial {field}");
        }
        ensure!(reset["minters"][0]["name"] == "Alice", "reset omitted Alice");
        ensure!(reset["acquisitions"][0]["amount_sats"] == 20_000, "reset seeded wrong acquisition");
        ensure!(reset["votes"][0]["processing_status"]["status"] == "accepted", "reset seed vote was rejected");
        ensure!(reset["flame"]["blocks"][0]["core"]["height"] == 1, "reset genesis is not core");
        ensure!(reset["flame"]["blocks"][0]["chain_weight"] == 7, "reset genesis has no weight");
        ensure!(
            reset["consensus"]["double_signs"]
                .as_array()
                .unwrap()
                .is_empty(),
            "reset retained double signs"
        );
        ensure!(
            snapshot(&app).await? == reset,
            "reset result differs from current state"
        );
        Ok(())
    }
    .await;
    let shutdown = state.shutdown().await;
    result?;
    shutdown?;
    let stopped = call(&app, "GET", "/api/state", None, 503).await?;
    ensure!(
        stopped["error"]["code"] == "unavailable",
        "wrong stopped response"
    );
    state.shutdown().await
}

#[tokio::test(flavor = "multi_thread")]
async fn api_validates_requests_and_serializes_concurrent_commands() -> Result<()> {
    let state = ApiState::start().await?;
    let app = router(state.clone());
    let result: Result<()> = async {
        let health = call(&app, "GET", "/api/health", None, 200).await?;
        ensure!(health["status"] == "ready", "health is not ready");
        call(
            &app,
            "POST",
            "/api/reset",
            Some(json!({"automatic_voting":true})),
            422,
        )
        .await?;
        let malformed = raw_call(&app, "POST", "/api/minters", "{".into(), 400).await?;
        ensure!(
            malformed["error"]["code"] == "invalid_json",
            "unstructured JSON error"
        );
        for body in [
            json!({"count":0}),
            json!({"count":-1}),
            json!({"count":1,"typo":true}),
        ] {
            call(&app, "POST", "/api/bitcoin/blocks", Some(body), 422).await?;
        }
        call(
            &app,
            "POST",
            "/api/bitcoin/blocks",
            Some(json!({"count":101})),
            400,
        )
        .await?;
        call(
            &app,
            "POST",
            "/api/minters",
            Some(json!({"name":"  "})),
            400,
        )
        .await?;
        call(
            &app,
            "POST",
            "/api/acquisitions",
            Some(json!({"minter_id":99,"amount_sats":1000})),
            404,
        )
        .await?;
        call(
            &app,
            "POST",
            "/api/votes",
            Some(json!({"minter_id":0,"core_height":1,"block_hash":"00"})),
            400,
        )
        .await?;
        call(&app, "GET", "/api/missing", None, 404).await?;
        call(&app, "GET", "/api/votes", None, 405).await?;
        raw_call(&app, "POST", "/api/minters", "x".repeat(17 * 1024), 413).await?;
        let before = snapshot(&app).await?;
        ensure!(
            before["minters"].as_array().unwrap().len() == 1,
            "invalid commands changed state"
        );
        let (first, second) = tokio::join!(
            call(
                &app,
                "POST",
                "/api/minters",
                Some(json!({"name":"Alice"})),
                200
            ),
            call(
                &app,
                "POST",
                "/api/minters",
                Some(json!({"name":"Bob"})),
                200
            ),
        );
        ensure!(
            first?["id"] != second?["id"],
            "concurrent commands reused an ID"
        );
        let current = snapshot(&app).await?;
        ensure!(
            current["minters"].as_array().unwrap().len() == 3,
            "lost concurrent minter"
        );
        mine(&app).await?;
        Ok(())
    }
    .await;
    let shutdown = state.shutdown().await;
    result?;
    shutdown
}
