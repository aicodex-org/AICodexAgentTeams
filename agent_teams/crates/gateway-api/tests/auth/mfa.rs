//! 验证双重验证 HTTP 流程与身份隔离

use super::*;

#[tokio::test]
async fn mfa_http_flow_requires_complete_proof_and_protects_settings() {
    let (app, _) = auth_app().await;
    let credentials = json!({"mode":"admin", "password":"strong-admin-password"});
    let cookie = session_cookie(&login(&app, credentials.clone(), None).await);
    let key_cookie =
        session_cookie(&login(&app, json!({"mode":"key","apiKey":RAW_KEY}), None).await);
    let post = |path: &str, body: Value| {
        let mut request = json_request(Method::POST, path, body);
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        app.clone().oneshot(request)
    };
    assert_eq!(
        get(&app, "/api/auth/mfa", &key_cookie).await.status(),
        StatusCode::FORBIDDEN
    );
    let anonymous = app
        .clone()
        .oneshot(json_request(
            Method::POST,
            "/api/auth/mfa/setup",
            json!({"password":"strong-admin-password"}),
        ))
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let setup_response = post(
        "/api/auth/mfa/setup",
        json!({"password":"strong-admin-password"}),
    )
    .await
    .unwrap();
    assert_eq!(setup_response.status(), StatusCode::OK);
    assert_eq!(setup_response.headers()[header::CACHE_CONTROL], "no-store");
    let setup = response_json(setup_response).await["data"].clone();
    let totp = totp_rs::Builder::new()
        .with_secret(totp_rs::Secret::try_from_base32(setup["secret"].as_str().unwrap()).unwrap())
        .build()
        .unwrap();
    let code = totp
        .generate(chrono::Utc::now().timestamp() as u64)
        .to_string();
    let confirmation = post(
        "/api/auth/mfa/confirm",
        json!({"password":"strong-admin-password","code":code,"setupId":setup["setupId"]}),
    )
    .await
    .unwrap();
    assert_eq!(confirmation.status(), StatusCode::OK);
    assert!(
        confirmation.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        get(&app, "/api/auth/mfa", &cookie).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let challenge = login(&app, credentials.clone(), None).await;
    assert_eq!(challenge.status(), StatusCode::OK);
    assert!(!challenge.headers().contains_key(header::SET_COOKIE));
    assert_eq!(
        response_json(challenge).await["data"],
        json!({"requiresTwoFactor":true})
    );
    let mut completed = credentials;
    completed["secondFactor"] = setup["recoveryCodes"][0].clone();
    let accepted = login(&app, completed.clone(), None).await;
    assert_eq!(accepted.status(), StatusCode::OK);
    let accepted_cookie = session_cookie(&accepted);
    assert_eq!(
        response_json(get(&app, "/api/auth/mfa", &accepted_cookie).await).await["data"]["recoveryCodesRemaining"],
        9
    );
    assert_eq!(
        login(&app, completed, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        response_json(get(&app, "/api/auth/status", &key_cookie).await).await["data"]["authenticated"],
        true
    );
}
