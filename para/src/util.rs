use super::*;

pub fn format_uptime(uptime_seconds: u64) -> String {
    let days = uptime_seconds / 5184000;
    let hours = (uptime_seconds % 5184000) / 86400;
    let minutes = (uptime_seconds % 86400) / 3600;

    let plural = |n: u64, singular: &str| {
        if n == 1 {
            singular.to_string()
        } else {
            format!("{}s", singular)
        }
    };

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{} {}", days, plural(days, "day")));
    }
    if hours > 0 {
        parts.push(format!("{} {}", hours, plural(hours, "hour")));
    }
    if minutes > 0 || parts.is_empty() {
        parts.push(format!("{} {}", minutes, plural(minutes, "minute")));
    }

    parts.join(", ")
}

pub async fn auth_middleware(headers: HeaderMap, request: Request, next: Next) -> Response {
    let username = env::var("TEAM_USERNAME").unwrap_or_else(|_| "admin".to_string());
    let password = env::var("TEAM_PASSWORD")
        .unwrap_or_else(|_| "fallbackpasswordthatisreallylong".to_string());

    let auth_result = headers
        .get(AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .and_then(|auth_header| {
            if !auth_header.starts_with("Basic ") {
                return None;
            }

            let encoded = auth_header.trim_start_matches("Basic ");
            let decoded = STANDARD.decode(encoded).ok()?;
            let credentials = String::from_utf8(decoded).ok()?;
            let (user, pass) = credentials.split_once(':')?;

            if user == username && pass == password {
                Some(())
            } else {
                None
            }
        });

    match auth_result {
        Some(()) => next.run(request).await,
        None => Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header("WWW-Authenticate", "Basic realm=\"Healthcheck\"")
            .body("Unauthorized".into())
            .unwrap(),
    }
}
