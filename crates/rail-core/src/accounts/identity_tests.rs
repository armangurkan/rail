use super::*;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn verify(
    kind: AccountKind,
    address: &str,
    response: ResponseTemplate,
) -> Result<(), OpError> {
    let server = MockServer::start().await;
    let at = if kind == AccountKind::Gmail {
        "/profile"
    } else {
        "/me"
    };
    let mut mock = Mock::given(method("GET"))
        .and(path(at))
        .and(header("authorization", "Bearer access"));
    if kind == AccountKind::M365 {
        mock = mock.and(query_param(
            "$select",
            "mail,userPrincipalName,proxyAddresses",
        ));
    }
    mock.respond_with(response).expect(1).mount(&server).await;
    let account = AccountConfig {
        kind,
        address: address.into(),
        ..crate::test_support::sample_account()
    };
    verify_mailbox(
        &crate::auth::http_client().unwrap(),
        &account,
        &crate::test_support::tokens(),
        Some(&format!("{}/", server.uri())),
    )
    .await
}

#[tokio::test]
async fn gmail_primary_address_is_compared_ignoring_case() {
    verify(
        AccountKind::Gmail,
        "Me@Example.com",
        ResponseTemplate::new(200).set_body_json(json!({"emailAddress": "me@example.com"})),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn graph_accepts_the_mailbox_address_sign_in_name_and_smtp_aliases() {
    let body = json!({"mail": "primary@example.com", "userPrincipalName": "login@example.com",
        "proxyAddresses": ["SMTP:primary@example.com", "smtp:alias@example.com", "SIP:other@example.com"]});
    for address in [
        "PRIMARY@example.com",
        "LOGIN@example.com",
        "ALIAS@example.com",
    ] {
        verify(
            AccountKind::M365,
            address,
            ResponseTemplate::new(200).set_body_json(&body),
        )
        .await
        .unwrap();
    }
    let error = verify(
        AccountKind::M365,
        "other@example.com",
        ResponseTemplate::new(200).set_body_json(body),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Auth);
    assert_eq!(error.class, ErrorClass::Permanent);
}

#[tokio::test]
async fn graph_accepts_an_smtp_alias_when_the_primary_fields_are_null() {
    verify(AccountKind::M365, "alias@example.com", ResponseTemplate::new(200)
        .set_body_json(json!({"mail": null, "userPrincipalName": null, "proxyAddresses": ["smtp:alias@example.com"]})))
        .await.unwrap();
}

#[test]
fn gmail_consumer_aliases_match_but_workspace_dots_are_significant() {
    for address in [
        "first.last@gmail.com",
        "firstlast+rail@gmail.com",
        "First.Last@googlemail.com",
    ] {
        assert!(gmail_matches(address, "firstlast@gmail.com"));
    }
    for address in ["other@gmail.com", "firstlast@elsewhere.com", "@gmail.com"] {
        assert!(!gmail_matches(address, "firstlast@gmail.com"));
    }
    assert!(!gmail_matches(
        "first.last@example.com",
        "firstlast@example.com"
    ));
    assert!(!gmail_matches(
        "firstlast+rail@example.com",
        "firstlast@example.com"
    ));
}

#[tokio::test]
async fn missing_malformed_and_oversized_profiles_fail_without_echoing_the_response() {
    let secret = "secret-token-in-a-profile";
    let responses = [
        ResponseTemplate::new(200).set_body_string(format!("not json {secret}")),
        ResponseTemplate::new(200).set_body_json(json!({"unexpected": secret})),
        ResponseTemplate::new(200).set_body_json(json!({"emailAddress": secret})),
        ResponseTemplate::new(200).set_body_string(secret.repeat(MAX_PROFILE_BYTES)),
    ];
    for response in responses {
        let error = verify(AccountKind::Gmail, "me@example.com", response)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Auth);
        assert_eq!(error.class, ErrorClass::Permanent);
        assert!(!format!("{error:?}").contains(secret));
    }
    let error = verify(
        AccountKind::M365,
        "me@example.com",
        ResponseTemplate::new(200).set_body_json(json!({})),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Auth);
}

#[tokio::test]
async fn profile_errors_are_classified_without_echoing_their_body() {
    for status in [401, 403, 404, 429, 500, 503] {
        let error = verify(
            AccountKind::Gmail,
            "me@example.com",
            ResponseTemplate::new(status).set_body_string("secret-token-in-an-error"),
        )
        .await
        .unwrap_err();
        let transient = status == 429 || status >= 500;
        assert_eq!(
            error.class,
            if transient {
                ErrorClass::Transient
            } else {
                ErrorClass::Permanent
            }
        );
        assert_eq!(
            error.code,
            if transient {
                ErrorCode::Network
            } else {
                ErrorCode::Auth
            }
        );
        assert!(!format!("{error:?}").contains("secret-token-in-an-error"));
    }
}

#[tokio::test]
async fn profile_redirects_are_not_followed() {
    let target = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"emailAddress": "me@example.com"})),
        )
        .expect(0)
        .mount(&target)
        .await;
    let error = verify(
        AccountKind::Gmail,
        "me@example.com",
        ResponseTemplate::new(302).insert_header("location", target.uri()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Auth);
}
