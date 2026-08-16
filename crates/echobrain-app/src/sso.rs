//! How much of the AWS SSO session is left, and whether it can renew itself.
//!
//! # Why the app reads a cache file the SDK already reads
//!
//! Nothing here refreshes anything. `aws-config` does that on its own, from
//! inside this process: `SsoTokenProvider` renews the access token five minutes
//! before it expires and rewrites the same file. Doing it a second time here
//! would race with it over that file for no gain.
//!
//! What the SDK cannot do is *say so in advance*. Its two outcomes are a working
//! call and `ExpiredToken`, and the second one reaches the user as a search that
//! failed for no visible reason, hours after the login that would have prevented
//! it. So this module reads the same cache the SDK reads and answers two
//! questions the connection screen can act on: when does this end, and will it
//! end quietly or on its own.
//!
//! # The distinction that actually matters
//!
//! A profile written the old way — `sso_start_url` directly under
//! `[profile x]`, with no `[sso-session]` block — gets a token with no
//! `refreshToken` in it, because the CLI never asked for the `sso:account:access`
//! scope that issues one. `aws-config` requires the refresh token, the client
//! registration and its expiry to be present before it will even try
//! (`CachedSsoToken::refreshable`), and for that shape of profile it does not
//! consult the refreshing token provider at all: it loads the cached token
//! directly and uses it until it dies.
//!
//! Which means "renewable" is not a property of the session, the account or the
//! app. It is decided by four lines in `~/.aws/config`, it is invisible from
//! everywhere else, and it is the difference between a login that lasts as long
//! as the app is open and one that dies in eight hours. That is the fact
//! [`SsoStatus::renewable`] carries to the screen.

use std::path::PathBuf;

use sha1::{Digest, Sha1};

/// Where the profile's token lives in the cache — and, by which variant it is,
/// whether that token can be refreshed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SsoReference {
    /// `sso_session = name`, the modern form. The cache entry is keyed by the
    /// **session name**, and this is the only form that carries a refresh token.
    Session(String),
    /// `sso_start_url = …` written straight onto the profile, the legacy form.
    /// The cache entry is keyed by the **start URL** and cannot be refreshed.
    StartUrl(String),
}

impl SsoReference {
    /// The string the cache file name is the SHA-1 of.
    pub fn identifier(&self) -> &str {
        match self {
            Self::Session(name) => name,
            Self::StartUrl(url) => url,
        }
    }
}

/// What the connection screen needs to know about the current login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsoStatus {
    /// The `[sso-session]` name, when the profile uses one. `None` is the legacy
    /// shape, and the reason [`Self::renewable`] is false.
    pub session: Option<String>,
    /// Unix seconds. Absolute rather than relative so the screen can recompute
    /// the countdown without asking again.
    pub expires_at: i64,
    /// Seconds remaining, negative once it has expired.
    pub expires_in: i64,
    /// Whether the SDK can renew this token without a browser, by the same rule
    /// the SDK applies: refresh token, client registration, and a registration
    /// that has not itself expired.
    pub renewable: bool,
}

/// The SSO wiring of `profile`, read from `~/.aws/config`.
///
/// Pure, and separate from [`status`], because every interesting case is about
/// parsing: which section wins, which of the two spellings is in use, and
/// whether the profile does SSO at all.
pub fn sso_reference(config_text: &str, profile: &str) -> Option<SsoReference> {
    // `~/.aws/config` writes `[profile name]`, except for the default profile,
    // which is a bare `[default]`. Both are the same profile to the caller.
    let wanted: [String; 2] = [format!("profile {profile}"), profile.to_string()];

    let mut in_profile = false;
    let mut session = None;
    let mut start_url = None;

    for line in config_text.lines().map(str::trim) {
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let section = section.trim();
            // Leaving the profile ends the search: a later `[sso-session corp]`
            // block has the same keys, and reading them as though they belonged
            // to the profile would report a session for a profile that has none.
            if in_profile {
                break;
            }
            in_profile = wanted.iter().any(|name| name == section);
            continue;
        }

        if !in_profile {
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "sso_session" => session = Some(value.trim().to_string()),
                "sso_start_url" => start_url = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }

    // `sso_session` wins when both are present, which is what a half-migrated
    // profile looks like — and it is also what the SDK does.
    match (session, start_url) {
        (Some(name), _) if !name.is_empty() => Some(SsoReference::Session(name)),
        (_, Some(url)) if !url.is_empty() => Some(SsoReference::StartUrl(url)),
        _ => None,
    }
}

