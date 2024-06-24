use crate::constants::{PEER_IP_HEADER_NAME, PROXY_MODE, TRUSTED_PROXIES};
use actix_web::dev::ServiceRequest;
use actix_web::http::header::HeaderMap;
use actix_web::HttpRequest;
use base64::{engine, engine::general_purpose, Engine as _};
use gethostname::gethostname;
use rand::distributions::Alphanumeric;
use rand::Rng;
use rauthy_error::{ErrorResponse, ErrorResponseType};
use std::env;
use std::net::IpAddr;
use std::str::FromStr;
use tracing::{error, trace};

const B64_URL_SAFE: engine::GeneralPurpose = general_purpose::URL_SAFE;
const B64_URL_SAFE_NO_PAD: engine::GeneralPurpose = general_purpose::URL_SAFE_NO_PAD;
const B64_STD: engine::GeneralPurpose = general_purpose::STANDARD;

// Returns the cache key for a given client
pub fn cache_entry_client(id: &str) -> String {
    format!("client_{}", id)
}

// Converts a given Json array / list into a Vec<String>
pub fn json_arr_to_vec(arr: &str) -> Vec<String> {
    arr.chars()
        .skip(1)
        .filter(|&c| c != '"')
        // TODO improve -> array inside array would not work
        .take_while(|&c| c != ']')
        .collect::<String>()
        .split(',')
        .map(|i| i.to_string())
        .collect()
}

pub fn get_local_hostname() -> String {
    let hostname_os = gethostname();
    hostname_os
        .to_str()
        .expect("Error getting the hostname from the OS")
        .to_string()
}

// Returns an alphanumeric random String with the requested length
pub fn get_rand(count: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(count)
        .map(char::from)
        .collect::<String>()
}

#[inline(always)]
pub fn base64_encode(input: &[u8]) -> String {
    B64_STD.encode(input)
}

#[inline(always)]
pub fn base64_decode(b64: &str) -> Result<Vec<u8>, ErrorResponse> {
    B64_STD
        .decode(b64)
        .map_err(|_| ErrorResponse::new(ErrorResponseType::BadRequest, "B64 decoding error"))
}

// Returns the given input as a base64 URL Encoded String
#[inline(always)]
pub fn base64_url_encode(input: &[u8]) -> String {
    let b64 = B64_STD.encode(input);
    b64.chars()
        .filter_map(|c| match c {
            '=' => None,
            '+' => Some('-'),
            '/' => Some('_'),
            x => Some(x),
        })
        .collect()
}

#[inline(always)]
pub fn base64_url_no_pad_encode(input: &[u8]) -> String {
    B64_URL_SAFE_NO_PAD.encode(input)
}

#[inline(always)]
pub fn base64_url_decode(b64: &str) -> Result<Vec<u8>, ErrorResponse> {
    B64_URL_SAFE
        .decode(b64)
        .map_err(|_| ErrorResponse::new(ErrorResponseType::BadRequest, "B64 decoding error"))
}

#[inline(always)]
pub fn base64_url_no_pad_decode(b64: &str) -> Result<Vec<u8>, ErrorResponse> {
    B64_URL_SAFE_NO_PAD
        .decode(b64)
        .map_err(|_| ErrorResponse::new(ErrorResponseType::BadRequest, "B64 decoding error"))
}

pub fn new_store_id() -> String {
    get_rand(24)
}

// Extracts the claims from a given token into a HashMap.
// Returns an empty HashMap if no values could be extracted at all.
// CAUTION: Does not validate the token!
pub fn extract_token_claims_unverified<T>(token: &str) -> Result<T, ErrorResponse>
where
    T: for<'a> serde::Deserialize<'a>,
{
    let body = match token.split_once('.') {
        None => None,
        Some((_metadata, rest)) => rest.split_once('.').map(|(body, _validation_str)| body),
    };
    if body.is_none() {
        return Err(ErrorResponse::new(
            ErrorResponseType::Unauthorized,
            "Invalid or malformed JWT Token",
        ));
    }
    let body = body.unwrap();

    let b64 = match B64_URL_SAFE_NO_PAD.decode(body) {
        Ok(values) => values,
        Err(err) => {
            error!(
                "Error decoding JWT token body '{}' from base64: {}",
                body, err
            );
            return Err(ErrorResponse::new(
                ErrorResponseType::BadRequest,
                "Invalid JWT Token body",
            ));
        }
    };
    let s = String::from_utf8_lossy(b64.as_slice());
    let claims = match serde_json::from_str::<T>(s.as_ref()) {
        Ok(claims) => claims,
        Err(err) => {
            error!("Error deserializing JWT Token claims: {}", err);
            return Err(ErrorResponse::new(
                ErrorResponseType::BadRequest,
                "Invalid JWT Token claims",
            ));
        }
    };

    Ok(claims)
}

// TODO unify real_ip_from_req and real_ip_from_svc_req by using an impl Trait
#[inline(always)]
pub fn real_ip_from_req(req: &HttpRequest) -> Result<IpAddr, ErrorResponse> {
    let peer_ip = parse_peer_addr(req.connection_info().peer_addr())?;
    if let Some(ip) = ip_from_cust_header(req.headers()) {
        check_trusted_proxy(&peer_ip)?;
        Ok(ip)
    } else if *PROXY_MODE {
        check_trusted_proxy(&peer_ip)?;
        parse_peer_addr(req.connection_info().realip_remote_addr())
    } else {
        Ok(peer_ip)
    }
}

