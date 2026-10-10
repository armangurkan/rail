use oauth2::AuthType;

use crate::config::AccountKind;

/// Gmail API scope for reading and modifying mail.
pub const GMAIL_SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify";

const GRAPH_SCOPES: [&str; 4] = [
    "https://graph.microsoft.com/Mail.ReadWrite",
    "https://graph.microsoft.com/Mail.Send",
    "https://graph.microsoft.com/User.Read",
    "offline_access",
];

/// Provider-specific OAuth endpoints and parameters.
#[derive(Debug, Clone)]
pub struct OAuthEndpoints {
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub auth_type: AuthType,
    /// Extra query parameters on the authorization URL.
    pub extra_auth_params: Vec<(String, String)>,
}

/// Endpoints for an account kind; `tenant` only applies to Microsoft 365.
pub fn endpoints_for(kind: AccountKind, tenant: Option<&str>) -> OAuthEndpoints {
    match kind {
        AccountKind::Gmail => OAuthEndpoints {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            scopes: vec![GMAIL_SCOPE.to_string()],
            auth_type: AuthType::BasicAuth,
            // Google only issues a refresh token with offline access and an explicit consent prompt.
            extra_auth_params: vec![
                ("access_type".to_string(), "offline".to_string()),
                ("prompt".to_string(), "consent".to_string()),
            ],
        },
        AccountKind::M365 => {
            let tenant = tenant.unwrap_or("common");
            OAuthEndpoints {
                auth_url: format!(
                    "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize"
                ),
                token_url: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
                scopes: GRAPH_SCOPES.iter().map(|s| (*s).to_string()).collect(),
                // Public client: credentials go in the body, never HTTP basic auth.
                auth_type: AuthType::RequestBody,
                extra_auth_params: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_requests_offline_access_and_consent() {
        let e = endpoints_for(AccountKind::Gmail, None);
        assert_eq!(e.token_url, "https://oauth2.googleapis.com/token");
        assert_eq!(e.scopes, vec![GMAIL_SCOPE.to_string()]);
        assert!(
            e.extra_auth_params
                .contains(&("access_type".to_string(), "offline".to_string()))
        );
        assert!(
            e.extra_auth_params
                .contains(&("prompt".to_string(), "consent".to_string()))
        );
    }

    #[test]
    fn m365_defaults_to_common_tenant_and_honours_override() {
        let e = endpoints_for(AccountKind::M365, None);
        assert_eq!(
            e.auth_url,
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
        );
        assert!(e.scopes.iter().any(|s| s == "offline_access"));
        assert!(e.scopes.iter().any(|s| s.ends_with("/Mail.Send")));
        let e = endpoints_for(AccountKind::M365, Some("contoso.onmicrosoft.com"));
        assert_eq!(
            e.token_url,
            "https://login.microsoftonline.com/contoso.onmicrosoft.com/oauth2/v2.0/token"
        );
    }
}
