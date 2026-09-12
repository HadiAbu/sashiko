use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iat: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typ: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub email: String,
    pub iat: Option<usize>,
    pub sid: Option<String>,
}

impl AuthUser {
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            iat: None,
            sid: None,
        }
    }
}

pub fn create_token(
    email: &str,
    secret: &str,
    typ: Option<String>,
    expiration_secs: u64,
) -> Result<String, jsonwebtoken::errors::Error> {
    create_token_with_session(email, secret, typ, expiration_secs, None, None)
}

pub fn create_token_with_session(
    email: &str,
    secret: &str,
    typ: Option<String>,
    expiration_secs: u64,
    iat: Option<usize>,
    sid: Option<String>,
) -> Result<String, jsonwebtoken::errors::Error> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

    let issued_at = iat.unwrap_or(now as usize);
    let session_id = sid.or_else(|| {
        if typ.as_deref() == Some("session") {
            Some(format!("{:032x}", fastrand::u128(..)))
        } else {
            None
        }
    });

    let claims = Claims {
        sub: email.to_owned(),
        exp: (now + expiration_secs) as usize,
        iat: Some(issued_at),
        sid: session_id,
        typ,
    };

    encode(
        &Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_ref()),
    )
}

pub fn verify_token(token: &str, secret: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    let validation = Validation::new(jsonwebtoken::Algorithm::HS256);
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_ref()),
        &validation,
    )
    .map(|data| data.claims)
}

impl FromRequestParts<std::sync::Arc<crate::api::AppState>> for AuthUser {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &std::sync::Arc<crate::api::AppState>,
    ) -> Result<Self, Self::Rejection> {
        let auth_header = parts
            .headers
            .get("Authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "));

        let token = match auth_header {
            Some(token) => token,
            None => {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    "Please log in to access bugs repository.",
                ));
            }
        };

        // Use the same configured secret as the login and authorization endpoints.
        let secret = match state
            .settings
            .server
            .jwt_secret
            .clone()
            .or_else(|| std::env::var("JWT_SECRET").ok())
        {
            Some(s) => s,
            None => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "JWT_SECRET is not configured on the server.",
                ));
            }
        };

        match verify_token(token, &secret) {
            Ok(claims) => {
                if claims.typ.as_deref() != Some("session") {
                    return Err((
                        StatusCode::UNAUTHORIZED,
                        "Invalid token type. Only session tokens are accepted.",
                    ));
                }
                Ok(AuthUser {
                    email: claims.sub,
                    iat: claims.iat,
                    sid: claims.sid,
                })
            }
            Err(_) => Err((
                StatusCode::UNAUTHORIZED,
                "Your session has expired or is invalid. Please log in again.",
            )),
        }
    }
}

pub struct OptionalAuthUser(pub Option<AuthUser>);

impl FromRequestParts<std::sync::Arc<crate::api::AppState>> for OptionalAuthUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &std::sync::Arc<crate::api::AppState>,
    ) -> Result<Self, Self::Rejection> {
        match AuthUser::from_request_parts(parts, state).await {
            Ok(user) => Ok(OptionalAuthUser(Some(user))),
            Err(_) => Ok(OptionalAuthUser(None)),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn test_create_and_verify_token() {
        let email = "test@example.com";
        let secret = "super_secret_key";

        let token = create_token(email, secret, Some("session".to_string()), 24 * 3600)
            .expect("Failed to create token");
        assert!(!token.is_empty());

        let claims = verify_token(&token, secret).expect("Failed to verify token");
        assert_eq!(claims.sub, email);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs() as usize;

        assert!(claims.exp > now);
        assert!(claims.exp <= now + 24 * 3600);
    }

    #[test]
    fn test_verify_token_invalid_secret() {
        let email = "test@example.com";
        let token = create_token(email, "secret1", None, 3600).unwrap();
        let result = verify_token(&token, "secret2");
        assert!(result.is_err());
    }

    #[test]
    fn test_session_token_generates_iat_and_sid() {
        let email = "test@example.com";
        let secret = "super_secret_key";
        let token = create_token(email, secret, Some("session".to_string()), 3600).unwrap();
        let claims = verify_token(&token, secret).unwrap();
        assert!(claims.iat.is_some());
        assert!(claims.sid.is_some());
        assert_eq!(claims.sid.as_ref().unwrap().len(), 32);
    }

    #[test]
    fn test_verify_token_rejects_unpinned_algorithm() {
        let email = "test@example.com";
        let secret = "super_secret_key";
        let claims = Claims {
            sub: email.to_string(),
            exp: 9999999999,
            iat: None,
            sid: None,
            typ: Some("session".to_string()),
        };
        // Encode with HS384 instead of HS256
        let token = encode(
            &Header::new(jsonwebtoken::Algorithm::HS384),
            &claims,
            &EncodingKey::from_secret(secret.as_ref()),
        )
        .unwrap();

        let result = verify_token(&token, secret);
        assert!(result.is_err());
    }
}