#[inline(always)]
pub fn real_ip_from_svc_req(req: &ServiceRequest) -> Result<IpAddr, ErrorResponse> {
    let peer_ip = parse_peer_addr(req.connection_info().peer_addr())?;
    if let Some(ip) = ip_from_cust_header(req.headers()) {
        check_trusted_proxy(&peer_ip)?;
        Ok(ip)
    } else if *PROXY_MODE {
        check_trusted_proxy(&peer_ip)?;
        parse_peer_addr(req.connection_info().realip_remote_addr())
    } else {
        Ok(peer_ip)
    }
}

#[inline(always)]
fn parse_peer_addr(peer_addr: Option<&str>) -> Result<IpAddr, ErrorResponse> {
    match peer_addr {
        None => Err(ErrorResponse::new(
            ErrorResponseType::BadRequest,
            "No IP Addr in Connection Info - this should only happen in tests",
        )),
        Some(peer) => match IpAddr::from_str(peer) {
            Ok(ip) => Ok(ip),
            Err(err) => Err(ErrorResponse::new(
                ErrorResponseType::BadRequest,
                format!("Cannot parse peer IP address: {}", err),
            )),
        },
    }
}

#[inline(always)]
fn check_trusted_proxy(peer_ip: &IpAddr) -> Result<(), ErrorResponse> {
    for cidr in &*TRUSTED_PROXIES {
        if cidr.contains(peer_ip) {
            return Ok(());
        }
    }

    error!(
        "Invalid request from IP {} which is not a trusted proxy",
        peer_ip
    );
    Err(ErrorResponse::new(
        ErrorResponseType::BadRequest,
        "Invalid IP Address",
    ))
}

pub(crate) fn build_trusted_proxies() -> Vec<cidr::IpCidr> {
    let raw = env::var("TRUSTED_PROXIES").expect("TRUSTED_PROXIES is not net set");
    let mut proxies = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match cidr::IpCidr::from_str(trimmed) {
            Ok(cidr) => {
                proxies.push(cidr);
            }
            Err(err) => {
                error!("Cannot parse trusted proxy entry to CIDR: {}", err);
            }
        }
    }
    proxies
}

#[inline(always)]
fn ip_from_cust_header(headers: &HeaderMap) -> Option<IpAddr> {
    // If a custom override has been set, try this first and use the default as fallback
    if let Some(header_name) = &*PEER_IP_HEADER_NAME {
        if let Some(Ok(value)) = headers.get(header_name).map(|s| s.to_str()) {
            match IpAddr::from_str(value) {
                Ok(ip) => {
                    return Some(ip);
                }
                Err(err) => {
                    error!("Cannot parse IP from PEER_IP_HEADER_NAME: {}", err);
                }
            }
        }
        trace!("no PEER IP from PEER_IP_HEADER_NAME: '{}'", header_name);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::string::String;

    #[test]
    fn test_json_arr_to_vec() {
        let arr = String::from("[\"one\",\"two\",\"three\"]");
        let arr_as_vec = vec!["one", "two", "three"];
        assert_eq!(json_arr_to_vec(&arr), arr_as_vec);

        let arr = String::from("[\"one\"]");
        let arr_as_vec = vec!["one"];
        assert_eq!(json_arr_to_vec(&arr), arr_as_vec);

        let arr = String::from("[]");
        let arr_as_vec = vec![""];
        assert_eq!(json_arr_to_vec(&arr), arr_as_vec);
    }

    #[test]
    fn test_get_rand() {
        let rnd = get_rand(11);
        assert_eq!(rnd.len(), 11);

        let rnd = get_rand(0);
        assert_eq!(rnd.len(), 0);

        let rnd = get_rand(1024);
        assert_eq!(rnd.len(), 1024);
    }

    #[test]
    fn test_trusted_proxy_check() {
        env::set_var(
            "TRUSTED_PROXIES",
            r#"
            192.168.100.0/24
            192.168.0.96/28
            172.16.0.1/32
            10.10.10.10/31"#,
        );

        println!("{:?}", build_trusted_proxies());

        assert!(check_trusted_proxy(&IpAddr::from_str("192.168.100.1").unwrap()).is_ok());
        assert!(check_trusted_proxy(&IpAddr::from_str("192.168.100.255").unwrap()).is_ok());
        assert!(check_trusted_proxy(&IpAddr::from_str("192.168.99.1").unwrap()).is_err());
        assert!(check_trusted_proxy(&IpAddr::from_str("192.168.99.255").unwrap()).is_err());

        assert!(check_trusted_proxy(&IpAddr::from_str("192.168.0.96").unwrap()).is_ok());
        assert!(check_trusted_proxy(&IpAddr::from_str("192.168.0.111").unwrap()).is_ok());
        assert!(check_trusted_proxy(&IpAddr::from_str("192.168.0.95").unwrap()).is_err());
        assert!(check_trusted_proxy(&IpAddr::from_str("192.168.0.112").unwrap()).is_err());

        assert!(check_trusted_proxy(&IpAddr::from_str("172.16.0.1").unwrap()).is_ok());
        assert!(check_trusted_proxy(&IpAddr::from_str("172.16.0.2").unwrap()).is_err());

        assert!(check_trusted_proxy(&IpAddr::from_str("10.10.10.10").unwrap()).is_ok());
        assert!(check_trusted_proxy(&IpAddr::from_str("10.10.10.11").unwrap()).is_ok());
        assert!(check_trusted_proxy(&IpAddr::from_str("10.10.10.9").unwrap()).is_err());
        assert!(check_trusted_proxy(&IpAddr::from_str("10.10.10.12").unwrap()).is_err());
    }
}