/// `~/.aws/sso/cache/<sha1 of the identifier>.json`, the file the SDK reads.
pub fn cache_path(reference: &SsoReference) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let digest = Sha1::digest(reference.identifier().as_bytes());
    let name: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Some(home.join(".aws/sso/cache").join(format!("{name}.json")))
}

/// The two facts worth knowing from a cached token, given the current time.
///
/// Pure for the same reason as [`sso_reference`]: the rules are about which
/// fields are present, and none of them are about the filesystem.
pub fn parse_cached_token(json: &str, now: i64) -> Option<(i64, bool)> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let expires_at = parse_timestamp(value.get("expiresAt")?.as_str()?)?;

    let present = |key: &str| value.get(key).and_then(serde_json::Value::as_str).is_some();

    // The SDK's own rule, in the SDK's own order: it will not attempt a refresh
    // without all three, and it treats an expired client registration as
    // unrefreshable even when the refresh token is right there.
    let registration_alive = value
        .get("registrationExpiresAt")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_timestamp)
        .is_some_and(|expiry| expiry > now);

    let renewable = present("refreshToken")
        && present("clientId")
        && present("clientSecret")
        && registration_alive;

    Some((expires_at, renewable))
}

/// Unix seconds from the timestamps the SSO cache actually contains.
///
/// Three shapes appear in one real `~/.aws/sso/cache`: `2026-08-15T20:47:06Z`,
/// `2025-09-21T22:48:23.688Z`, and `2026-05-27T07:30:16.626UTC` — the last one
/// written by a toolkit rather than the CLI. All three agree on the first
/// nineteen characters and all three are UTC, so that prefix is the whole
/// parse; sub-second precision is noise on a value measured in hours.
pub fn parse_timestamp(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }

    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days between 1970-01-01 and the given date, by Howard Hinnant's algorithm.
