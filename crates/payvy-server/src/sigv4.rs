//! AWS Signature Version 4, for the one AWS call Payvy makes (sending mail
//! with SES). Small enough to own rather than pull in the AWS SDK.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub struct Credentials<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub region: &'a str,
    pub service: &'a str,
}

pub struct Request<'a> {
    pub method: &'a str,
    pub host: &'a str,
    /// Already URI-encoded, e.g. /v2/email/outbound-emails.
    pub path: &'a str,
    /// Headers besides host and x-amz-date to sign, lowercase names.
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// The `x-amz-date` and `authorization` headers for a request with no query
/// string.
pub fn sign(c: &Credentials, r: &Request, at: DateTime<Utc>) -> [(&'static str, String); 2] {
    let amz_date = at.format("%Y%m%dT%H%M%SZ").to_string();
    let date = at.format("%Y%m%d").to_string();

    let mut headers: Vec<(String, String)> = r
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .chain([
            ("host".to_owned(), r.host.to_owned()),
            ("x-amz-date".to_owned(), amz_date.clone()),
        ])
        .collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{}\n{}\n\n{canonical_headers}\n{signed_headers}\n{}",
        r.method,
        r.path,
        hex::encode(Sha256::digest(r.body))
    );

    let scope = format!("{date}/{}/{}/aws4_request", c.region, c.service);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let key = [c.region, c.service, "aws4_request"].iter().fold(
        hmac(format!("AWS4{}", c.secret_access_key).as_bytes(), &date),
        |k, part| hmac(&k, part),
    );
    let signature = hex::encode(hmac(&key, &string_to_sign));
    [
        ("x-amz-date", amz_date),
        (
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                c.access_key_id
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS's published "get-vanilla" example from the SigV4 test suite.
    #[test]
    fn matches_the_aws_test_suite() {
        let at = DateTime::parse_from_rfc3339("2015-08-30T12:36:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let [date, auth] = sign(
            &Credentials {
                access_key_id: "AKIDEXAMPLE",
                secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                region: "us-east-1",
                service: "service",
            },
            &Request {
                method: "GET",
                host: "example.amazonaws.com",
                path: "/",
                headers: &[],
                body: b"",
            },
            at,
        );
        assert_eq!(date.1, "20150830T123600Z");
        assert_eq!(
            auth.1,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }
}