///
/// Hand-rolled rather than pulling in a date crate for one conversion, the same
/// trade as `config::dirs_config_dir`. It is a closed-form expression with no
/// lookup tables and no leap-year special cases.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The live session for `profile`, or `None` when there is nothing to report.
///
/// `None` covers three different situations the screen treats identically,
/// because none of them is a problem to warn about: the profile does not use
/// SSO, the machine has no `~/.aws/config`, or nobody has logged in yet.
pub fn status(profile: &str) -> Option<SsoStatus> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let config = std::fs::read_to_string(home.join(".aws/config")).ok()?;
    let reference = sso_reference(&config, profile)?;

    let token = std::fs::read_to_string(cache_path(&reference)?).ok()?;
    let now = now_seconds();
    let (expires_at, renewable) = parse_cached_token(&token, now)?;

    Some(SsoStatus {
        session: match &reference {
            SsoReference::Session(name) => Some(name.clone()),
            SsoReference::StartUrl(_) => None,
        },
        expires_at,
        expires_in: expires_at - now,
        renewable,
    })
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The modern shape. The cache entry is keyed by the session name, so
    /// reading the start URL out of the `[sso-session]` block — which is where
    /// it now lives — would look for a file that does not exist.
    #[test]
    fn a_session_profile_is_keyed_by_the_session_name() {
        let reference = sso_reference(
            r#"
            [sso-session corp]
            sso_start_url = https://example.awsapps.com/start
            sso_region = sa-east-1
            sso_registration_scopes = sso:account:access

            [profile work]
            sso_session = corp
            sso_account_id = 111122223333
            sso_role_name = AdministratorAccess
            "#,
            "work",
        );

        assert_eq!(reference, Some(SsoReference::Session("corp".into())));
    }

    /// The legacy shape, and the one that cannot refresh. Recognising it is the
    /// whole point of this module: it is indistinguishable from the modern one
    /// until the token dies.
    #[test]
    fn a_legacy_profile_is_keyed_by_the_start_url() {
        let reference = sso_reference(
            r#"
            [profile old]
            sso_start_url = https://example.awsapps.com/start
            sso_region = sa-east-1
            sso_account_id = 111122223333
            "#,
            "old",
        );

        assert_eq!(
            reference,
            Some(SsoReference::StartUrl(
                "https://example.awsapps.com/start".into()
            ))
        );
    }

    /// A profile's keys stop at the next section header. Without that, the
    /// `[sso-session]` block below would lend its `sso_start_url` to a profile
    /// that signs with static keys, and the screen would announce an SSO
    /// session that has nothing to do with it.
    #[test]
    fn keys_from_a_later_section_do_not_leak_into_the_profile() {
        let text = r#"
            [profile static]
            region = us-east-1

            [sso-session corp]
            sso_start_url = https://example.awsapps.com/start
            "#;

        assert_eq!(sso_reference(text, "static"), None);
    }

    /// `[default]` is spelled without the `profile ` prefix, and it is what the
    /// screen asks about when no profile is named — the default chain reads
    /// exactly that section.
    #[test]
    fn the_default_profile_is_found_under_its_bare_section_name() {
        let text = "[default]\nsso_session = corp\n";
        assert_eq!(
            sso_reference(text, "default"),
            Some(SsoReference::Session("corp".into()))
        );
    }

    /// The file name is the SDK's, not ours: get it wrong and the app reports
    /// "no session" while the SDK is happily using one.
    #[test]
    fn the_cache_file_is_named_after_the_sha1_of_the_identifier() {
        // SAFETY-free equivalent of the SDK's own hashing, checked against
        // values computed outside this codebase.
        let session = cache_path(&SsoReference::Session("corp".into())).expect("HOME is set");
        assert!(
            session.ends_with("ee0bfd2552fbd840c02cc48b6e823320543c450f.json"),
            "unexpected cache path: {}",
            session.display()
        );

        let legacy = cache_path(&SsoReference::StartUrl(
            "https://example.awsapps.com/start".into(),
        ))
        .expect("HOME is set");
        assert!(
            legacy.ends_with("e8be5486177c5b5392bd9aa76563515b29358e6e.json"),
            "unexpected cache path: {}",
            legacy.display()
        );
    }

    /// The exact shape `aws sso login` writes for a legacy profile: a client
    /// registration, an access token, and no way to renew either.
    #[test]
    fn a_token_without_a_refresh_token_cannot_be_renewed() {
        let json = r#"{
            "startUrl": "https://example.awsapps.com/start",
            "region": "sa-east-1",
            "accessToken": "secret",
            "expiresAt": "2026-08-15T20:47:06Z",
            "clientId": "id",
            "clientSecret": "shh",
            "registrationExpiresAt": "2026-11-09T00:17:11Z"
        }"#;

        let (expires_at, renewable) = parse_cached_token(json, 0).expect("the token parses");

        assert_eq!(expires_at, 1_786_826_826);
        assert!(
            !renewable,
            "no refreshToken means the SDK will not even attempt a refresh"
        );
    }

    /// The same file after a login through an `[sso-session]`, which is the
    /// state the warning is trying to get the user to.
    #[test]
    fn a_token_with_a_refresh_token_can_be_renewed() {
        let json = r#"{
            "accessToken": "secret",
            "expiresAt": "2026-08-15T20:47:06Z",
            "refreshToken": "renew-me",
            "clientId": "id",
            "clientSecret": "shh",
            "registrationExpiresAt": "2026-11-09T00:17:11Z"
        }"#;

        let (_, renewable) = parse_cached_token(json, 0).expect("the token parses");
        assert!(renewable);
    }

    /// The client registration outlives the access token by months, but not
    /// forever — and once it lapses, the refresh token is useless. The SDK
    /// checks this separately, so the screen has to as well or it would promise
    /// a renewal that cannot happen.
    #[test]
    fn an_expired_client_registration_cannot_renew_anything() {
        let json = r#"{
            "accessToken": "secret",
            "expiresAt": "2026-08-15T20:47:06Z",
            "refreshToken": "renew-me",
            "clientId": "id",
            "clientSecret": "shh",
            "registrationExpiresAt": "2020-01-01T00:00:00Z"
        }"#;

        let now = 1_786_500_000; // well past 2020
        let (_, renewable) = parse_cached_token(json, now).expect("the token parses");
        assert!(!renewable);
    }

    /// All three spellings found in one real cache directory. A parse that
    /// handled only the CLI's would report "no session" for a token that is
    /// perfectly valid.
    #[test]
    fn every_timestamp_shape_in_a_real_cache_directory_parses() {
        assert_eq!(parse_timestamp("2026-08-15T20:47:06Z"), Some(1_786_826_826));
        assert_eq!(
            parse_timestamp("2025-09-21T22:48:23.688Z"),
            Some(1_758_494_903)
        );
        assert_eq!(
            parse_timestamp("2026-05-27T07:30:16.626UTC"),
            Some(1_779_867_016)
        );
        assert_eq!(parse_timestamp("not a date"), None);
        assert_eq!(parse_timestamp(""), None);
    }
}
